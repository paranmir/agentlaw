//! Frozen error/exit compatibility through the real CLI. Hostile isolated state
//! snapshots check visible effects; they do not trace every filesystem read.

use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        if path.is_dir() {
            entries.insert(relative, None);
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        } else {
            entries.insert(relative, Some(fs::read(path).unwrap()));
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

struct HostileHome {
    temp: tempfile::TempDir,
    state: PathBuf,
}
impl HostileHome {
    fn new(state_is_file: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        if state_is_file {
            fs::write(&state, b"State must remain a file.\n").unwrap();
        } else {
            fs::create_dir(&state).unwrap();
            fs::write(state.join("config.json"), b"invalid configuration").unwrap();
            fs::write(state.join("machine.json"), b"invalid identity").unwrap();
        }
        Self { temp, state }
    }

    fn run(&self, args: &[&str]) -> Output {
        let before = snapshot(self.temp.path());
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentlaw"));
        command
            .args(args)
            .current_dir(self.temp.path())
            .env("AGENTLAW_HOME", &self.state)
            .env("CODEX_HOME", self.temp.path().join("codex"))
            .env("PI_CODING_AGENT_DIR", self.temp.path().join("pi"))
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
        let mut child = OwnedChild(command.spawn().unwrap());
        let held_stdin = child.0.stdin.take().unwrap();
        let mut stdout = child.0.stdout.take().unwrap();
        let mut stderr = child.0.stderr.take().unwrap();
        let out_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let err_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "CLI waited for stdin: {args:?}");
            std::thread::sleep(Duration::from_millis(10));
        };
        drop(held_stdin);
        let output = Output {
            status,
            stdout: out_reader.join().unwrap(),
            stderr: err_reader.join().unwrap(),
        };
        assert_eq!(snapshot(self.temp.path()), before, "{args:?}: {output:?}");
        output
    }
}

fn expected_error(output: &Output, code: &str, exit: i32, message: &str) {
    let expected = json!({
        "code":code,
        "message":message,
        "retryable":false,
        "next_action":"Explain the issue in the user's language. Do not treat it as an empty or successful result."
    });
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        output.stdout,
        format!("{expected}\n").as_bytes(),
        "{output:?}"
    );
}

#[test]
fn unsupported_harness_keeps_installation_error_and_exit_one() {
    for state_is_file in [false, true] {
        let home = HostileHome::new(state_is_file);
        for harness in ["private-unsupported-harness", "CODEX", ""] {
            expected_error(
                &home.run(&["install", "--harness", harness]),
                "installation_failed",
                1,
                "Use harness codex or oh-my-pi. Use 'agentlaw install --help' for help.",
            );
        }
    }
}

#[test]
fn missing_harness_keeps_required_error_and_exit_two() {
    let home = HostileHome::new(false);
    for args in [&["install"][..], &["install", "--confirm-install"][..]] {
        expected_error(
            &home.run(args),
            "harness_required",
            2,
            "Specify --harness codex or --harness oh-my-pi. Use 'agentlaw install --help' for help.",
        );
    }
}

#[test]
fn installation_syntax_errors_keep_argument_error_and_exit_two() {
    let home = HostileHome::new(true);
    for (args, message) in [
        (
            &["install", "--harness"][..],
            "An argument value is invalid. Use 'agentlaw install --help' for help.",
        ),
        (
            &["install", "--harness", "codex", "--private-unknown-option"][..],
            "Unknown argument. Use 'agentlaw install --help' for help.",
        ),
        (
            &["install", "--harness", "codex", "--harness", "oh-my-pi"][..],
            "Duplicate or incompatible arguments. Use 'agentlaw install --help' for help.",
        ),
        (
            &["install", "--harness", "codex", "--harness-dir"][..],
            "An argument value is invalid. Use 'agentlaw install --help' for help.",
        ),
    ] {
        expected_error(&home.run(args), "invalid_arguments", 2, message);
    }
}

#[test]
fn install_help_bypasses_resources_and_retains_possible_values() {
    let home = HostileHome::new(true);
    for args in [&["install", "--help"][..], &["help", "install"][..]] {
        let output = home.run(args);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let text = std::str::from_utf8(&output.stdout).unwrap();
        assert!(text.contains("Usage: agentlaw install"), "{text}");
        assert!(text.contains("codex, oh-my-pi"), "{text}");
    }
    let output = home.run(&["describe", "install"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let harness = value["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["long"] == "harness")
        .unwrap();
    assert_eq!(harness["required"], true);
    assert_eq!(harness["takes_value"], true);
    assert_eq!(harness["possible_values"], json!(["codex", "oh-my-pi"]));
}

#[test]
fn config_set_invalid_key_and_number_keep_configuration_error() {
    let home = HostileHome::new(false);
    for args in [
        &["config", "set", "private-unknown-key", "4096"][..],
        &[
            "config",
            "set",
            "response_limit_bytes",
            "private-not-number",
        ][..],
        &[
            "config",
            "set",
            "history.response_limit_bytes",
            "184467440737095516160",
        ][..],
    ] {
        expected_error(
            &home.run(args),
            "invalid_configuration",
            2,
            "Use a supported delivery setting and an integer byte count. Use 'agentlaw config set --help' for help.",
        );
    }
    for (args, message) in [
        (
            &["config", "set", "response_limit_bytes"][..],
            "A required argument is missing. Use 'agentlaw config set --help' for help.",
        ),
        (
            &[
                "config",
                "set",
                "response_limit_bytes",
                "4096",
                "--private-unknown-option",
            ][..],
            "Unknown argument. Use 'agentlaw config set --help' for help.",
        ),
    ] {
        expected_error(&home.run(args), "invalid_arguments", 2, message);
    }
}

#[test]
fn config_get_unknown_key_remains_argument_error() {
    let home = HostileHome::new(true);
    expected_error(
        &home.run(&["config", "get", "private-unknown-key"]),
        "invalid_arguments",
        2,
        "An argument value is invalid. Use 'agentlaw config get --help' for help.",
    );
}
