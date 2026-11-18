//! Opt-in SQLite transaction and external database write audit. No callbacks are compiled into default builds.
use rusqlite::{ffi, Connection};
use serde_json::json;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Default, Debug)]
pub struct Counts {
    pub writes: u64,
    pub commits: u64,
    pub rollbacks: u64,
    commit_attempts: u64,
    external_write_attempts: u64,
}
static COUNTS: OnceLock<Mutex<BTreeMap<String, Counts>>> = OnceLock::new();
static OUTPUT: Mutex<()> = Mutex::new(());
static STATUS: Mutex<BTreeMap<std::path::PathBuf, u64>> = Mutex::new(BTreeMap::new());
static FAILURES: AtomicU64 = AtomicU64::new(0);
static STARTED: OnceLock<String> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);
struct State {
    path: String,
    handle: usize,
    id: u64,
    active: AtomicBool,
    committing: AtomicBool,
    statements: AtomicU64,
}

pub fn snapshot(path: &str) -> Counts {
    COUNTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .copied()
        .unwrap_or_default()
}
fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string()
}
fn status(directory: &std::path::Path) -> std::io::Result<()> {
    let failures = FAILURES.load(Ordering::Relaxed);
    let mut reported = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    if reported.get(directory) == Some(&failures) {
        return Ok(());
    }
    let value = json!({"pid":std::process::id(), "started_ns":STARTED.get_or_init(now), "audit_failures":failures, "measurement_valid":failures == 0});
    let mut file = std::fs::File::create(directory.join(format!("{}.json", std::process::id())))?;
    file.write_all(&serde_json::to_vec(&value)?)?;
    file.sync_all()?;
    reported.insert(directory.to_owned(), failures);
    Ok(())
}
fn failed() {
    FAILURES.fetch_add(1, Ordering::Relaxed);
    if let Some(directory) = std::env::var_os("AGENTLAW_WRITE_AUDIT_STATUS") {
        if status(std::path::Path::new(&directory)).is_err() {
            FAILURES.fetch_add(1, Ordering::Relaxed);
        }
    }
    let _ = writeln!(
        std::io::stderr(),
        "AGENTLAW_WRITE_AUDIT_INVALID {{\"pid\":{},\"audit_failures\":{},\"time_ns\":\"{}\"}}",
        std::process::id(),
        FAILURES.load(Ordering::Relaxed),
        now()
    );
}
fn write_record(writer: &mut impl Write, value: &serde_json::Value) -> std::io::Result<()> {
    let mut value = value.clone();
    let failures = FAILURES.load(Ordering::Relaxed);
    value["audit_failures"] = json!(failures);
    value["measurement_valid"] = json!(failures == 0);
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)
}
fn record_output_result(result: std::io::Result<()>) {
    if result.is_err() {
        failed();
    }
}
fn output(
    value: &serde_json::Value,
    path: Option<&std::path::Path>,
    directory: Option<&std::path::Path>,
) {
    let _guard = OUTPUT.lock().unwrap_or_else(|e| e.into_inner());
    // A separate per-process status survives loss of the event stream. A missing
    // status is also invalid to a consumer that requested audit output.
    if let Some(directory) = directory {
        if status(directory).is_err() {
            failed();
        }
    }
    if let Some(path) = path {
        let written = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            write_record(&mut file, value)
        })();
        record_output_result(written);
    }
    if FAILURES.load(Ordering::Relaxed) != 0 {
        if let Some(directory) = directory {
            if status(directory).is_err() {
                failed();
            }
        }
    }
}
fn event(state: &State, kind: &str) {
    {
        let mut all = COUNTS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let count = all.entry(state.path.clone()).or_default();
        match kind {
            "write_transaction" => count.writes += 1,
            "commit_attempt" => count.commit_attempts += 1,
            "commit" => count.commits += 1,
            "rollback" => count.rollbacks += 1,
            "external_write_attempt" => count.external_write_attempts += 1,
            _ => {}
        }
    }
    let value = json!({"time_ns":now(), "pid":std::process::id(),"connection":state.id,"db":state.path,"event":kind,"statements":state.statements.load(Ordering::Relaxed)});
    output(
        &value,
        std::env::var_os("AGENTLAW_WRITE_AUDIT")
            .as_deref()
            .map(std::path::Path::new),
        std::env::var_os("AGENTLAW_WRITE_AUDIT_STATUS")
            .as_deref()
            .map(std::path::Path::new),
    );
}
fn confirm_commit(state: &State) {
    // The commit hook runs before commit I/O. PROFILE runs after VDBE halt,
    // including its error rollback; a still-open transaction is not committed.
    if state.committing.load(Ordering::Relaxed)
        && unsafe { ffi::sqlite3_get_autocommit(state.handle as *mut ffi::sqlite3) } != 0
        && !main_write(state)
    {
        state.committing.store(false, Ordering::Relaxed);
        state.active.store(false, Ordering::Relaxed);
        event(state, "commit");
    }
}
fn main_write(state: &State) -> bool {
    // Hook callbacks execute while this connection is valid and on its SQLite thread.
    unsafe {
        ffi::sqlite3_txn_state(state.handle as *mut ffi::sqlite3, b"main\0".as_ptr().cast())
            == ffi::SQLITE_TXN_WRITE
    }
}
fn mark_write(state: &State) {
    if !state.active.swap(true, Ordering::Relaxed) {
        event(state, "write_transaction");
    }
}
unsafe extern "C" fn trace(
    mask: u32,
    context: *mut c_void,
    _statement: *mut c_void,
    _detail: *mut c_void,
) -> i32 {
    let result = std::panic::catch_unwind(|| {
        let state = unsafe { &*(context as *const State) };
        if mask == ffi::SQLITE_TRACE_STMT as u32 {
            state.statements.fetch_add(1, Ordering::Relaxed);
        }
        if mask == ffi::SQLITE_TRACE_PROFILE as u32 {
            if main_write(state) {
                mark_write(state);
            }
            confirm_commit(state);
        }
        if mask == ffi::SQLITE_TRACE_CLOSE as u32 {
            event(state, "close");
            // One Arc was transferred to this trace registration; CLOSE consumes it.
            unsafe {
                drop(Arc::from_raw(context as *const State));
            }
        }
    });
    if result.is_err() {
        failed();
    }
    0
}

