//! Durable derived job coordination. Does not pretend to provide an inference engine.
pub mod derived;
pub mod execution;
pub mod indexing;
#[cfg(test)]
mod indexing_tests;
mod model_child;
pub mod onnx;
pub mod process;
pub mod spool;
mod supervisor;
pub use model_child::run_model_child;
pub use onnx::{ModelAssets, OnnxProvider};
pub use process::{run_daemon, Client, ProcessRuntime, RuntimeConfig};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::Path;
pub use supervisor::inspect_runtime;
use thiserror::Error;
pub const MODEL_ID: &str = "ibm-granite/granite-embedding-311m-multilingual-r2";
pub const DIMENSIONS: usize = 256;
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error("retryable overload: durable admission capacity reached")]
    Overload,
    #[error("late result from an obsolete worker incarnation")]
    ObsoleteIncarnation,
    #[error("invalid lifecycle transition or job state")]
    InvalidState,
    #[error("counter or timestamp overflow")]
    Overflow,
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkerState {
    Stopped,
    Starting,
    Loading,
    Ready,
    Failed,
    Stopping,
}
impl WorkerState {
    fn label(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Stopping => "stopping",
        }
    }
    fn parse(v: &str) -> Result<Self> {
        Ok(match v {
            "stopped" => Self::Stopped,
            "starting" => Self::Starting,
            "loading" => Self::Loading,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            "stopping" => Self::Stopping,
            _ => return Err(Error::InvalidState),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SemanticAvailability {
    Ready,
    Loading,
    Unavailable,
    Failed,
}
#[derive(Clone, Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmbeddingError {
    #[error("embedding model loading")]
    Loading,
    #[error("embedding inference unavailable: {0}")]
    Unavailable(String),
    #[error("embedding inference failed: {0}")]
    Failed(String),
    #[error("retryable embedding inference failure: {0}")]
    Retryable(String),
    #[error("request cancelled")]
    Cancelled,
}
pub trait EmbeddingProvider: Send + Sync {
    fn model_digest(&self) -> &str;
    fn availability(&self) -> SemanticAvailability;
    fn embed(&self, input: &str) -> std::result::Result<Vec<f32>, EmbeddingError>;
}
pub struct UnavailableProvider;
impl EmbeddingProvider for UnavailableProvider {
    fn model_digest(&self) -> &str {
        MODEL_ID
    }
    fn availability(&self) -> SemanticAvailability {
        SemanticAvailability::Unavailable
    }
    fn embed(&self, _: &str) -> std::result::Result<Vec<f32>, EmbeddingError> {
        Err(EmbeddingError::Unavailable(
            "ONNX runtime/model assets are not installed; no synthetic embedding substituted"
                .into(),
        ))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobKey {
    pub model_digest: String,
    pub config_digest: String,
    pub memory_id: String,
    pub change_id: String,
    pub content_digest: String,
    pub section: String,
}
#[derive(Clone, Debug)]
pub struct Job {
    pub id: i64,
    pub payload: String,
    pub attempts: i64,
    pub incarnation: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    Accepted(i64),
    Joined(i64),
}
#[derive(Clone, Debug)]
pub struct WorkerHandle {
    pub incarnation: String,
    pub should_start: bool,
}
pub struct Broker {
    connection: Connection,
    capacity: i64,
}
impl Broker {
    pub fn open(path: impl AsRef<Path>, capacity: usize) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA cache_size=-256;PRAGMA temp_store=FILE;
 CREATE TABLE IF NOT EXISTS worker(singleton INTEGER PRIMARY KEY CHECK(singleton=1),incarnation TEXT NOT NULL,state TEXT NOT NULL,model TEXT NOT NULL,heartbeat INTEGER NOT NULL,idle_since INTEGER);
 INSERT OR IGNORE INTO worker VALUES(1,'','stopped','',0,NULL);
 CREATE TABLE IF NOT EXISTS leases(id TEXT PRIMARY KEY,kind TEXT NOT NULL,expires INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS jobs(id INTEGER PRIMARY KEY,model TEXT NOT NULL,config TEXT NOT NULL,memory TEXT NOT NULL,change_id TEXT NOT NULL,digest TEXT NOT NULL,section TEXT NOT NULL,payload TEXT NOT NULL,durable INTEGER NOT NULL,priority INTEGER NOT NULL,state TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0,incarnation TEXT,error TEXT,result BLOB,UNIQUE(model,config,memory,change_id,digest,section));
 CREATE INDEX IF NOT EXISTS runnable ON jobs(state,next_attempt,priority,id);
 CREATE TABLE IF NOT EXISTS waiters(job INTEGER NOT NULL,lease TEXT NOT NULL,PRIMARY KEY(job,lease));
 CREATE TABLE IF NOT EXISTS waiter_failures(job INTEGER NOT NULL,lease TEXT NOT NULL,cause TEXT NOT NULL,PRIMARY KEY(job,lease));
 CREATE TABLE IF NOT EXISTS scheduling(singleton INTEGER PRIMARY KEY CHECK(singleton=1),turn INTEGER NOT NULL);INSERT OR IGNORE INTO scheduling VALUES(1,0);")?;
        Ok(Self {
            connection,
            capacity: i64::try_from(capacity).map_err(|_| Error::Overflow)?,
        })
    }
    /// SQLite's immediate writer transaction serializes start decisions across processes.
    /// This is a coordination reservation, not an OS process launcher or IPC server.
    pub fn reserve_worker_start(&mut self, model: &str, now: i64) -> Result<WorkerHandle> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (inc, state, current): (String, String, String) =
            tx.query_row("SELECT incarnation,state,model FROM worker", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        if state != "stopped" && state != "failed" {
            if current != model {
                return Err(Error::InvalidState);
            }
            return Ok(WorkerHandle {
                incarnation: inc,
                should_start: false,
            });
        }
        let incarnation = uuid::Uuid::new_v4().to_string();
        tx.execute("UPDATE worker SET incarnation=?1,state='starting',model=?2,heartbeat=?3,idle_since=NULL",params![incarnation,model,now])?;
        tx.execute("UPDATE jobs SET state='retry_wait',next_attempt=?1,incarnation=NULL WHERE state='running' AND durable=1 AND incarnation=?2",params![now,inc])?;
        tx.execute("UPDATE jobs SET state='failed',error='previous worker execution ended',payload='' WHERE state='running' AND durable=0 AND incarnation=?1",[&inc])?;
        tx.commit()?;
        Ok(WorkerHandle {
            incarnation,
            should_start: true,
        })
    }
    pub fn transition(&mut self, incarnation: &str, next: WorkerState, now: i64) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (inc, state): (String, String) =
            tx.query_row("SELECT incarnation,state FROM worker", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if inc != incarnation {
            return Err(Error::ObsoleteIncarnation);
        }
        let prev = WorkerState::parse(&state)?;
        let allowed = matches!(
            (&prev, &next),
            (WorkerState::Starting, WorkerState::Loading)
                | (WorkerState::Loading, WorkerState::Ready)
                | (WorkerState::Ready, WorkerState::Stopping)
                | (WorkerState::Stopping, WorkerState::Stopped)
        ) || next == WorkerState::Failed;
        if !allowed {
            return Err(Error::InvalidState);
        }
        tx.execute(
            "UPDATE worker SET state=?1,heartbeat=?2 WHERE singleton=1",
            params![next.label(), now],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn availability(&self) -> Result<SemanticAvailability> {
        let s: String = self
            .connection
            .query_row("SELECT state FROM worker", [], |r| r.get(0))?;
        Ok(match WorkerState::parse(&s)? {
            WorkerState::Ready => SemanticAvailability::Ready,
            WorkerState::Starting | WorkerState::Loading => SemanticAvailability::Loading,
            WorkerState::Failed => SemanticAvailability::Failed,
            _ => SemanticAvailability::Unavailable,
        })
    }
    pub fn renew_lease(&self, id: &str, operation: bool, now: i64, ttl: i64) -> Result<()> {
        let operation = operation || id.starts_with("op:");
        if ttl <= 0 {
            return Err(Error::InvalidState);
        }
        let expiry = now.checked_add(ttl).ok_or(Error::Overflow)?;
        self.connection.execute("INSERT INTO leases VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET expires=excluded.expires",params![id,if operation{"operation"}else{"client"},expiry])?;
        self.connection
            .execute("UPDATE worker SET idle_since=NULL", [])?;
        Ok(())
    }
    pub fn release_lease(&mut self, id: &str) -> Result<()> {
        let tx = self.connection.transaction()?;
        tx.execute("DELETE FROM waiters WHERE lease=?1", [id])?;
        tx.execute("DELETE FROM waiter_failures WHERE lease=?1", [id])?;
        tx.execute("DELETE FROM leases WHERE id=?1", [id])?;
        tx.execute("UPDATE jobs SET state='cancelled',payload='' WHERE durable=0 AND state IN('queued','retry_wait') AND NOT EXISTS(SELECT 1 FROM waiters WHERE job=jobs.id)",[])?;
        tx.commit()?;
        Ok(())
    }
    pub fn enqueue(
        &mut self,
        key: &JobKey,
        payload: &str,
        durable: bool,
        foreground: bool,
        lease: Option<&str>,
    ) -> Result<Admission> {
        if payload.len() > 4 * 1024 * 1024 {
            return Err(Error::Overload);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old:Option<i64>=tx.query_row("SELECT id FROM jobs WHERE model=?1 AND config=?2 AND memory=?3 AND change_id=?4 AND digest=?5 AND section=?6",params![key.model_digest,key.config_digest,key.memory_id,key.change_id,key.content_digest,key.section],|r|r.get(0)).optional()?;
        let result = if let Some(id) = old {
            let state: String =
                tx.query_row("SELECT state FROM jobs WHERE id=?1", [id], |r| r.get(0))?;
            if matches!(state.as_str(), "cancelled" | "failed") {
                let count:i64=tx.query_row("SELECT COUNT(*) FROM jobs WHERE (durable=0 OR ?1=1) AND state IN('queued','running','retry_wait','ready_to_index')",[durable],|r|r.get(0))?;
                if count >= self.capacity {
                    return Err(Error::Overload);
                }
                tx.execute("UPDATE jobs SET state='queued',payload=?1,next_attempt=0,incarnation=NULL WHERE id=?2",params![payload,id])?;
            }
            tx.execute(
                "UPDATE jobs SET durable=MAX(durable,?1),priority=MAX(priority,?2) WHERE id=?3",
                params![durable, foreground, id],
            )?;
            Admission::Joined(id)
        } else {
            let count:i64=tx.query_row("SELECT COUNT(*) FROM jobs WHERE (durable=0 OR ?1=1) AND state IN('queued','running','retry_wait','ready_to_index')",[durable],|r|r.get(0))?;
            if count >= self.capacity {
                return Err(Error::Overload);
            }
            tx.execute("INSERT INTO jobs(model,config,memory,change_id,digest,section,payload,durable,priority,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued')",params![key.model_digest,key.config_digest,key.memory_id,key.change_id,key.content_digest,key.section,payload,durable,foreground])?;
            Admission::Accepted(tx.last_insert_rowid())
        };
        let id = match result {
            Admission::Accepted(id) | Admission::Joined(id) => id,
        };
        if let Some(lease) = lease {
            tx.execute(
                "INSERT OR IGNORE INTO waiters VALUES(?1,?2)",
                params![id, lease],
            )?;
        }
        tx.execute("UPDATE worker SET idle_since=NULL", [])?;
        tx.commit()?;
        Ok(result)
    }
    /// At most one job is RUNNING. Three foreground turns to one background turn.
    pub fn claim(&mut self, incarnation: &str, now: i64) -> Result<Option<Job>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (inc, state, model): (String, String, String) =
            tx.query_row("SELECT incarnation,state,model FROM worker", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        if inc != incarnation {
            return Err(Error::ObsoleteIncarnation);
        }
        if state != "ready" {
            return Err(Error::InvalidState);
        }
        let busy: i64 =
            tx.query_row("SELECT COUNT(*) FROM jobs WHERE state='running'", [], |r| {
                r.get(0)
            })?;
        if busy > 0 {
            return Ok(None);
        }
        let turn: i64 = tx.query_row("SELECT turn FROM scheduling", [], |r| r.get(0))?;
        let priority = if turn % 4 == 3 { 0 } else { 1 };
        let found=tx.query_row("SELECT id,payload,attempts FROM jobs WHERE model=?1 AND (state='queued' OR (state='retry_wait' AND next_attempt<=?2)) AND (durable=1 OR EXISTS(SELECT 1 FROM waiters w JOIN leases l ON w.lease=l.id WHERE w.job=jobs.id AND l.expires>?2)) ORDER BY (priority=?3) DESC,id LIMIT 1",params![model,now,priority],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?))).optional()?;
        if let Some((id, payload, attempts)) = found {
            let attempts = attempts.checked_add(1).ok_or(Error::Overflow)?;
            tx.execute(
                "UPDATE jobs SET state='running',attempts=?1,incarnation=?2 WHERE id=?3",
                params![attempts, incarnation, id],
            )?;
            tx.execute("UPDATE scheduling SET turn=?1", [(turn + 1) % 4])?;
            tx.commit()?;
            Ok(Some(Job {
                id,
                payload,
                attempts,
                incarnation: incarnation.into(),
            }))
        } else {
            Ok(None)
        }
    }
    pub fn complete(&mut self, job: &Job, vector: &[f32]) -> Result<()> {
        if vector.len() != DIMENSIONS
            || vector.iter().any(|v| !v.is_finite())
            || vector.iter().all(|v| *v == 0.0)
        {
            return Err(Error::InvalidState);
        }
        let bytes: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inc: String = tx.query_row("SELECT incarnation FROM worker", [], |r| r.get(0))?;
        if inc != job.incarnation {
            return Err(Error::ObsoleteIncarnation);
        }
        let n=tx.execute("UPDATE jobs SET state='ready_to_index',result=?1,error=NULL WHERE id=?2 AND state='running' AND incarnation=?3 AND attempts=?4",params![bytes,job.id,job.incarnation,job.attempts])?;
        if n != 1 {
            return Err(Error::InvalidState);
        }
        tx.commit()?;
        Ok(())
    }
    pub fn fail(&mut self, job: &Job, cause: &str, retryable: bool, now: i64) -> Result<()> {
        let delay = 1000i64
            .saturating_mul(
                1i64.checked_shl((job.attempts.saturating_sub(1)).clamp(0, 6) as u32)
                    .unwrap_or(64),
            )
            .min(60000);
        let next = now.checked_add(delay).ok_or(Error::Overflow)?;
        let n=self.connection.execute("UPDATE jobs SET state=?1,error=?2,next_attempt=?3 WHERE id=?4 AND state='running' AND incarnation=?5 AND incarnation=(SELECT incarnation FROM worker) AND attempts=?6",params![if retryable{"retry_wait"}else{"failed"},cause,next,job.id,job.incarnation,job.attempts])?;
        if n != 1 {
            return Err(Error::ObsoleteIncarnation);
        }
        Ok(())
    }
    pub fn retry_failed(&self, id: i64, now: i64) -> Result<()> {
        if self.connection.execute(
            "UPDATE jobs SET state='retry_wait',next_attempt=?1 WHERE id=?2 AND state='failed'",
            params![now, id],
        )? != 1
        {
            return Err(Error::InvalidState);
        }
        Ok(())
    }
    /// Only call after the matching derived backend commit/read-view acknowledgment.
    pub fn acknowledge(&self, id: i64) -> Result<()> {
        if self.connection.execute(
            "UPDATE jobs SET state='acknowledged' WHERE id=?1 AND state='ready_to_index'",
            [id],
        )? != 1
        {
            return Err(Error::InvalidState);
        }
        Ok(())
    }
    pub fn job_result(&self, id: i64) -> Result<Option<Vec<f32>>> {
        let bytes: Option<Vec<u8>> =
            self.connection
                .query_row("SELECT result FROM jobs WHERE id=?1", [id], |r| r.get(0))?;
        bytes
            .map(|b| {
                if b.len() != DIMENSIONS * 4 {
                    return Err(Error::InvalidState);
                }
                Ok(b.chunks_exact(4)
                    .map(|v| f32::from_le_bytes([v[0], v[1], v[2], v[3]]))
                    .collect())
            })
            .transpose()
    }
    pub fn idle_shutdown_due(&mut self, now: i64, grace: i64) -> Result<bool> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM leases WHERE expires<=?1", [now])?;
        tx.execute(
            "DELETE FROM waiter_failures WHERE lease NOT IN(SELECT id FROM leases)",
            [],
        )?;
        tx.execute(
            "DELETE FROM waiters WHERE lease NOT IN(SELECT id FROM leases)",
            [],
        )?;
        let leases: i64 = tx.query_row("SELECT COUNT(*) FROM leases", [], |r| r.get(0))?;
        tx.execute("UPDATE jobs SET state='cancelled',payload='' WHERE durable=0 AND state IN('queued','retry_wait') AND NOT EXISTS(SELECT 1 FROM waiters WHERE job=jobs.id)",[])?;
        let runnable:i64=tx.query_row("SELECT COUNT(*) FROM jobs WHERE model=(SELECT model FROM worker) AND (state IN('queued','running') OR (state='retry_wait' AND next_attempt<=?1)) AND (durable=1 OR EXISTS(SELECT 1 FROM waiters w JOIN leases l ON w.lease=l.id WHERE w.job=jobs.id AND l.expires>?1))",[now],|r|r.get(0))?;
        if leases > 0 || runnable > 0 {
            tx.execute("UPDATE worker SET idle_since=NULL", [])?;
            tx.commit()?;
            return Ok(false);
        }
        let since: Option<i64> = tx.query_row("SELECT idle_since FROM worker", [], |r| r.get(0))?;
        if since.is_none() {
            tx.execute("UPDATE worker SET idle_since=?1", [now])?;
        }
        let due = since
            .map(|s| now.saturating_sub(s) >= grace)
            .unwrap_or(false);
        tx.commit()?;
        Ok(due)
    }
    pub fn orphaned(&self, now: i64, grace: i64) -> Result<bool> {
        let heartbeat: i64 =
            self.connection
                .query_row("SELECT heartbeat FROM worker", [], |r| r.get(0))?;
        Ok(now.saturating_sub(heartbeat) > grace)
    }
    pub fn broker_heartbeat(&self, incarnation: &str, now: i64) -> Result<()> {
        if self.connection.execute(
            "UPDATE worker SET heartbeat=?1 WHERE incarnation=?2",
            params![now, incarnation],
        )? != 1
        {
            return Err(Error::ObsoleteIncarnation);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> JobKey {
        JobKey {
            model_digest: MODEL_ID.into(),
            config_digest: "c".into(),
            memory_id: "m".into(),
            change_id: "v".into(),
            content_digest: "d".into(),
            section: "body".into(),
        }
    }
    fn ready(b: &mut Broker) -> String {
        let h = b.reserve_worker_start(MODEL_ID, 0).unwrap();
        b.transition(&h.incarnation, WorkerState::Loading, 0)
            .unwrap();
        b.transition(&h.incarnation, WorkerState::Ready, 0).unwrap();
        h.incarnation
    }
    #[test]
    fn single_flight_queue_and_unavailable_are_distinct() {
        let mut b = Broker::open(":memory:", 1).unwrap();
        let inc = ready(&mut b);
        assert!(!b.reserve_worker_start(MODEL_ID, 1).unwrap().should_start);
        assert_eq!(
            b.enqueue(&key(), "body", true, true, None).unwrap(),
            Admission::Accepted(1)
        );
        assert_eq!(
            b.enqueue(&key(), "body", true, true, None).unwrap(),
            Admission::Joined(1)
        );
        assert_eq!(b.availability().unwrap(), SemanticAvailability::Ready);
        let job = b.claim(&inc, 0).unwrap().unwrap();
        assert!(b.claim(&inc, 0).unwrap().is_none());
        b.fail(&job, "temporary", true, 0).unwrap();
        assert!(b.claim(&inc, 999).unwrap().is_none());
        assert!(b.claim(&inc, 1000).unwrap().is_some());
        assert!(UnavailableProvider.embed("x").is_err());
    }
    #[test]
    fn capacity_failed_retention_and_old_result() {
        let mut b = Broker::open(":memory:", 1).unwrap();
        let inc = ready(&mut b);
        b.enqueue(&key(), "body", true, false, None).unwrap();
        let mut other = key();
        other.memory_id = "other".into();
        assert!(matches!(
            b.enqueue(&other, "body", true, false, None),
            Err(Error::Overload)
        ));
        let job = b.claim(&inc, 0).unwrap().unwrap();
        b.fail(&job, "fatal", false, 0).unwrap();
        assert!(!b.idle_shutdown_due(1, 100).unwrap());
        assert!(b.idle_shutdown_due(101, 100).unwrap());
        b.transition(&inc, WorkerState::Failed, 102).unwrap();
        b.reserve_worker_start(MODEL_ID, 103).unwrap();
        assert!(matches!(
            b.complete(&job, &vec![1.0; DIMENSIONS]),
            Err(Error::ObsoleteIncarnation)
        ));
    }
    #[test]
    fn client_lease_does_not_drop_other_operation() {
        let mut b = Broker::open(":memory:", 4).unwrap();
        ready(&mut b);
        b.renew_lease("client", false, 0, 60).unwrap();
        b.renew_lease("operation", true, 0, 100).unwrap();
        b.release_lease("client").unwrap();
        assert!(!b.idle_shutdown_due(80, 0).unwrap());
        assert!(!b.idle_shutdown_due(101, 0).unwrap());
        assert!(b.idle_shutdown_due(102, 0).unwrap());
    }
    #[test]
    fn durable_reopen_and_cross_connection_start() {
        let p = std::env::temp_dir().join(format!("agentlaw-worker-{}.db", uuid::Uuid::new_v4()));
        let mut first = Broker::open(&p, 4).unwrap();
        let inc = ready(&mut first);
        first
            .enqueue(&key(), "retained payload", true, false, None)
            .unwrap();
        let mut second = Broker::open(&p, 4).unwrap();
        assert!(
            !second
                .reserve_worker_start(MODEL_ID, 1)
                .unwrap()
                .should_start
        );
        drop(first);
        let job = second.claim(&inc, 1).unwrap().unwrap();
        assert_eq!(job.payload, "retained payload");
        second.complete(&job, &vec![0.25; DIMENSIONS]).unwrap();
        assert_eq!(
            second.job_result(job.id).unwrap().unwrap().len(),
            DIMENSIONS
        );
        second.acknowledge(job.id).unwrap();
        drop(second);
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn cancelled_last_waiter_releases_admission_but_not_other_waiters() {
        let mut broker = Broker::open(":memory:", 1).unwrap();
        let incarnation = ready(&mut broker);
        broker.renew_lease("one", true, 0, 100).unwrap();
        broker.renew_lease("two", true, 0, 100).unwrap();
        broker
            .enqueue(&key(), "body", false, true, Some("one"))
            .unwrap();
        broker
            .enqueue(&key(), "body", false, true, Some("two"))
            .unwrap();
        broker.release_lease("one").unwrap();
        let state: String = broker
            .connection
            .query_row("SELECT state FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "queued");
        broker.release_lease("two").unwrap();
        assert!(broker.claim(&incarnation, 0).unwrap().is_none());
        broker.renew_lease("three", true, 0, 100).unwrap();
        assert!(broker
            .enqueue(&key(), "body", false, true, Some("three"))
            .is_ok());
        assert!(broker.claim(&incarnation, 0).unwrap().is_some());
    }
}
