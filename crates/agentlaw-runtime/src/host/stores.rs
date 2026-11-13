//! Per-binding ownership of store services.
use crate::{
    installation::{
        config,
        registry::{ConfigRegistry, ConfigSnapshot, ConfigState},
    },
    ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus},
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
    time::SystemTime,
};

enum Entry {
    Opening,
    Open {
        key: BindingKey,
        service: Arc<StoreService>,
        active: usize,
    },
    Closing,
}

struct State {
    entries: HashMap<BindingRef, Entry>,
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

pub(super) struct MaintenanceGuard;
pub(super) struct AuditGuard;

#[cfg(test)]
impl Stores {
    fn observe(&self, observer: impl Fn(BindingRef, &'static str) + Send + Sync + 'static) {
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
    pub(super) fn reopen(&mut self, _control: &RequestControl) -> Result<(), RejectReason> {
        Err(RejectReason::StoreUnavailable(
            "maintenance unavailable".into(),
        ))
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
    ) -> Result<StoreService, String> {
        if matches!(self.registry.current(), ConfigState::Ready(current) if current.key == snapshot.key)
        {
            config::require_existing_binding(&self.installation, &snapshot.config)
                .map_err(|e| e.to_string())?;
        }
        let identity =
            identity::resolve(&self.installation, snapshot).map_err(|e| e.to_string())?;
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
        StoreService::open(identity, deps, control).map_err(|e| e.to_string())
    }
}

impl Stores {
    fn acquire_for_execution(
        &self,
        _snapshot: &Arc<ConfigSnapshot>,
        _control: &RequestControl,
    ) -> Result<StoreGuard, agentlaw_contracts::DomainError> {
        Err(agentlaw_contracts::DomainError::new(
            "unavailable",
            "Execution acquire is not implemented.",
        ))
    }
    pub(super) fn maintain(
        &self,
        _snapshot: &Arc<ConfigSnapshot>,
        _control: &RequestControl,
    ) -> Result<MaintenanceGuard, RejectReason> {
        Err(RejectReason::StoreUnavailable(
            "maintenance unavailable".into(),
        ))
    }
    pub(super) fn audit_guard(
        &self,
        _snapshot: &Arc<ConfigSnapshot>,
        _control: &RequestControl,
    ) -> Result<AuditGuard, RejectReason> {
        Err(RejectReason::StoreUnavailable("audit unavailable".into()))
    }
    pub(super) fn new(
        installation: PathBuf,
        deps: StoreDeps,
        registry: Arc<ConfigRegistry>,
    ) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                entries: HashMap::new(),
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
                        .wait(state)
                        .unwrap_or_else(|e| e.into_inner())
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
                            return Err(RejectReason::StoreUnavailable(error));
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
                } if Some(key) != current.as_ref() => Some((*reference, service.clone())),
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
            if state.entries.is_empty() {
                return;
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
        let close = match state.entries.get_mut(&self.reference) {
            Some(Entry::Open {
                key,
                service,
                active,
            }) => {
                *active -= 1;
                if *active == 0 && selected.as_ref() != Some(key) {
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