pub(crate) fn install(connection: &Connection) {
    let path = connection
        .path()
        .filter(|p| !p.is_empty())
        .unwrap_or(":memory:")
        .to_owned();
    let state = Arc::new(State {
        path,
        handle: unsafe { connection.handle() } as usize,
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        active: AtomicBool::new(false),
        committing: AtomicBool::new(false),
        statements: AtomicU64::new(0),
    });
    let committed = state.clone();
    connection.commit_hook(Some(move || {
        if main_write(&committed) || committed.active.load(Ordering::Relaxed) {
            mark_write(&committed);
            event(&committed, "commit_attempt");
            committed.committing.store(true, Ordering::Relaxed);
        }
        false
    }));
    let rolled_back = state.clone();
    connection.rollback_hook(Some(move || {
        rolled_back.committing.store(false, Ordering::Relaxed);
        if main_write(&rolled_back) {
            mark_write(&rolled_back);
        }
        if rolled_back.active.swap(false, Ordering::Relaxed) {
            event(&rolled_back, "rollback");
        }
    }));
    let context = Arc::into_raw(state.clone()) as *mut c_void;
    let result = unsafe {
        ffi::sqlite3_trace_v2(
            state.handle as *mut ffi::sqlite3,
            (ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_CLOSE) as u32,
            Some(trace),
            context,
        )
    };
    if result != ffi::SQLITE_OK {
        unsafe {
            drop(Arc::from_raw(context as *const State));
        }
        failed();
        event(&state, "install_failed");
    }
    event(&state, "open");
}

/// Audit the separate database written internally by SQLite, outside main's hooks.
pub fn vacuum_into(connection: &Connection, path: &std::path::Path) -> rusqlite::Result<usize> {
    let state = State {
        path: path.to_string_lossy().into_owned(),
        handle: 0,
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        active: AtomicBool::new(false),
        committing: AtomicBool::new(false),
        statements: AtomicU64::new(1),
    };
    event(&state, "external_write_attempt");
    let result = connection.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()]);
    match &result {
        Ok(_) => {
            event(&state, "write_transaction");
            event(&state, "commit");
        }
        Err(_) => event(&state, "external_write_failed"),
    }
    result
}

#[cfg(test)]
mod tests;
