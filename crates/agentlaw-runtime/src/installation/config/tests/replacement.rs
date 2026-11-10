use super::*;
use std::os::windows::fs::OpenOptionsExt;

fn selections(root: &Path) -> (Config, Config) {
    let old = Config {
        memory_store_path: root.join("old"),
        user_id: "user".into(),
        history_response_limit_bytes: 8192,
        runtime_path: None,
        response_limit_bytes: 65536,
    };
    let next = Config {
        memory_store_path: root.join("new"),
        ..old.clone()
    };
    fs::write(root.join("config.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    (old, next)
}
fn prevent_replacement(root: &Path) -> fs::File {
    // Permit readers while withholding delete sharing, as a real competing handle may.
    fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(root.join("config.json"))
        .unwrap()
}

#[test]
fn locked_destination_preserves_selection_and_reports_os_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (old, next) = selections(root);
    let original = fs::read(root.join("config.json")).unwrap();
    let handle = prevent_replacement(root);
    let error = replace_selection(root, &old, &next).unwrap_err();
    assert_eq!(error.code, "configuration_io");
    assert!(error.retryable);
    let details = error.details.unwrap();
    assert!(matches!(
        details["raw_os_error"].as_i64(),
        Some(5 | 32 | 33)
    ));
    assert!(!details["os_error_kind"].as_str().unwrap().is_empty());
    assert!(error.message.contains("raw code:"));
    assert_eq!(fs::read(root.join("config.json")).unwrap(), original);
    drop(handle);
    replace_selection(root, &old, &next).unwrap();
    assert_eq!(load(root).unwrap(), Some(next));
}

#[test]
fn transient_lock_is_retried_within_the_same_call() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (old, next) = selections(root);
    let handle = prevent_replacement(root);
    let (failed_tx, failed_rx) = std::sync::mpsc::channel();
    let (released_tx, released_rx) = std::sync::mpsc::channel();
    let released_rx = std::sync::Mutex::new(released_rx);
    let observed = std::sync::atomic::AtomicBool::new(false);
    selection_retry_hook().lock().unwrap().insert(
        root.join("config.json"),
        std::sync::Arc::new(move || {
            if !observed.swap(true, std::sync::atomic::Ordering::AcqRel) {
                failed_tx.send(()).unwrap();
                released_rx.lock().unwrap().recv().unwrap();
            }
        }),
    );
    let worker_root = root.to_path_buf();
    let worker_old = old.clone();
    let worker_next = next.clone();
    let worker =
        std::thread::spawn(move || replace_selection(&worker_root, &worker_old, &worker_next));
    failed_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    drop(handle);
    released_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    selection_retry_hook()
        .lock()
        .unwrap()
        .remove(&root.join("config.json"));
    assert_eq!(load(root).unwrap(), Some(next));
}

#[test]
fn expired_deadline_does_not_start_another_replace() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (old, next) = selections(root);
    let handle = prevent_replacement(root);
    let (failed_tx, failed_rx) = std::sync::mpsc::channel();
    let (released_tx, released_rx) = std::sync::mpsc::channel();
    let released_rx = std::sync::Mutex::new(released_rx);
    selection_retry_hook().lock().unwrap().insert(
        root.join("config.json"),
        std::sync::Arc::new(move || {
            failed_tx.send(()).unwrap();
            released_rx.lock().unwrap().recv().unwrap();
        }),
    );
    let worker_root = root.to_path_buf();
    let worker_old = old.clone();
    let worker_next = next.clone();
    let worker =
        std::thread::spawn(move || replace_selection(&worker_root, &worker_old, &worker_next));
    failed_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(550));
    drop(handle);
    released_tx.send(()).unwrap();
    let error = worker.join().unwrap().unwrap_err();
    selection_retry_hook()
        .lock()
        .unwrap()
        .remove(&root.join("config.json"));
    assert_eq!(error.code, "configuration_io");
    assert_eq!(load(root).unwrap(), Some(old));
}
