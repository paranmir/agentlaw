//! Dispatch installation setup and pinned store requests.
use super::{dispatch::Dispatcher, stores::Stores};
use crate::{
    installation::{
        config,
        registry::{ConfigRegistry, ConfigSnapshot, ConfigState},
        setup,
    },
    protocol::{
        AdminCommand, BindingError, BindingRef, CallTarget, Classified, CommandRequest,
        QueryRequest, RejectReason, SetupCommand, SetupRequest,
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
        Err(error) => ConfigState::Invalid(crate::protocol::ConfigurationFailure::Domain(error)),
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
                Err(error) => ConfigState::Invalid(crate::protocol::ConfigurationFailure::Domain(
                    DomainError::new("configuration_io", format!("Cannot access the configured store binding: {error}"))
                        .with_next_action("Restore access to the configured memory store and binding directory, then retry."),
                )),
            }
        }
    }
}

pub(super) struct StoreDispatch {
    state: PathBuf,
    registry: Arc<ConfigRegistry>,
    stores: Arc<Stores>,
    setup: Mutex<()>,
    assets: Arc<dyn Fn() -> Result<Option<agentlaw_worker::ModelAssets>, String> + Send + Sync>,
    imports: crate::git::imports::ProgressRegistry,
}

pub(super) enum Admitted {
    Query(Arc<ConfigSnapshot>, QueryRequest),
    Command(Arc<ConfigSnapshot>, CommandRequest),
    Admin(Arc<ConfigSnapshot>, AdminCommand),
    Setup(SetupRequest),
}

impl StoreDispatch {
    pub(super) fn new(
        state: PathBuf,
        registry: Arc<ConfigRegistry>,
        stores: Arc<Stores>,
        assets: Arc<dyn Fn() -> Result<Option<agentlaw_worker::ModelAssets>, String> + Send + Sync>,
    ) -> Self {
        Self {
            state,
            registry,
            stores,
            setup: Mutex::new(()),
            assets,
            imports: crate::git::imports::ProgressRegistry::default(),
        }
    }
    fn setup(
        &self,
        request: SetupRequest,
        control: &RequestControl,
    ) -> Result<(Value, Option<usize>), DomainError> {
        let lane = self.setup.lock().unwrap_or_else(|e| e.into_inner());
        let mut follow_up = None;
        let result = match request {
            SetupRequest::ConnectStore(mut request) => {
                let path = request.memory_store_path.take().ok_or_else(|| {
                    DomainError::new("invalid_path", "A memory store path is required.")
                })?;
                control.phase("preparing_memory_store");
                let (result, _) = setup::connect_prepared(
                    &self.state,
                    Path::new(&path),
                    false,
                    control,
                    &self.registry,
                    |snapshot, control| self.prepare(snapshot, control),
                )?;
                follow_up = Some(request);
                result
            }
            SetupRequest::Command(SetupCommand::StoreConnect { path, create }) => {
                setup::connect_prepared(
                    &self.state,
                    &path,
                    create,
                    control,
                    &self.registry,
                    |snapshot, control| self.prepare(snapshot, control),
                )?
                .0
            }
            SetupRequest::Command(command) => setup::command(&self.state, &self.registry, command)?,
        };
        let selected = self.registry.current();
        let limit = match &selected {
            ConfigState::Ready(snapshot) => Some(snapshot.config.response_limit_bytes),
            _ => None,
        };
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
        let value = if let (Some(request), Some((guard, snapshot))) = (follow_up, pinned) {
            if matches!(request.intent, None | Some(ConnectIntent::Discover))
                && request.restore_context != Some(true)
            {
                guard.query_with_snapshot(QueryRequest::Discover(request), &snapshot, control)
            } else {
                guard.command_with_snapshot(CommandRequest::Connect(request), &snapshot, control)
            }
        } else {
            Ok(result)
        }?;
        Ok((value, limit))
    }

    fn prepare(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        control: &RequestControl,
    ) -> Result<Value, DomainError> {
        let guard = self
            .stores
            .acquire(snapshot)
            .map_err(|e| DomainError::new("store_unavailable", format!("{e:?}")))?;
        let configured = (self.assets)()
            .map_err(|e| DomainError::new("semantic_build_failed", e))?
            .is_some();
        guard.prepare_selection(control, configured)
    }
}

