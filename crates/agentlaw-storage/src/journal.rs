//! Local ledger and connection policy. File publication remains separate.
use crate::{Error, Result, Store};
#[cfg(test)]
mod tests;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

struct WriterState {
    connection: Option<rusqlite::Connection>,
    paused: usize,
    required: bool,
    failure: Option<String>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}
type WriterSlot = Mutex<WriterState>;
static WRITERS: OnceLock<Mutex<HashMap<PathBuf, Weak<WriterSlot>>>> = OnceLock::new();

#[cfg(test)]
type RepairHook = Arc<dyn Fn() + Send + Sync>;
#[cfg(test)]
static REPAIR_HOOK: OnceLock<Mutex<Option<(PathBuf, RepairHook)>>> = OnceLock::new();
#[cfg(test)]
fn repair_hook() -> &'static Mutex<Option<(PathBuf, RepairHook)>> {
    REPAIR_HOOK.get_or_init(|| Mutex::new(None))
}
#[cfg(test)]
pub(super) fn after_repair_pause(local: &Path) {
    let hook = repair_hook()
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(path, _)| path == local)
        .map(|(_, hook)| hook.clone());
    if let Some(hook) = hook {
        hook();
    }
}

/// Keeps the active binding's journal writer alive while its owner is open.
pub struct JournalWriter {
    slot: Arc<WriterSlot>,
    path: PathBuf,
}
impl JournalWriter {
    /// Register a nonblocking notification for the owning lane.
    pub fn set_waker(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        let mut state = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        state.wake = Some(wake.clone());
        let failed = state.failure.is_some();
        drop(state);
        if failed {
            wake();
        }
    }
    pub fn needs_reopen(&self) -> bool {
        self.slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failure
            .is_some()
    }
    pub(super) fn retry(&self, local: &Path) -> Result<()> {
        if self.path != key(&local.join("journal.sqlite")) {
            return Err(Error::RecoveryRequired(
                "Journal writer belongs to another binding.".into(),
            ));
        }
        let state = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if state.paused != 0 {
            return Err(Error::RecoveryRequired(
                "Journal writer is paused for repair.".into(),
            ));
        }
        if state.failure.is_none() {
            return Ok(());
        }
        let required = state.required;
        drop(state);
        let result = open_replacement(&self.path, required);
        let mut state = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(connection) => {
                state.required |= connection.is_some();
                state.connection = connection;
                state.failure = None;
                Ok(())
            }
            Err(error) => {
                state.failure = Some(error.to_string());
                Err(error)
            }
        }
    }
}

fn slots() -> &'static Mutex<HashMap<PathBuf, Weak<WriterSlot>>> {
    WRITERS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn key(path: &Path) -> PathBuf {
    path.parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map(|parent| parent.join("journal.sqlite"))
        .unwrap_or_else(|| path.to_path_buf())
}
fn find(path: &Path) -> Option<Arc<WriterSlot>> {
    let mut slots = slots().lock().unwrap_or_else(|e| e.into_inner());
    slots.retain(|_, slot| slot.strong_count() > 0);
    slots.get(&key(path)).and_then(Weak::upgrade)
}

pub(super) fn hold(local: &Path) -> Result<JournalWriter> {
    let path = key(&local.join("journal.sqlite"));
    let mut entries = slots().lock().unwrap_or_else(|e| e.into_inner());
    let slot = if let Some(existing) = entries.get(&path).and_then(Weak::upgrade) {
        existing
    } else {
        let connection = if path.try_exists()? {
            Some(open_existing(&path)?)
        } else {
            None
        };
        let slot = Arc::new(Mutex::new(WriterState {
            required: connection.is_some(),
            connection,
            paused: 0,
            failure: None,
            wake: None,
        }));
        entries.insert(path.clone(), Arc::downgrade(&slot));
        slot
    };
    Ok(JournalWriter { slot, path })
}

pub(super) struct PausedWriter {
    path: PathBuf,
    slot: Option<Arc<WriterSlot>>,
}
pub(super) fn pause(local: &Path) -> PausedWriter {
    let path = key(&local.join("journal.sqlite"));
    let slot = find(&path);
    if let Some(slot) = &slot {
        let mut state = slot.lock().unwrap_or_else(|e| e.into_inner());
        state.paused += 1;
        state.connection.take();
    }
    PausedWriter { path, slot }
}
impl PausedWriter {
    pub(super) fn resume(mut self) -> Result<()> {
        if let Some(slot) = self.slot.take() {
            return finish_pause(&self.path, &slot);
        }
        Ok(())
    }
}
impl Drop for PausedWriter {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            let _ = finish_pause(&self.path, &slot);
        }
    }
}

