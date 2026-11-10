#[cfg(test)]
mod tests;

// Binding-local durable semantic index. One owner holds the writer connection.
use crate::{ann, cosine, keep_best, Error as SearchError, Hit, ScopeFilter, VectorRecord};
use rusqlite::{config::DbConfig, params, Connection, OpenFlags};
use std::path::Path;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VectorFileKind {
    Current,
    History { subject: String },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VectorFileIdentity {
    pub binding: String,
    pub kind: VectorFileKind,
    pub epoch: String,
    pub model_digest: String,
    pub config: String,
    pub generation: String,
    pub dimension: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePosition {
    pub epoch: String,
    pub sequence: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryCursor {
    pub start: SourcePosition,
    pub completed_identity: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VectorFileProgress {
    pub ack: Option<SourcePosition>,
    pub commit_id: i64,
    pub inventory: Option<InventoryCursor>,
    pub complete: bool,
}
#[derive(Clone, Debug)]
pub struct CurrentHeads {
    pub memory_id: String,
    pub change_ids: Vec<String>,
}
#[derive(Default)]
pub struct VectorFileBatch {
    pub heads: Vec<CurrentHeads>,
    pub records: Vec<VectorRecord>,
    pub ack: Option<SourcePosition>,
    pub inventory: Option<InventoryCursor>,
    pub complete: bool,
    pub completed_changes: Vec<(String, String)>,
}
#[derive(Debug, Error)]
pub enum VectorFileError {
    #[error("vector file missing")]
    Missing,
    #[error("vector file identity mismatch or unsupported format")]
    Mismatch,
    #[error("corrupt vector file: {0}")]
    Corrupt(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Search(#[from] SearchError),
}
pub type VectorFileResult<T> = std::result::Result<T, VectorFileError>;
pub struct VectorFileWriter {
    connection: Connection,
    identity: VectorFileIdentity,
}
pub struct VectorFileReadView {
    view: crate::VectorReadView,
    progress: VectorFileProgress,
}
pub struct VectorFileSearch {
    pub hits: Vec<Hit>,
    pub progress: VectorFileProgress,
}
pub struct VectorFileAnnSearch {
    pub hits: Vec<Hit>,
    pub metrics: crate::AnnMetrics,
    pub progress: VectorFileProgress,
}

fn validate_identity(id: &VectorFileIdentity) -> VectorFileResult<()> {
    if id.dimension == 0 || id.dimension > 65536 {
        return Err(VectorFileError::Invalid("dimension out of range".into()));
    }
    Ok(())
}
fn kind(id: &VectorFileIdentity) -> (&str, &str) {
    match &id.kind {
        VectorFileKind::Current => ("current", ""),
        VectorFileKind::History { subject } => ("history", subject),
    }
}
fn stored_value_error(error: rusqlite::Error) -> VectorFileError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _)
            if !matches!(
                code.code,
                rusqlite::ErrorCode::Unknown
                    | rusqlite::ErrorCode::DatabaseCorrupt
                    | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            VectorFileError::Sql(error)
        }
        _ => VectorFileError::Corrupt(error.to_string()),
    }
}
fn check(c: &Connection, id: &VectorFileIdentity) -> VectorFileResult<()> {
    let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='binding_vector_meta')", [], |r| r.get(0)).map_err(stored_value_error)?;
    if !exists {
        return Err(VectorFileError::Mismatch);
    }
    let found: (String,String,String,String,String,String,String,i64) = c.query_row("SELECT binding,kind,subject,epoch,model,config,generation,dimension FROM binding_vector_meta WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).map_err(stored_value_error)?;
    let (k, s) = kind(id);
    if found
        != (
            id.binding.clone(),
            k.into(),
            s.into(),
            id.epoch.clone(),
            id.model_digest.clone(),
            id.config.clone(),
            id.generation.clone(),
            id.dimension as i64,
        )
    {
        return Err(VectorFileError::Mismatch);
    }
    c.prepare("SELECT memory,change_id,section,scope,vector FROM vectors LIMIT 0")
        .map_err(stored_value_error)?;
    c.prepare("SELECT table_id,bucket,scope,memory,change_id,section FROM vector_buckets LIMIT 0")
        .map_err(stored_value_error)?;
    c.prepare("SELECT memory,change_id FROM completed_changes LIMIT 0")
        .map_err(stored_value_error)?;
    Ok(())
}
fn progress(c: &Connection, epoch: &str) -> VectorFileResult<VectorFileProgress> {
    let (ack,commit_id,start,cursor,complete):(Option<i64>,i64,Option<i64>,Option<String>,i64)=c.query_row("SELECT ack,commit_id,inventory_start,inventory_cursor,complete FROM binding_vector_meta WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(stored_value_error)?;
    if ack.is_some_and(|n| n < 0)
        || commit_id < 0
        || start.is_some_and(|n| n < 0)
        || !(0..=1).contains(&complete)
    {
        return Err(VectorFileError::Corrupt("invalid progress metadata".into()));
    }
    let inventory = match (start, cursor) {
        (Some(sequence), Some(completed_identity)) => Some(InventoryCursor {
            start: SourcePosition {
                epoch: epoch.into(),
                sequence,
            },
            completed_identity,
        }),
        (None, None) => None,
        _ => {
            return Err(VectorFileError::Corrupt(
                "incomplete inventory cursor".into(),
            ))
        }
    };
    Ok(VectorFileProgress {
        ack: ack.map(|sequence| SourcePosition {
            epoch: epoch.into(),
            sequence,
        }),
        commit_id,
        inventory,
        complete: complete != 0,
    })
}
impl VectorFileWriter {
    pub fn open(path: impl AsRef<Path>, id: &VectorFileIdentity) -> VectorFileResult<Self> {
        Self::open_with_initialization_observer(path, id, || {})
    }
    #[doc(hidden)]
    pub fn open_with_initialization_observer(
        path: impl AsRef<Path>,
        id: &VectorFileIdentity,
        before_commit: impl FnOnce(),
    ) -> VectorFileResult<Self> {
        validate_identity(id)?;
        let path = path.as_ref();
        let existing = path.exists();
        if existing {
            let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            c.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
            check(&c, id)?;
            progress(&c, &id.epoch)?;
        }
        let c = Connection::open(path)?;
        // Closing a quarantined writer must not checkpoint or delete its WAL.
        c.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY;",
        )?;
        if !existing {
            c.execute_batch("BEGIN IMMEDIATE; CREATE TABLE binding_vector_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),binding TEXT NOT NULL,kind TEXT NOT NULL,subject TEXT NOT NULL,epoch TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,generation TEXT NOT NULL,dimension INTEGER NOT NULL,ack INTEGER,commit_id INTEGER NOT NULL,inventory_start INTEGER,inventory_cursor TEXT,complete INTEGER NOT NULL); CREATE TABLE vectors(memory TEXT NOT NULL,change_id TEXT NOT NULL,section TEXT NOT NULL,scope TEXT NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(memory,change_id,section)) WITHOUT ROWID; CREATE TABLE completed_changes(memory TEXT NOT NULL,change_id TEXT NOT NULL,PRIMARY KEY(memory,change_id)) WITHOUT ROWID;")?;
            ann::initialize(&c)?;
            let (k, s) = kind(id);
            c.execute("INSERT INTO binding_vector_meta VALUES(1,?1,?2,?3,?4,?5,?6,?7,?8,NULL,0,NULL,NULL,0)",params![id.binding,k,s,id.epoch,id.model_digest,id.config,id.generation,id.dimension as i64])?;
            before_commit();
            c.execute_batch("COMMIT")?;
        }
        Ok(Self {
            connection: c,
            identity: id.clone(),
        })
    }
    pub fn progress(&self) -> VectorFileResult<VectorFileProgress> {
        progress(&self.connection, &self.identity.epoch)
    }
    pub fn is_change_completed(&self, memory: &str, change: &str) -> VectorFileResult<bool> {
        change_completed(&self.connection, memory, change)
    }
    pub fn checkpoint(&mut self) -> VectorFileResult<()> {
        let _: (i64, i64, i64) =
            self.connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        Ok(())
    }
    pub fn contains(&self, memory: &str, change: &str, section: &str) -> VectorFileResult<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM vectors WHERE memory=?1 AND change_id=?2 AND section=?3)",
            params![memory, change, section],
            |r| r.get(0),
        )?)
    }
    pub fn commit(&mut self, batch: VectorFileBatch) -> VectorFileResult<()> {
        let id = &self.identity;
        for p in batch
            .ack
            .iter()
            .chain(batch.inventory.iter().map(|i| &i.start))
        {
            if p.epoch != id.epoch || p.sequence < 0 {
                return Err(VectorFileError::Invalid("source position mismatch".into()));
            }
        }
        if !matches!(id.kind, VectorFileKind::History { .. }) && !batch.completed_changes.is_empty()
        {
            return Err(VectorFileError::Invalid(
                "change completion requires history".into(),
            ));
        }
        let tx = self.connection.transaction()?;
        let old = progress(&tx, &id.epoch)?;
        if let (Some(a), Some(b)) = (&batch.ack, &old.ack) {
            if a.sequence < b.sequence {
                return Err(VectorFileError::Invalid("ack regression".into()));
            }
        }
        if matches!(id.kind, VectorFileKind::Current) {
            for h in &batch.heads {
                let mut st = tx
                    .prepare("SELECT DISTINCT change_id FROM vectors WHERE memory=?1")
                    .map_err(stored_value_error)?;
                let changes = st
                    .query_map([&h.memory_id], |r| r.get::<_, String>(0))
                    .map_err(stored_value_error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(stored_value_error)?;
                drop(st);
                for change in changes {
                    if !h.change_ids.contains(&change) {
                        tx.execute(
                            "DELETE FROM vector_buckets WHERE memory=?1 AND change_id=?2",
                            params![h.memory_id, change],
                        )?;
                        tx.execute(
                            "DELETE FROM vectors WHERE memory=?1 AND change_id=?2",
                            params![h.memory_id, change],
                        )?;
                    }
                }
            }
        }
        for r in &batch.records {
            if r.vector.len() != id.dimension {
                return Err(VectorFileError::Invalid("dimension mismatch".into()));
            }
            cosine(&r.vector, &r.vector)
                .map_err(|error| VectorFileError::Invalid(error.to_string()))?;
            let bytes: Vec<u8> = r.vector.iter().flat_map(|v| v.to_le_bytes()).collect();
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO vectors VALUES(?1,?2,?3,?4,?5)",
                params![r.memory_id, r.change_id, r.section, r.scope, bytes],
            )?;
            if inserted == 1 {
                ann::insert(&tx, r)?;
            }
        }
        for (m, c) in &batch.completed_changes {
            tx.execute(
                "INSERT OR IGNORE INTO completed_changes VALUES(?1,?2)",
                params![m, c],
            )?;
        }
        let ack = batch.ack.map(|p| p.sequence);
        let (start, cursor) = match batch.inventory {
            Some(i) => (Some(i.start.sequence), Some(i.completed_identity)),
            None => (None, None),
        };
        if tx.execute("UPDATE binding_vector_meta SET ack=COALESCE(?1,ack),inventory_start=COALESCE(?2,inventory_start),inventory_cursor=COALESCE(?3,inventory_cursor),complete=MAX(complete,?4),commit_id=commit_id+1 WHERE singleton=1 AND commit_id<9223372036854775807",params![ack,start,cursor,batch.complete as i64])? != 1 { return Err(VectorFileError::Invalid("commit counter overflow".into())); }
        tx.commit()?;
        Ok(())
    }
}
impl VectorFileReadView {
    pub fn open(path: impl AsRef<Path>, id: &VectorFileIdentity) -> VectorFileResult<Self> {
        validate_identity(id)?;
        let path = path.as_ref();
        if !path.exists() {
            return Err(VectorFileError::Missing);
        }
        let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        c.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA temp_store=MEMORY; BEGIN DEFERRED;")?;
        check(&c, id)?;
        let p = progress(&c, &id.epoch)?;
        Ok(Self {
            view: crate::VectorReadView {
                connection: c,
                dimension: id.dimension,
                stamp: crate::ViewStamp {
                    generation: 0,
                    commit: p.commit_id,
                    ack: p.ack.as_ref().map_or(0, |a| a.sequence),
                },
                model_digest: id.model_digest.clone(),
                config_digest: id.config.clone(),
            },
            progress: p,
        })
    }
    pub fn progress(&self) -> &VectorFileProgress {
        &self.progress
    }
    pub fn is_change_completed(&self, memory: &str, change: &str) -> VectorFileResult<bool> {
        change_completed(&self.view.connection, memory, change)
    }
    /// Uses the existing ANN scanner against this view's pinned read transaction.
    pub fn search_ann(
        &self,
        query: &[f32],
        scope: &ScopeFilter,
        limit: usize,
    ) -> VectorFileResult<VectorFileAnnSearch> {
        validate_query(query, self.view.dimension, limit)?;
        let (hits, metrics) =
            self.view
                .search_ann(query, scope, limit)
                .map_err(|error| match error {
                    SearchError::Invalid(reason) => VectorFileError::Corrupt(reason),
                    SearchError::CoverageUnknown => {
                        VectorFileError::Corrupt("unsupported ann profile".into())
                    }
                    SearchError::Sql(error) => stored_value_error(error),
                })?;
        Ok(VectorFileAnnSearch {
            hits,
            metrics,
            progress: self.progress.clone(),
        })
    }
    pub fn search(
        &self,
        query: &[f32],
        scope: &ScopeFilter,
        limit: usize,
    ) -> VectorFileResult<VectorFileSearch> {
        validate_query(query, self.view.dimension, limit)?;
        let mut st = self
            .view
            .connection
            .prepare(
                "SELECT memory,change_id,scope,vector FROM vectors ORDER BY memory,change_id,section",
            )
            .map_err(stored_value_error)?;
        let mut rows = st.query([]).map_err(stored_value_error)?;
        let mut hits = Vec::new();
        while let Some(r) = rows.next().map_err(stored_value_error)? {
            let s: String = r.get(2).map_err(stored_value_error)?;
            if !scope.allowed_scopes.contains(&s) {
                continue;
            }
            let bytes: Vec<u8> = r.get(3).map_err(stored_value_error)?;
            if bytes.len() != self.view.dimension * 4 {
                return Err(VectorFileError::Corrupt("vector length".into()));
            }
            let v: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            keep_best(
                &mut hits,
                Hit {
                    memory_id: r.get(0).map_err(stored_value_error)?,
                    change_id: r.get(1).map_err(stored_value_error)?,
                    score: cosine(query, &v)
                        .map_err(|error| VectorFileError::Corrupt(error.to_string()))?,
                },
                limit,
            );
        }
        Ok(VectorFileSearch {
            hits,
            progress: self.progress.clone(),
        })
    }
}

fn validate_query(query: &[f32], dimension: usize, limit: usize) -> VectorFileResult<()> {
    if query.len() != dimension || limit > 10000 {
        return Err(VectorFileError::Invalid("dimension/limit mismatch".into()));
    }
    cosine(query, query).map_err(|error| VectorFileError::Invalid(error.to_string()))?;
    Ok(())
}

fn change_completed(c: &Connection, memory: &str, change: &str) -> VectorFileResult<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM completed_changes WHERE memory=?1 AND change_id=?2)",
        params![memory, change],
        |row| row.get(0),
    )
    .map_err(stored_value_error)
}
