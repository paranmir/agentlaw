use super::*;
use std::ffi::{c_char, c_int};
use std::sync::atomic::AtomicPtr;

#[test]
fn commits_are_confirmed_after_completion_in_delete_and_wal_modes() {
    for mode in ["DELETE", "WAL"] {
        let temp = tempfile::tempdir().unwrap();
        let db = crate::open(temp.path().join("transactions.sqlite")).unwrap();
        db.execute_batch(&format!(
            "PRAGMA journal_mode={mode}; CREATE TABLE data(value);"
        ))
        .unwrap();
        let before = snapshot(db.path().unwrap());
        db.execute_batch("BEGIN IMMEDIATE; INSERT INTO data VALUES(1); COMMIT; INSERT INTO data VALUES(2); BEGIN IMMEDIATE; ROLLBACK;")
            .unwrap();
        let after = snapshot(db.path().unwrap());
        assert_eq!(after.writes - before.writes, 3, "{mode}");
        assert_eq!(after.commit_attempts - before.commit_attempts, 2, "{mode}");
        assert_eq!(after.commits - before.commits, 2, "{mode}");
        assert_eq!(after.rollbacks - before.rollbacks, 1, "{mode}");
        db.execute_batch("BEGIN; SELECT * FROM data; COMMIT;")
            .unwrap();
        assert_eq!(snapshot(db.path().unwrap()).commits, after.commits);
    }
}

static ORIGINAL_VFS: AtomicPtr<ffi::sqlite3_vfs> = AtomicPtr::new(std::ptr::null_mut());
static FAIL_SYNC: AtomicBool = AtomicBool::new(false);
static METHODS: Mutex<BTreeMap<usize, (usize, Box<ffi::sqlite3_io_methods>)>> =
    Mutex::new(BTreeMap::new());

unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> c_int {
    let (original, replacement) = METHODS.lock().unwrap().remove(&(file as usize)).unwrap();
    unsafe {
        (*file).pMethods = original as *const ffi::sqlite3_io_methods;
        let result = (*(*file).pMethods).xClose.unwrap()(file);
        drop(replacement);
        result
    }
}
unsafe extern "C" fn sync(file: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    let original = METHODS.lock().unwrap()[&(file as usize)].0 as *const ffi::sqlite3_io_methods;
    if FAIL_SYNC.swap(false, Ordering::Relaxed) {
        ffi::SQLITE_IOERR_FSYNC
    } else {
        unsafe { (*original).xSync.unwrap()(file, flags) }
    }
}
unsafe extern "C" fn open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    output: *mut c_int,
) -> c_int {
    let original_vfs = ORIGINAL_VFS.load(Ordering::Relaxed);
    let result = unsafe { (*original_vfs).xOpen.unwrap()(original_vfs, name, file, flags, output) };
    if result == ffi::SQLITE_OK {
        unsafe {
            let original = (*file).pMethods;
            let mut replacement = Box::new(*original);
            replacement.xClose = Some(close);
            replacement.xSync = Some(sync);
            (*file).pMethods = &*replacement;
            METHODS
                .lock()
                .unwrap()
                .insert(file as usize, (original as usize, replacement));
        }
    }
    result
}
struct FailingVfs(Box<ffi::sqlite3_vfs>);
impl Drop for FailingVfs {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_vfs_unregister(&mut *self.0);
        }
    }
}

#[test]
fn commit_io_failure_is_a_rollback_and_does_not_hide_the_next_commit() {
    let temp = tempfile::tempdir().unwrap();
    let mut vfs = unsafe {
        let original = ffi::sqlite3_vfs_find(std::ptr::null());
        assert!(!original.is_null());
        ORIGINAL_VFS.store(original, Ordering::Relaxed);
        let mut vfs = FailingVfs(Box::new(*original));
        vfs.0.zName = b"audit-failing-sync\0".as_ptr().cast();
        vfs.0.xOpen = Some(open);
        assert_eq!(ffi::sqlite3_vfs_register(&mut *vfs.0, 0), ffi::SQLITE_OK);
        vfs
    };
    let db = Connection::open_with_flags_and_vfs(
        temp.path().join("failure.sqlite"),
        rusqlite::OpenFlags::default(),
        "audit-failing-sync",
    )
    .unwrap();
    install(&db);
    db.execute_batch(
        "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; CREATE TABLE data(value);",
    )
    .unwrap();
    let before = snapshot(db.path().unwrap());
    db.execute_batch("BEGIN IMMEDIATE; INSERT INTO data VALUES(1);")
        .unwrap();
    FAIL_SYNC.store(true, Ordering::Relaxed);
    assert!(db.execute_batch("COMMIT;").is_err());
    let failed = snapshot(db.path().unwrap());
    assert_eq!(failed.commit_attempts - before.commit_attempts, 1);
    assert_eq!(failed.writes - before.writes, 1);
    assert_eq!(failed.commits - before.commits, 0);
    assert_eq!(failed.rollbacks - before.rollbacks, 1);
    assert_eq!(
        db.query_row("SELECT count(*) FROM data", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute_batch("INSERT INTO data VALUES(2);").unwrap();
    assert_eq!(snapshot(db.path().unwrap()).commits - before.commits, 1);
    drop(db);
    // Keep the registered VFS allocation alive until every connection is closed.
    let _ = &mut vfs;
}

#[test]
fn vacuum_records_the_destination_only_after_success() {
    let temp = tempfile::tempdir().unwrap();
    let db = crate::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE data(value); INSERT INTO data VALUES('retained');")
        .unwrap();
    let target = temp.path().join("spill.sqlite");
    vacuum_into(&db, &target).unwrap();
    let count = snapshot(target.to_str().unwrap());
    assert_eq!(count.external_write_attempts, 1);
    assert_eq!(count.writes, 1);
    assert_eq!(count.commits, 1);
    assert!(vacuum_into(&db, &target).is_err());
    let count = snapshot(target.to_str().unwrap());
    assert_eq!(count.external_write_attempts, 2);
    assert_eq!(count.writes, 1);
    assert_eq!(count.commits, 1);
}

#[test]
fn lost_output_is_cumulative_and_invalid_even_after_logging_recovers() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("status");
    std::fs::create_dir(&directory).unwrap();
    let before = FAILURES.load(Ordering::Relaxed);
    output(
        &json!({"event":"probe"}),
        Some(temp.path()),
        Some(&directory),
    );
    assert!(FAILURES.load(Ordering::Relaxed) > before);
    let path = temp.path().join("events.jsonl");
    output(&json!({"event":"probe"}), Some(&path), Some(&directory));
    let event: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let status: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.join(format!("{}.json", std::process::id()))).unwrap(),
    )
    .unwrap();
    assert_eq!(event["measurement_valid"], false);
    assert_eq!(status["measurement_valid"], false);
    assert!(status["audit_failures"].as_u64().unwrap() > 0);
}

#[cfg(windows)]
#[test]
fn exclusive_log_handle_invalidates_the_measurement() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let before = FAILURES.load(Ordering::Relaxed);
    output(&json!({"event":"probe"}), Some(&path), Some(temp.path()));
    assert!(FAILURES.load(Ordering::Relaxed) > before);
    drop(held);
}

#[test]
fn write_failure_is_accumulated() {
    struct Full;
    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from_raw_os_error(112))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let before = FAILURES.load(Ordering::Relaxed);
    record_output_result(write_record(&mut Full, &json!({"event":"probe"})));
    assert!(FAILURES.load(Ordering::Relaxed) > before);
}