impl Dispatcher for StoreDispatch {
    type Admitted = Admitted;
    fn response_limit(&self, admitted: &Admitted) -> Option<usize> {
        match admitted {
            Admitted::Query(s, QueryRequest::History(_)) => Some(
                s.config
                    .response_limit_bytes
                    .min(s.config.history_response_limit_bytes),
            ),
            Admitted::Query(s, _) | Admitted::Command(s, _) | Admitted::Admin(s, _) => {
                Some(s.config.response_limit_bytes)
            }
            Admitted::Setup(_) => None,
        }
    }
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
                match request {
                    Classified::Query(request) => Ok(Admitted::Query(snapshot, request)),
                    Classified::Command(request) => Ok(Admitted::Command(snapshot, request)),
                    Classified::Admin(request) => Ok(Admitted::Admin(snapshot, request)),
                    Classified::Setup(_) => Err(RejectReason::KindMismatch),
                }
            }
            CallTarget::Setup => match request {
                Classified::Setup(request) => Ok(Admitted::Setup(request)),
                Classified::Admin(_) => Err(RejectReason::InvalidBody(DomainError::new(
                    "invalid_input",
                    "Admin commands require a store binding.",
                ))),
                _ => Err(RejectReason::KindMismatch),
            },
        }
    }
    fn run(&self, request: Admitted, control: &RequestControl) -> Result<Value, DomainError> {
        match request {
            Admitted::Query(snapshot, request) => {
                let guard = self.stores.acquire_for_execution(&snapshot, control)?;
                guard.query_with_snapshot(request, &snapshot, control)
            }
            Admitted::Command(snapshot, request) => {
                let guard = self.stores.acquire_for_execution(&snapshot, control)?;
                guard.command_with_snapshot(request, &snapshot, control)
            }
            Admitted::Admin(snapshot, request) => match request {
                AdminCommand::Audit | AdminCommand::Repair => {
                    super::maintenance::run(&self.stores, &snapshot, request, control)
                }
                AdminCommand::ImportList => {
                    control.check()?;
                    let local = snapshot.config.runtime_root(&self.state).join("git");
                    let progress = self.imports.scoped(&local);
                    Ok(serde_json::json!({"imports":crate::git::imports::list(&local, &progress)?}))
                }
                request @ (AdminCommand::ImportPrepare { .. }
                | AdminCommand::ImportInspect { .. }
                | AdminCommand::ImportCall { .. }
                | AdminCommand::ImportResolve { .. }
                | AdminCommand::ImportPublish { .. }) => {
                    let local = snapshot.config.runtime_root(&self.state).join("git");
                    super::imports::run(
                        &snapshot,
                        request,
                        control,
                        &self.stores,
                        &self.imports.scoped(&local),
                    )
                }
                other => {
                    let guard = self.stores.acquire_for_execution(&snapshot, control)?;
                    match other {
                        AdminCommand::ContinuitySave => guard.continuity_save(control),
                        AdminCommand::HistoryExport { memory_id, output } => {
                            guard.export_history(memory_id, output, control)
                        }
                        AdminCommand::ProcedureList { filter, output } => {
                            guard.procedure_list(filter, output, control)
                        }
                        AdminCommand::ProcedureSearch {
                            query,
                            limit,
                            filter,
                            output,
                        } => guard.procedure_search(query, limit, filter, output, control),
                        AdminCommand::ShareInspect { remote, target_ref } => {
                            guard.share_inspect(&remote, &target_ref, control)
                        }
                        AdminCommand::SharePush {
                            review,
                            allow_sensitive,
                            user_confirmed,
                        } => guard.share_push(&review, allow_sensitive, user_confirmed, control),
                        AdminCommand::ShareFetch { remote } => guard.share_fetch(&remote, control),
                        AdminCommand::SyncAcceptFindings {
                            operation,
                            candidate,
                            findings_digest,
                            confirm_sharing,
                        } => guard.sync_accept_findings(
                            &operation,
                            &candidate,
                            &findings_digest,
                            confirm_sharing,
                            control,
                        ),
                        AdminCommand::SyncPolicyPropose { remote, target_ref } => {
                            guard.sync_policy_propose(&remote, &target_ref, control)
                        }
                        AdminCommand::SyncPolicyConfigure {
                            record,
                            confirm_delegation,
                        } => guard.sync_policy_configure(record, confirm_delegation, control),
                        _ => Err(DomainError::new(
                            "unavailable",
                            "Administrative command is not implemented.",
                        )),
                    }
                }
            },
            Admitted::Setup(request) => self.setup(request, control).map(|(value, _)| value),
        }
    }
    fn run_with_limit(
        &self,
        admitted: Admitted,
        control: &RequestControl,
    ) -> Result<(Value, Option<usize>), DomainError> {
        match admitted {
            Admitted::Setup(request) => self.setup(request, control),
            other => {
                let limit = self.response_limit(&other);
                self.run(other, control).map(|value| (value, limit))
            }
        }
    }
}