fn finish_pause(path: &Path, slot: &WriterSlot) -> Result<()> {
    let mut state = slot.lock().unwrap_or_else(|e| e.into_inner());
    if state.paused > 1 {
        state.paused -= 1;
        return Ok(());
    }
    let required = state.required;
    drop(state);
    let result = open_replacement(path, required);
    let mut state = slot.lock().unwrap_or_else(|e| e.into_inner());
    state.paused -= 1;
    match result {
        Ok(connection) => {
            state.required |= connection.is_some();
            state.connection = connection;
            state.failure = None;
            Ok(())
        }
        Err(error) => {
            state.failure = Some(error.to_string());
            let wake = state.wake.clone();
            drop(state);
            if let Some(wake) = wake {
                wake();
            }
            Err(error)
        }
    }
}
pub(super) fn open(path: &Path) -> Result<rusqlite::Connection> {
    let conn = open_raw(path)?;
    if let Some(slot) = find(path) {
        let mut state = slot.lock().unwrap_or_else(|e| e.into_inner());
        if state.paused == 0 && state.connection.is_none() {
            state.connection = Some(open_existing(path)?);
            state.required = true;
            state.failure = None;
        }
    }
    Ok(conn)
}

fn open_replacement(path: &Path, required: bool) -> Result<Option<rusqlite::Connection>> {
    if !required && !path.try_exists()? {
        return Ok(None);
    }
    if !path.try_exists()? {
        return Err(Error::RecoveryRequired(
            "The active journal is missing; restore its existing state.".into(),
        ));
    }
    Ok(Some(open_existing(path)?))
}

/// Called while holding a source gate before a reader can open the journal.
pub(super) fn ensure_ready(local: &Path) -> Result<()> {
    let path = key(&local.join("journal.sqlite"));
    if let Some(slot) = find(&path) {
        let state = slot.lock().unwrap_or_else(|e| e.into_inner());
        if state.paused != 0 {
            return Err(Error::RecoveryRequired(
                "Journal writer is paused for repair.".into(),
            ));
        }
        if let Some(reason) = &state.failure {
            return Err(Error::RecoveryRequired(format!(
                "Journal writer is not ready: {reason}"
            )));
        }
    }
    Ok(())
}
fn open_raw(path: &Path) -> Result<rusqlite::Connection> {
    let conn = agentlaw_sqlite::open(path)?;
    configure(conn)
}
fn open_existing(path: &Path) -> Result<rusqlite::Connection> {
    let conn = agentlaw_sqlite::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    configure(conn)
}
fn configure(conn: rusqlite::Connection) -> Result<rusqlite::Connection> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS publications(operation_id TEXT PRIMARY KEY, receipt TEXT NOT NULL, manifest TEXT NOT NULL, sequence BLOB NOT NULL CHECK(length(sequence)=8));")?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS publications_sequence ON publications(sequence);",
    )?;
    let missing_sequence: i64 = conn.query_row(
        "SELECT COUNT(*) FROM publications WHERE sequence IS NULL",
        [],
        |row| row.get(0),
    )?;
    if missing_sequence != 0 {
        return Err(Error::Corrupt(
            "publication ledger has rows without sequence".into(),
        ));
    }
    conn.execute_batch("CREATE TABLE IF NOT EXISTS canonical_change_ids(change_id TEXT PRIMARY KEY); CREATE TABLE IF NOT EXISTS registry_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1), complete INTEGER NOT NULL CHECK(complete IN(0,1)));")?;
    Ok(conn)
}
pub(super) fn reject_existing_changes(
    store: &Store,
    ids: &std::collections::BTreeSet<String>,
) -> Result<()> {
    let mut conn = open(&store.local.join("journal.sqlite"))?;
    let complete = conn
        .query_row(
            "SELECT complete FROM registry_state WHERE singleton=1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 1;
    if !complete {
        let tx = conn.transaction()?;
        store.scan_history(|frame| {
            if frame.kind == "change_descriptor" {
                tx.execute(
                    "INSERT OR IGNORE INTO canonical_change_ids(change_id) VALUES (?1)",
                    [frame.key],
                )?;
            }
            Ok(())
        })?;
        tx.execute(
            "INSERT OR REPLACE INTO registry_state(singleton,complete) VALUES(1,1)",
            [],
        )?;
        tx.commit()?;
    }
    let mut query =
        conn.prepare("SELECT EXISTS(SELECT 1 FROM canonical_change_ids WHERE change_id=?1)")?;
    for id in ids {
        if query.query_row([id], |r| r.get::<_, bool>(0))? {
            return Err(Error::Corrupt(
                "immutable historical change identity reused".into(),
            ));
        }
    }
    Ok(())
}
