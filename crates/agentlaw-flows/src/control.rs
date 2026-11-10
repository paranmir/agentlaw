pub use agentlaw_contracts::RequestControl;

use crate::context::*;
use crate::runtime::*;
use crate::*;
use agentlaw_storage::Store;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

/// A lane-owned, writable control connection.
pub struct ControlWriter {
    pub(crate) connection: Connection,
}
impl ControlWriter {
    pub fn open(local: impl AsRef<Path>) -> Result<Self> {
        let local = local.as_ref();
        if local.join("control-initialized.json").exists() && !local.join("control.sqlite").exists()
        {
            return Err(control_backup_required());
        }
        let marker_path = local.join("control-initialized.json");
        if marker_path.exists() {
            let marker: Value = serde_json::from_slice(
                &std::fs::read(marker_path).map_err(|_| control_backup_required())?,
            )
            .map_err(|_| control_backup_required())?;
            let existing = Connection::open_with_flags(
                local.join("control.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|_| control_backup_required())?;
            let binding: String = existing
                .query_row(
                    "SELECT value FROM settings WHERE key='binding_id'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| control_backup_required())?;
            if marker["binding_id"] != binding {
                return Err(control_backup_required());
            }
        }
        let connection = sql(Connection::open(local.join("control.sqlite")))?;
        sql(connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS associations(binding TEXT NOT NULL,path TEXT NOT NULL,project TEXT NOT NULL,PRIMARY KEY(binding,path));
            CREATE TABLE IF NOT EXISTS project_creations(binding TEXT NOT NULL,path TEXT NOT NULL,project TEXT NOT NULL,operation TEXT NOT NULL,payload TEXT NOT NULL,done INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(binding,path));
            CREATE TABLE IF NOT EXISTS pending(id TEXT PRIMARY KEY,version TEXT NOT NULL,binding TEXT NOT NULL,payload TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('pending','published','discarded')),result TEXT,discard_reason TEXT);
            CREATE TABLE IF NOT EXISTS authoring(token TEXT PRIMARY KEY,binding TEXT NOT NULL,basis TEXT NOT NULL,used INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS authoring_executions(token TEXT PRIMARY KEY,binding TEXT NOT NULL,operation TEXT NOT NULL,generation TEXT NOT NULL,mutation TEXT NOT NULL,submission TEXT NOT NULL,status TEXT NOT NULL,result TEXT);
        "))?;
        Ok(Self { connection })
    }

    /// Checks the durable identity before the new owner starts serving requests.
    pub fn bind_identity(&mut self, identity: &StoreIdentity) -> Result<()> {
        let root = std::fs::canonicalize(&identity.root).map_err(|_| {
            DomainError::new("store_unavailable", "Cannot resolve memory store root.")
        })?;
        let tx = sql(self.connection.transaction())?;
        for (key, value) in [
            ("store_root", root.to_string_lossy().into_owned()),
            ("machine_id", identity.machine_id.clone()),
            ("binding_id", identity.binding_id.clone()),
        ] {
            sql(tx.execute(
                "INSERT OR IGNORE INTO settings(key,value) VALUES(?1,?2)",
                params![key, value],
            ))?;
            let actual: String = sql(tx.query_row(
                "SELECT value FROM settings WHERE key=?1",
                [key],
                |row| row.get(0),
            ))?;
            if actual != value {
                return Err(DomainError::new(
                    "store_binding_mismatch",
                    "Local control state belongs to a different binding or machine.",
                ));
            }
        }
        sql(tx.execute_batch("CREATE TABLE IF NOT EXISTS sync_operations(operation_id TEXT PRIMARY KEY,source TEXT NOT NULL,state TEXT NOT NULL,active INTEGER NOT NULL); CREATE UNIQUE INDEX IF NOT EXISTS sync_source_slot ON sync_operations(source) WHERE active=1; CREATE TABLE IF NOT EXISTS sync_requests(request_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL,digest TEXT NOT NULL,response TEXT);"))?;
        sql(tx.commit())?;
        Ok(())
    }

    /// Record completion only after canonical source initialization succeeds.
    pub fn finish_binding_initialization(&mut self, identity: &StoreIdentity) -> Result<()> {
        let marker = identity.local.join("control-initialized.json");
        if !marker.exists() {
            use std::io::Write;
            let mut temp = tempfile::NamedTempFile::new_in(&identity.local)
                .map_err(|_| control_backup_required())?;
            temp.write_all(
                encode(&json!({"binding_id":identity.binding_id,"format":1}))?.as_bytes(),
            )
            .map_err(|_| control_backup_required())?;
            temp.as_file()
                .sync_all()
                .map_err(|_| control_backup_required())?;
            match temp.persist_noclobber(&marker) {
                Ok(_) => {}
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(control_backup_required()),
            }
        }
        let value: Value =
            serde_json::from_slice(&std::fs::read(marker).map_err(|_| control_backup_required())?)
                .map_err(|_| control_backup_required())?;
        if value["binding_id"] != identity.binding_id {
            return Err(control_backup_required());
        }
        Ok(())
    }

    pub fn set_exact_generation(&mut self, generation_id: &str, exact_file: &Path) -> Result<()> {
        let name = exact_file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                DomainError::new("exact_index_corrupt", "Invalid exact projection path.")
            })?;
        if name != format!("exact-{generation_id}.sqlite")
            && name != format!("exact-repair-{generation_id}.sqlite")
        {
            return Err(DomainError::new(
                "exact_index_corrupt",
                "Projection identity does not match its file.",
            ));
        }
        drop(crate::exact::ExactIndexWriter::open(exact_file)?);
        let index = sql(Connection::open_with_flags(
            exact_file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ))?;
        let epoch: String = sql(index.query_row(
            "SELECT epoch FROM exact_position WHERE singleton=1",
            [],
            |row| row.get(0),
        ))?;
        let lexical: String = sql(index.query_row(
            "SELECT filename FROM exact_lexical WHERE singleton=1",
            [],
            |row| row.get(0),
        ))?;
        if Path::new(&lexical).components().count() != 1
            || Path::new(&lexical).file_name().and_then(|v| v.to_str()) != Some(lexical.as_str())
        {
            return Err(DomainError::new(
                "exact_index_corrupt",
                "Invalid lexical projection path.",
            ));
        }
        let value = encode(
            &json!({"id":generation_id,"exact_file":exact_file,"epoch":epoch,"lexical_file":exact_file.parent().ok_or_else(||DomainError::new("exact_index_corrupt","Invalid exact projection path."))?.join(lexical)}),
        )?;
        sql(self.connection.execute("INSERT INTO settings(key,value) VALUES('active_projection',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[value]))?;
        Ok(())
    }

    pub fn sync_persist(
        &mut self,
        id: &str,
        source: &str,
        state: &str,
        active: bool,
    ) -> Result<()> {
        sql(self.connection.execute(
            "INSERT OR REPLACE INTO sync_operations VALUES(?1,?2,?3,?4)",
            params![id, source, state, i64::from(active)],
        ))?;
        Ok(())
    }

    pub fn sync_reserve(&mut self, id: &str, operation: &str, digest: &str) -> Result<bool> {
        Ok(sql(self.connection.execute("INSERT OR IGNORE INTO sync_requests(request_id,operation_id,digest,response) VALUES(?1,?2,?3,NULL)",params![id,operation,digest]))? == 1)
    }

    pub fn sync_complete(&mut self, id: &str, response: &str) -> Result<()> {
        let n = sql(self.connection.execute(
            "UPDATE sync_requests SET response=?1 WHERE request_id=?2",
            params![response, id],
        ))?;
        if n != 1 {
            return Err(DomainError::new(
                "sync_request_not_found",
                "Sync request was not reserved.",
            ));
        }
        Ok(())
    }
}
/// A request-scoped read-only control snapshot.
/// Keep the writer connection open; otherwise SQLite creates `-wal` and `-shm` files.
pub struct ControlReader {
    connection: Connection,
}
impl ControlReader {
    pub fn open(local: impl AsRef<Path>) -> Result<Self> {
        let connection = sql(Connection::open_with_flags(
            local.as_ref().join("control.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ))?;
        sql(connection.execute_batch("BEGIN"))?;
        let tables: i64 = sql(connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('settings','associations','project_creations','pending','authoring','authoring_executions')", [], |r| r.get(0)))?;
        if tables != 6 {
            return Err(control_backup_required());
        }
        Ok(Self { connection })
    }
    pub fn exact_association(
        &self,
        binding: &str,
        path: &str,
    ) -> Result<Option<ProjectConnection>> {
        exact_association(&self.connection, binding, path)
    }
    pub fn active_projection(
        &self,
    ) -> Result<Option<(String, String, std::path::PathBuf, std::path::PathBuf)>> {
        let value: Option<String> = sql(self
            .connection
            .query_row(
                "SELECT value FROM settings WHERE key='active_projection'",
                [],
                |row| row.get(0),
            )
            .optional())?;
        value
            .map(|value| {
                let parsed: Value = decode(&value)?;
                let id = parsed["id"].as_str().ok_or_else(|| {
                    DomainError::new("control_corrupt", "Invalid active projection identity.")
                })?;
                let path = parsed["exact_file"].as_str().ok_or_else(|| {
                    DomainError::new("control_corrupt", "Invalid active projection path.")
                })?;
                let epoch = parsed["epoch"].as_str().ok_or_else(|| {
                    DomainError::new("control_corrupt", "Invalid projection epoch.")
                })?;
                let lexical = parsed["lexical_file"].as_str().ok_or_else(|| {
                    DomainError::new("control_corrupt", "Invalid lexical projection path.")
                })?;
                Ok((
                    id.to_owned(),
                    epoch.to_owned(),
                    std::path::PathBuf::from(path),
                    std::path::PathBuf::from(lexical),
                ))
            })
            .transpose()
    }
    pub fn sync_any_active(&self) -> Result<bool> {
        sql(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_operations WHERE active=1)",
            [],
            |row| row.get(0),
        ))
    }
    pub fn sync_active_for(&self, source: &str) -> Result<Option<String>> {
        sql(self
            .connection
            .query_row(
                "SELECT operation_id FROM sync_operations WHERE source=?1 AND active=1",
                [source],
                |row| row.get(0),
            )
            .optional())
    }
    pub fn sync_request(&self, id: &str) -> Result<Option<(String, String, Option<String>)>> {
        sql(self
            .connection
            .query_row(
                "SELECT operation_id,digest,response FROM sync_requests WHERE request_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional())
    }
    pub fn sync_operation(&self, id: &str) -> Result<Option<String>> {
        sql(self
            .connection
            .query_row(
                "SELECT state FROM sync_operations WHERE operation_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional())
    }
    pub fn pending_inspect(&self, binding: &str, reference: &PendingBatchRef) -> Result<Value> {
        let (retained, status, result) = load_pending(&self.connection, binding, reference)?;
        if status == "published" {
            return decode(result.as_deref().unwrap_or("null"));
        }
        Ok(Runtime::pending_response(&retained, &status))
    }
    pub fn pending_notices(&self, binding: &str, project_id: Option<&str>) -> Result<Vec<Value>> {
        pending_notices(&self.connection, binding, project_id)
    }
    pub fn authoring_result(
        &self,
        binding: &str,
        token: &str,
    ) -> Result<Option<(String, String, Option<String>)>> {
        authoring_result(&self.connection, binding, token)
    }
}
pub(crate) fn pending_notices(
    connection: &Connection,
    binding: &str,
    project_id: Option<&str>,
) -> Result<Vec<Value>> {
    let mut statement = sql(connection.prepare("SELECT id,version FROM pending WHERE binding=?1 AND status='pending' AND (json_extract(payload,'$.context.project_id') IS NULL OR json_extract(payload,'$.context.project_id')=?2) ORDER BY id"))?;
    let notices = sql(statement.query_map(params![binding, project_id], |row| {
        Ok(json!({"pending_batch_ref":{"pending_batch_id":row.get::<_, String>(0)?,"observed_version":row.get::<_, String>(1)?}}))
    }))?
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(|_| DomainError::new("control_storage_failed", "Pending notices could not be read; retained work was not discarded."));
    notices
}
fn exact_association(
    connection: &Connection,
    binding: &str,
    path: &str,
) -> Result<Option<ProjectConnection>> {
    let project: Option<String> = sql(connection
        .query_row(
            "SELECT project FROM associations WHERE binding=?1 AND path=?2",
            params![binding, path],
            |r| r.get(0),
        )
        .optional())?;
    Ok(project.map(|project_id| ProjectConnection {
        project_id,
        project_path: path.into(),
    }))
}
pub(crate) fn load_pending(
    connection: &Connection,
    binding: &str,
    reference: &PendingBatchRef,
) -> Result<(Retained, String, Option<String>)> {
    validate_id(&reference.pending_batch_id)?;
    let row: Option<(String, String, Option<String>)> = sql(connection
        .query_row(
            "SELECT payload,status,result FROM pending WHERE id=?1 AND binding=?2",
            params![reference.pending_batch_id, binding],
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
pub(crate) fn authoring_result(
    connection: &Connection,
    binding: &str,
    token: &str,
) -> Result<Option<(String, String, Option<String>)>> {
    sql(connection.query_row("SELECT status,submission,result FROM authoring_executions WHERE token=?1 AND binding=?2", params![token,binding], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional())
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
        let control = ControlWriter::open(&local)?.connection;
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
    pub(crate) fn connect_request(&mut self, mut request: ConnectRequest) -> Result<Value> {
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
}
impl ContextRepository for Runtime {
    fn selected_store_path(&self) -> Result<Option<String>> {
        Ok(Some(self.root.to_string_lossy().into_owned()))
    }
    fn connect_existing_store(&mut self, _: &str) -> Result<()> {
        Err(DomainError::new("store_binding_transition_required","Select the store through installation configuration. The next request uses the selected binding while retained operations keep their original binding."))
    }
    fn exact_association(&self, path: &str) -> Result<Option<ProjectConnection>> {
        exact_association(&self.control, &self.binding_id, path)
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
        crate::apply::bind_parts(&self.store, &self.control, &self.binding_id, path, id)
    }
    fn create_and_bind(
        &mut self,
        path: &str,
        name: &str,
        description: Option<&str>,
    ) -> Result<ProjectConnection> {
        let id = uuid::Uuid::new_v4().to_string();
        let operation = uuid::Uuid::new_v4().to_string();
        let payload = json!({"project_id":id,"name":name,"description":description,"aliases":[],"repository_observations":[],"catalog_revision":uuid::Uuid::new_v4().to_string()});
        crate::apply::create_and_bind_parts(
            &self.store,
            &self.control,
            &self.binding_id,
            path,
            &id,
            &operation,
            &payload,
        )
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

#[cfg(test)]
mod tests;
