//! Per-binding ownership of store services.
use crate::{
    installation::{
        config,
        registry::{ConfigRegistry, ConfigSnapshot, ConfigState},
    },
    ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus, RuntimeError},
    projection::ProjectionConfig,
    protocol::{BindingRef, RejectReason},
    store::{
        identity,
        service::{StoreDeps, StoreService},
    },
};
use agentlaw_contracts::RequestControl;
use agentlaw_storage::BindingKey;
use std::{
    collections::HashMap,
    ops::Deref,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, SystemTime},
};

enum Entry {
    Opening,
    Open {
        key: BindingKey,
        service: Arc<StoreService>,
        active: usize,
        staged: bool,
    },
    Closing,
}

struct State {
    entries: HashMap<BindingRef, Entry>,
    maintaining: HashMap<BindingRef, bool>,
    audits: HashMap<BindingRef, usize>,
    stopping: bool,
}

struct Inner {
    state: Mutex<State>,
    changed: Condvar,
    deps: StoreDeps,
    installation: PathBuf,
    registry: Arc<ConfigRegistry>,
    prewarm_cancel: Arc<AtomicBool>,
    #[cfg(test)]
    observer: Mutex<Option<Arc<dyn Fn(BindingRef, &'static str) + Send + Sync>>>,
}

pub(super) struct Stores(Arc<Inner>);

pub(super) struct MaintenanceGuard {
    inner: Arc<Inner>,
    snapshot: Arc<ConfigSnapshot>,
    reopened: Option<Arc<StoreService>>,
}
pub(super) struct AuditGuard {
    inner: Arc<Inner>,
    reference: BindingRef,
}

impl Stores {
    pub(super) fn acquire_stage(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        stage: &crate::git::git_ops::ImportStageContext,
        control: &RequestControl,
    ) -> agentlaw_contracts::Result<StoreGuard> {
        let identity = identity::resolve_import_stage(&self.0.installation, snapshot, stage)
            .map_err(|e| {
                agentlaw_contracts::DomainError::new("store_unavailable", e.to_string())
            })?;
        let key =
            agentlaw_storage::binding_identity(&identity.root, &identity.local).map_err(|e| {
                agentlaw_contracts::DomainError::new("store_unavailable", e.to_string())
            })?;
        let reference = BindingRef::new(&key, &identity.user_id);
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            control.check()?;
            if state.stopping {
                return Err(agentlaw_contracts::DomainError::new(
                    "runtime_stopping",
                    "The runtime is stopping.",
                ));
            }
            match state.entries.get_mut(&reference) {
                Some(Entry::Open {
                    service, active, ..
                }) => {
                    *active += 1;
                    return Ok(StoreGuard {
                        inner: self.0.clone(),
                        reference,
                        service: service.clone(),
                    });
                }
                Some(_) => {
                    state = self
                        .0
                        .changed
                        .wait_timeout(state, Duration::from_millis(50))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                None => {
                    state.entries.insert(reference, Entry::Opening);
                    drop(state);
                    let result = self.0.open_identity(identity, control);
                    state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    match result {
                        Ok(service) => {
                            let service = Arc::new(service);
                            state.entries.insert(
                                reference,
                                Entry::Open {
                                    key,
                                    service: service.clone(),
                                    active: 1,
                                    staged: true,
                                },
                            );
                            self.0.changed.notify_all();
                            return Ok(StoreGuard {
                                inner: self.0.clone(),
                                reference,
                                service,
                            });
                        }
                        Err(error) => {
                            state.entries.remove(&reference);
                            self.0.changed.notify_all();
                            return Err(match error {
                                RuntimeError::Domain(error) => error,
                                other => agentlaw_contracts::DomainError::new(
                                    "store_unavailable",
                                    other.to_string(),
                                ),
                            });
                        }
                    }
                }
            }
        }
    }
    pub(super) fn close_stage(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        stage: &crate::git::git_ops::ImportStageContext,
        control: &RequestControl,
    ) -> agentlaw_contracts::Result<()> {
        let key = agentlaw_storage::binding_identity(&stage.store_path, &stage.runtime_path)
            .map_err(|e| {
                agentlaw_contracts::DomainError::new("store_unavailable", e.to_string())
            })?;
        let reference = BindingRef::new(&key, &snapshot.config.user_id);
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            control.check()?;
            match state.entries.get(&reference) {
                None => return Ok(()),
                Some(Entry::Open {
                    active: 0,
                    service,
                    staged: true,
                    ..
                }) => {
                    let service = service.clone();
                    state.entries.insert(reference, Entry::Closing);
                    drop(state);
                    self.0.close(reference, service);
                    return Ok(());
                }
                _ => {
                    state = self
                        .0
                        .changed
                        .wait_timeout(state, Duration::from_millis(50))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
    }
    #[cfg(test)]
    pub(super) fn observe(
        &self,
        observer: impl Fn(BindingRef, &'static str) + Send + Sync + 'static,
    ) {
        *self.0.observer.lock().unwrap() = Some(Arc::new(observer));
    }
}

impl Stores {
    #[allow(dead_code)]
    fn notify_observer(&self, _binding: BindingRef, _event: &'static str) {
        #[cfg(test)]
        {
            let observer = self.0.observer.lock().unwrap().clone();
            if let Some(observer) = observer {
                observer(_binding, _event);
            }
        }
    }
}

impl MaintenanceGuard {
    pub(super) fn reopen(&mut self, control: &RequestControl) -> Result<(), RejectReason> {
        if self.reopened.is_none() {
            self.reopened = Some(Arc::new(
                self.inner
                    .open(&self.snapshot, control)
                    .map_err(open_rejection)?,
            ));
        }
        Ok(())
    }
    pub(super) fn service(&self) -> Option<&StoreService> {
        self.reopened.as_deref()
    }
}
impl Drop for MaintenanceGuard {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(service) = self.reopened.take() {
            if state.stopping {
                state
                    .entries
                    .insert(self.snapshot.reference, Entry::Closing);
                drop(state);
                service.close();
                drop(service);
                state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                state.entries.remove(&self.snapshot.reference);
            } else {
                state.entries.insert(
                    self.snapshot.reference,
                    Entry::Open {
                        key: self.snapshot.key.clone(),
                        service,
                        active: 0,
                        staged: false,
                    },
                );
            }
        }
        state.maintaining.remove(&self.snapshot.reference);
        self.inner.changed.notify_all();
    }
}
impl Drop for AuditGuard {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = state.audits.get_mut(&self.reference) {
            *count -= 1;
            if *count == 0 {
                state.audits.remove(&self.reference);
            }
        }
        self.inner.changed.notify_all();
    }
}

pub(super) struct StoreGuard {
    inner: Arc<Inner>,
    reference: BindingRef,
    service: Arc<StoreService>,
}

impl Deref for StoreGuard {
    type Target = StoreService;
    fn deref(&self) -> &StoreService {
        &self.service
    }
}

impl Inner {
    fn close(&self, reference: BindingRef, service: Arc<StoreService>) {
        service.close();
        drop(service);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.entries.remove(&reference);
        self.changed.notify_all();
    }
    fn open(
        &self,
        snapshot: &ConfigSnapshot,
        control: &RequestControl,
    ) -> Result<StoreService, RuntimeError> {
        if matches!(self.registry.current(), ConfigState::Ready(current) if current.key == snapshot.key)
        {
            config::require_existing_binding(&self.installation, &snapshot.config)?;
        }
        let identity = identity::resolve(&self.installation, snapshot)?;
        self.open_identity(identity, control)
    }
    fn open_identity(
        &self,
        identity: agentlaw_flows::store::StoreIdentity,
        control: &RequestControl,
    ) -> Result<StoreService, RuntimeError> {
        let deps = StoreDeps {
            health: self.deps.health.clone(),
            semantic: self.deps.semantic.clone(),
            indexer: self.deps.indexer.clone(),
            projection: ProjectionConfig {
                clock: self.deps.projection.clock.clone(),
                retry_initial: self.deps.projection.retry_initial,
                retry_max: self.deps.projection.retry_max,
            },
            coordination: self.deps.coordination.clone(),
            state: self.deps.state.clone(),
        };
        StoreService::open(identity, deps, control)
    }
}

fn open_rejection(error: RuntimeError) -> RejectReason {
    match error {
        RuntimeError::Domain(error) => RejectReason::InvalidBody(error),
        other => RejectReason::StoreUnavailable(other.to_string()),
    }
}

impl Stores {
    pub(super) fn installation(&self) -> &std::path::Path {
        &self.0.installation
    }
    pub(super) fn indexing_status(&self, snapshot: &ConfigSnapshot) -> serde_json::Value {
        let marker = snapshot
            .config
            .runtime_root(&self.0.installation)
            .join("control-initialized.json");
        let binding = std::fs::read(marker)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| value["binding_id"].as_str().map(str::to_owned));
        let Some(binding) = binding else {
            return serde_json::Value::Null;
        };
        let components: Vec<_> = self.0.deps.health.snapshot().into_iter()
            .filter(|(id, _)| id.binding.as_deref() == Some(binding.as_str()) && matches!(id.kind, ComponentKind::Exact | ComponentKind::Semantic))
            .map(|(id, status)| serde_json::json!({"kind":format!("{:?}", id.kind).to_ascii_lowercase(),"state":format!("{:?}", status.state).to_ascii_lowercase(),"reason":status.reason}))
            .collect();
        if components.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!({"components":components})
        }
    }
    pub(super) fn acquire_for_execution(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        control: &RequestControl,
    ) -> Result<StoreGuard, agentlaw_contracts::DomainError> {
        self.acquire_with_control(snapshot, control).map_err(|e| {
            if control.cancel.load(Ordering::Acquire) {
                agentlaw_contracts::DomainError::new("cancelled", "The request was cancelled.")
            } else if let RejectReason::InvalidBody(error) = e {
                error
            } else {
                agentlaw_contracts::DomainError::new("store_unavailable", format!("{e:?}"))
            }
        })
    }
    pub(super) fn maintain(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        control: &RequestControl,
    ) -> Result<MaintenanceGuard, RejectReason> {
        let reference = snapshot.reference;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.maintaining.contains_key(&reference) {
            if state.stopping {
                return Err(RejectReason::Stopping);
            }
            if control.cancel.load(Ordering::Acquire) {
                return Err(RejectReason::InvalidBody(
                    agentlaw_contracts::DomainError::new("cancelled", "The request was cancelled."),
                ));
            }
            self.notify_observer(reference, "maintain_waiting");
            state = self
                .0
                .changed
                .wait_timeout(state, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if state.stopping {
            return Err(RejectReason::Stopping);
        }
        if control.cancel.load(Ordering::Acquire) {
            return Err(RejectReason::InvalidBody(
                agentlaw_contracts::DomainError::new("cancelled", "The request was cancelled."),
            ));
        }
        state.maintaining.insert(reference, true);
        self.notify_observer(reference, "maintain_reserved");
        loop {
            if state.stopping || control.cancel.load(Ordering::Acquire) {
                state.maintaining.remove(&reference);
                self.0.changed.notify_all();
                return Err(if state.stopping {
                    RejectReason::Stopping
                } else {
                    RejectReason::InvalidBody(agentlaw_contracts::DomainError::new(
                        "cancelled",
                        "The request was cancelled.",
                    ))
                });
            }
            if state.audits.contains_key(&reference) {
                self.notify_observer(reference, "maintain_waiting");
                state = self
                    .0
                    .changed
                    .wait_timeout(state, Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
                continue;
            }
            match state.entries.get(&reference) {
                None => break,
                Some(Entry::Open {
                    active: 0, service, ..
                }) => {
                    let service = service.clone();
                    state.entries.insert(reference, Entry::Closing);
                    drop(state);
                    let result = service.close_for_maintenance();
                    state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(error) = result {
                        state.maintaining.remove(&reference);
                        self.0.changed.notify_all();
                        let inner = self.0.clone();
                        std::thread::spawn(move || {
                            while service.close_for_maintenance().is_err() {
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            drop(service);
                            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                            state.entries.remove(&reference);
                            inner.changed.notify_all();
                        });
                        return Err(RejectReason::StoreUnavailable(error.to_string()));
                    }
                    state.entries.remove(&reference);
                    self.0.changed.notify_all();
                    break;
                }
                _ => {
                    self.notify_observer(reference, "maintain_waiting");
                    state = self
                        .0
                        .changed
                        .wait_timeout(state, Duration::from_millis(50))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
        Ok(MaintenanceGuard {
            inner: self.0.clone(),
            snapshot: snapshot.clone(),
            reopened: None,
        })
    }
    pub(super) fn audit_guard(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        control: &RequestControl,
    ) -> Result<AuditGuard, RejectReason> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.maintaining.contains_key(&snapshot.reference) {
            if state.stopping {
                return Err(RejectReason::Stopping);
            }
            if control.cancel.load(Ordering::Acquire) {
                return Err(RejectReason::InvalidBody(
                    agentlaw_contracts::DomainError::new("cancelled", "The request was cancelled."),
                ));
            }
            state = self
                .0
                .changed
                .wait_timeout(state, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if state.stopping {
            return Err(RejectReason::Stopping);
        }
        *state.audits.entry(snapshot.reference).or_default() += 1;
        Ok(AuditGuard {
            inner: self.0.clone(),
            reference: snapshot.reference,
        })
    }
    pub(super) fn new(
        installation: PathBuf,
        deps: StoreDeps,
        registry: Arc<ConfigRegistry>,
    ) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                entries: HashMap::new(),
                maintaining: HashMap::new(),
                audits: HashMap::new(),
                stopping: false,
            }),
            changed: Condvar::new(),
            deps,
            installation,
            registry,
            prewarm_cancel: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            observer: Mutex::new(None),
        }))
    }
    pub(super) fn acquire(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
    ) -> Result<StoreGuard, RejectReason> {
        self.acquire_with_control(snapshot, &RequestControl::default())
    }
    fn acquire_with_control(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        control: &RequestControl,
    ) -> Result<StoreGuard, RejectReason> {
        let reference = snapshot.reference;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.stopping {
                return Err(RejectReason::Stopping);
            }
            if control.cancel.load(Ordering::Acquire) {
                return Err(RejectReason::InvalidBody(
                    agentlaw_contracts::DomainError::new("cancelled", "The request was cancelled."),
                ));
            }
            if state.maintaining.contains_key(&reference) {
                self.notify_observer(reference, "acquire_waiting");
                state = self
                    .0
                    .changed
                    .wait_timeout(state, Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
                continue;
            }
            match state.entries.get_mut(&reference) {
                Some(Entry::Open {
                    service, active, ..
                }) => {
                    *active += 1;
                    return Ok(StoreGuard {
                        inner: self.0.clone(),
                        reference,
                        service: service.clone(),
                    });
                }
                Some(_) => {
                    state = self
                        .0
                        .changed
                        .wait_timeout(state, Duration::from_millis(50))
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
                None => {
                    state.entries.insert(reference, Entry::Opening);
                    drop(state);
                    let result = self.0.open(snapshot, control);
                    state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    match result {
                        Ok(service) => {
                            let service = Arc::new(service);
                            state.entries.insert(
                                reference,
                                Entry::Open {
                                    key: snapshot.key.clone(),
                                    service: service.clone(),
                                    active: 1,
                                    staged: false,
                                },
                            );
                            self.0.changed.notify_all();
                            return Ok(StoreGuard {
                                inner: self.0.clone(),
                                reference,
                                service,
                            });
                        }
                        Err(error) => {
                            state.entries.remove(&reference);
                            self.0.changed.notify_all();
                            return Err(open_rejection(error));
                        }
                    }
                }
            }
        }
    }
    pub(super) fn prewarm(&self, snapshot: &Arc<ConfigSnapshot>) {
        let control = RequestControl::new(self.0.prewarm_cancel.clone(), |_| {});
        if let Err(error) = self.acquire_with_control(snapshot, &control) {
            self.0.deps.health.publish(
                ComponentId {
                    binding: identity::resolve(&self.0.installation, snapshot)
                        .ok()
                        .map(|id| id.binding_id),
                    kind: ComponentKind::Store,
                },
                ComponentStatus {
                    state: ComponentState::Degraded,
                    observed_at: SystemTime::now(),
                    reason: Some(format!("{error:?}")),
                },
            );
        }
    }
    pub(super) fn selection_changed(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let current = match self.0.registry.current() {
            ConfigState::Ready(snapshot) => Some(snapshot.key.clone()),
            _ => None,
        };
        let closing: Vec<_> = state
            .entries
            .iter()
            .filter_map(|(reference, entry)| match entry {
                Entry::Open {
                    key,
                    service,
                    active: 0,
                    staged: false,
                } if Some(key) != current.as_ref()
                    && !state.maintaining.contains_key(reference) =>
                {
                    Some((*reference, service.clone()))
                }
                _ => None,
            })
            .collect();
        for (reference, _) in &closing {
            state.entries.insert(*reference, Entry::Closing);
        }
        drop(state);
        for (reference, service) in closing {
            self.0.close(reference, service);
        }
    }
    pub(super) fn close_all(&self) {
        self.stop_opening();
        loop {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.entries.is_empty() && state.maintaining.is_empty() && state.audits.is_empty() {
                return;
            }
            for reference in state.maintaining.keys().chain(state.audits.keys()) {
                self.notify_observer(*reference, "shutdown_waiting");
            }
            let closing: Vec<_> = state
                .entries
                .iter()
                .filter_map(|(reference, entry)| match entry {
                    Entry::Open {
                        service, active: 0, ..
                    } => Some((*reference, service.clone())),
                    _ => None,
                })
                .collect();
            if closing.is_empty() {
                drop(
                    self.0
                        .changed
                        .wait(state)
                        .unwrap_or_else(|e| e.into_inner()),
                );
                continue;
            }
            for (reference, _) in &closing {
                state.entries.insert(*reference, Entry::Closing);
            }
            drop(state);
            for (reference, service) in closing {
                self.0.close(reference, service);
            }
        }
    }
    pub(super) fn stop_opening(&self) {
        self.0.prewarm_cancel.store(true, Ordering::Release);
        {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopping = true;
            self.0.changed.notify_all();
        }
    }
}

impl Drop for StoreGuard {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let selected = match self.inner.registry.current() {
            ConfigState::Ready(snapshot) => Some(snapshot.key.clone()),
            _ => None,
        };
        let maintaining = state.maintaining.contains_key(&self.reference);
        let close = match state.entries.get_mut(&self.reference) {
            Some(Entry::Open {
                key,
                service,
                active,
                staged,
            }) => {
                *active -= 1;
                if *active == 0 && !*staged && !maintaining && selected.as_ref() != Some(key) {
                    Some(service.clone())
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(service) = close {
            state.entries.insert(self.reference, Entry::Closing);
            drop(state);
            self.inner.close(self.reference, service);
        } else {
            self.inner.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests;
