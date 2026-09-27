//! Durable local context and pending state, connected to canonical storage.
use crate::*;
use crate::{context::*, recall::*, write::*};
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
mod retrieval;
mod streaming;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
fn sql<T>(r: std::result::Result<T, rusqlite::Error>) -> Result<T> {
    r.map_err(|_| {
        DomainError::new(
            "control_storage_failed",
            "Local control database operation failed; retained work was not discarded.",
        )
    })
}
fn control_backup_required() -> DomainError {
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
fn source<T>(r: agentlaw_storage::Result<T>) -> Result<T> {
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
fn digest(v: &impl Serialize) -> Result<String> {
    let b = serde_json::to_vec(v).map_err(|_| {
        DomainError::new("serialization_failed", "Could not serialize review basis.")
    })?;
    Ok(format!("{:x}", Sha256::digest(b)))
}
fn external_current(state: &CurrentState) -> Value {
    json!({"memory_id":state.resolved_id,"current_heads":state.heads.iter().cloned().map(RecallHead::from).collect::<Vec<_>>(),"head_reconciliation_required":state.heads.len()>1})
}
fn decode<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|_| {
        DomainError::new(
            "control_corrupt",
            "Stored local control payload is invalid; no work was discarded.",
        )
    })
}
fn encode(v: &impl Serialize) -> Result<String> {
    serde_json::to_string(v)
        .map_err(|_| DomainError::new("serialization_failed", "Could not encode control state."))
}
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub struct Runtime {
    store: Store,
    control: Connection,
    root: PathBuf,
    local: PathBuf,
    user_id: String,
    machine_id: String,
    binding_id: String,
    worker: Option<std::sync::Arc<agentlaw_worker::process::Client>>,
    pump_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    background_error: std::sync::Arc<std::sync::Mutex<Option<DomainError>>>,
    history_response_limit: usize,
    request_control: RequestControl,
}
#[derive(Clone)]
struct Snapshot {
    streaming: streaming::StreamedSelection,
    generation: u64,
    states: BTreeMap<String, CurrentState>,
    units: Vec<CurrentUnit>,
    basis: ReadSet,
    publication_basis: agentlaw_storage::ReadSet,
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
fn canonical_scope(a: &Applicability) -> Result<Value> {
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
fn memory_from(unit: &CurrentUnit, head: &Head) -> Result<Memory> {
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
struct Retained {
    #[serde(default)]
    diagnostics: Vec<DomainError>,
    batch: PendingBatch,
    context: RequestContext,
    issues: Vec<Value>,
    basis: String,
    resolution_ref: Option<String>,
    review_refs: Vec<String>,
    approved_review_refs: Vec<String>,
    overlap_approved: bool,
    operation_id: Option<String>,
    mutations: Vec<Mutation>,
    expected_generation: Option<u64>,
    #[serde(default)]
    publication_basis: Option<agentlaw_storage::ReadSet>,
}
impl Runtime {
    pub fn open(
        store_root: impl AsRef<Path>,
        local_root: impl AsRef<Path>,
        user_id: impl Into<String>,
    ) -> Result<Self> {
        let coordination = local_root.as_ref().join("source-coordination");
        Self::open_inner(store_root, local_root, user_id, None, coordination)
    }
    pub fn open_with_machine(
        store_root: impl AsRef<Path>,
        local_root: impl AsRef<Path>,
        user_id: impl Into<String>,
        machine_id: impl Into<String>,
    ) -> Result<Self> {
        let coordination = local_root.as_ref().join("source-coordination");
        Self::open_with_machine_and_coordination(
            store_root,
            local_root,
            user_id,
            machine_id,
            coordination,
        )
    }
    pub fn open_with_machine_and_coordination(
        store_root: impl AsRef<Path>,
        local_root: impl AsRef<Path>,
        user_id: impl Into<String>,
        machine_id: impl Into<String>,
        coordination: impl AsRef<Path>,
    ) -> Result<Self> {
        let machine_id = machine_id.into();
        validate_id(&machine_id)?;
        Self::open_inner(
            store_root,
            local_root,
            user_id,
            Some(machine_id),
            coordination,
        )
    }
    fn open_inner(
        store_root: impl AsRef<Path>,
        local_root: impl AsRef<Path>,
        user_id: impl Into<String>,
        requested_machine: Option<String>,
        coordination: impl AsRef<Path>,
    ) -> Result<Self> {
        // Check the local binding before opening any canonical writer/fence.
        // A failed A -> B selection must not initialize B with A's local state.
        let existing_control = local_root.as_ref().join("control.sqlite");
        let initialized_marker = local_root.as_ref().join("control-initialized.json");
        if initialized_marker.exists() && !existing_control.exists() {
            return Err(control_backup_required());
        }
        if existing_control.exists() {
            let control = Connection::open_with_flags(
                &existing_control,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|_| control_backup_required())?;
            let initialized:bool=control.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='settings')",[],|r|r.get(0)).map_err(|_|control_backup_required())?;
            if initialized_marker.exists() && !initialized {
                return Err(control_backup_required());
            }
            let selected: Option<String> = if initialized {
                control
                    .query_row(
                        "SELECT value FROM settings WHERE key='store_root'",
                        [],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|_| control_backup_required())?
            } else {
                None
            };
            if let Some(selected) = selected {
                let requested=std::fs::canonicalize(store_root.as_ref()).map_err(|_|DomainError::new("store_binding_mismatch","The requested store cannot use this existing runtime directory. Switch through the installation configuration; existing state was preserved."))?;
                if selected != requested.to_string_lossy() {
                    return Err(DomainError::new("store_binding_mismatch","Local control state belongs to another store. Select the new store through installation configuration using its separate runtime binding; existing state was preserved."));
                }
            }
        }
        let store = source(Store::open_with_coordination(
            store_root.as_ref(),
            local_root.as_ref().join("canonical"),
            coordination,
        ))?;
        let root = std::fs::canonicalize(store_root).map_err(|_| {
            DomainError::new("store_unavailable", "Cannot resolve memory store root.")
        })?;
        let local = local_root.as_ref().to_path_buf();
        let control = sql(Connection::open(local.join("control.sqlite")))?;
        sql(control.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS associations(binding TEXT NOT NULL,path TEXT NOT NULL,project TEXT NOT NULL,PRIMARY KEY(binding,path));
            CREATE TABLE IF NOT EXISTS project_creations(binding TEXT NOT NULL,path TEXT NOT NULL,project TEXT NOT NULL,operation TEXT NOT NULL,payload TEXT NOT NULL,done INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(binding,path));
            CREATE TABLE IF NOT EXISTS pending(id TEXT PRIMARY KEY,version TEXT NOT NULL,binding TEXT NOT NULL,payload TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('pending','published','discarded')),result TEXT,discard_reason TEXT);
            CREATE TABLE IF NOT EXISTS authoring(token TEXT PRIMARY KEY,binding TEXT NOT NULL,basis TEXT NOT NULL,used INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS authoring_executions(token TEXT PRIMARY KEY,binding TEXT NOT NULL,operation TEXT NOT NULL,generation TEXT NOT NULL,mutation TEXT NOT NULL,submission TEXT NOT NULL,status TEXT NOT NULL,result TEXT);
        "))?;
        let setting = |key: &str, default: String| -> Result<String> {
            sql(control.execute(
                "INSERT OR IGNORE INTO settings VALUES(?1,?2)",
                params![key, default],
            ))?;
            sql(
                control.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                    r.get(0)
                }),
            )
        };
        let machine_id = setting(
            "machine_id",
            requested_machine
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        )?;
        if requested_machine
            .as_ref()
            .is_some_and(|expected| expected != &machine_id)
        {
            return Err(DomainError::new("machine_identity_mismatch","This runtime binding belongs to a different installation machine identity; it was not silently reassigned."));
        }
        let binding_id = setting("binding_id", uuid::Uuid::new_v4().to_string())?;
        let bound_root = setting("store_root", root.to_string_lossy().into_owned())?;
        if bound_root != root.to_string_lossy() {
            return Err(DomainError::new("store_binding_mismatch","This local state belongs to another memory store. Switch through installation configuration; pending work keeps its original store binding."));
        }
        if !initialized_marker.exists() {
            use std::io::Write;
            let mut marker =
                tempfile::NamedTempFile::new_in(&local).map_err(|_| control_backup_required())?;
            marker
                .write_all(encode(&json!({"binding_id":binding_id,"format":1}))?.as_bytes())
                .map_err(|_| control_backup_required())?;
            marker
                .as_file()
                .sync_all()
                .map_err(|_| control_backup_required())?;
            match marker.persist_noclobber(&initialized_marker) {
                Ok(_) => {}
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(control_backup_required()),
            }
        }
        let marker: Value = serde_json::from_slice(
            &std::fs::read(&initialized_marker).map_err(|_| control_backup_required())?,
        )
        .map_err(|_| control_backup_required())?;
        if marker["binding_id"] != binding_id {
            return Err(control_backup_required());
        }
        let runtime = Self {
            store,
            control,
            root,
            local,
            user_id: user_id.into(),
            machine_id,
            binding_id,
            worker: None,
            pump_stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            background_error: Default::default(),
            history_response_limit: 8192,
            request_control: RequestControl::default(),
        };
        runtime.recover_authoring()?;
        runtime.recover_pending_executions()?;
        Ok(runtime)
    }
    pub fn with_worker(mut self, worker: agentlaw_worker::process::Client) -> Self {
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
                let outcome = retrieval::pump(&worker, &adapter, &binding, &local, &stop);
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
                crate::history_projection::project_with_worker(
                    &self.store,
                    &self.local,
                    &r,
                    self.history_response_limit,
                    &self.request_control,
                    self.worker
                        .as_deref()
                        .map(|client| (client, self.binding_id.as_str())),
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
    fn recall_request(&self, r: RecallRequest) -> Result<Value> {
        let context = match resolve_context(self, r.project_path.as_deref()) {
            Ok(context) => context,
            Err(error) if error.code == "project_connection_required" => {
                // Discovery is read-only. A candidate, even a sole candidate, is not a binding.
                let candidates = self.discover(None)?;
                return Ok(json!({
                    "status":"needs_user_input",
                    "code":"project_connection_required",
                    "message":"Choose a project memory for this folder.",
                    "candidates":candidates,
                    "project_path":r.project_path,
                    "turn_instruction":"Project memory has not been retrieved. Use an already explicit user choice, or ask the user to select a candidate, approve creating a new project, or skip for now. Never auto-select merely because one candidate exists. After approved connect_project_memory, use its recall_result if requested; otherwise repeat the original recall."
                }));
            }
            Err(error) => return Err(error),
        };
        self.recall_with_context(r, context)
    }
    fn connect_request(&mut self, mut request: ConnectRequest) -> Result<Value> {
        let restore = request.restore_context == Some(true);
        request.restore_context = None;
        let connected = connect(self, &request)?;
        let project = connected
            .project_connection
            .as_ref()
            .map(|p| p.project_id.clone());
        let mut result = to_value(connected)?;
        if project.is_none() {
            result["next_action"] = json!("No project was connected and project memory has not been retrieved. Explain candidates in the user's language and ask the user which project to connect, even for one candidate. Call agentlaw with action=\"connect_project_memory\" and a connect_project_memory object containing the same verified project_path, intent=\"connect\", and the selected project_id. Only after first-time adoption is confirmed, use intent=\"create\" and project_name instead. No candidates alone is not permission to create. After connection, retry the original recall; alternatively include restore_context=true and recall_for in the connection call to restore context there.");
        }
        if restore {
            if let Some(project) = project {
                let prepared = self.prepare_for_connection(self.request_control.clone())?;
                let context = self.request_context(Some(project))?;
                let recall = self.recall_with_context(
                    RecallRequest {
                        recall_for: request.recall_for,
                        restore_context: Some(true),
                        ..Default::default()
                    },
                    context,
                )?;
                let incomplete = prepared["semantic_complete"] != true
                    || recall["diagnostics"].as_array().is_some_and(|items| {
                        items.iter().any(|d| {
                            d["code"]
                                .as_str()
                                .is_some_and(|code| code.starts_with("semantic_channel_"))
                        })
                    });
                result["recall_result"] = recall;
                if incomplete {
                    result["code"] = json!("restore_preparation_incomplete");
                    result["turn_instruction"]=json!("Project association is retained, but semantic context restoration is incomplete. Read the returned diagnostics and any full-content artifact; do not claim complete restoration. Repeat this connect request with restore_context after the provider recovers.");
                    result["diagnostics"] = prepared["diagnostics"].clone();
                }
            }
        }
        Ok(result)
    }
    fn recall_with_context(&self, r: RecallRequest, mut context: RequestContext) -> Result<Value> {
        if let Some(hint) = &r.project_hint {
            let candidates = self.discover(Some(&ProjectClues {
                name: Some(hint.clone()),
                description: Some(hint.clone()),
                repository_url: None,
            }))?;
            if let Some(selected) = &r.selected_project_id {
                validate_id(selected)?;
                if !candidates.iter().any(|c| &c.project_id == selected) {
                    return Err(DomainError::new(
                        "project_selection_invalid",
                        "The selected project no longer matches the repeated project hint.",
                    ));
                }
                context.project_id = Some(selected.clone());
            } else if candidates.len() == 1 {
                context.project_id = Some(candidates[0].project_id.clone());
            } else {
                return Ok(
                    json!({"code":"project_selection_required","project_candidates":candidates,"turn_instruction":"Repeat this recall with the same project_hint and a selected_project_id after resolving the intended project. Current project associations were not changed."}),
                );
            }
        }
        if let Some(machine) = &r.machine_id {
            validate_id(machine)?;
            context.machine_id = machine.clone()
        }
        let (mut snapshot, search) = match self.recall_material(&r, &context) {
            Ok(material) => material,
            Err(e) if e.code == "source_payload_requires_spool" && r.recall_for.is_none() => {
                return self.stream_exact_recall(&r)
            }
            Err(e) => return Err(e),
        };
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| DomainError::new("clock_failed", "Cannot format the current time."))?;
        let mut memory_request = r.clone();
        memory_request.procedure_ids = None;
        memory_request.project_hint = None;
        memory_request.selected_project_id = None;
        let mut result = recall(&snapshot, &search, &context, &memory_request, now)?;
        result
            .diagnostics
            .retain(|d| !d.message.starts_with("Learned procedure discovery"));
        let mut result = to_value(result)?;
        if r.include_active_tasks == Some(true) {
            let mut output = Vec::new();
            for candidate in result["candidates"].as_array().cloned().unwrap_or_default() {
                let id = candidate["memory_id"].as_str().unwrap_or("");
                if let Some(state) = snapshot.states.get(id) {
                    let active: Vec<_> = state
                        .heads
                        .iter()
                        .filter(|h| {
                            h.in_working_set == Some(true)
                                && scope_matches(&h.applicability, &context)
                        })
                        .collect();
                    if !active.is_empty() {
                        for head in active {
                            let [objective, current_position, resume_point] =
                                snapshot.streaming.task_sections(&head.what_to_remember)?;
                            let mut paths = candidate["retrieval_paths"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default();
                            paths.push(json!({"via":"active_working_set","clue":"Active Task"}));
                            output.push(json!({"memory_id":id,"objective":objective,"current_position":current_position,"resume_point":resume_point,"retrieval_paths":paths}));
                        }
                        continue;
                    }
                }
                output.push(candidate)
            }
            result["candidates"] = json!(output);
        }
        let exact: BTreeSet<_> = r
            .procedure_ids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut procedures = Vec::new();
        let mut missing = Vec::new();
        for id in &exact {
            validate_id(id)?;
            match snapshot
                .units
                .iter()
                .find(|u| u.entity_type == "learned_procedure" && u.entity_id == *id)
            {
                None => missing.push(id.clone()),
                Some(u) => procedures.push(procedure_output(u, true)?),
            }
        }
        if r.recall_for.is_some() {
            let automatic = search
                .full_procedure
                .as_ref()
                .filter(|id| !exact.contains(*id));
            if let Some(id) = automatic {
                if let Some(unit) = snapshot
                    .units
                    .iter()
                    .find(|u| u.entity_type == "learned_procedure" && u.entity_id == *id)
                {
                    procedures.push(procedure_output(unit, true)?);
                }
            }
            let ids: Vec<_> = search
                .procedure_order
                .iter()
                .filter(|id| !exact.contains(*id) && Some(*id) != automatic)
                .collect();
            let count = ids.len();
            let limit = r.procedure_candidate_limit.unwrap_or(5) as usize;
            for id in ids.into_iter().take(limit) {
                if let Some(unit) = snapshot
                    .units
                    .iter()
                    .find(|u| u.entity_type == "learned_procedure" && u.entity_id == *id)
                {
                    let mut descriptors = Vec::new();
                    for head in unit.heads() {
                        let a = scope_value(&head.metadata["applicability"])?;
                        if scope_matches(&a, &context) {
                            let descriptor = json!({"procedure_id":id,"name":head.metadata["name"],"use_when":head.metadata["use_when"],"applicability":a});
                            if !descriptors.contains(&descriptor) {
                                descriptors.push(descriptor);
                            }
                        }
                    }
                    descriptors.sort_by_key(Value::to_string);
                    procedures.extend(descriptors);
                }
            }
            result["candidate_counts"]["learned_procedures"] =
                json!({"matched":count,"shown":count.min(limit)});
        }
        result["learned_procedures"] = json!(procedures);
        if !missing.is_empty() {
            result["code"] = json!("exact_selection_incomplete");
            if result.get("missing_ids").is_none() {
                result["missing_ids"] = json!({"memory_ids":[],"procedure_ids":[]})
            }
            result["missing_ids"]["procedure_ids"] = json!(missing)
        }
        let mut statement=sql(self.control.prepare("SELECT id,version FROM pending WHERE binding=?1 AND status='pending' AND (json_extract(payload,'$.context.project_id') IS NULL OR json_extract(payload,'$.context.project_id')=?2) ORDER BY id"))?;
        let pending=sql(statement.query_map(params![self.binding_id,context.project_id],|row|Ok(json!({"pending_batch_ref":{"pending_batch_id":row.get::<_,String>(0)?,"observed_version":row.get::<_,String>(1)?}}))))?.collect::<std::result::Result<Vec<_>,_>>().map_err(|_|DomainError::new("control_storage_failed","Pending notices could not be read; retained work was not discarded."))?;
        if !pending.is_empty() {
            result["pending_batches"] = json!(pending);
            result["pending_work_instruction"]=json!("Retained proposals still need attention. Use remember_this pending_action=inspect with a listed pending_batch_ref before revising or continuing them. This notice does not publish or approve the proposals.");
        }
        snapshot
            .streaming
            .finish(result, &self.local, &self.request_control)
    }
    fn load_pending(
        &self,
        reference: &PendingBatchRef,
    ) -> Result<(Retained, String, Option<String>)> {
        validate_id(&reference.pending_batch_id)?;
        let row: Option<(String, String, Option<String>)> = sql(self
            .control
            .query_row(
                "SELECT payload,status,result FROM pending WHERE id=?1 AND binding=?2",
                params![reference.pending_batch_id, self.binding_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional())?;
        let (payload, status, result) = row.ok_or_else(|| {
            DomainError::new(
                "pending_not_found",
                "Pending batch does not exist in the selected store binding.",
            )
        })?;
        Ok((decode(&payload)?, status, result))
    }
    fn pending_response(retained: &Retained, status: &str) -> Value {
        json!({"status":status,"pending_batch_ref":retained.batch.reference,"proposal_map":retained.batch.proposals.iter().enumerate().map(|(i,p)|json!({"proposal_index":i,"proposal_id":p.proposal_id})).collect::<Vec<_>>(),"proposals":retained.batch.proposals,"issues":retained.issues,"resolution_ref":retained.resolution_ref,"diagnostics":retained.diagnostics})
    }
    fn remember(&mut self, r: RememberRequest) -> Result<Value> {
        if r.procedure.is_some() {
            return self.procedure(&r);
        }
        let mut old_version = None;
        let mut retained = if let Some(reference) = &r.pending_batch_ref {
            let (mut retained, status, result) = self.load_pending(reference)?;
            match r.pending_action.as_ref() {
                Some(PendingAction::Inspect) => {
                    return if status == "published" {
                        decode(result.as_deref().unwrap_or("null"))
                    } else {
                        Ok(Self::pending_response(&retained, &status))
                    }
                }
                Some(PendingAction::Discard) => {
                    if r.user_confirmed != Some(true)
                        || r.discard_reason.as_deref().unwrap_or("").is_empty()
                    {
                        return Err(DomainError::new(
                            "invalid_input",
                            "Explicit user confirmation and a discard reason are required.",
                        ));
                    }
                    if retained.batch.reference != *reference {
                        return Err(DomainError::new(
                            "stale_pending_ref",
                            "Inspect latest pending work before discarding.",
                        ));
                    }
                    if status != "pending" || retained.operation_id.is_some() {
                        return Err(DomainError::new(
                            "publication_in_progress",
                            "A decided or completed publication cannot be discarded as pending.",
                        ));
                    }
                    let changed=sql(self.control.execute("UPDATE pending SET status='discarded',discard_reason=?1 WHERE id=?2 AND version=?3 AND status='pending'",params![r.discard_reason,reference.pending_batch_id,reference.observed_version]))?;
                    if changed != 1 {
                        return Err(DomainError::new(
                            "stale_pending_ref",
                            "Pending state changed.",
                        ));
                    }
                    return Ok(json!({"status":"discarded","pending_batch_ref":reference}));
                }
                Some(PendingAction::Continue) => {}
                _ => {
                    return Err(DomainError::new(
                        "invalid_input",
                        "pending_action is required.",
                    ))
                }
            }
            if status == "published" {
                return decode(result.as_deref().unwrap_or("null"));
            }
            if status != "pending" {
                return Err(DomainError::new(
                    "pending_discarded",
                    "This pending batch was explicitly discarded.",
                ));
            }
            if retained.batch.reference != *reference {
                return Err(DomainError::new(
                    "stale_pending_ref",
                    "Inspect the latest retained proposals before continuing.",
                ));
            }
            old_version = Some(reference.observed_version.clone());
            if let Some(delta) = &r.memories {
                if retained.operation_id.is_some() {
                    return Err(DomainError::new(
                        "publication_in_progress",
                        "Resume the retained execution before editing its proposals.",
                    ));
                }
                retained.batch = retained
                    .batch
                    .apply_delta(&retained.context, reference, delta)?;
            }
            retained
        } else {
            let context = resolve_context(self, r.project_path.as_deref())?;
            let proposals = r
                .memories
                .ok_or_else(|| DomainError::new("invalid_input", "memories are required."))?;
            Retained {
                diagnostics: vec![],
                batch: PendingBatch::new(&context, proposals),
                context,
                issues: vec![],
                basis: String::new(),
                resolution_ref: None,
                review_refs: vec![],
                approved_review_refs: vec![],
                overlap_approved: false,
                operation_id: None,
                mutations: vec![],
                expected_generation: None,
                publication_basis: None,
            }
        };
        // Replaying the same internal operation reuses its exact persisted mutations.
        if retained.operation_id.is_some() {
            return self.publish_retained(retained);
        }
        let mut snapshot = self.write_material(&retained.context, &retained.batch.proposals)?;
        struct StructuralOnly;
        impl ReviewGate for StructuralOnly {
            fn review(&self, _: &RequestContext, _: &[MemoryProposal], _: &ReadSet) -> Result<()> {
                Ok(())
            }
        }
        let prepared = preflight(
            &snapshot,
            &StructuralOnly,
            &retained.context,
            &retained.batch.proposals,
        )?;
        for (proposal, scope) in prepared
            .proposals
            .iter()
            .zip(&prepared.resolved_applicability)
        {
            if retrieval::requires_overlap(&snapshot, proposal, scope) {
                snapshot
                    .publication_basis
                    .watched_scopes
                    .push(canonical_scope(scope)?);
            }
        }
        let basis = digest(&(
            snapshot.basis.fingerprint.clone(),
            &prepared.proposals,
            &prepared.resolved_applicability,
        ))?;
        let same_basis = retained.basis == basis;
        if r.resolution_ref.is_some() && r.resolution_ref != retained.resolution_ref {
            return Err(DomainError::new("invalid_resolution_ref","The resolution token does not match retained review. Inspect the pending batch for its current context."));
        }
        let overlap_confirmed = same_basis
            && (retained.overlap_approved
                || (retained.resolution_ref.is_some()
                    && r.resolution_ref == retained.resolution_ref));
        let acknowledged: BTreeSet<_> = {
            retained
                .approved_review_refs
                .iter()
                .cloned()
                .chain(
                    r.review_judgments
                        .unwrap_or_default()
                        .into_iter()
                        .map(|r| r.review_ref),
                )
                .collect()
        };
        if acknowledged
            .iter()
            .any(|token| !retained.review_refs.contains(token))
        {
            return Err(DomainError::new(
                "invalid_review_ref",
                "A judgment does not match a retained dependent review.",
            ));
        }
        let affected: BTreeSet<_> = prepared
            .proposals
            .iter()
            .flat_map(|p| {
                p.parent_refs
                    .iter()
                    .flatten()
                    .chain(p.consolidation_refs.iter().flatten())
                    .map(|r| r.memory_id.clone())
            })
            .collect();
        let dependent: Vec<_> = snapshot
            .states
            .values()
            .filter(|s| {
                !affected.contains(&s.resolved_id)
                    && s.heads
                        .iter()
                        .any(|h| h.required_memory_ids.iter().any(|id| affected.contains(id)))
            })
            .collect();
        let (overlaps, diagnostics) = self.overlap_candidates(&snapshot, &prepared, &affected)?;
        retained.diagnostics = diagnostics;
        let mut issues = Vec::new();
        if !overlaps.is_empty() && !overlap_confirmed {
            issues.push(json!({"code":"possible_overlap","path":"/resolution_ref","message":"Review these current memories and confirm they remain distinct, or replace the retained proposals with evolve/consolidate requests.","current_memories":overlaps.iter().map(external_current).collect::<Vec<_>>()}));
        }
        let mut review_refs = Vec::new();
        for dependent in dependent {
            let required: BTreeSet<_> = dependent
                .heads
                .iter()
                .flat_map(|h| h.required_memory_ids.iter())
                .collect();
            let relevant: Vec<_> = prepared
                .proposals
                .iter()
                .filter(|p| {
                    p.parent_refs
                        .iter()
                        .flatten()
                        .chain(p.consolidation_refs.iter().flatten())
                        .any(|r| required.contains(&r.memory_id))
                })
                .collect();
            let token = digest(&(&dependent.resolved_id, &dependent.heads, relevant))?;
            review_refs.push(token.clone());
            if !acknowledged.contains(&token) {
                issues.push(json!({"code":"required_dependent_review","path":"/review_judgments","review_ref":token,"current_memory":external_current(dependent),"message":"Review this dependent against the proposed change; submit its complete update or an unchanged judgment."}));
            }
        }
        retained.basis = basis;
        retained.issues = issues;
        retained.review_refs = review_refs;
        retained.approved_review_refs = acknowledged
            .into_iter()
            .filter(|token| retained.review_refs.contains(token))
            .collect();
        retained.overlap_approved = overlap_confirmed;
        if !same_basis {
            retained.resolution_ref =
                (!overlaps.is_empty()).then(|| uuid::Uuid::new_v4().to_string());
        }
        retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
        if retained.issues.is_empty() {
            retained.mutations = self.mutations(&prepared, &retained.batch.proposals, &snapshot)?;
            retained.operation_id = Some(uuid::Uuid::new_v4().to_string());
            retained.expected_generation = Some(snapshot.generation);
            retained.publication_basis = Some(snapshot.publication_basis.clone());
        }
        self.save_pending(&retained, old_version.as_deref())?;
        if !retained.issues.is_empty() {
            return Ok(Self::pending_response(&retained, "resolution_required"));
        }
        self.publish_retained(retained)
    }
    fn publish_retained(&self, mut retained: Retained) -> Result<Value> {
        let generation = retained.expected_generation.ok_or_else(|| {
            DomainError::new(
                "control_corrupt",
                "Retained execution lacks its source generation.",
            )
        })?;
        self.request_control.phase("publishing");
        let publication = if let Some(basis) = &retained.publication_basis {
            self.store.publish_with_readset(
                retained.operation_id.as_ref().unwrap(),
                retained.mutations.clone(),
                basis,
                Some(&self.request_control.cancel),
            )
        } else {
            self.store.publish_if_generation_control(
                retained.operation_id.as_ref().unwrap(),
                retained.mutations.clone(),
                generation,
                &self.request_control.cancel,
            )
        };
        match publication {
            Ok(receipt) => {
                self.request_control.phase("published");
                self.complete_pending(&retained, receipt)
            }
            Err(agentlaw_storage::Error::CancelledBeforeDecision) => {
                let old = retained.batch.reference.observed_version.clone();
                retained.operation_id = None;
                retained.expected_generation = None;
                retained.resolution_ref = Some(uuid::Uuid::new_v4().to_string());
                retained.mutations.clear();
                retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
                retained.issues = vec![
                    json!({"code":"cancelled","message":"Publication was cancelled before its durable decision. The proposal remains retained and will not publish automatically."}),
                ];
                self.save_pending(&retained, Some(&old))?;
                Ok(Self::pending_response(&retained, "pending"))
            }
            Err(agentlaw_storage::Error::Stale(_)) => {
                let old = retained.batch.reference.observed_version.clone();
                retained.operation_id = None;
                retained.expected_generation = None;
                retained.resolution_ref = Some(uuid::Uuid::new_v4().to_string());
                retained.mutations.clear();
                retained.basis.clear();
                retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
                retained.issues = vec![
                    json!({"code":"source_changed","message":"The source changed after review. Inspect retained proposals and continue to revalidate all heads and new dependencies."}),
                ];
                self.save_pending(&retained, Some(&old))?;
                Ok(Self::pending_response(&retained, "resolution_required"))
            }
            Err(e) => source(Err(e)),
        }
    }
    fn save_pending(&self, r: &Retained, old: Option<&str>) -> Result<()> {
        let payload = encode(r)?;
        if let Some(old) = old {
            let n=sql(self.control.execute("UPDATE pending SET version=?1,payload=?2 WHERE id=?3 AND version=?4 AND status='pending'",params![r.batch.reference.observed_version,payload,r.batch.reference.pending_batch_id,old]))?;
            if n != 1 {
                return Err(DomainError::new(
                    "stale_pending_ref",
                    "Pending proposals changed concurrently; inspect latest retained work.",
                ));
            }
        } else {
            sql(self.control.execute("INSERT INTO pending(id,version,binding,payload,status) VALUES(?1,?2,?3,?4,'pending')",params![r.batch.reference.pending_batch_id,r.batch.reference.observed_version,r.batch.store_binding_id,payload]))?;
        }
        Ok(())
    }
    fn complete_pending(
        &self,
        r: &Retained,
        receipt: agentlaw_storage::PublishReceipt,
    ) -> Result<Value> {
        let results=r.mutations.iter().filter(|m|matches!(m.unit.state,UnitState::Live{..})).enumerate().map(|(index,m)|{
            let reference=receipt.references.iter().find(|reference|reference.memory_id==m.unit.entity_id).ok_or_else(||DomainError::new("publication_receipt_incomplete","Published receipt lacks a proposal destination; preserve retained execution and inspect source."))?;
            Ok(json!({"proposal_index":index,"memory_ref":{"memory_id":reference.memory_id,"observed_version":reference.observed_version}}))
        }).collect::<Result<Vec<_>>>()?;
        let result = json!({"status":"remembered","results":results,"diagnostics":r.diagnostics});
        // The background reader consumes the durable source ledger. Publication
        // completion does not wait for historical indexing or embedding work.
        sql(self.control.execute(
            "UPDATE pending SET status='published',result=?1 WHERE id=?2",
            params![encode(&result)?, r.batch.reference.pending_batch_id],
        ))?;
        Ok(result)
    }
    /// Schedule durable indexing through the authenticated worker IPC. Source
    /// access remains in this read-only adapter; the worker receives no paths or
    /// canonical write authority. Queue acceptance is not an index acknowledgment.
    pub fn enqueue_derived(&self) -> Result<()> {
        let worker = self.worker.as_ref().ok_or_else(|| {
            DomainError::new(
                "indexing_pending",
                "No worker is attached; source publication remains complete.",
            )
        })?;
        retrieval::pump(
            worker,
            &crate::derived::PublishedAdapter::new(
                self.store.owned_published_reader(),
                self.binding_id.clone(),
            ),
            &self.binding_id,
            &self.local,
            &self.request_control.cancel,
        )
    }
    fn mutations(
        &self,
        p: &PreparedIntent,
        originals: &[MemoryProposal],
        snapshot: &Snapshot,
    ) -> Result<Vec<Mutation>> {
        let mut result = Vec::new();
        for (index, (proposal, a)) in p
            .proposals
            .iter()
            .zip(&p.resolved_applicability)
            .enumerate()
        {
            let refs: Vec<_> = proposal
                .parent_refs
                .iter()
                .flatten()
                .chain(proposal.consolidation_refs.iter().flatten())
                .collect();
            let id = refs
                .iter()
                .map(|r| r.memory_id.clone())
                .min()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let mut relations: Vec<_> = proposal
                .related_memory_ids
                .iter()
                .flatten()
                .map(|id| json!({"kind":"related","target_memory_id":id}))
                .chain(
                    proposal
                        .required_memory_ids
                        .iter()
                        .flatten()
                        .map(|id| json!({"kind":"required","target_memory_id":id})),
                )
                .collect();
            // Omitted relationship fields inherit the entire canonical relation,
            // including optional descriptions, not merely the exposed target ID.
            let original = &originals[index];
            for (kind, explicit) in [
                ("related", original.related_memory_ids.is_some()),
                ("required", original.required_memory_ids.is_some()),
            ] {
                if !explicit && !refs.is_empty() {
                    let mut parent_sets = Vec::new();
                    for reference in &refs {
                        let unit = snapshot
                            .units
                            .iter()
                            .find(|u| u.entity_id == reference.memory_id)
                            .ok_or_else(|| {
                                DomainError::new(
                                    "source_changed",
                                    "Parent identity disappeared from the prepared snapshot.",
                                )
                            })?;
                        for h in unit.heads() {
                            if source(agentlaw_storage::version(unit, h))?
                                == reference.observed_version
                            {
                                let mut set = h.metadata["relations"]
                                    .as_array()
                                    .ok_or_else(|| {
                                        DomainError::new(
                                            "source_corrupt",
                                            "Parent relations are missing.",
                                        )
                                    })?
                                    .iter()
                                    .filter(|r| r["kind"] == kind)
                                    .cloned()
                                    .collect::<Vec<_>>();
                                set.sort_by_key(Value::to_string);
                                parent_sets.push(set)
                            }
                        }
                    }
                    if let Some(first) = parent_sets.first() {
                        if parent_sets.iter().any(|set| set != first) {
                            return Err(DomainError::new("metadata_resolution_required",format!("Explicitly resolve {kind} relationships; canonical parent relation metadata differs.")));
                        }
                        relations.retain(|r| r["kind"] != kind);
                        relations.extend(first.iter().cloned());
                    }
                }
            }
            let mut metadata = json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":canonical_scope(a)?,"origin":{"machine_id":self.machine_id,"project_id":p.context.project_id,"user_id":self.user_id},"recorded_at_ms":now_ms(),"is_rule":proposal.is_rule.unwrap_or(false),"relations":relations,"work_targets":proposal.work_targets.clone().unwrap_or_default()});
            if let Some(v) = proposal.in_working_set {
                metadata["in_working_set"] = json!(v)
            }
            let change = metadata["change_id"].as_str().unwrap().to_owned();
            result.push(Mutation {
                unit: CurrentUnit {
                    entity_id: id.clone(),
                    entity_type: "memory".into(),
                    state: UnitState::Live {
                        heads: vec![Head {
                            metadata,
                            body: proposal.what_to_remember.clone(),
                        }],
                    },
                },
                expected_versions: refs
                    .iter()
                    .filter(|r| r.memory_id == id)
                    .map(|r| r.observed_version.clone())
                    .collect(),
                evidence: proposal.evidence.clone(),
            });
            if proposal.operation == Operation::Consolidate {
                let sources: BTreeSet<_> = refs
                    .iter()
                    .filter(|r| r.memory_id != id)
                    .map(|r| r.memory_id.clone())
                    .collect();
                for redirected in sources {
                    result.push(Mutation {
                        unit: CurrentUnit {
                            entity_id: redirected.clone(),
                            entity_type: "memory".into(),
                            state: UnitState::Redirect {
                                redirect_to: id.clone(),
                                consolidation_change_id: change.clone(),
                            },
                        },
                        expected_versions: refs
                            .iter()
                            .filter(|r| r.memory_id == redirected)
                            .map(|r| r.observed_version.clone())
                            .collect(),
                        evidence: proposal.evidence.clone(),
                    });
                }
            }
        }
        Ok(result)
    }
    fn procedure(&self, r: &RememberRequest) -> Result<Value> {
        let previous: Option<(String, String, Option<String>)> = if let Some(token) =
            &r.authoring_ref
        {
            sql(self.control.query_row("SELECT status,submission,result FROM authoring_executions WHERE token=?1 AND binding=?2",params![token,self.binding_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional())?
        } else {
            None
        };
        if let Some((status, submission, Some(result))) = &previous {
            if status == "published"
                && decode::<RememberRequest>(submission).and_then(|stored| encode(&stored))?
                    == encode(r)?
            {
                return decode(result);
            }
        }
        let p = r.procedure.as_ref().unwrap();
        let context = resolve_context(self, r.project_path.as_deref())?;
        let procedure_ids = p
            .parent_refs
            .as_ref()
            .map(|refs| {
                refs.iter()
                    .map(|r| r.procedure_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let snapshot = self.snapshot_for_recall(&p.evidence_memory_ids, &procedure_ids)?;
        let mut evidence = Vec::new();
        for id in &p.evidence_memory_ids {
            validate_id(id)?;
            evidence.push(snapshot.current(id)?.ok_or_else(|| {
                DomainError::new("memory_not_found", "Procedure evidence memory is absent.")
            })?)
        }
        let mut parents = Vec::new();
        let mut stale_parents = false;
        let procedure_id = match p.operation {
            Operation::Create => None,
            Operation::Evolve => {
                let refs = p.parent_refs.as_ref().ok_or_else(|| {
                    DomainError::new("invalid_input", "Procedure evolve requires parent_refs.")
                })?;
                let ids: BTreeSet<_> = refs.iter().map(|r| r.procedure_id.clone()).collect();
                if ids.len() != 1 {
                    return Err(DomainError::new(
                        "invalid_parent_identity",
                        "Procedure evolve requires exactly one identity.",
                    ));
                }
                let id = ids.into_iter().next().unwrap();
                validate_id(&id)?;
                let unit = snapshot
                    .units
                    .iter()
                    .find(|u| u.entity_id == id && u.entity_type == "learned_procedure")
                    .ok_or_else(|| {
                        DomainError::new("procedure_not_found", "Procedure parent is absent.")
                    })?;
                let actual: BTreeSet<_> = source(unit.references())?
                    .into_iter()
                    .map(|r| snapshot.streaming.actual_version(r.observed_version))
                    .collect();
                let submitted: BTreeSet<_> =
                    refs.iter().map(|r| r.observed_version.clone()).collect();
                if actual != submitted {
                    stale_parents = true;
                }
                parents.extend(unit.heads().iter().cloned());
                Some(id)
            }
            Operation::Consolidate => {
                return Err(DomainError::unsupported(
                    "Procedure consolidation is not an accepted operation.",
                ))
            }
        };
        let applies = if let Some(scope) = &p.applies_to {
            if scope.contains(&ScopeKind::Project) && context.project_id.is_none() {
                return Err(DomainError::new(
                    "project_connection_required",
                    "Project-scoped procedure requires a connected project.",
                ));
            }
            Some(Applicability {
                scope: scope.clone(),
                project_id: if scope.contains(&ScopeKind::Project) {
                    context.project_id.clone()
                } else {
                    None
                },
                machine_id: scope
                    .contains(&ScopeKind::Machine)
                    .then(|| context.machine_id.clone()),
            })
        } else if !parents.is_empty() {
            let first = scope_value(&parents[0].metadata["applicability"])?;
            if parents
                .iter()
                .any(|h| scope_value(&h.metadata["applicability"]).ok().as_ref() != Some(&first))
            {
                return Err(DomainError::new(
                    "metadata_resolution_required",
                    "Specify applies_to because procedure parent scopes differ.",
                ));
            }
            Some(first)
        } else {
            None
        };
        // Preparation may omit create scope, so bind evidence/parents independently
        // of the final chosen applicability while still validating final scope.
        let basis = digest(&(
            self.binding_id.clone(),
            &p.operation,
            &p.evidence_memory_ids,
            evidence
                .iter()
                .flat_map(|state| state.heads.iter().map(|head| &head.memory_ref))
                .collect::<Vec<_>>(),
            snapshot
                .units
                .iter()
                .filter(|unit| unit.entity_type == "learned_procedure")
                .map(|unit| {
                    source(unit.references()).map(|refs| {
                        refs.into_iter()
                            .map(|r| snapshot.streaming.actual_version(r.observed_version))
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            "builtin-procedure-authoring-v1",
        ))?;
        let valid = if let Some(token) = &r.authoring_ref {
            sql(self
                .control
                .query_row(
                    "SELECT basis FROM authoring WHERE token=?1 AND binding=?2 AND used=0",
                    params![token, self.binding_id],
                    |row| row.get::<_, String>(0),
                )
                .optional())?
            .is_some_and(|stored| stored == basis)
        } else {
            false
        };
        if p.instructions.is_none() || !valid || stale_parents {
            let token = uuid::Uuid::new_v4().to_string();
            sql(self.control.execute(
                "INSERT INTO authoring(token,binding,basis) VALUES(?1,?2,?3)",
                params![token, self.binding_id, basis],
            ))?;
            let current = snapshot
                .units
                .iter()
                .filter(|u| {
                    u.entity_type == "learned_procedure"
                        && Some(&u.entity_id) == procedure_id.as_ref()
                })
                .map(|u| procedure_output(u, true))
                .collect::<Result<Vec<_>>>()?;
            let query = evidence
                .iter()
                .flat_map(|state| state.heads.iter())
                .map(|head| snapshot.streaming.excerpt(&head.what_to_remember, 512))
                .collect::<Result<Vec<_>>>()?
                .join("\n");
            let nearby = self.search_procedures(&query, r.project_path.as_deref(), 5)?;
            let mut response = json!({"status":"authoring_required","authoring_ref":token,"builtin_instructions":"Write a complete reusable instruction-only procedure. State when to use it, preserve conditions and exceptions supported by the supplied memories, distinguish observed evidence from hypotheses, and avoid executable installation scripts. Review every parent head. Nearby entries are descriptors only: use exact procedure_ids recall to read their full instructions before relying on them.","evidence_memories":evidence.iter().map(external_current).collect::<Vec<_>>(),"current_procedures":current,"nearby_procedures":nearby["procedures"],"diagnostics":nearby["diagnostics"]});
            if let Some((status, submission, result)) = previous {
                response["retained_submission"] = decode::<Value>(&submission)?;
                response["previous_execution_status"] = json!(status);
                if let Some(result) = result {
                    response["previous_execution_result"] = decode::<Value>(&result)?;
                }
                response["turn_instruction"]=json!("Inspect the retained submission and current evidence. Revise or resubmit the complete procedure using this fresh authoring_ref; the previous authorization is not reused.");
            }
            return snapshot
                .streaming
                .finish(response, &self.local, &self.request_control);
        }
        let applicability = applies.ok_or_else(|| {
            DomainError::new("invalid_input", "Final create requires applies_to.")
        })?;
        let id = procedure_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let metadata = json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":canonical_scope(&applicability)?,"origin":{"machine_id":self.machine_id,"project_id":context.project_id,"user_id":self.user_id},"recorded_at_ms":now_ms(),"name":p.name,"use_when":p.use_when,"evidence_memory_ids":p.evidence_memory_ids});
        let mutation = Mutation {
            unit: CurrentUnit {
                entity_id: id,
                entity_type: "learned_procedure".into(),
                state: UnitState::Live {
                    heads: vec![Head {
                        metadata,
                        body: p.instructions.clone().unwrap(),
                    }],
                },
            },
            expected_versions: p
                .parent_refs
                .iter()
                .flatten()
                .map(|r| r.observed_version.clone())
                .collect(),
            evidence: p.evidence.clone().ok_or_else(|| {
                DomainError::new("invalid_input", "Procedure evidence is required.")
            })?,
        };
        // Consume context and retain the exact authorized execution atomically.
        // A crash before canonical decision can resume this operation; it cannot
        // erase the final submitted instructions merely by consuming the token.
        let operation = uuid::Uuid::new_v4().to_string();
        let tx = sql(self.control.unchecked_transaction())?;
        let n = sql(tx.execute(
            "UPDATE authoring SET used=1 WHERE token=?1 AND binding=?2 AND used=0",
            params![r.authoring_ref, self.binding_id],
        ))?;
        if n != 1 {
            return Err(DomainError::new(
                "stale_authoring_ref",
                "Authoring context was used concurrently; prepare again.",
            ));
        }
        sql(tx.execute("INSERT INTO authoring_executions(token,binding,operation,generation,mutation,submission,status) VALUES(?1,?2,?3,?4,?5,?6,'pending')",params![r.authoring_ref,self.binding_id,operation,snapshot.generation.to_string(),encode(&mutation)?,encode(r)?]))?;
        sql(tx.commit())?;
        self.request_control.phase("publishing");
        let receipt = match self.store.publish_if_generation_control(
            &operation,
            vec![mutation],
            snapshot.generation,
            &self.request_control.cancel,
        ) {
            Ok(receipt) => receipt,
            Err(agentlaw_storage::Error::CancelledBeforeDecision) => {
                sql(self.control.execute(
                    "UPDATE authoring_executions SET status='needs_review' WHERE token=?1",
                    params![r.authoring_ref],
                ))?;
                return Ok(
                    json!({"status":"authoring_required","code":"cancelled","authoring_ref":r.authoring_ref,"retained_submission":r,"turn_instruction":"Publication was cancelled before its durable decision. Repeat the complete retained final submission with its authoring_ref to inspect and revise the retained submission; it will not publish automatically."}),
                );
            }
            Err(agentlaw_storage::Error::Stale(_)) => {
                sql(self.control.execute(
                    "UPDATE authoring_executions SET status='needs_review' WHERE token=?1",
                    params![r.authoring_ref],
                ))?;
                return Ok(
                    json!({"status":"authoring_required","code":"source_changed","authoring_ref":r.authoring_ref,"retained_submission":r,"turn_instruction":"Source evidence changed before publication. Repeat the complete retained final submission with this authoring_ref to inspect the retained submission and fresh evidence."}),
                );
            }
            Err(e) => return source(Err(e)),
        };
        self.request_control.phase("published");
        let reference = receipt.references.first().ok_or_else(|| {
            DomainError::new(
                "publication_result_invalid",
                "Procedure reference is absent.",
            )
        })?;
        let result = json!({"status":"remembered","procedure_ref":{"procedure_id":reference.memory_id,"observed_version":reference.observed_version}});
        sql(self.control.execute(
            "UPDATE authoring_executions SET status='published',result=?1 WHERE token=?2",
            params![encode(&result)?, r.authoring_ref],
        ))?;
        Ok(result)
    }
    fn recover_authoring(&self) -> Result<()> {
        let mut statement=sql(self.control.prepare("SELECT token,operation,generation,mutation FROM authoring_executions WHERE binding=?1 AND status='pending'"))?;
        let rows = sql(statement.query_map([&self.binding_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        }))?;
        let pending = sql(rows.collect::<std::result::Result<Vec<_>, _>>())?;
        drop(statement);
        for (token, operation, generation, mutation) in pending {
            let generation = generation.parse::<u64>().map_err(|_| {
                DomainError::new(
                    "control_corrupt",
                    "Invalid retained authoring source generation.",
                )
            })?;
            let mutation: Mutation = decode(&mutation)?;
            match self
                .store
                .publish_if_generation(&operation, vec![mutation], generation)
            {
                Ok(receipt) => {
                    let r = receipt.references.first().ok_or_else(|| {
                        DomainError::new(
                            "publication_result_invalid",
                            "Recovered procedure has no reference.",
                        )
                    })?;
                    let result = json!({"status":"remembered","procedure_ref":{"procedure_id":r.memory_id,"observed_version":r.observed_version}});
                    sql(self.control.execute("UPDATE authoring_executions SET status='published',result=?1 WHERE token=?2",params![encode(&result)?,token]))?;
                }
                Err(agentlaw_storage::Error::Stale(_)) => {
                    sql(self.control.execute(
                        "UPDATE authoring_executions SET status='needs_review' WHERE token=?1",
                        [token],
                    ))?;
                }
                Err(error) => return source(Err(error)),
            }
        }
        Ok(())
    }
    fn recover_pending_executions(&self) -> Result<()> {
        let mut st = sql(self
            .control
            .prepare("SELECT payload FROM pending WHERE binding=?1 AND status='pending'"))?;
        let rows = sql(st.query_map([&self.binding_id], |r| r.get::<_, String>(0)))?;
        let payloads = sql(rows.collect::<std::result::Result<Vec<_>, _>>())?;
        drop(st);
        for payload in payloads {
            let retained: Retained = decode(&payload)?;
            if retained.operation_id.is_some() {
                self.publish_retained(retained)?;
            }
        }
        Ok(())
    }
}

impl ContextRepository for Runtime {
    fn selected_store_path(&self) -> Result<Option<String>> {
        Ok(Some(self.root.to_string_lossy().into_owned()))
    }
    fn connect_existing_store(&mut self, _: &str) -> Result<()> {
        Err(DomainError::new("store_binding_transition_required","Select the store through installation configuration. The next request uses the selected binding while retained operations keep their original binding."))
    }
    fn exact_association(&self, path: &str) -> Result<Option<ProjectConnection>> {
        let project: Option<String> = sql(self
            .control
            .query_row(
                "SELECT project FROM associations WHERE binding=?1 AND path=?2",
                params![self.binding_id, path],
                |r| r.get(0),
            )
            .optional())?;
        Ok(project.map(|project_id| ProjectConnection {
            project_id,
            project_path: path.into(),
        }))
    }
    fn discover(&self, clues: Option<&ProjectClues>) -> Result<Vec<ProjectCandidate>> {
        let root = self.root.join("catalog/projects");
        if !root.exists() {
            return Ok(vec![]);
        }
        let mut result = Vec::new();
        let dirs = std::fs::read_dir(root).map_err(|_| {
            DomainError::new(
                "catalog_unavailable",
                "Project catalog cannot be enumerated.",
            )
        })?;
        for dir in dirs {
            let dir = dir.map_err(|_| {
                DomainError::new(
                    "catalog_unavailable",
                    "Project catalog entry is unreadable.",
                )
            })?;
            if !dir.path().is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(dir.path()).map_err(|_| {
                DomainError::new(
                    "catalog_unavailable",
                    "Project catalog shard cannot be read.",
                )
            })? {
                let entry = entry.map_err(|_| {
                    DomainError::new(
                        "catalog_unavailable",
                        "Project catalog entry cannot be read.",
                    )
                })?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let id = path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| {
                    DomainError::new("catalog_corrupt", "Invalid project filename.")
                })?;
                validate_id(id)?;
                let p = source(self.store.read_catalog(id))?;
                let name = p["name"]
                    .as_str()
                    .ok_or_else(|| DomainError::new("catalog_corrupt", "Project name is missing."))?
                    .to_owned();
                let description = p["description"].as_str().map(str::to_owned);
                let mut reasons = Vec::new();
                if let Some(c) = clues {
                    if c.repository_url.as_ref().is_some_and(|url| {
                        p["repository_observations"].as_array().is_some_and(|a| {
                            a.iter()
                                .any(|v| v["repository_address"].as_str() == Some(url))
                        })
                    }) {
                        reasons.push("Observed repository URL matches.".into())
                    }
                    if c.name
                        .as_ref()
                        .is_some_and(|n| name.to_lowercase().contains(&n.to_lowercase()))
                    {
                        reasons.push("Observed project name matches.".into())
                    }
                    if c.name.as_ref().is_some_and(|name| {
                        p["aliases"].as_array().is_some_and(|aliases| {
                            aliases.iter().any(|alias| {
                                alias.as_str().is_some_and(|a| {
                                    a.to_lowercase().contains(&name.to_lowercase())
                                })
                            })
                        })
                    }) {
                        reasons.push("Observed name matches a known project alias.".into())
                    }
                    if c.description.as_ref().is_some_and(|d| {
                        description
                            .as_ref()
                            .is_some_and(|v| v.to_lowercase().contains(&d.to_lowercase()))
                    }) {
                        reasons.push("Observed description matches.".into())
                    }
                } else {
                    reasons
                        .push("Available project identity; no identifying clues supplied.".into())
                }
                if !reasons.is_empty() {
                    result.push(ProjectCandidate {
                        project_id: id.into(),
                        name,
                        description,
                        reasons,
                    })
                }
            }
        }
        result.sort_by(|a, b| a.project_id.cmp(&b.project_id));
        Ok(result)
    }
    fn existing_project(&self, id: &str) -> Result<bool> {
        validate_id(id)?;
        match self.store.read_catalog(id) {
            Ok(_) => Ok(true),
            Err(agentlaw_storage::Error::NotFound(_)) => Ok(false),
            Err(e) => source(Err(e)),
        }
    }
    fn bind(&mut self, path: &str, id: &str) -> Result<ProjectConnection> {
        validate_id(id)?;
        absolute_project_path(path)?;
        if !self.existing_project(id)? {
            return Err(DomainError::new(
                "project_not_found",
                "Project identity does not exist.",
            ));
        }
        sql(self.control.execute("INSERT INTO associations(binding,path,project) VALUES(?1,?2,?3) ON CONFLICT(binding,path) DO UPDATE SET project=excluded.project",params![self.binding_id,path,id]))?;
        Ok(ProjectConnection {
            project_id: id.into(),
            project_path: path.into(),
        })
    }
    fn create_and_bind(
        &mut self,
        path: &str,
        name: &str,
        description: Option<&str>,
    ) -> Result<ProjectConnection> {
        let existing:Option<(String,String,String)>=sql(self.control.query_row("SELECT project,operation,payload FROM project_creations WHERE binding=?1 AND path=?2",params![self.binding_id,path],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional())?;
        let (id, operation, payload) = match existing {
            Some(v) => v,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let op = uuid::Uuid::new_v4().to_string();
                let payload = encode(
                    &json!({"project_id":id,"name":name,"description":description,"aliases":[],"repository_observations":[],"catalog_revision":uuid::Uuid::new_v4().to_string()}),
                )?;
                sql(self.control.execute("INSERT INTO project_creations(binding,path,project,operation,payload) VALUES(?1,?2,?3,?4,?5)",params![self.binding_id,path,id,op,payload]))?;
                (id, op, payload)
            }
        };
        // Saga record is durable before catalog publication, so a crash resumes the same ID.
        source(
            self.store
                .publish_catalog(&operation, &id, decode(&payload)?),
        )?;
        let connection = self.bind(path, &id)?;
        sql(self.control.execute(
            "UPDATE project_creations SET done=1 WHERE binding=?1 AND path=?2",
            params![self.binding_id, path],
        ))?;
        Ok(connection)
    }
    fn request_context(&self, project_id: Option<String>) -> Result<RequestContext> {
        Ok(RequestContext {
            store_binding_id: self.binding_id.clone(),
            user_id: self.user_id.clone(),
            machine_id: self.machine_id.clone(),
            project_id,
            connection_version: "1".into(),
        })
    }
    fn prepare_and_restore(
        &mut self,
        context: &RequestContext,
        query: &str,
    ) -> Result<RecallResponse> {
        if !self.worker.as_ref().is_some_and(|w| {
            matches!(
                w.availability(),
                Ok(agentlaw_worker::SemanticAvailability::Ready)
            )
        }) {
            return Err(DomainError::new("restore_preparation_incomplete","Project connection completed, but full restore preparation requires a ready embedding provider. The connection is retained; ordinary recall can report lexical-only results."));
        }
        let value = self.recall_with_context(
            RecallRequest {
                recall_for: Some(query.into()),
                restore_context: Some(true),
                ..Default::default()
            },
            context.clone(),
        )?;
        let response: RecallResponse = serde_json::from_value(value).map_err(|_| {
            DomainError::new(
                "response_contract_failed",
                "Restore response could not be typed.",
            )
        })?;
        if response
            .diagnostics
            .iter()
            .any(|d| d.code.starts_with("semantic_channel_"))
        {
            return Err(DomainError::new("restore_preparation_failed","Project connection completed but semantic preparation failed. Retry preparation; existing source and connection are preserved."));
        }
        Ok(response)
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
fn procedure_output(u: &CurrentUnit, full: bool) -> Result<Value> {
    let mut heads = Vec::new();
    for h in u.heads() {
        let mut value = json!({"procedure_id":u.entity_id,"name":h.metadata["name"],"use_when":h.metadata["use_when"],"applicability":scope_value(&h.metadata["applicability"])?});
        if full {
            value["instructions"] = json!(h.body);
            value["procedure_ref"] = json!({"procedure_id":u.entity_id,"observed_version":source(agentlaw_storage::version(u,h))?});
            value["evidence_memory_ids"] = h.metadata["evidence_memory_ids"].clone();
        }
        heads.push(value)
    }
    if heads.len() == 1 {
        Ok(heads.remove(0))
    } else {
        Ok(
            json!({"procedure_id":u.entity_id,"current_heads":heads,"head_reconciliation_required":true,"head_reconciliation_instruction":"Review all competing procedure heads; do not merge their instructions implicitly."}),
        )
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    #[test]
    fn canonical_scope_string_roundtrip_and_invalid_combinations() {
        let a = scope_value(&json!({"scope":"project_machine","project_id":ID,"machine_id":ID}))
            .unwrap();
        assert_eq!(a.scope, vec![ScopeKind::Project, ScopeKind::Machine]);
        assert_eq!(canonical_scope(&a).unwrap()["scope"], "project_machine");
        assert!(scope_value(&json!({"scope":"user","project_id":ID})).is_err());
        assert!(scope_value(&json!({"scope":"project"})).is_err());
        assert!(scope_value(&json!({"scope":["user"]})).is_err());
    }
    #[test]
    fn missing_metadata_is_not_false_or_empty() {
        let mut h = Head {
            metadata: json!({"change_id":ID,"applicability":{"scope":"user"},"origin":{"machine_id":ID},"recorded_at_ms":0,"relations":[],"work_targets":[]}),
            body: "body".into(),
        };
        let unit = CurrentUnit {
            entity_id: ID.into(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![h.clone()],
            },
        };
        assert!(memory_from(&unit, &h).is_err());
        h.metadata["is_rule"] = json!(false);
        assert!(!memory_from(&unit, &h).unwrap().is_rule);
        h.metadata.as_object_mut().unwrap().remove("relations");
        assert!(memory_from(&unit, &h).is_err());
    }
}
