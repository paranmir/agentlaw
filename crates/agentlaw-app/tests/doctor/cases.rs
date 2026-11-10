use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

fn run(state: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentlaw"));
    command.env("AGENTLAW_HOME", state).args(args);
    for name in [
        "AGENTLAW_ONNX_MODEL",
        "AGENTLAW_TOKENIZER",
        "AGENTLAW_ORT_LIBRARY",
    ] {
        command.env_remove(name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.output().unwrap()
}

fn json_output(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn create(state: &Path, source: &Path) {
    let result = run(
        state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
}

fn files(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, dir: &Path, result: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        if !dir.exists() {
            return;
        }
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink());
            result.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                if kind.is_dir() {
                    None
                } else {
                    Some(fs::read(&path).unwrap())
                },
            );
            if kind.is_dir() {
                visit(root, &path, result);
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn store(state: &Path, source: &Path) -> agentlaw_storage::Store {
    let entries = files(state);
    let lock = entries
        .keys()
        .find(|p| p.file_name().is_some_and(|n| n == "source.lock"))
        .expect("source gate");
    agentlaw_storage::Store::open_with_coordination(
        source,
        state.join(lock.parent().unwrap()),
        state.join("source-coordination"),
    )
    .unwrap()
}

fn assert_health(value: &Value) {
    assert!(value["worker_runtime"].is_object(), "{value}");
    assert!(
        value["observed_at_ms"].as_u64().is_some_and(|v| v > 0),
        "missing observation time: {value}"
    );
}

fn assert_no_audit(value: &Value) {
    assert!(value.get("audit").is_none(), "{value}");
}

#[test]
fn doctor_flags_are_in_generated_help_and_unknown_flag_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let help = run(&state, &["doctor", "--help"]);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("--health"), "{help}");
    assert!(help.contains("--audit"), "{help}");
    let invalid = run(&state, &["doctor", "--unknown-doctor-option"]);
    assert!(!invalid.status.success());
    assert_eq!(json_output(&invalid)["code"], "invalid_arguments");
    assert!(!state.exists());
}

#[test]
fn doctor_modes_report_setup_without_creating_installation() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("absent");
    for args in [
        &["doctor"][..],
        &["doctor", "--health"][..],
        &["doctor", "--audit"][..],
    ] {
        let output = run(&state, args);
        let result = json_output(&output);
        assert!(output.status.success(), "{args:?}: {result}");
        assert_eq!(result["status"], "setup_required");
        assert_health(&result);
        assert_no_audit(&result);
        assert!(!state.exists());
    }
}

#[test]
fn doctor_corrupted_source_is_not_examined_by_default_health() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("source");
    let create = run(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stdout)
    );
    let format = source.join("format.md");
    fs::write(&format, b"corrupted source bytes").unwrap();
    let before = fs::read(&format).unwrap();
    for args in [&["doctor"][..], &["doctor", "--health"][..]] {
        let output = run(&state, args);
        let value = json_output(&output);
        assert!(output.status.success(), "{args:?}: {value}");
        assert_health(&value);
        assert_no_audit(&value);
        assert_eq!(fs::read(&format).unwrap(), before);
    }
}

#[test]
fn doctor_health_does_not_change_any_state_files() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("source");
    create(&state, &source);
    let before = files(&state);
    for args in [&["doctor"][..], &["doctor", "--health"][..]] {
        let output = run(&state, args);
        let value = json_output(&output);
        assert!(output.status.success(), "{value}");
        assert_health(&value);
        assert_no_audit(&value);
        assert_eq!(files(&state), before, "{args:?} changed state files");
    }
}

#[test]
fn doctor_health_finishes_while_storage_exclusive_gate_is_held() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("source");
    create(&state, &source);
    let store = store(&state, &source);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        store
            .with_source_read(|_, _| {
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap()
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    for args in [vec!["doctor"], vec!["doctor", "--health"]] {
        let state = state.clone();
        let (tx, rx) = mpsc::channel();
        let start = Instant::now();
        std::thread::spawn(move || {
            let _ = tx.send(run(&state, &args));
        });
        let output = rx.recv_timeout(Duration::from_secs(10));
        if output.is_err() {
            release_tx.send(()).unwrap();
            holder.join().unwrap();
            panic!("doctor waited for source gate");
        }
        let output = output.unwrap();
        let value = json_output(&output);
        assert!(output.status.success(), "{value}");
        assert_health(&value);
        assert!(start.elapsed() < Duration::from_secs(10));
        assert_no_audit(&value);
    }
    release_tx.send(()).unwrap();
    holder.join().unwrap();
}

#[test]
fn doctor_health_finishes_during_real_publications() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("source");
    create(&state, &source);
    let store = store(&state, &source);
    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = stop.clone();
    let (tx, rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut count = 0;
        while !writer_stop.load(Ordering::Relaxed) {
            let project = uuid::Uuid::new_v4().to_string();
            store
                .publish_catalog(
                    &uuid::Uuid::new_v4().to_string(),
                    &project,
                    serde_json::json!({"project_id":project}),
                )
                .unwrap();
            count += 1;
            if count == 1 {
                tx.send(()).unwrap();
            }
        }
        count
    });
    rx.recv_timeout(Duration::from_secs(20)).unwrap();
    let start = Instant::now();
    let output = run(&state, &["doctor"]);
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    assert!(writer.join().unwrap() >= 1);
    let value = json_output(&output);
    assert!(output.status.success(), "{value}");
    assert_health(&value);
    assert_no_audit(&value);
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
}

#[test]
fn doctor_audit_includes_health_and_successful_source_result() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("source");
    create(&state, &source);
    let health = json_output(&run(&state, &["doctor"]));
    let output = run(&state, &["doctor", "--audit"]);
    let value = json_output(&output);
    assert!(output.status.success(), "{value}");
    assert_health(&value);
    assert_no_audit(&health);
    assert_eq!(value["audit"]["status"], "passed", "{value}");
    assert_eq!(value["audit"]["source"]["validated"], true, "{value}");
}

#[test]
fn doctor_audit_failure_preserves_health_and_corrupt_source_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("source");
    create(&state, &source);
    let format = source.join("format.md");
    fs::write(&format, b"corrupted source bytes").unwrap();
    let before = fs::read(&format).unwrap();
    let output = run(&state, &["doctor", "--audit"]);
    let value = json_output(&output);
    assert!(output.status.success(), "{value}");
    assert_health(&value);
    assert_eq!(value["audit"]["status"], "failed", "{value}");
    assert_eq!(value["audit"]["error"]["code"], "source_corrupt", "{value}");
    assert!(value["worker_runtime"].get("code").is_none(), "{value}");
    assert_eq!(fs::read(&format).unwrap(), before);
}
