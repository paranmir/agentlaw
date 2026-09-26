//! Atomic derived-generation publication with cross-process reader leases.
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub struct GenerationCatalog {
    root: PathBuf,
    db: Connection,
}
pub struct BuildingGeneration {
    pub id: String,
    pub directory: PathBuf,
    lease: File,
}
pub struct GenerationLease {
    pub id: String,
    pub directory: PathBuf,
    _lease: File,
}
impl GenerationCatalog {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        let root = fs::canonicalize(root)?;
        let db = Connection::open(root.join("catalog.sqlite"))?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS generations(id TEXT PRIMARY KEY,state TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL);CREATE TABLE IF NOT EXISTS active(singleton INTEGER PRIMARY KEY,id TEXT NOT NULL);")?;
        Ok(Self { root, db })
    }
    pub fn begin(&self, model: &str, config: &str) -> Result<BuildingGeneration> {
        let id = uuid::Uuid::new_v4().to_string();
        let directory = self.root.join(&id);
        fs::create_dir(&directory)?;
        let lease = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(directory.join("readers.lock"))?;
        lease.lock_exclusive()?;
        self.db.execute(
            "INSERT INTO generations VALUES(?1,'building',?2,?3)",
            params![id, model, config],
        )?;
        Ok(BuildingGeneration {
            id,
            directory,
            lease,
        })
    }
    /// Caller has finished populating separate backend files. Both stamps must cover the
    /// requested fence before the pointer can become visible; no empty-success rebuild.
    pub fn publish(
        &mut self,
        build: BuildingGeneration,
        required: i64,
        lexical: &crate::ViewStamp,
        vector: Option<&crate::ViewStamp>,
    ) -> Result<()> {
        self.publish_channels(build, required, required, lexical, vector)
    }
    pub fn publish_channels(
        &mut self,
        build: BuildingGeneration,
        lexical_required: i64,
        vector_required: i64,
        lexical: &crate::ViewStamp,
        vector: Option<&crate::ViewStamp>,
    ) -> Result<()> {
        if lexical.ack < lexical_required || vector.is_some_and(|v| v.ack < vector_required) {
            return Err("rebuild does not cover source fence".into());
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (model, config): (String, String) = tx.query_row(
            "SELECT model,config FROM generations WHERE id=?1 AND state='building'",
            [&build.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let actual = crate::SearchIndex::open(build.directory.join("lexical.sqlite"))?
            .read_view()?
            .stamp;
        if actual != *lexical {
            return Err("rebuild lexical immutable stamp mismatch".into());
        }
        if let Some(vector) = vector {
            let actual = crate::ExactVectorIndex::open(
                build.directory.join("vector.sqlite"),
                &model,
                &config,
                256,
            )?
            .read_view()?
            .stamp;
            if actual != *vector {
                return Err("rebuild vector immutable stamp mismatch".into());
            }
        }
        tx.execute(
            "UPDATE generations SET state='retired' WHERE id IN(SELECT id FROM active)",
            [],
        )?;
        tx.execute(
            "UPDATE generations SET state='active' WHERE id=?1",
            [&build.id],
        )?;
        tx.execute(
            "INSERT INTO active VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET id=excluded.id",
            [&build.id],
        )?;
        tx.commit()?;
        FileExt::unlock(&build.lease)?;
        Ok(())
    }
    pub fn acquire(&mut self) -> Result<Option<GenerationLease>> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id: Option<String> = tx
            .query_row("SELECT id FROM active", [], |r| r.get(0))
            .optional()?;
        let Some(id) = id else { return Ok(None) };
        let directory = self.root.join(&id);
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("readers.lock"))?;
        lease.lock_shared()?;
        tx.commit()?;
        Ok(Some(GenerationLease {
            id,
            directory,
            _lease: lease,
        }))
    }
    /// Retired files are reclaimed only after every process releases its shared lease.
    pub fn reclaim(&mut self) -> Result<usize> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let ids = {
            let mut st = tx.prepare("SELECT id FROM generations WHERE state='retired'")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut count = 0;
        for id in ids {
            if uuid::Uuid::parse_str(&id).is_err() {
                return Err("invalid generation directory".into());
            }
            let directory = self.root.join(&id);
            let lease = OpenOptions::new()
                .read(true)
                .write(true)
                .open(directory.join("readers.lock"))?;
            if lease.try_lock_exclusive().is_err() {
                continue;
            }
            let canonical = fs::canonicalize(&directory)?;
            if canonical.parent() != Some(self.root.as_path()) {
                return Err("generation path escaped catalog".into());
            }
            drop(lease);
            fs::remove_dir_all(&canonical)?;
            tx.execute("DELETE FROM generations WHERE id=?1", [id])?;
            count += 1;
        }
        tx.commit()?;
        Ok(count)
    }
}
