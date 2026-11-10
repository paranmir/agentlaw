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
    runtime::{ReadCtx, StoreIdentity},
    semantic::{SemanticCoverage, SemanticError},
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
                        self.query_for_project(request, project_id, control, identity)
                    },
                );
                match recall {
                    Ok(value) => result["recall_result"] = value,
                    Err(error) => diagnostics
                        .push(json!({"code":"restore_recall_failed","message":error.message})),
                }
            }
            if !diagnostics.is_empty() {
                result["diagnostics"] = json!(diagnostics);
            }
        }
        Ok(result)
    }
    fn query_for_project(
        &self,
        request: RecallRequest,
        project_id: &str,
        control: &RequestControl,
        identity: &StoreIdentity,
    ) -> Result<Value> {
        self.with_read_ctx(identity, control, |ctx| {
            self.reads.run_recall_for_project(ctx, request, project_id)
        })
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
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.projection.shutdown(Duration::from_secs(30));
        self.indexer.close_binding(&self.identity.binding_id);
        self.lane.close();
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
