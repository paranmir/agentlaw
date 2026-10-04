//! Negative payload checks through the actual CLI boundary. Input error codes
//! and unchanged hostile fixtures establish rejection before visible state
//! effects; these tests do not trace every read, process launch, or network call.

use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const DEADLINE: Duration = Duration::from_secs(5);

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        if path.is_dir() {
            result.insert(relative, None);
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), result);
            }
        } else {
            result.insert(relative, Some(fs::read(path).unwrap()));
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        bytes
    })
}

struct HostileHome {
    temp: tempfile::TempDir,
    state: PathBuf,
}
impl HostileHome {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::write(state.join("config.json"), b"{ invalid configuration\n").unwrap();
        Self { temp, state }
    }

    /// `None` leaves stdin open without sending bytes; an argv/file rejection
    /// must return without waiting for input. Writers/drainers run concurrently
    /// so the oversized-input case cannot deadlock on pipe capacity.
    fn run(&self, args: Vec<OsString>, payload: Option<Vec<u8>>) -> Output {
        let original = snapshot(self.temp.path());
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentlaw"));
        command
            .args(&args)
            .current_dir(self.temp.path())
            .env("AGENTLAW_HOME", &self.state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in [
            "AGENTLAW_ONNX_MODEL",
            "AGENTLAW_TOKENIZER",
            "AGENTLAW_ORT_LIBRARY",
            "AGENTLAW_UPDATE_PROBE_PLAN",
        ] {
            command.env_remove(name);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = OwnedChild(command.spawn().expect("spawn owned CLI"));
        let stdout = drain(child.0.stdout.take().unwrap());
        let stderr = drain(child.0.stderr.take().unwrap());
        let mut held_stdin = Some(child.0.stdin.take().unwrap());
        let writer = payload.map(|payload| {
            let mut stdin = held_stdin.take().unwrap();
            thread::spawn(move || stdin.write_all(&payload))
        });
        let deadline = Instant::now() + DEADLINE;
        let (status, timed_out) = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break (status, false);
            }
            if Instant::now() >= deadline {
                child.0.kill().expect("kill only timed-out owned CLI");
                break (child.0.wait().unwrap(), true);
            }
            thread::sleep(Duration::from_millis(10));
        };
        drop(held_stdin);
        if let Some(writer) = writer {
            match writer.join().expect("join stdin writer") {
                Ok(()) => {}
                // A bounded reader may reject before the writer sends the tail.
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
                Err(error) => panic!("unexpected stdin write failure: {error}"),
            }
        }
        let output = Output {
            status,
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        };
        assert!(
            !timed_out,
            "CLI waited for input or hung for {args:?}: {output:?}"
        );
        assert_eq!(
            snapshot(self.temp.path()),
            original,
            "input rejection changed fixture for {args:?}"
        );
        output
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.temp.path().join(name);
        fs::write(&path, contents).unwrap();
        path
    }
}

fn argv(args: &[&str]) -> Vec<OsString> {
    args.iter().map(|value| OsString::from(*value)).collect()
}

fn rejected(output: Output, code: &str, exit: i32) {
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(
        output.stderr.is_empty(),
        "unexpected progress/backend error: {output:?}"
    );
    let error: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("expected one structured input error: {output:?}"));
    assert_eq!(
        error["code"], code,
        "expected payload rejection, not a configuration/backend error: {error}"
    );
}

#[test]
fn call_and_import_reject_bad_stdin_before_visible_state_effects() {
    let home = HostileHome::new();
    let cases = [
        (
            "malformed JSON",
            b"{\"action\":".to_vec(),
            "invalid_input",
            2,
        ),
        (
            "duplicate keys",
            br#"{"action":"recall","recall":{"recall_for":"first","recall_for":"second"}}"#
                .to_vec(),
            "invalid_input",
            2,
        ),
        ("invalid UTF-8", vec![0xff, 0xfe], "invalid_encoding", 2),
        (
            "oversized stdin",
            vec![b' '; agentlaw_app::MAX_REQUEST_BYTES + 1024],
            "transport_capacity",
            1,
        ),
    ];
    for (name, payload, code, exit) in cases {
        for command in [
            &["call", "--json", "-"][..],
            &["share", "import", "call", "--ref", "REF", "--json", "-"][..],
        ] {
            let output = home.run(argv(command), Some(payload.clone()));
            assert!(!output.status.success(), "accepted {name} for {command:?}");
            rejected(output, code, exit);
        }
    }
}

#[test]
fn sync_rejects_bad_solution_files_before_visible_state_effects() {
    let home = HostileHome::new();
    let cases = [
        ("malformed.json", b"{".to_vec(), "invalid_input", 2),
        (
            "duplicate.json",
            br#"{"units":[],"units":[]}"#.to_vec(),
            "invalid_input",
            2,
        ),
        (
            "oversized.json",
            vec![b' '; agentlaw_app::MAX_REQUEST_BYTES + 1],
            "request_too_large",
            1,
        ),
    ];
    for (name, payload, code, exit) in cases {
        let path = home.file(name, &payload);
        let mut args = argv(&[
            "sync",
            "resolve",
            "--operation",
            "OP",
            "--revision",
            "1",
            "--request-id",
            "REQ",
            "--solution",
        ]);
        args.push(path.into_os_string());
        rejected(home.run(args, None), code, exit);
    }
}

#[test]
fn sync_rejects_invalid_policy_payload_before_visible_state_effects() {
    let home = HostileHome::new();
    let path = home.file("policy.json", b"{}");
    let mut args = argv(&[
        "sync",
        "policy",
        "configure",
        "--confirm-delegation",
        "--file",
    ]);
    args.push(path.into_os_string());
    rejected(home.run(args, None), "git_io", 1);
}

#[test]
fn invalid_sync_revision_does_not_wait_for_solution_stdin() {
    let home = HostileHome::new();
    rejected(
        home.run(
            argv(&[
                "sync",
                "resolve",
                "--operation",
                "OP",
                "--revision",
                "0",
                "--request-id",
                "REQ",
                "--solution",
                "-",
            ]),
            None,
        ),
        "invalid_arguments",
        2,
    );
}
