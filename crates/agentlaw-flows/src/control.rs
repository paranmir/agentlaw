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
}
/// A request-scoped read-only control snapshot.
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

#[cfg(test)]
mod control_reader_tests {
    use super::*;

    #[test]
    fn reader_is_read_only_and_does_not_create_database() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ControlReader::open(dir.path()).is_err());
        assert!(!dir.path().join("control.sqlite").exists());
        drop(ControlWriter::open(dir.path()).unwrap());
        let path = dir.path().join("control.sqlite");
        let before = std::fs::read(&path).unwrap();
        {
            let reader = ControlReader::open(dir.path()).unwrap();
            assert!(reader
                .connection
                .execute("INSERT INTO settings VALUES('test','value')", [])
                .is_err());
        }
        assert_eq!(before, std::fs::read(path).unwrap());
    }

    #[test]
    fn reader_snapshot_is_stable_while_writer_commits() {
        let dir = tempfile::tempdir().unwrap();
        let writer = ControlWriter::open(dir.path()).unwrap();
        writer
            .connection
            .execute(
                "INSERT INTO associations VALUES('binding','path','old')",
                [],
            )
            .unwrap();
        let reader = ControlReader::open(dir.path()).unwrap();
        assert_eq!(
            reader
                .exact_association("binding", "path")
                .unwrap()
                .unwrap()
                .project_id,
            "old"
        );
        writer
            .connection
            .execute("UPDATE associations SET project='new'", [])
            .unwrap();
        assert_eq!(
            reader
                .exact_association("binding", "path")
                .unwrap()
                .unwrap()
                .project_id,
            "old"
        );
        let fresh = ControlReader::open(dir.path()).unwrap();
        assert_eq!(
            fresh
                .exact_association("binding", "path")
                .unwrap()
                .unwrap()
                .project_id,
            "new"
        );
    }

    #[test]
    fn reader_matches_runtime_association() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        let local = dir.path().join("local");
        std::fs::create_dir_all(&root).unwrap();
        let runtime = Runtime::open(&root, &local, "user").unwrap();
        runtime
            .control
            .execute(
                "INSERT INTO associations VALUES(?1,'path','project')",
                [&runtime.binding_id],
            )
            .unwrap();
        let reader = ControlReader::open(&local).unwrap();
        let expected = ContextRepository::exact_association(&runtime, "path")
            .unwrap()
            .unwrap();
        let actual = reader
            .exact_association(&runtime.binding_id, "path")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(expected).unwrap(),
            serde_json::to_value(actual).unwrap()
        );
    }

    #[test]
    fn reader_matches_runtime_pending_and_authoring_results() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        let local = dir.path().join("local");
        std::fs::create_dir_all(&root).unwrap();
        let mut runtime = Runtime::open(&root, &local, "user").unwrap();
        let context = ContextRepository::request_context(&runtime, None).unwrap();
        let batch = crate::write::PendingBatch::new(&context, vec![]);
        let reference = batch.reference.clone();
        let retained = Retained {
            diagnostics: vec![],
            batch,
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
        };
        runtime.save_pending(&retained, None).unwrap();
        runtime.control.execute("INSERT INTO authoring_executions VALUES(?1,?2,'op','1','{}','submission','published','result')", params!["token", runtime.binding_id]).unwrap();
        let reader = ControlReader::open(&local).unwrap();
        let expected = runtime
            .remember(
                serde_json::from_value(
                    json!({"pending_batch_ref": reference, "pending_action": "inspect"}),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            reader
                .pending_inspect(&runtime.binding_id, &reference)
                .unwrap(),
            expected
        );
        assert_eq!(
            reader
                .authoring_result(&runtime.binding_id, "token")
                .unwrap(),
            authoring_result(&runtime.control, &runtime.binding_id, "token").unwrap()
        );
    }

    #[test]
    fn reader_pending_notices_match_runtime_recall() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        let local = dir.path().join("local");
        std::fs::create_dir_all(&root).unwrap();
        let runtime = Runtime::open(&root, &local, "user").unwrap();
        for (id, binding, project, status) in [
            (
                "b",
                runtime.binding_id.as_str(),
                Some("project-a"),
                "pending",
            ),
            ("a", runtime.binding_id.as_str(), None, "pending"),
            (
                "c",
                runtime.binding_id.as_str(),
                Some("project-b"),
                "pending",
            ),
            ("d", runtime.binding_id.as_str(), None, "discarded"),
            ("e", "other-binding", None, "pending"),
        ] {
            runtime.control.execute(
                "INSERT INTO pending(id,version,binding,payload,status) VALUES(?1,'version',?2,?3,?4)",
                params![id, binding, json!({"context":{"project_id":project}}).to_string(), status],
            ).unwrap();
        }
        let reader = ControlReader::open(&local).unwrap();
        for (project, expected_ids) in [
            (None, vec!["a"]),
            (Some("project-a"), vec!["a", "b"]),
            (Some("project-b"), vec!["a", "c"]),
        ] {
            let context =
                ContextRepository::request_context(&runtime, project.map(str::to_owned)).unwrap();
            let response = runtime
                .recall_with_context(RecallRequest::default(), context)
                .unwrap();
            let actual = reader
                .pending_notices(&runtime.binding_id, project)
                .unwrap();
            assert_eq!(json!(actual), response["pending_batches"]);
            let ids: Vec<_> = actual
                .iter()
                .map(|v| v["pending_batch_ref"]["pending_batch_id"].as_str().unwrap())
                .collect();
            assert_eq!(ids, expected_ids);
        }
    }

    #[test]
    fn writer_is_send_and_not_sync() {
        fn send<T: Send>() {}
        send::<ControlWriter>();
        trait AmbiguousIfSync<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSync<()> for T {}
        impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
        let _ = <ControlWriter as AmbiguousIfSync<_>>::check;
    }
}
