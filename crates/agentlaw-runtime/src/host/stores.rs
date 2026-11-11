//! Per-binding ownership of store services.
use crate::{
    installation::{
        config, machine,
        registry::{ConfigRegistry, ConfigSnapshot, ConfigState},
    },
    ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus},
    projection::ProjectionConfig,
    protocol::{BindingRef, RejectReason},
    store::service::{StoreDeps, StoreService},
};
use agentlaw_contracts::RequestControl;
use agentlaw_flows::runtime::StoreIdentity;
use agentlaw_storage::BindingKey;
use std::{
    collections::HashMap,
    ops::Deref,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
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
}

struct Inner {
    state: Mutex<State>,
    changed: Condvar,
    deps: StoreDeps,
    installation: PathBuf,
    registry: Arc<ConfigRegistry>,
}

pub(super) struct Stores(Arc<Inner>);

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
    fn open(&self, snapshot: &ConfigSnapshot) -> Result<StoreService, String> {
        config::require_existing_binding(&self.installation, &snapshot.config)
            .map_err(|e| e.to_string())?;
        let local = snapshot.config.runtime_root(&self.installation);
        let marker: serde_json::Value = serde_json::from_slice(
            &std::fs::read(local.join("control-initialized.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let binding_id = marker
            .get("binding_id")
            .and_then(|v| v.as_str())
            .ok_or("missing binding identity")?
            .to_owned();
        let machine_id = machine::load_or_create(&self.installation)
            .map_err(|e| e.to_string())?
            .machine_id;
        let identity = StoreIdentity {
            root: snapshot.config.memory_store_path.clone(),
            local,
            user_id: snapshot.config.user_id.clone(),
            machine_id,
            binding_id,
            history_limit: snapshot.config.history_response_limit_bytes,
            response_limit: snapshot.config.response_limit_bytes,
        };
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
        StoreService::open(identity, deps, &RequestControl::default()).map_err(|e| e.to_string())
    }
}

impl Stores {
    pub(super) fn new(
        installation: PathBuf,
        deps: StoreDeps,
        registry: Arc<ConfigRegistry>,
    ) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                entries: HashMap::new(),
            }),
            changed: Condvar::new(),
            deps,
            installation,
            registry,
        }))
    }
    pub(super) fn acquire(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
    ) -> Result<StoreGuard, RejectReason> {
        let reference = snapshot.reference;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
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
                    let result = self.0.open(snapshot);
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
        if let Err(error) = self.acquire(snapshot) {
            self.0.deps.health.publish(
                ComponentId {
                    binding: Some(format!("{:?}", snapshot.reference)),
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
