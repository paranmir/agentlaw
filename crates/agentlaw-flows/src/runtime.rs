//! Durable local context and pending state, connected to canonical storage.
use crate::write::*;
use crate::*;
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
pub(crate) fn sql<T>(r: std::result::Result<T, rusqlite::Error>) -> Result<T> {
    r.map_err(|_| {
        DomainError::new(
            "control_storage_failed",
            "Local control database operation failed; retained work was not discarded.",
        )
    })
}
pub(crate) fn control_backup_required() -> DomainError {
    DomainError::new("control_backup_required","Authoritative local control state is absent or cannot be retained. Existing pending proposals and authoring decisions cannot be reconstructed from canonical memories. Preserve database and WAL/SHM files, stop clients, and restore a consistent backup for this binding; no empty replacement was created.")
}
fn record_background_status(local: &Path, error: Option<DomainError>) -> Result<()> {
    let connection = sql(Connection::open(local.join("background-status.sqlite")))?;
    sql(connection.busy_timeout(std::time::Duration::from_secs(5)))?;
    sql(connection.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE IF NOT EXISTS status(id INTEGER PRIMARY KEY,payload TEXT NOT NULL);"))?;
    if let Some(mut error) = error {
        error.code = format!("background_{}", error.code);
        sql(connection.execute("INSERT INTO status VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload",[encode(&error)?]))?;
    } else {
        sql(connection.execute("DELETE FROM status WHERE id=1", []))?;
    }
    Ok(())
}
pub(crate) fn source<T>(r: agentlaw_storage::Result<T>) -> Result<T> {
    r.map_err(|e| match e {
        agentlaw_storage::Error::InsufficientResource { .. } => { let mut error = DomainError::new("insufficient_resource", "Canonical work requires more available disk or memory; source and retained pending work were preserved. Free resources and retry."); error.retryable = true; error },
        agentlaw_storage::Error::ResourceUnknown(_) => { let mut error = DomainError::new("resource_status_unknown", "Available resource capacity could not be established; source and retained pending work were preserved. Retry after checking host resource status."); error.retryable = true; error },
        agentlaw_storage::Error::Capacity => DomainError::new("source_payload_requires_spool", "Selected source requires file-backed delivery."),
        agentlaw_storage::Error::Stale(_) => DomainError::new("stale_memory_ref", "Source state changed; inspect current heads and retry review."),
        agentlaw_storage::Error::NotFound(_) => DomainError::new("memory_not_found", "Requested source identity is absent."),
        agentlaw_storage::Error::RecoveryRequired(_) => DomainError::new("recovery_required", "Source publication recovery must complete before access."),
        _ => DomainError::new("source_operation_failed", "Canonical operation failed; inspect diagnostics without discarding source or pending work."),
    })
}
pub(crate) fn digest(v: &impl Serialize) -> Result<String> {
    let b = serde_json::to_vec(v).map_err(|_| {
        DomainError::new("serialization_failed", "Could not serialize review basis.")
    })?;
    Ok(format!("{:x}", Sha256::digest(b)))
}
pub(crate) fn external_current(state: &CurrentState) -> Value {
    json!({"memory_id":state.resolved_id,"current_heads":state.heads.iter().cloned().map(RecallHead::from).collect::<Vec<_>>(),"head_reconciliation_required":state.heads.len()>1})
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|_| {
        DomainError::new(
            "control_corrupt",
            "Stored local control payload is invalid; no work was discarded.",
        )
    })
}
pub(crate) fn encode(v: &impl Serialize) -> Result<String> {
    serde_json::to_string(v)
        .map_err(|_| DomainError::new("serialization_failed", "Could not encode control state."))
}
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub struct StoreIdentity {
    pub root: PathBuf,
    pub local: PathBuf,
    pub user_id: String,
    pub machine_id: String,
    pub binding_id: String,
    pub history_limit: usize,
    pub response_limit: usize,
    pub config_file: Option<PathBuf>,
}
pub struct ReadCtx<'a> {
    pub identity: &'a StoreIdentity,
    pub source: &'a agentlaw_storage::published::OwnedPublishedReader,
    pub control: &'a crate::control::ControlReader,
    pub semantic: &'a dyn crate::semantic::SemanticPort,
    pub history_semantic: &'a dyn crate::semantic::HistorySemanticPort,
    pub history_cache: &'a crate::history_cache::HistoryCacheGuards,
    pub request: &'a RequestControl,
}
pub struct LaneCtx<'a> {
    pub identity: &'a StoreIdentity,
    pub source: &'a Store,
    pub control: &'a mut crate::control::ControlWriter,
    pub request: &'a RequestControl,
}

