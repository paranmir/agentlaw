//! Dispatch installation setup and pinned store requests.
use super::{
    dispatch::Dispatcher,
    stores::{StoreGuard, Stores},
};
use crate::{
    installation::{
        config,
        registry::{ConfigRegistry, ConfigSnapshot, ConfigState},
        setup,
    },
    protocol::{
        BindingError, BindingRef, CallTarget, Classified, CommandRequest, QueryRequest,
        RejectReason, SetupRequest,
    },
};
use agentlaw_contracts::{ConnectIntent, DomainError, RequestControl};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(super) fn read_config(state: &Path) -> ConfigState {
    match config::load(state) {
        Ok(None) => ConfigState::Missing,
        Err(error) => ConfigState::Invalid(error.to_string()),
        Ok(Some(config)) => {
            match agentlaw_storage::binding_identity(
                &config.memory_store_path,
                &config.runtime_root(state),
            ) {
                Ok(key) => {
                    let reference = BindingRef::new(&key, &config.user_id);
                    ConfigState::Ready(Arc::new(ConfigSnapshot {
                        config,
                        key,
                        reference,
                    }))
                }
                Err(error) => ConfigState::Invalid(error.to_string()),
            }
        }
    }
}

pub(super) struct StoreDispatch {
    state: PathBuf,
    registry: Arc<ConfigRegistry>,
    stores: Arc<Stores>,
    setup: Mutex<()>,
    worker: agentlaw_worker::RuntimeConfig,
}

pub(super) enum Admitted {
    Query(StoreGuard, Arc<ConfigSnapshot>, QueryRequest),
    Command(StoreGuard, Arc<ConfigSnapshot>, CommandRequest),
    Setup(SetupRequest),
}

impl StoreDispatch {
    pub(super) fn new(
        state: PathBuf,
        registry: Arc<ConfigRegistry>,
        stores: Arc<Stores>,
        worker: agentlaw_worker::RuntimeConfig,
    ) -> Self {
        Self {
            state,
            registry,
            stores,
            setup: Mutex::new(()),
            worker,
        }
    }
    fn setup(&self, request: SetupRequest, control: &RequestControl) -> Result<Value, DomainError> {
        let lane = self.setup.lock().unwrap_or_else(|e| e.into_inner());
        let mut follow_up = None;
        let result = match request {
            SetupRequest::ConnectStore(mut request) => {
                let path = request.memory_store_path.take().ok_or_else(|| {
                    DomainError::new("invalid_path", "A memory store path is required.")
                })?;
                control.phase("preparing_memory_store");
                let worker = self.worker.clone();
                let (result, _) = setup::connect_selection_with_control(
                    &self.state,
                    Path::new(&path),
                    false,
                    control.clone(),
                    |_| Ok(worker),
                )?;
                follow_up = Some(request);
                result
            }
            SetupRequest::StoreConnect { path, create } => {
                let worker = self.worker.clone();
                setup::connect_selection_with_control(
                    &self.state,
                    &path,
                    create,
                    control.clone(),
                    |_| Ok(worker),
                )?
                .0
            }
            SetupRequest::ConfigSet { key, value } => config::set_limit(&self.state, &key, &value)?,
        };
        let selected = read_config(&self.state);
        self.registry.publish(selected.clone());
        self.stores.selection_changed();
        if let ConfigState::Ready(snapshot) = &selected {
            self.stores.prewarm(snapshot);
        }
        let pinned = if follow_up.is_some() {
            let snapshot = match selected {
                ConfigState::Ready(snapshot) => snapshot,
                _ => {
                    return Err(DomainError::new(
                        "configuration_io",
                        "Cannot read the selected store.",
                    ))
                }
            };
            let guard = self
                .stores
                .acquire(&snapshot)
                .map_err(|error| DomainError::new("store_unavailable", format!("{error:?}")))?;
            Some((guard, snapshot))
        } else {
            None
        };
        drop(lane);
        if let (Some(request), Some((guard, snapshot))) = (follow_up, pinned) {
            if matches!(request.intent, None | Some(ConnectIntent::Discover))
                && request.restore_context != Some(true)
            {
                guard.query_with_snapshot(QueryRequest::Discover(request), &snapshot, control)
            } else {
                guard.command_with_snapshot(CommandRequest::Connect(request), &snapshot, control)
            }
        } else {
            Ok(result)
        }
    }
}

impl Dispatcher for StoreDispatch {
    type Admitted = Admitted;
    fn admit(&self, target: &CallTarget, request: Classified) -> Result<Admitted, RejectReason> {
        match target {
            CallTarget::Store(reference) => {
                let snapshot = self
                    .registry
                    .resolve(reference)
                    .map_err(|error| match error {
                        BindingError::BindingChanged => RejectReason::BindingChanged,
                        BindingError::ConnectionRequired => RejectReason::ConnectionRequired,
                        BindingError::ConfigInvalid(reason) => RejectReason::ConfigInvalid(reason),
                    })?;
                let guard = self.stores.acquire(&snapshot)?;
                match request {
                    Classified::Query(request) => Ok(Admitted::Query(guard, snapshot, request)),
                    Classified::Command(request) => Ok(Admitted::Command(guard, snapshot, request)),
                    Classified::Setup(_) => Err(RejectReason::KindMismatch),
                }
            }
            CallTarget::Setup => match request {
                Classified::Setup(request) => Ok(Admitted::Setup(request)),
                _ => Err(RejectReason::KindMismatch),
            },
        }
    }
    fn run(&self, request: Admitted, control: &RequestControl) -> Result<Value, DomainError> {
        match request {
            Admitted::Query(guard, snapshot, request) => {
                guard.query_with_snapshot(request, &snapshot, control)
            }
            Admitted::Command(guard, snapshot, request) => {
                guard.command_with_snapshot(request, &snapshot, control)
            }
            Admitted::Setup(request) => self.setup(request, control),
        }
    }
}
