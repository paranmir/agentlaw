//! Black-box help and argv validation. Every subprocess keeps stdin open until
//! it exits, and hostile installation fixtures detect accidental state access.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

const STATIC_COMMAND_DEADLINE: Duration = Duration::from_secs(5);

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(state: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentlaw"));
    command
        .args(args)
        .current_dir(cwd)
        .env("AGENTLAW_HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let mut child = OwnedChild(command.spawn().expect("spawn owned CLI"));
    // Do not send EOF: a help/invalid-argv path must finish without reading stdin.
    let held_stdin = child.0.stdin.take().unwrap();
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    // Drain concurrently so a full help-output pipe cannot look like a hang.
    let output_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let error_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + STATIC_COMMAND_DEADLINE;
    let (status, timed_out) = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break (status, false);
        }
        if Instant::now() >= deadline {
            // Kill only the process this test spawned, never a global daemon/PID.
            child.0.kill().expect("kill timed-out owned CLI");
            break (child.0.wait().unwrap(), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(held_stdin);
    let output = Output {
        status,
        stdout: output_reader.join().unwrap(),
        stderr: error_reader.join().unwrap(),
    };
    assert!(
        !timed_out,
        "CLI read held-open stdin or did not return promptly for {args:?}: {output:?}"
    );
    output
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
    original: BTreeMap<PathBuf, Option<Vec<u8>>>,
}
impl HostileHome {
    fn new(state_is_file: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        if state_is_file {
            fs::write(&state, b"This state path must remain a file.\n").unwrap();
        } else {
            fs::create_dir(&state).unwrap();
            fs::write(state.join("config.json"), b"{ invalid configuration\n").unwrap();
        }
        let original = snapshot(temp.path());
        Self {
            temp,
            state,
            original,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        let output = run(&self.state, self.temp.path(), args);
        assert_eq!(
            snapshot(self.temp.path()),
            self.original,
            "help/static validation changed installation or working files for {args:?}"
        );
        output
    }
}

fn successful_json(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("Expected JSON: {output:?}"))
}

fn successful_text(output: &Output, path: &[&str]) -> String {
    assert!(output.status.success(), "{path:?}: {output:?}");
    assert!(output.stderr.is_empty(), "{path:?}: {output:?}");
    assert!(
        serde_json::from_slice::<Value>(&output.stdout).is_err(),
        "Contextual help must be text: {path:?}: {output:?}"
    );
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    let usage = text
        .lines()
        .find(|line| line.contains("Usage:"))
        .unwrap_or_else(|| panic!("Missing usage: {path:?}: {text}"));
    assert!(usage.contains("agentlaw"), "{path:?}: {text}");
    for component in path {
        assert!(usage.contains(component), "{path:?}: {text}");
    }
    text
}

fn invalid_arguments(output: &Output, procedure_command: bool) {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let bytes = if procedure_command {
        assert!(
            output.stdout.is_empty(),
            "Procedure error leaked to stdout: {output:?}"
        );
        &output.stderr
    } else {
        assert!(
            output.stderr.is_empty(),
            "Ordinary error leaked to stderr: {output:?}"
        );
        &output.stdout
    };
    let error: Value = serde_json::from_slice(bytes)
        .unwrap_or_else(|_| panic!("Expected one structured argument error: {output:?}"));
    assert_eq!(error["code"], "invalid_arguments", "{error}");
    assert!(error["message"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn root_help_aliases_are_json_and_hide_internal_commands() {
    for state_is_file in [true, false] {
        let home = HostileHome::new(state_is_file);
        let mut expected = None;
        for args in [&[][..], &["--help"][..], &["-h"][..], &["help"][..]] {
            let value = successful_json(&home.run(args));
            assert_eq!(value["name"], "agentlaw");
            assert_eq!(value["status"], "help");
            let commands = value["commands"]
                .as_array()
                .expect("root command inventory");
            assert!(commands.iter().all(Value::is_string));
            let usages: Vec<_> = commands.iter().map(|v| v.as_str().unwrap()).collect();
            for public in [
                "schema",
                "call",
                "install",
                "sync resolve",
                "learned-procedure search",
            ] {
                assert!(
                    usages.iter().any(|usage| usage.starts_with(public)),
                    "{value}"
                );
            }
            for private in [
                "worker-daemon",
                "model-child",
                "update apply",
                "update --confirm-update",
            ] {
                assert!(
                    usages.iter().all(|usage| !usage.contains(private)),
                    "{value}"
                );
            }
            if let Some(expected) = &expected {
                assert_eq!(&value, expected, "Root aliases disagree");
            } else {
                expected = Some(value);
            }
        }
        let text = successful_text(&home.run(&["help", "--format", "text"]), &[]);
        assert!(text.contains("Commands:"), "{text}");
        assert!(text.contains("learned-procedure"), "{text}");
        for private in [
            "worker-daemon",
            "model-child",
            "update apply",
            "--confirm-update",
        ] {
            assert!(!text.contains(private), "{text}");
        }
    }
}

#[test]
fn every_public_help_node_works_with_hostile_state_and_open_stdin() {
    // Explicitly name the current public tree so an accidentally omitted command
    // cannot disappear from both discovery and this regression's coverage.
    let paths: &[&[&str]] = &[
        &["help"],
        &["describe"],
        &["schema"],
        &["call"],
        &["mcp"],
        &["mcp", "serve"],
        &["update"],
        &["update", "check"],
        &["update", "status"],
        &["support"],
        &["support", "star"],
        &["config"],
        &["config", "path"],
        &["config", "get"],
        &["config", "set"],
        &["install"],
        &["machine"],
        &["machine", "inspect"],
        &["machine", "name"],
        &["doctor"],
        &["repair"],
        &["history"],
        &["history", "export"],
        &["store"],
        &["store", "propose-location"],
        &["store", "create"],
        &["store", "connect"],
        &["learned-procedure"],
        &["learned-procedure", "list"],
        &["learned-procedure", "search"],
        &["continuity"],
        &["continuity", "save"],
        &["share"],
        &["share", "inspect"],
        &["share", "push"],
        &["share", "fetch"],
        &["share", "import"],
        &["share", "import", "prepare"],
        &["share", "import", "inspect"],
        &["share", "import", "call"],
        &["share", "import", "resolve"],
        &["share", "import", "publish"],
        &["sync"],
        &["sync", "start"],
        &["sync", "status"],
        &["sync", "resolve"],
        &["sync", "resume"],
        &["sync", "hold"],
        &["sync", "cancel"],
        &["sync", "policy"],
        &["sync", "policy", "propose"],
        &["sync", "policy", "configure"],
        &["sync", "accept-findings"],
    ];
    for state_is_file in [true, false] {
        let home = HostileHome::new(state_is_file);
        for path in paths {
            let mut long = path.to_vec();
            long.push("--help");
            let expected = successful_text(&home.run(&long), path);
            let mut short = path.to_vec();
            short.push("-h");
            let actual = successful_text(&home.run(&short), path);
            assert_eq!(actual, expected, "Help flag aliases disagree for {path:?}");
            let mut help = vec!["help"];
            help.extend_from_slice(path);
            let actual = successful_text(&home.run(&help), path);
            assert_eq!(actual, expected, "Help command disagrees for {path:?}");
        }
    }
}

#[test]
fn help_after_input_options_does_not_wait_for_stdin_or_required_operands() {
    let home = HostileHome::new(false);
    for (args, path) in [
        (&["call", "--json", "-", "--help"][..], &["call"][..]),
        (
            &["sync", "resolve", "--solution", "-", "--help"][..],
            &["sync", "resolve"][..],
        ),
        (
            &["share", "import", "call", "--json", "-", "--help"][..],
            &["share", "import", "call"][..],
        ),
        (
            &["share", "import", "resolve", "--choices", "-", "--help"][..],
            &["share", "import", "resolve"][..],
        ),
    ] {
        successful_text(&home.run(args), path);
    }
    // Pinned clap rejects the missing option value before interpreting help.
    // This still must return with held-open stdin and unchanged hostile state.
    invalid_arguments(&home.run(&["machine", "name", "--value", "--help"]), false);
}

#[test]
fn describe_returns_selected_cli_metadata_and_schema_remains_mcp_metadata() {
    let home = HostileHome::new(false);
    let root = successful_json(&home.run(&["describe"]));
    assert_eq!(root["name"], "agentlaw");
    assert_eq!(root["path"], json!([]));
    let children = root["commands"].as_array().expect("public child metadata");
    assert!(children.iter().any(|c| c["name"] == "sync"), "{root}");
    for private in ["worker-daemon", "model-child"] {
        assert!(children.iter().all(|c| c["name"] != private), "{root}");
    }
    let resolve = successful_json(&home.run(&["describe", "sync", "resolve"]));
    assert_eq!(resolve["name"], "agentlaw");
    assert_eq!(resolve["path"], json!(["sync", "resolve"]));
    let usage = resolve["usage"].as_str().expect("command usage");
    assert!(usage.contains("agentlaw sync resolve"), "{resolve}");
    let options = resolve["options"]
        .as_array()
        .expect("command option metadata");
    for long in ["operation", "revision", "request-id", "solution"] {
        let option = options
            .iter()
            .find(|o| o["long"] == long)
            .unwrap_or_else(|| panic!("Missing {long}: {resolve}"));
        assert_eq!(option["takes_value"], true, "{option}");
        assert_eq!(option["required"], true, "{option}");
    }
    let update = successful_json(&home.run(&["describe", "update"]));
    assert!(update["commands"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["name"] != "apply"));
    assert!(update["options"]
        .as_array()
        .unwrap()
        .iter()
        .all(|o| o["long"] != "confirm-update"));
    let list = successful_json(&home.run(&["describe", "learned-procedure", "list"]));
    let format = list["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["long"] == "format")
        .expect("procedure format metadata");
    assert_eq!(format["required"], false, "{format}");
    assert!(format["possible_values"]
        .as_array()
        .unwrap()
        .contains(&json!("jsonl")));
    assert!(format["possible_values"]
        .as_array()
        .unwrap()
        .contains(&json!("table")));
    let schema = successful_json(&home.run(&["schema"]));
    assert_eq!(schema["name"], "agentlaw");
    assert!(schema["inputSchema"].is_object(), "{schema}");
    assert!(
        schema.get("path").is_none(),
        "CLI description replaced MCP schema: {schema}"
    );
}

#[test]
fn unknown_duplicate_and_missing_arguments_fail_before_state_or_stdin() {
    let cases: &[&[&str]] = &[
        &["unknown-command"],
        &["help", "unknown-command"],
        &["describe", "sync", "unknown-command"],
        &["install", "--harness", "codex", "--harness", "codex"],
        &["machine", "name", "--value", "one", "--value", "two"],
        &["history", "export", "--memory-id", "ID"],
        &["call", "--json", "-", "--unknown"],
        &["mcp", "serve", "--stdio", "--unknown"],
        &["sync", "status", "--operation", "OP", "--solution", "-"],
        &[
            "sync",
            "resolve",
            "--operation",
            "OP",
            "--revision",
            "not-number",
            "--request-id",
            "REQ",
            "--solution",
            "-",
        ],
        &[
            "sync",
            "resolve",
            "--operation",
            "OP",
            "--revision",
            "1",
            "--request-id",
            "REQ",
            "--solution",
            "-",
            "--operation",
            "OP",
        ],
        &[
            "share",
            "import",
            "call",
            "--ref",
            "IMPORT",
            "--json",
            "-",
            "--unknown",
        ],
        &[
            "share",
            "import",
            "resolve",
            "--ref",
            "IMPORT",
            "--choices",
            "-",
        ],
    ];
    for state_is_file in [true, false] {
        let home = HostileHome::new(state_is_file);
        for args in cases {
            invalid_arguments(&home.run(args), false);
        }
    }
}

#[test]
fn invalid_argv_wins_over_unreadable_solution_and_choice_files() {
    let home = HostileHome::new(false);
    let missing = home.temp.path().join("must-not-be-read.json");
    let file = missing.to_str().unwrap();
    for args in [
        vec!["sync", "status", "--operation", "OP", "--solution", file],
        vec![
            "sync",
            "resolve",
            "--operation",
            "OP",
            "--revision",
            "1",
            "--request-id",
            "REQ",
            "--solution",
            file,
            "--unknown",
        ],
        vec![
            "share",
            "import",
            "resolve",
            "--ref",
            "IMPORT",
            "--choices",
            file,
            "--user-confirmed",
            "--unknown",
        ],
    ] {
        invalid_arguments(&home.run(&args), false);
    }
    assert!(!missing.exists());
}

#[test]
fn procedure_argument_errors_keep_stderr_and_stdout_separate() {
    let home = HostileHome::new(false);
    for args in [
        &["learned-procedure", "list", "--scope", "unknown"][..],
        &[
            "learned-procedure",
            "list",
            "--format",
            "jsonl",
            "--format",
            "table",
        ][..],
        &["learned-procedure", "search"][..],
        &[
            "learned-procedure",
            "search",
            "--query",
            "one",
            "--query",
            "two",
        ][..],
        &[
            "learned-procedure",
            "search",
            "--query",
            "one",
            "--limit",
            "0",
        ][..],
    ] {
        invalid_arguments(&home.run(args), true);
    }
    successful_text(
        &home.run(&["learned-procedure", "search", "--help"]),
        &["learned-procedure", "search"],
    );
}

#[test]
fn equals_value_is_literal_and_end_of_options_does_not_trigger_help() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let named = successful_json(&run(
        &state,
        temp.path(),
        &["machine", "name", "--value=--help"],
    ));
    assert_eq!(named["display_name"], "--help", "{named}");
    let persisted: Value =
        serde_json::from_slice(&fs::read(state.join("machine.json")).unwrap()).unwrap();
    assert_eq!(persisted["display_name"], "--help");
    let home = HostileHome::new(false);
    for args in [
        &["help", "--", "--help"][..],
        &["describe", "--", "--help"][..],
        &["machine", "name", "--", "--help"][..],
        &["call", "--json", "-", "--", "--help"][..],
    ] {
        invalid_arguments(&home.run(args), false);
    }
}
