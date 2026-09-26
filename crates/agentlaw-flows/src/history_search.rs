//! Immutable-history semantic index, isolated from current recall by context.
//! Caller supplies C6-acquired readers only for previously unseen changes.
use agentlaw_worker::{
    derived::{DerivedContext, DerivedDocument, PublishedBatch, PublishedPage, SourcePosition},
    process::SearchPacket,
    spool::{PublishedSpoolPage, SpoolBodyRef},
    Client,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    io::Read,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub struct HistoricalSearch<'a> {
    client: &'a Client,
    db: Connection,
    context: DerivedContext,
    key: String,
}
impl<'a> HistoricalSearch<'a> {
    pub fn open(
        client: &'a Client,
        catalog: impl AsRef<Path>,
        binding: &str,
        identity: &str,
        epoch: &str,
    ) -> Result<Self> {
        if binding.is_empty() || identity.is_empty() || epoch.is_empty() {
            return Err("historical search requires a bound identity and source epoch".into());
        }
        let model = client.model_digest()?;
        let context = DerivedContext {
            repository_id: format!("{binding}:history:{identity}"),
            model_digest: model,
            config_digest: "immutable-history-v1".into(),
            initial_basis: SourcePosition {
                epoch: epoch.into(),
                sequence: 0,
            },
        };
        let key = serde_json::to_string(&context)?;
        let db = Connection::open(catalog)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA cache_size=-8192;CREATE TABLE IF NOT EXISTS historical_jobs(context TEXT NOT NULL,sequence INTEGER NOT NULL,change_id TEXT NOT NULL,page TEXT NOT NULL,accepted INTEGER NOT NULL,PRIMARY KEY(context,sequence),UNIQUE(context,change_id));")?;
        Ok(Self {
            client,
            db,
            context,
            key,
        })
    }
    /// Pending entries are durable too; `search` resumes them without rereading C6.
    pub fn contains(&self, change_id: &str) -> Result<bool> {
        Ok(self
            .db
            .query_row(
                "SELECT 1 FROM historical_jobs WHERE context=?1 AND change_id=?2",
                params![self.key, change_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }
    pub fn ingest(&mut self, change_id: &str, scope: &str, body: &mut impl Read) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let known: i64 = tx.query_row(
            "SELECT COUNT(*) FROM historical_jobs WHERE context=?1 AND change_id=?2",
            params![self.key, change_id],
            |r| r.get(0),
        )?;
        if known == 0 {
            let previous: i64 = tx.query_row(
                "SELECT COALESCE(MAX(sequence),0) FROM historical_jobs WHERE context=?1",
                [&self.key],
                |r| r.get(0),
            )?;
            let seq = previous
                .checked_add(1)
                .filter(|n| *n > 0)
                .ok_or("historical publication counter overflow")?;
            let spool = self.client.stage_source_body(body)?;
            let position = SourcePosition {
                epoch: self.context.initial_basis.epoch.clone(),
                sequence: seq as u64,
            };
            let page = PublishedSpoolPage {
                page: PublishedPage {
                    basis: SourcePosition {
                        epoch: position.epoch.clone(),
                        sequence: position.sequence - 1,
                    },
                    covered_through: position.clone(),
                    source_position: position,
                    coverage_complete: true,
                    batches: vec![PublishedBatch {
                        sequence: seq as u64,
                        documents: vec![DerivedDocument {
                            memory_id: format!("change:{change_id}"),
                            change_id: change_id.into(),
                            scope: scope.into(),
                            section: "history".into(),
                            body: Some(String::new()),
                        }],
                    }],
                },
                bodies: vec![SpoolBodyRef {
                    batch_index: 0,
                    document_index: 0,
                    body: spool,
                }],
            };
            tx.execute(
                "INSERT INTO historical_jobs VALUES(?1,?2,?3,?4,0)",
                params![self.key, seq, change_id, serde_json::to_string(&page)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn synchronize(&mut self, cancel: &AtomicBool) -> Result<SourcePosition> {
        // Serialize this catalog's logical sequence across processes. A daemon receipt
        // surviving a local crash is recovered from its accepted source cursor.
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut accepted = self.client.derived_position(&self.context)?;
        let maximum: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM historical_jobs WHERE context=?1",
            [&self.key],
            |r| r.get(0),
        )?;
        let accepted_i64 = i64::try_from(accepted.sequence)
            .map_err(|_| "historical accepted counter exceeds SQLite range")?;
        if maximum < 0
            || accepted.epoch != self.context.initial_basis.epoch
            || accepted_i64 > maximum
        {
            return Err("historical catalog coverage lost; explicit rebuild required".into());
        }
        let recorded: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM historical_jobs WHERE context=?1 AND accepted=1",
            [&self.key],
            |r| r.get(0),
        )?;
        if recorded > accepted_i64 {
            return Err("worker historical ledger regressed; accepted input spools may have been reclaimed; explicit source-backed rebuild required".into());
        }
        let mut st=tx.prepare("SELECT sequence,page FROM historical_jobs WHERE context=?1 AND sequence>?2 ORDER BY sequence")?;
        let mut rows = st.query(params![self.key, accepted_i64])?;
        while let Some(row) = rows.next()? {
            if cancel.load(Ordering::Relaxed) {
                return Err(agentlaw_worker::EmbeddingError::Cancelled.into());
            }
            let sequence: i64 = row.get(0)?;
            let next = accepted
                .sequence
                .checked_add(1)
                .ok_or("historical accepted counter overflow")?;
            if sequence < 1 || sequence as u64 != next {
                return Err("historical local sequence gap".into());
            }
            let page: PublishedSpoolPage = serde_json::from_str(&row.get::<_, String>(1)?)?;
            let position = self.client.ingest_spooled(&self.context, &page)?;
            if position != page.page.covered_through {
                return Err("historical ingestion receipt mismatch".into());
            }
            accepted = position;
        }
        drop(rows);
        drop(st);
        let acknowledged = i64::try_from(accepted.sequence)
            .map_err(|_| "historical accepted counter exceeds SQLite range")?;
        tx.execute(
            "UPDATE historical_jobs SET accepted=1 WHERE context=?1 AND sequence<=?2",
            params![self.key, acknowledged],
        )?;
        tx.commit()?;
        Ok(accepted)
    }
    pub fn search(
        &mut self,
        query: &str,
        scopes: &[String],
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<SearchPacket> {
        let required = self.synchronize(cancel)?;
        let vector = self.client.embed_cancellable(query, cancel)?;
        self.client.search_index_cancellable(
            &self.context,
            query,
            scopes,
            limit,
            Some(&vector),
            required,
            cancel,
        )
    }
}
