//! Owns one binding's control database, source and maintenance handles.

use crate::{
    git::handle::GitStoreHandle,
    installation::registry::ConfigSnapshot,
    ports::{HealthSink, RuntimeError, RuntimeResult},
    projection::{
        ActiveGeneration, ActivePointerSink, ExactPreparation, ProjectionConfig, ProjectionHandle,
    },
    protocol::{BindingRef, CommandRequest, QueryRequest},
    semantic::{SemanticIndexer, SemanticPreparationHandle, SemanticSearch},
    store::{
        lane::{LaneCommand, LaneReply, LaneSender, MutationLane},
        read::ReadCoordinator,
        recovery::RetiredFiles,
    },
};
use agentlaw_flows::{
    apply::Applied,
    control::{ControlReader, ControlWriter},
    exact::{DeltaBudget, ExactIndexWriter, ProjectionGenerations},
    history_cache::HistoryCacheGuards,
    prepare::Prepared,
    semantic::{SemanticCoverage, SemanticError},
    store::{ReadCtx, StoreIdentity},
    DomainError, RecallRequest, RequestControl, Result,
};
use agentlaw_storage::{
    published::{OwnedPublishedReader, SourcePosition},
    Store,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

pub struct StoreDeps {
    pub health: HealthSink,
    pub semantic: Arc<SemanticSearch>,
    pub indexer: SemanticIndexer,
    pub projection: ProjectionConfig,
    pub coordination: PathBuf,
    /// Installation state that holds the local Git delegation policies.
    pub state: PathBuf,
}
pub struct TargetPreparation {
    pub exact: ExactPreparation,
    pub semantic: std::result::Result<SemanticCoverage, SemanticError>,
}

pub struct StoreService {
    identity: StoreIdentity,
    reference: BindingRef,
    coordination: PathBuf,
    state: PathBuf,
    source: OwnedPublishedReader,
    lane: MutationLane,
    projection: ProjectionHandle,
    semantic: Arc<SemanticSearch>,
    indexer: SemanticIndexer,
    preparation: SemanticPreparationHandle,
    history_cache: HistoryCacheGuards,
    reads: ReadCoordinator,
    closed: AtomicBool,
}

struct PointerSink(LaneSender);
impl ActivePointerSink for PointerSink {
    fn persist_active(&self, _binding: &str, generation: &ActiveGeneration) -> RuntimeResult<()> {
        match self.0.call(
            LaneCommand::SetExactGeneration {
                generation_id: generation.id.clone(),
                exact_file: generation.exact_file.clone(),
            },
            &RequestControl::default(),
        )? {
            LaneReply::Done => Ok(()),
            _ => Err(RuntimeError::Protocol("unexpected pointer reply".into())),
        }
    }
}

impl StoreService {
    pub(crate) fn import_stage(
        &self,
        import_ref: &str,
        published: bool,
    ) -> Result<crate::git::git_ops::ImportStageContext> {
        if published {
            crate::git::git_ops::get_import_stage_for_publish(
                &self.git_handle(),
                &self.identity.local.join("git"),
                import_ref,
            )
        } else {
            crate::git::git_ops::get_import_stage(
                &self.git_handle(),
                &self.identity.local.join("git"),
                import_ref,
            )
        }
    }
    pub(crate) fn deliver_import(
        &self,
        value: Value,
        history: bool,
        snapshot: &ConfigSnapshot,
    ) -> Result<Value> {
        let limit = if history {
            snapshot
                .config
                .response_limit_bytes
                .min(snapshot.config.history_response_limit_bytes)
        } else {
            snapshot.config.response_limit_bytes
        };
        crate::installation::delivery::adapt(value, &self.state, limit)
    }
    fn import_identity(&self, snapshot: &ConfigSnapshot) -> StoreIdentity {
        StoreIdentity {
            root: self.identity.root.clone(),
            local: self.identity.local.clone(),
            user_id: self.identity.user_id.clone(),
            machine_id: self.identity.machine_id.clone(),
            binding_id: self.identity.binding_id.clone(),
            history_limit: snapshot.config.history_response_limit_bytes,
            response_limit: snapshot.config.response_limit_bytes,
            config_file: self.identity.config_file.clone(),
        }
    }
    pub(crate) fn query_import(
        &self,
        request: QueryRequest,
        snapshot: &ConfigSnapshot,
        control: &RequestControl,
    ) -> Result<Value> {
        self.query_identity(request, control, &self.import_identity(snapshot))
    }
    pub(crate) fn command_import(
        &self,
        request: CommandRequest,
        snapshot: &ConfigSnapshot,
        control: &RequestControl,
    ) -> Result<Value> {
        self.command_identity(request, control, &self.import_identity(snapshot))
    }
    pub fn share_inspect(
        &self,
        remote: &str,
        target_ref: &str,
        control: &RequestControl,
    ) -> Result<Value> {
        control.check()?;
        let mut result = agentlaw_contracts::to_value(crate::git::git_ops::inspect_share(
            &self.git_handle(),
            &self.identity.local.join("git"),
            remote,
            target_ref,
        )?)?;
        if result["findings"].as_array().is_some_and(|v| !v.is_empty()) {
            result["next_action"] = json!("Explain the detected pattern categories in the user's language. Ask whether to share unchanged or cancel. Do not infer permission from a private destination, modify memory, mask content or rewrite history without a separate decision.");
        }
        Ok(result)
    }
    pub fn share_push(
        &self,
        review: &str,
        allow_sensitive: bool,
        user_confirmed: bool,
        control: &RequestControl,
    ) -> Result<Value> {
        agentlaw_contracts::to_value(crate::git::git_ops::push_review(
            &self.git_handle(),
            &self.identity.local.join("git"),
            review,
            allow_sensitive && user_confirmed,
            control,
        )?)
    }
    pub fn share_fetch(&self, remote: &str, control: &RequestControl) -> Result<Value> {
        agentlaw_contracts::to_value(crate::git::git_ops::fetch_share(
            &self.git_handle(),
            &self.identity.local.join("git"),
            remote,
            control,
        )?)
    }
    pub fn import_prepare(
        &self,
        commit: &str,
        import_ref: &str,
        control: &RequestControl,
    ) -> Result<Value> {
        let mut result =
            agentlaw_contracts::to_value(crate::git::git_ops::prepare_import_with_ref(
                &self.git_handle(),
                &self.identity.local.join("git"),
                commit,
                import_ref,
                control,
            )?)?;
        result["next_action"] = json!("This is an isolated review workspace, not active memory. Inspect structural_conflicts, explain both states in the user's language, and ask the user to choose local or incoming where required. Submit agreed structural choices with share import resolve --ref <import_ref> --choices <JSON-file-or-> --user-confirmed. Use share import call --ref <import_ref> --json - with ordinary agentlaw requests for memory-head inspection and agreed semantic edits. No active memory or Git HEAD has changed. Publication requires a separate confirmation of the complete resolved import.");
        Ok(result)
    }
    pub fn import_inspect(&self, import_ref: &str, control: &RequestControl) -> Result<Value> {
        agentlaw_contracts::to_value(crate::git::git_ops::inspect_import(
            &self.git_handle(),
            &self.identity.local.join("git"),
            import_ref,
            control,
        )?)
    }
    pub fn import_resolve(&self, import_ref: &str, control: &RequestControl) -> Result<Value> {
        let resolution = crate::git::git_ops::resolve_import(
            &self.git_handle(),
            &self.identity.local.join("git"),
            import_ref,
            control,
        )?;
        let mut result = agentlaw_contracts::to_value(resolution)?;
        if result["next_action"].is_null() {
            result["next_action"] = json!("If unresolved structural conflicts or concurrent heads remain, inspect them and ask the user to decide before editing the isolated review workspace. Otherwise explain the exact resolved import and request confirmation. Only after confirmation, use share import publish --ref <import_ref> --resolution <resolution_token> --user-confirmed. Do not invent confirmation.");
        }
        Ok(result)
    }
    pub fn import_publish(
        &self,
        import_ref: &str,
        resolution: &str,
        confirmed: bool,
        control: &RequestControl,
    ) -> Result<Value> {
        agentlaw_contracts::to_value(crate::git::git_ops::publish_import(
            &self.git_handle(),
            &self.identity.local.join("git"),
            import_ref,
            resolution,
            confirmed,
            control,
        )?)
    }
    pub(crate) fn import_publish_observed(
        &self,
        import_ref: &str,
        resolution: &str,
        confirmed: bool,
        control: &RequestControl,
        after_publication: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        agentlaw_contracts::to_value(crate::git::git_ops::publish_import_observed(
            &self.git_handle(),
            &self.identity.local.join("git"),
            import_ref,
            resolution,
            confirmed,
            control,
            after_publication,
        )?)
    }
    pub fn sync_accept_findings(
        &self,
        operation: &str,
        candidate: &str,
        digest: &str,
        confirmed: bool,
        control: &RequestControl,
    ) -> Result<Value> {
        control.check()?;
        crate::git::sync::accept_findings(
            &self.git_handle(),
            &self.identity.local,
            &self.state,
            operation,
            candidate,
            digest,
            confirmed,
        )
    }
    pub fn sync_policy_propose(
        &self,
        remote: &str,
        target: &str,
        control: &RequestControl,
    ) -> Result<Value> {
        control.check()?;
        agentlaw_contracts::to_value(crate::git::sync::propose_policy(
            &self.git_handle(),
            &self.identity.local,
            remote,
            target,
        )?)
    }
    pub fn sync_policy_configure(
        &self,
        record: Value,
        confirmed: bool,
        control: &RequestControl,
    ) -> Result<Value> {
        control.check()?;
        let text = serde_json::to_string(&record)
            .map_err(|e| DomainError::new("invalid_input", e.to_string()))?;
        let policy = crate::git::sync::prepare_policy_configuration(&text, confirmed)?;
        crate::git::sync::configure_prepared_policy(
            &self.state,
            &self.git_handle(),
            &self.identity.local,
            policy,
        )
    }
    pub fn repair_derived(&self, control: &RequestControl) -> Result<Value> {
        control.phase("rebuilding_private_derived_generation");
        control.check()?;
        self.projection.rebuild(control).map_err(|e| {
            control
                .check()
                .err()
                .unwrap_or_else(|| DomainError::new("derived_repair_failed", e.to_string()))
        })?;
        control.check()?;
        let target = self
            .source
            .read_snapshot()
            .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?
            .position()
            .clone();
        let (semantic_complete, diagnostic) =
            match self.indexer.start_repair(&self.identity.binding_id) {
                Err(error) => (
                    false,
                    Some(DomainError::new(
                        "semantic_repair_incomplete",
                        error.to_string(),
                    )),
                ),
                Ok(()) => match self.indexer.repair_progress(&self.identity.binding_id) {
                    Some(progress)
                        if progress.state == crate::semantic::SemanticRepairState::Complete
                            && progress.ack.as_ref().is_some_and(|ack| {
                                ack.epoch == target.epoch
                                    && u64::try_from(ack.sequence)
                                        .is_ok_and(|sequence| sequence >= target.sequence)
                            }) =>
                    {
                        (true, None)
                    }
                    Some(progress) => (
                        false,
                        Some(DomainError::new(
                            "semantic_repair_incomplete",
                            progress
                                .reason
                                .unwrap_or_else(|| "Semantic repair is still in progress.".into()),
                        )),
                    ),
                    None => (
                        false,
                        Some(DomainError::new(
                            "semantic_repair_incomplete",
                            "Semantic repair progress is unavailable.",
                        )),
                    ),
                },
            };
        control.check()?;
        Ok(
            json!({"lexical_complete":true,"semantic_complete":semantic_complete,"diagnostics":diagnostic.into_iter().collect::<Vec<_>>()}),
        )
    }
    pub fn continuity_save(&self, control: &RequestControl) -> Result<Value> {
        control.check()?;
        agentlaw_contracts::to_value(crate::git::git_ops::continuity_save(
            &self.git_handle(),
            &self.identity.local.join("git"),
            "Agentlaw continuity snapshot\n",
        )?)
    }
    pub fn export_history(
        &self,
        memory_id: String,
        output: PathBuf,
        control: &RequestControl,
    ) -> Result<Value> {
        crate::store::export::history_export(&self.source, &memory_id, &output, control)
    }
    pub fn procedure_list(
        &self,
        filter: crate::protocol::InventoryFilter,
        output: PathBuf,
        control: &RequestControl,
    ) -> Result<Value> {
        crate::store::export::procedure_list(
            &self.source,
            &self.identity.local.join("exports"),
            &filter,
            &output,
            control,
        )
    }
    pub fn procedure_search(
        &self,
        query: String,
        limit: Option<usize>,
        filter: crate::protocol::InventoryFilter,
        output: PathBuf,
        control: &RequestControl,
    ) -> Result<Value> {
        let result = self.with_read_ctx(&self.identity, control, |ctx| {
            self.reads
                .search_procedures(ctx, &query, &filter, limit.unwrap_or(5))
        })?;
        crate::store::export::write_search(&self.source, &filter, &output, result, control)
    }
    pub fn command(&self, request: CommandRequest, control: &RequestControl) -> Result<Value> {
        self.command_identity(request, control, &self.identity)
    }

    pub fn command_with_snapshot(
        &self,
        request: CommandRequest,
        snapshot: &ConfigSnapshot,
        control: &RequestControl,
    ) -> Result<Value> {
        let identity = self.request_identity(snapshot)?;
        self.command_identity(request, control, &identity)
    }
    fn command_identity(
        &self,
        request: CommandRequest,
        control: &RequestControl,
        identity: &StoreIdentity,
    ) -> Result<Value> {
        let prepared = match request {
            CommandRequest::Remember(request) | CommandRequest::Procedure(request) => self
                .with_read_ctx(identity, control, |ctx| {
                    self.reads.run_prepare(ctx, request)
                })?,
            CommandRequest::Connect(request) => self.with_read_ctx(identity, control, |ctx| {
                self.reads.run_prepare_connect(ctx, request)
            })?,
            CommandRequest::Sync(request) => {
                return crate::git::sync::call_with_handle(
                    &self.git_handle(),
                    &self.state,
                    &identity.user_id,
                    &identity.machine_id,
                    request,
                    control.clone(),
                )
            }
        };
        self.apply_identity(prepared, control, identity)
    }
    pub fn open(
        identity: StoreIdentity,
        deps: StoreDeps,
        control: &RequestControl,
    ) -> RuntimeResult<Self> {
        control.check()?;
        if identity.root.join("format.md").exists()
            && ((!identity.local.join("canonical/source-fence").exists()
                && !identity.local.join("control-initialized.json").exists())
                || identity
                    .local
                    .join("canonical/validation-required")
                    .exists())
        {
            Store::attach_existing_with_coordination(
                &identity.root,
                identity.local.join("canonical"),
                &deps.coordination,
            )
            .map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?;
        }
        if identity.local.join("control-initialized.json").exists()
            && !identity.local.join("canonical/source-fence").is_file()
        {
            return Err(RuntimeError::StoreUnavailable(
                "The source fence for this binding is missing; restore its existing state.".into(),
            ));
        }
        let mut writer = ControlWriter::open(&identity.local)?;
        writer.bind_identity(&identity)?;
        let store = Store::open_with_coordination(
            &identity.root,
            identity.local.join("canonical"),
            &deps.coordination,
        )
        .map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?;
        writer.finish_binding_initialization(&identity)?;
        let source = store.owned_published_reader();
        let key = agentlaw_storage::binding_identity(&identity.root, &identity.local)
            .map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?;
        let reference = BindingRef::new(&key, &identity.user_id);
        let lane_identity = StoreIdentity {
            root: identity.root.clone(),
            local: identity.local.clone(),
            user_id: identity.user_id.clone(),
            machine_id: identity.machine_id.clone(),
            binding_id: identity.binding_id.clone(),
            history_limit: identity.history_limit,
            response_limit: identity.response_limit,
            config_file: identity.config_file.clone(),
        };
        let lane = MutationLane::start(lane_identity, store, writer)?;
        match lane.sender().call(LaneCommand::Recover, control)? {
            LaneReply::Done => {}
            _ => return Err(RuntimeError::Protocol("unexpected recovery reply".into())),
        }
        let pointer = Arc::new(PointerSink(lane.sender()));
        let reader = ControlReader::open(&identity.local)?;
        let active =
            if let Some((id, epoch, exact_file, lexical_file)) = reader.active_projection()? {
                ActiveGeneration {
                    id,
                    epoch,
                    exact_file,
                    lexical_file,
                }
            } else {
                let directory = identity.local.join("projection");
                let position = source
                    .read_snapshot()
                    .map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?
                    .position()
                    .clone();
                let shadow = ExactIndexWriter::rebuild_shadow_in(&directory, &source, control)?;
                let registry = ProjectionGenerations::open_for_recovery(
                    "bootstrap",
                    &position.epoch,
                    directory.join("exact-bootstrap.sqlite"),
                    directory.join("lexical-bootstrap.sqlite"),
                )?;
                let staged = registry.stage(shadow)?;
                let generation = ActiveGeneration {
                    id: staged.generation_id().to_owned(),
                    epoch: staged.epoch().to_owned(),
                    exact_file: staged.exact_file().to_owned(),
                    lexical_file: staged.lexical_file().to_owned(),
                };
                pointer.persist_active(&identity.binding_id, &generation)?;
                generation
            };
        drop(reader);
        let projection = ProjectionHandle::start(
            identity.binding_id.clone(),
            source.clone(),
            active,
            pointer,
            Arc::new(RetiredFiles),
            deps.health,
            deps.projection,
        )?;
        if let Err(error) = deps
            .indexer
            .open_binding(&identity.binding_id, source.clone())
        {
            projection.shutdown(Duration::from_secs(30));
            return Err(error);
        }
        let preparation = deps.semantic.preparation(&identity.binding_id);
        let history_cache = HistoryCacheGuards::new();
        let reads = ReadCoordinator::new(projection.clone(), DeltaBudget::default());
        let projection_wake = projection.clone();
        let semantic_wake = preparation.clone();
        lane.set_waker(Arc::new(move || {
            projection_wake.wake();
            semantic_wake.wake();
        }));
        Ok(Self {
            identity,
            reference,
            coordination: deps.coordination,
            state: deps.state,
            source,
            lane,
            projection,
            semantic: deps.semantic,
            indexer: deps.indexer,
            preparation,
            history_cache,
            reads,
            closed: AtomicBool::new(false),
        })
    }

    pub fn query(&self, request: QueryRequest, control: &RequestControl) -> Result<Value> {
        self.query_identity(request, control, &self.identity)
    }
    pub fn query_with_snapshot(
        &self,
        request: QueryRequest,
        snapshot: &ConfigSnapshot,
        control: &RequestControl,
    ) -> Result<Value> {
        let identity = self.request_identity(snapshot)?;
        self.query_identity(request, control, &identity)
    }
    fn request_identity(&self, snapshot: &ConfigSnapshot) -> Result<StoreIdentity> {
        if snapshot.reference != self.reference
            || BindingRef::new(&snapshot.key, &snapshot.config.user_id) != self.reference
        {
            return Err(DomainError::new(
                "binding_changed",
                "The accepted configuration names a different store binding.",
            ));
        }
        Ok(StoreIdentity {
            root: self.identity.root.clone(),
            local: self.identity.local.clone(),
            user_id: self.identity.user_id.clone(),
            machine_id: self.identity.machine_id.clone(),
            binding_id: self.identity.binding_id.clone(),
            history_limit: snapshot.config.history_response_limit_bytes,
            response_limit: snapshot.config.response_limit_bytes,
            config_file: self.identity.config_file.clone(),
        })
    }
    fn query_identity(
        &self,
        request: QueryRequest,
        control: &RequestControl,
        identity: &StoreIdentity,
    ) -> Result<Value> {
        self.with_read_ctx(identity, control, |ctx| self.reads.run_query(ctx, request))
    }

    pub fn apply_prepared(&self, prepared: Prepared, control: &RequestControl) -> Result<Value> {
        self.apply_identity(prepared, control, &self.identity)
    }
    pub fn apply_prepared_with_snapshot(
        &self,
        prepared: Prepared,
        snapshot: &ConfigSnapshot,
        control: &RequestControl,
    ) -> Result<Value> {
        let identity = self.request_identity(snapshot)?;
        self.apply_identity(prepared, control, &identity)
    }
    fn apply_identity(
        &self,
        prepared: Prepared,
        control: &RequestControl,
        identity: &StoreIdentity,
    ) -> Result<Value> {
        let prepared = match prepared {
            Prepared::Replay(value) => return Ok(value),
            other => other,
        };
        let applied = match self.lane.sender().call_with_limits(
            LaneCommand::Apply(prepared),
            control,
            Some((identity.history_limit, identity.response_limit)),
        )? {
            LaneReply::Applied(value) => value,
            _ => {
                return Err(DomainError::new(
                    "store_unavailable",
                    "Unexpected mutation reply.",
                ))
            }
        };
        match applied {
            Applied::Response(value) => Ok(value),
            Applied::Connected {
                response,
                follow_up,
            } => self.finish_connect(response, follow_up, control, identity),
        }
    }
    fn finish_connect(
        &self,
        response: agentlaw_flows::ConnectResponse,
        follow_up: Option<agentlaw_flows::apply::FollowUp>,
        control: &RequestControl,
        identity: &StoreIdentity,
    ) -> Result<Value> {
        let project_id = response
            .project_connection
            .as_ref()
            .map(|connection| connection.project_id.clone());
        let mut result = agentlaw_flows::to_value(response)?;
        if let Some(follow_up) = follow_up {
            let mut diagnostics = Vec::new();
            match self.source.read_snapshot() {
                Ok(snapshot) => {
                    let target = snapshot.position().clone();
                    drop(snapshot);
                    let prepared = self.ensure_target(target, control);
                    if let ExactPreparation::Incomplete {
                        target,
                        through,
                        reason,
                    } = prepared.exact
                    {
                        diagnostics.push(json!({"code":"exact_preparation_incomplete","target":target,"through":through,"reason":reason}));
                    }
                    match prepared.semantic { Ok(SemanticCoverage::CompleteAt(_))=>{}, other=>diagnostics.push(json!({"code":"semantic_preparation_incomplete","detail":format!("{other:?}")})) }
                }
                Err(error) => diagnostics
                    .push(json!({"code":"source_preparation_failed","message":error.to_string()})),
            }
            if follow_up.restore {
                let recall = restore_connected(
                    project_id.as_deref(),
                    follow_up.recall_for,
                    |request, project_id| {
                        self.with_read_ctx(identity, control, |ctx| {
                            let (value, completeness) = self
                                .reads
                                .run_restore_for_project(ctx, request, project_id)?;
                            for diagnostic in completeness {
                                diagnostics.push(agentlaw_flows::to_value(diagnostic)?);
                            }
                            Ok(value)
                        })
                    },
                );
                match recall {
                    Ok(value) => result["recall_result"] = value,
                    Err(error) => diagnostics
                        .push(json!({"code":"restore_recall_failed","message":error.message})),
                }
            }
            if !diagnostics.is_empty() {
                if follow_up.restore {
                    result["code"] = json!("restore_preparation_incomplete");
                    result["turn_instruction"] =
                        json!(agentlaw_flows::context::RESTORE_PREPARATION_INCOMPLETE_INSTRUCTION);
                }
                result["diagnostics"] = json!(diagnostics);
            }
        }
        Ok(result)
    }
    /// Builds the request-scoped read context over a fresh read-only control connection.
    fn with_read_ctx<T>(
        &self,
        identity: &StoreIdentity,
        control: &RequestControl,
        read: impl FnOnce(&ReadCtx<'_>) -> Result<T>,
    ) -> Result<T> {
        let reader = ControlReader::open(&self.identity.local)?;
        read(&ReadCtx {
            identity,
            source: &self.source,
            control: &reader,
            semantic: self.semantic.as_ref(),
            history_semantic: self.semantic.as_ref(),
            history_cache: &self.history_cache,
            request: control,
        })
    }
    pub fn ensure_exact_target(
        &self,
        target: SourcePosition,
        control: &RequestControl,
    ) -> ExactPreparation {
        self.projection.wake();
        self.projection.wait_exact(&target, control)
    }
    pub fn ensure_target(
        &self,
        target: SourcePosition,
        control: &RequestControl,
    ) -> TargetPreparation {
        let exact = self.ensure_exact_target(target.clone(), control);
        self.preparation.wake();
        let semantic = self.preparation.wait_target(&target, control);
        TargetPreparation { exact, semantic }
    }
    pub fn prepare_selection(
        &self,
        control: &RequestControl,
        semantic_required: bool,
    ) -> Result<Value> {
        let target = self
            .source
            .read_snapshot()
            .map_err(|e| DomainError::new("store_unavailable", e.to_string()))?
            .position()
            .clone();
        if semantic_required {
            let prepared = self.ensure_target(target, control);
            if let ExactPreparation::Incomplete { reason, .. } = prepared.exact {
                return Err(DomainError::new("exact_build_incomplete", reason));
            }
            match prepared.semantic {
                Ok(SemanticCoverage::CompleteAt(_)) => {
                    Ok(json!({"lexical_complete":true,"semantic_complete":true}))
                }
                other => Err(DomainError::new(
                    "semantic_build_incomplete",
                    format!("Semantic preparation failed: {other:?}"),
                )),
            }
        } else {
            if let ExactPreparation::Incomplete { reason, .. } =
                self.ensure_exact_target(target, control)
            {
                return Err(DomainError::new("exact_build_incomplete", reason));
            }
            Ok(
                json!({"lexical_complete":true,"semantic_complete":false,"diagnostics":[{"code":"semantic_unavailable","message":"No model is configured; vector build was not performed."}]}),
            )
        }
    }
    pub fn recover(&self, control: &RequestControl) -> Result<()> {
        match self.lane.sender().call(LaneCommand::Recover, control)? {
            LaneReply::Done => Ok(()),
            _ => Err(DomainError::new(
                "store_unavailable",
                "Unexpected recovery reply.",
            )),
        }
    }
    pub fn lane_sender(&self) -> LaneSender {
        self.lane.sender()
    }
    pub fn git_handle(&self) -> GitStoreHandle {
        GitStoreHandle {
            root: self.identity.root.clone(),
            local: self.identity.local.clone(),
            coordination: self.coordination.clone(),
            source: self.source.clone(),
            lane: self.lane.sender(),
        }
    }
    pub fn close(&self) {
        while self.close_for_maintenance().is_err() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    pub fn close_for_maintenance(&self) -> RuntimeResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        if !self.projection.shutdown(Duration::from_secs(30)) {
            return Err(RuntimeError::Unavailable(
                "Projection shutdown did not finish.".into(),
            ));
        }
        self.indexer
            .close_binding_checked(&self.identity.binding_id)?;
        self.lane.close_checked()?;
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}
fn restore_request(recall_for: Option<String>) -> RecallRequest {
    RecallRequest {
        recall_for,
        restore_context: Some(true),
        ..Default::default()
    }
}
fn restore_connected(
    project_id: Option<&str>,
    recall_for: Option<String>,
    query: impl FnOnce(RecallRequest, &str) -> Result<Value>,
) -> Result<Value> {
    let id = project_id.ok_or_else(|| {
        DomainError::new(
            "project_connection_required",
            "The connected project identity is missing.",
        )
    })?;
    query(restore_request(recall_for), id)
}
impl Drop for StoreService {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests;