pub struct Runtime {
    pub(crate) store: Store,
    pub(crate) control: Connection,
    pub(crate) root: PathBuf,
    pub(crate) local: PathBuf,
    pub(crate) user_id: String,
    pub(crate) machine_id: String,
    pub(crate) binding_id: String,
    pub(crate) worker: Option<std::sync::Arc<agentlaw_worker::Client>>,
    pub(crate) pump_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) background_error: std::sync::Arc<std::sync::Mutex<Option<DomainError>>>,
    pub(crate) history_response_limit: usize,
    pub(crate) config_file: Option<PathBuf>,
    pub(crate) request_control: RequestControl,
}
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) streaming: crate::read::streaming::StreamedSelection,
    pub(crate) generation: u64,
    pub(crate) states: BTreeMap<String, CurrentState>,
    pub(crate) units: Vec<CurrentUnit>,
    pub(crate) basis: ReadSet,
    pub(crate) publication_basis: agentlaw_storage::ReadSet,
}
impl SourceSnapshot for Snapshot {
    fn current(&self, id: &str) -> Result<Option<CurrentState>> {
        validate_id(id)?;
        let mut cursor = id.to_owned();
        let mut visited = BTreeSet::new();
        let mut path = Vec::new();
        loop {
            if !visited.insert(cursor.clone()) {
                return Err(DomainError::new(
                    "source_corrupt",
                    "Redirect cycle detected.",
                ));
            }
            if let Some(s) = self.states.get(&cursor) {
                let mut s = s.clone();
                s.requested_id = id.into();
                if !path.is_empty() {
                    s.redirect_path = path;
                }
                return Ok(Some(s));
            }
            match self
                .units
                .iter()
                .find(|u| u.entity_type == "memory" && u.entity_id == cursor)
            {
                Some(CurrentUnit {
                    state: UnitState::Redirect { redirect_to, .. },
                    ..
                }) => {
                    path.push(cursor);
                    cursor = redirect_to.clone()
                }
                _ => return Ok(None),
            }
        }
    }
    fn inventory(&self) -> Result<Vec<CurrentState>> {
        Ok(self
            .states
            .values()
            .map(|s| (s.resolved_id.clone(), s.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect())
    }
    fn read_set(&self) -> ReadSet {
        self.basis.clone()
    }
}
pub(crate) fn scope_value(v: &Value) -> Result<Applicability> {
    let mut v = v.clone();
    let scope = v["scope"].as_str().ok_or_else(|| {
        DomainError::new("source_corrupt", "Canonical scope must be a named variant.")
    })?;
    v["scope"] = match scope {
        "project_machine" => json!(["project", "machine"]),
        "user" | "project" | "machine" => json!([scope]),
        _ => {
            return Err(DomainError::new(
                "source_corrupt",
                "Unknown canonical scope variant.",
            ))
        }
    };
    let a: Applicability = serde_json::from_value(v)
        .map_err(|_| DomainError::new("source_corrupt", "Invalid resolved applicability."))?;
    a.validate()?;
    for id in a.project_id.iter().chain(a.machine_id.iter()) {
        validate_id(id)?;
    }
    Ok(a)
}
pub(crate) fn canonical_scope(a: &Applicability) -> Result<Value> {
    a.validate()?;
    let mut value = to_value(a)?;
    value["scope"] = json!(match a.scope.as_slice() {
        [ScopeKind::User] => "user",
        [ScopeKind::Project] => "project",
        [ScopeKind::Machine] => "machine",
        [ScopeKind::Project, ScopeKind::Machine] => "project_machine",
        _ => unreachable!(),
    });
    Ok(value)
}
pub(crate) fn memory_from(unit: &CurrentUnit, head: &Head) -> Result<Memory> {
    let a = scope_value(&head.metadata["applicability"])?;
    let origin: Origin = serde_json::from_value(head.metadata["origin"].clone())
        .map_err(|_| DomainError::new("source_corrupt", "Invalid source origin."))?;
    let relations = head.metadata["relations"]
        .as_array()
        .cloned()
        .ok_or_else(|| {
            DomainError::new(
                "source_corrupt",
                "Memory relations must be explicitly present as an array.",
            )
        })?;
    for relation in &relations {
        let id = relation["target_memory_id"].as_str().ok_or_else(|| {
            DomainError::new("source_corrupt", "Relation target identity is missing.")
        })?;
        validate_id(id)?;
        if !matches!(relation["kind"].as_str(), Some("related" | "required")) {
            return Err(DomainError::new("source_corrupt", "Unknown relation kind."));
        }
    }
    if head
        .metadata
        .get("in_working_set")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(DomainError::new(
            "source_corrupt",
            "Task membership must be a boolean when present.",
        ));
    }
    let relation_ids = |kind: &str| {
        relations
            .iter()
            .filter(|r| r["kind"] == kind)
            .filter_map(|r| r["target_memory_id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    Ok(Memory {
        memory_ref: MemoryRef {
            memory_id: unit.entity_id.clone(),
            observed_version: source(agentlaw_storage::version(unit, head))?,
        },
        what_to_remember: head.body.clone(),
        evidence: String::new(),
        applies_to: a.scope.clone(),
        applicability: a,
        origin,
        in_working_set: head.metadata["in_working_set"].as_bool(),
        is_rule: head.metadata["is_rule"].as_bool().ok_or_else(|| {
            DomainError::new(
                "source_corrupt",
                "Memory is_rule must be explicitly present as a boolean.",
            )
        })?,
        related_memory_ids: relation_ids("related"),
        required_memory_ids: relation_ids("required"),
        work_targets: serde_json::from_value(
            head.metadata.get("work_targets").cloned().ok_or_else(|| {
                DomainError::new(
                    "source_corrupt",
                    "Memory work_targets must be explicitly present.",
                )
            })?,
        )
        .map_err(|_| DomainError::new("source_corrupt", "Invalid work-target metadata."))?,
    })
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Retained {
    #[serde(default)]
    pub(crate) diagnostics: Vec<DomainError>,
    pub(crate) batch: PendingBatch,
    pub(crate) context: RequestContext,
    pub(crate) issues: Vec<Value>,
    pub(crate) basis: String,
    pub(crate) resolution_ref: Option<String>,
    pub(crate) review_refs: Vec<String>,
    pub(crate) approved_review_refs: Vec<String>,
    pub(crate) overlap_approved: bool,
    pub(crate) operation_id: Option<String>,
    pub(crate) mutations: Vec<Mutation>,
    pub(crate) expected_generation: Option<u64>,
    #[serde(default)]
    pub(crate) publication_basis: Option<agentlaw_storage::ReadSet>,
}
impl Runtime {
    pub fn with_worker(mut self, worker: agentlaw_worker::Client) -> Self {
        let worker = std::sync::Arc::new(worker);
        self.worker = Some(worker.clone());
        let reader = self.store.owned_published_reader();
        let binding = self.binding_id.clone();
        let local = self.local.clone();
        let stop = self.pump_stop.clone();
        let background_error = self.background_error.clone();
        std::thread::spawn(move || {
            let adapter = crate::derived::PublishedAdapter::new(reader, binding.clone());
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let outcome = crate::read::pump(&worker, &adapter, &binding, &local, &stop);
                let error = outcome.err();
                let persisted = record_background_status(&local, error.clone());
                if let Ok(mut current) = background_error.lock() {
                    *current=match persisted{Ok(())=>error.map(|mut e|{e.code=format!("background_{}",e.code);e}),Err(_)=>Some(DomainError::new("background_status_unavailable","Background indexing status could not be persisted. No indexing completion is claimed; inspect local storage health."))};
                }
                for _ in 0..10 {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
        self
    }
    pub fn with_history_response_limit(mut self, bytes: usize) -> Self {
        self.history_response_limit = bytes;
        self
    }
    pub fn with_config_file(mut self, path: PathBuf) -> Result<Self> {
        if !path.is_absolute() {
            return Err(DomainError::new(
                "invalid_path",
                "Configuration path must be absolute.",
            ));
        }
        self.config_file = Some(path);
        Ok(self)
    }
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }
    pub fn binding_id(&self) -> &str {
        &self.binding_id
    }
    pub fn published_source(
        &self,
    ) -> crate::derived::PublishedAdapter<agentlaw_storage::published::PublishedReader<'_>> {
        crate::derived::PublishedAdapter::new(
            self.store.published_reader(),
            self.binding_id.clone(),
        )
    }
    pub fn call(&mut self, request: Request) -> Result<Value> {
        if !self.local.join("control.sqlite").is_file() {
            return Err(control_backup_required());
        }
        self.request_control.check()?;
        self.request_control.phase("resolving");
        let mut result = match request {
            Request::ConnectProjectMemory(r) => self.connect_request(r),
            Request::Sync(_) => Err(DomainError::new("sync_requires_app_adapter", "Use the installed Agentlaw MCP/CLI sync adapter; isolated ordinary-memory runtimes cannot transport Git or activate delegation.")),
            Request::Recall(r) => self.recall_request(r),
            Request::RememberThis(r) => self.remember(r),
            Request::History(r) => {
                let id = r
                    .memory_id
                    .as_ref()
                    .or(r.procedure_id.as_ref())
                    .ok_or_else(|| {
                        DomainError::new("invalid_input", "A history subject is required.")
                    })?;
                validate_id(id)?;
                crate::history_projection::project_with_worker_config(
                    &self.store,
                    &self.local,
                    &r,
                    self.history_response_limit,
                    &self.request_control,
                    self.worker
                        .as_deref()
                        .map(|client| (client, self.binding_id.as_str())),
                    self.config_file.as_deref(),
                )
            }
        }?;
        let background = self
            .background_error
            .lock()
            .ok()
            .and_then(|e| e.clone())
            .or_else(|| match Self::background_status(&self.local) {
                Ok(status) => status,
                Err(_) => Some(DomainError::new(
                    "background_status_unavailable",
                    "Background indexing status cannot be read; no indexing completion is claimed.",
                )),
            });
        if let Some(error) = background {
            if let Some(object) = result.as_object_mut() {
                let diagnostics = object.entry("diagnostics").or_insert_with(|| json!([]));
                if let Some(items) = diagnostics.as_array_mut() {
                    items.push(to_value(error)?);
                }
            }
        }
        Ok(result)
    }
    pub fn background_status(local: impl AsRef<Path>) -> Result<Option<DomainError>> {
        let path = local.as_ref().join("background-status.sqlite");
        if !path.exists() {
            return Ok(None);
        }
        let connection = sql(Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ))?;
        let status: Option<String> = sql(connection
            .query_row("SELECT payload FROM status WHERE id=1", [], |r| r.get(0))
            .optional())?;
        status.map(|s| decode(&s)).transpose()
    }
    pub fn call_with_control(
        &mut self,
        request: Request,
        control: RequestControl,
    ) -> Result<Value> {
        let prior = std::mem::replace(&mut self.request_control, control);
        let result = self.call(request);
        self.request_control = prior;
        result
    }
}
pub(crate) fn scope_token(a: &Applicability) -> String {
    match a.scope.as_slice() {
        [ScopeKind::User] => "user".into(),
        [ScopeKind::Machine] => format!("machine:{}", a.machine_id.as_deref().unwrap_or("")),
        [ScopeKind::Project] => format!("project:{}", a.project_id.as_deref().unwrap_or("")),
        [ScopeKind::Project, ScopeKind::Machine] => format!(
            "project:{}:machine:{}",
            a.project_id.as_deref().unwrap_or(""),
            a.machine_id.as_deref().unwrap_or("")
        ),
        _ => "invalid".into(),
    }
}
#[cfg(test)]
mod tests;
