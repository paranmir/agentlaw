pub use agentlaw_contracts::RequestControl;

use crate::store::*;
use crate::*;
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
            let existing = agentlaw_sqlite::open_with_flags(
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
        let connection = sql(agentlaw_sqlite::open(local.join("control.sqlite")))?;
        sql(connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS associations(binding TEXT NOT NULL,path TEXT NOT NULL,project TEXT NOT NULL,PRIMARY KEY(binding,path));
            CREATE TABLE IF NOT EXISTS project_creations(binding TEXT NOT NULL,path TEXT NOT NULL,project TEXT NOT NULL,operation TEXT NOT NULL,payload TEXT NOT NULL,done INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(binding,path));
            CREATE TABLE IF NOT EXISTS pending(id TEXT PRIMARY KEY,version TEXT NOT NULL,binding TEXT NOT NULL,payload TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('pending','published','discarded')),result TEXT,discard_reason TEXT);
            CREATE TABLE IF NOT EXISTS authoring(token TEXT PRIMARY KEY,binding TEXT NOT NULL,basis TEXT NOT NULL,used INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS authoring_executions(token TEXT PRIMARY KEY,binding TEXT NOT NULL,operation TEXT NOT NULL,generation TEXT NOT NULL,mutation TEXT NOT NULL,submission TEXT NOT NULL,status TEXT NOT NULL,result TEXT);
            CREATE TABLE IF NOT EXISTS prefix_executions(binding TEXT NOT NULL,operation TEXT NOT NULL,subject TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(binding,operation),UNIQUE(binding,subject));
            CREATE TABLE IF NOT EXISTS prefix_execution_outcomes(
                completion_seq INTEGER PRIMARY KEY,
                binding TEXT NOT NULL,
                operation TEXT NOT NULL,
                state TEXT NOT NULL,
                rejection_code TEXT,
                pending_batch_id TEXT,
                observed_version TEXT,
                UNIQUE(binding,operation),
                CHECK((state='rejected' AND rejection_code IS NOT NULL AND pending_batch_id IS NULL AND observed_version IS NULL)
                   OR (state='review_required' AND rejection_code IS NULL AND pending_batch_id IS NOT NULL AND observed_version IS NOT NULL))
            );
            CREATE INDEX IF NOT EXISTS prefix_execution_outcomes_retention ON prefix_execution_outcomes(binding,completion_seq);
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
        let index = sql(agentlaw_sqlite::open_with_flags(
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

pub(crate) enum PrefixExecutionOutcome {
    InProgress,
    Rejected(String),
    ReviewRequired(PendingBatchRef),
}

pub(crate) fn record_prefix_outcome(
    tx: &rusqlite::Transaction<'_>,
    binding: &str,
    operation: &str,
    outcome: PrefixTerminalOutcome<'_>,
) -> Result<()> {
    let (state, code, id, version) = match outcome {
        PrefixTerminalOutcome::Rejected(code) => ("rejected", Some(code), None, None),
        PrefixTerminalOutcome::ReviewRequired(reference) => (
            "review_required",
            None,
            Some(reference.pending_batch_id.as_str()),
            Some(reference.observed_version.as_str()),
        ),
    };
    let inserted = sql(tx.execute(
        "INSERT INTO prefix_execution_outcomes(binding,operation,state,rejection_code,pending_batch_id,observed_version)
         VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(binding,operation) DO NOTHING",
        params![binding, operation, state, code, id, version],
    ))?;
    if inserted != 0 {
        const RETAINED_OUTCOMES: usize = 4096;
        sql(tx.execute(
            "DELETE FROM prefix_execution_outcomes WHERE binding=?1 AND completion_seq <= (
                SELECT completion_seq FROM prefix_execution_outcomes WHERE binding=?1
                ORDER BY completion_seq DESC LIMIT 1 OFFSET ?2
             )",
            params![binding, RETAINED_OUTCOMES],
        ))?;
    }
    Ok(())
}

pub(crate) enum PrefixTerminalOutcome<'a> {
    Rejected(&'a str),
    ReviewRequired(&'a PendingBatchRef),
}

impl ControlReader {
    pub fn open(local: impl AsRef<Path>) -> Result<Self> {
        let connection = sql(agentlaw_sqlite::open_with_flags(
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
    pub fn binding_id(&self) -> Result<String> {
        sql(self.connection.query_row(
            "SELECT value FROM settings WHERE key='binding_id'",
            [],
            |row| row.get(0),
        ))
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
    pub(crate) fn prefix_execution_outcome(
        &self,
        binding: &str,
        operation: &str,
    ) -> Result<Option<PrefixExecutionOutcome>> {
        sql(self.connection.query_row(
            "SELECT state,rejection_code,pending_batch_id,observed_version
             FROM prefix_execution_outcomes WHERE binding=?1 AND operation=?2
             UNION ALL
             SELECT 'in_progress',NULL,NULL,NULL FROM prefix_executions WHERE binding=?1 AND operation=?2",
            params![binding, operation],
            |row| match row.get::<_, String>(0)?.as_str() {
                "in_progress" => Ok(PrefixExecutionOutcome::InProgress),
                "rejected" => Ok(PrefixExecutionOutcome::Rejected(row.get(1)?)),
                "review_required" => Ok(PrefixExecutionOutcome::ReviewRequired(PendingBatchRef {
                    pending_batch_id: row.get(2)?,
                    observed_version: row.get(3)?,
                })),
                _ => Err(rusqlite::Error::InvalidQuery),
            },
        ).optional())
    }

    pub fn pending_inspect(&self, binding: &str, reference: &PendingBatchRef) -> Result<Value> {
        let (retained, status, result) = load_pending(&self.connection, binding, reference)?;
        if status == "published" {
            return decode(result.as_deref().unwrap_or("null"));
        }
        Ok(crate::prepare::pending_response(&retained, &status))
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
    pub(crate) fn load_pending(
        &self,
        binding: &str,
        reference: &PendingBatchRef,
    ) -> Result<(Retained, String, Option<String>)> {
        load_pending(&self.connection, binding, reference)
    }
    pub(crate) fn authoring_basis(&self, binding: &str, token: &str) -> Result<Option<String>> {
        sql(self
            .connection
            .query_row(
                "SELECT basis FROM authoring WHERE token=?1 AND binding=?2 AND used=0",
                params![token, binding],
                |row| row.get(0),
            )
            .optional())
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

#[cfg(test)]
mod tests;
