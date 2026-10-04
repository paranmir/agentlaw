//! Managed update subprocess tests use only temporary roots and owned processes.
use fs2::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio},
    time::{Duration, Instant},
};

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "agentlaw.exe"
    } else {
        "agentlaw"
    }
}
fn worker_name() -> &'static str {
    if cfg!(windows) {
        "agentlaw-worker.exe"
    } else {
        "agentlaw-worker"
    }
}
fn hash(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}
fn run(binary: &Path, state: &Path, args: &[&str]) -> Value {
    let mut child = None;
    for attempt in 0..20 {
        match Command::new(binary)
            .args(args)
            .env("AGENTLAW_HOME", state)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(value) => {
                child = Some(value);
                break;
            }
            Err(error)
                if cfg!(unix)
                    && error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt < 19 =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("Could not start {}: {error}", binary.display()),
        }
    }
    let mut child = child.expect("bounded executable-busy retry");
    let stdout = child.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = Vec::new();
        let _ = BufReader::new(stdout).read_until(b'\n', &mut line);
        let _ = send.send(line);
    });
    let status = child.wait().unwrap();
    let line = receive.recv_timeout(Duration::from_secs(2)).unwrap();
    let value: Value =
        serde_json::from_slice(&line).unwrap_or_else(|_| panic!("Invalid CLI output: {line:?}"));
    assert!(status.success(), "{value}");
    value
}

struct OwnedStaticChild(Child);
impl Drop for OwnedStaticChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn static_output(binary: &Path, state: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(cwd)
        .env("AGENTLAW_HOME", state)
        .stdin(Stdio::null())
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
    let mut child = OwnedStaticChild(command.spawn().unwrap());
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
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
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "Static CLI did not finish: {args:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: output_reader.join().unwrap(),
        stderr: error_reader.join().unwrap(),
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<String>> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Option<String>>) {
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        if path.is_dir() {
            entries.insert(relative, None);
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        } else {
            entries.insert(relative, Some(hash(path)));
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn cli_error(output: &Output) -> Value {
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"))
}

struct McpClient {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn new(binary: &Path, state: &Path) -> Self {
        let mut child = Command::new(binary)
            .args(["mcp", "serve", "--stdio"])
            .env("AGENTLAW_HOME", state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            input,
            output,
            next_id: 1,
        };
        let initialized = client.request(
            "initialize",
            json!({
                "protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"mixed-release-test","version":"1"}
            }),
        );
        assert!(
            initialized["result"]["serverInfo"]["version"].is_string(),
            "{initialized}"
        );
        client.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        assert!(client.request("tools/list", json!({}))["result"]["tools"].is_array());
        client
    }

    fn send(&mut self, request: Value) {
        writeln!(self.input, "{request}").unwrap();
        self.input.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let mut line = String::new();
            assert!(
                self.output.read_line(&mut line).unwrap() > 0,
                "MCP closed before reply"
            );
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] == id {
                return response;
            }
        }
    }

    fn call(&mut self, argument: Value) -> Value {
        self.request(
            "tools/call",
            json!({"name":"agentlaw","arguments":argument}),
        )["result"]
            .clone()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    host: PathBuf,
    helper: PathBuf,
    id: String,
}
impl Fixture {
    fn new() -> Self {
        Self::with_old_binary(None)
    }

    fn with_old_binary(old_binary: Option<&Path>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("managed");
        let state = root.join("state");
        let host = temp.path().join("codex");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&host).unwrap();
        fs::write(
            root.join(".agentlaw-layout"),
            "agentlaw-managed-layout-v1\n",
        )
        .unwrap();
        let root = fs::canonicalize(root).unwrap();
        let state = root.join("state");
        let bin = root.join("bin");
        let source = Path::new(env!("CARGO_BIN_EXE_agentlaw"));
        let old = bin.join(binary_name());
        fs::copy(old_binary.unwrap_or(source), &old).unwrap();
        if let Some(old_binary) = old_binary {
            let release_dir = old_binary.parent().unwrap();
            fs::copy(release_dir.join(worker_name()), bin.join(worker_name())).unwrap();
            fs::copy(release_dir.join("LICENSE"), bin.join("LICENSE.agentlaw")).unwrap();
        } else {
            fs::write(bin.join(worker_name()), b"new-worker").unwrap();
            fs::write(bin.join("LICENSE.agentlaw"), b"old-license").unwrap();
        }
        let install = run(
            &old,
            &state,
            &[
                "install",
                "--harness",
                "codex",
                "--harness-dir",
                host.to_str().unwrap(),
                "--confirm-install",
            ],
        );
        assert_eq!(install["status"], "installed");
        let receipt = fs::read_dir(&state)
            .unwrap()
            .map(|item| item.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("harness-codex-")
            })
            .unwrap();
        let instructions = PathBuf::from(install["instructions_path"].as_str().unwrap());
        let config = PathBuf::from(install["config_path"].as_str().unwrap());
        let id = uuid::Uuid::new_v4().to_string();
        let staged = root.join(format!(".update-{id}"));
        let unpacked = staged.join("unpacked");
        fs::create_dir_all(&unpacked).unwrap();
        fs::copy(source, unpacked.join(binary_name())).unwrap();
        fs::write(unpacked.join(worker_name()), b"new-worker").unwrap();
        fs::write(unpacked.join("LICENSE.agentlaw"), b"new-license").unwrap();
        let receipt_value: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        let helper = PathBuf::from(receipt_value["executable"].as_str().unwrap());
        let memory = temp.path().join("memory");
        assert_eq!(
            run(
                &old,
                &state,
                &[
                    "store",
                    "create",
                    "--path",
                    memory.to_str().unwrap(),
                    "--confirm-create"
                ]
            )["created"],
            true
        );
        fs::write(staged.join("launcher.asset"), b"pinned launcher asset").unwrap();
        let archive_name = if cfg!(windows) {
            "agentlaw-x86_64-pc-windows-msvc.zip"
        } else if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                "agentlaw-aarch64-apple-darwin.tar.gz"
            } else {
                "agentlaw-x86_64-apple-darwin.tar.gz"
            }
        } else {
            "agentlaw-x86_64-unknown-linux-gnu.tar.gz"
        };
        let archive = staged.join(archive_name);
        fs::write(
            &archive,
            b"isolated bundle fixture; archive digest is pinned",
        )
        .unwrap();
        let hashes = json!({
            binary_name(): hash(&unpacked.join(binary_name())),
            worker_name(): hash(&unpacked.join(worker_name())),
            "LICENSE.agentlaw": hash(&unpacked.join("LICENSE.agentlaw"))
        });
        let plan = json!({
            "id":id,"root":root,"tag":format!("v{}",env!("CARGO_PKG_VERSION")),
            "archive":archive_name,
            "archive_digest":hash(&archive),"bin_before":hash(&old),
            "bin_before_hashes":{
                binary_name():hash(&old),worker_name():hash(&bin.join(worker_name())),
                "LICENSE.agentlaw":hash(&bin.join("LICENSE.agentlaw"))
            },
            "registrations":[{"harness":"codex","directory":host,
                "config_path":config,"instructions_path":instructions,"receipt_path":receipt,
                "config_before":fs::read_to_string(&config).unwrap(),
                "instructions_before":fs::read_to_string(&instructions).unwrap(),
                "receipt_before":fs::read_to_string(&receipt).unwrap()}],
            "phase":"prepared","launcher_digest":hash(&staged.join("launcher.asset")),"bundle_hashes":hashes
        });
        fs::create_dir_all(state.join("update-plans")).unwrap();
        fs::write(
            state.join("update-plans").join(format!("{id}.json")),
            serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            state,
            host,
            helper,
            id,
        }
    }
    fn apply(&self) -> Value {
        run(
            &self.helper,
            &self.state,
            &[
                "update",
                "apply",
                &self.id,
                "--root",
                self.root.to_str().unwrap(),
            ],
        )
    }
}

#[test]
fn candidate_finishes_replacement_probe_and_exact_cleanup_before_restart() {
    let fixture = Fixture::new();
    let memory_format = fixture.root.parent().unwrap().join("memory/format.md");
    let original_memory = fs::read(&memory_format).unwrap();
    let previous = fixture.root.join(format!(".bin-previous-{}", fixture.id));
    let stage = fixture.root.join(format!(".update-{}", fixture.id));
    let complete = fixture.apply();
    assert_eq!(complete["status"], "completed", "{complete}");
    assert_eq!(complete["bundle"], "installed_and_verified");
    assert_eq!(complete["registrations"], "verified");
    assert_eq!(complete["cleanup"], "completed");
    assert_eq!(complete["recovery_obligations_closed"], true);
    assert!(!previous.exists());
    assert!(!stage.exists());
    assert!(!fixture.state.join("update-maintenance.json").exists());
    assert_eq!(fs::read(memory_format).unwrap(), original_memory);
    assert!(complete["activation"] == "verified");
}

#[test]
fn maintenance_drains_an_open_mcp_before_replacing_bundle() {
    let fixture = Fixture::new();
    let mut active = McpClient::new(&fixture.helper, &fixture.state);
    let recall = active
        .call(json!({"action":"recall","recall":{"recall_for":"verify managed update drain"}}));
    assert_eq!(recall["isError"], false, "{recall}");
    let complete = fixture.apply();
    assert_eq!(complete["status"], "completed", "{complete}");
    assert!(
        active.child.try_wait().unwrap().is_some(),
        "old MCP stayed alive after the update"
    );
}

#[test]
fn completion_ignores_unrelated_host_edits_and_resumes_without_restarting_mcp() {
    let fixture = Fixture::new();
    assert_eq!(fixture.apply()["status"], "completed");
    let config = fixture.host.join("config.toml");
    let instructions = fixture.host.join("AGENTS.md");
    let original_instructions = fs::read_to_string(&instructions).unwrap();
    fs::write(
        &config,
        format!(
            "{}\n[mcp_servers.unrelated]\ncommand = 'other'\n",
            fs::read_to_string(&config).unwrap()
        ),
    )
    .unwrap();
    fs::write(
        &instructions,
        format!("User instructions outside Agentlaw\n{original_instructions}"),
    )
    .unwrap();
    let status = || {
        run(
            &fixture.helper,
            &fixture.state,
            &[
                "update",
                "status",
                &fixture.id,
                "--root",
                fixture.root.to_str().unwrap(),
            ],
        )
    };
    assert_eq!(status()["status"], "completed");

    let path = fixture
        .state
        .join("update-plans")
        .join(format!("{}.json", fixture.id));
    let mut plan: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    plan["cleanup_completed"] = json!(false);
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    assert_eq!(status()["status"], "incomplete");

    plan["cleanup_completed"] = json!(true);
    plan["phase"] = json!("finalizing");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    let runtime = fixture.root.join("bin").join(binary_name());
    let preview = run(&runtime, &fixture.state, &["update"]);
    assert_eq!(preview["status"], "confirmation_required", "{preview}");
    assert_eq!(preview["plan_id"], fixture.id);
    let handoff = run(
        &runtime,
        &fixture.state,
        &["update", "--confirm-update", &fixture.id],
    );
    assert_eq!(handoff["status"], "handoff_ready", "{handoff}");
    assert_eq!(handoff["plan_id"], fixture.id);
    assert_eq!(handoff["candidate"], fixture.helper.to_str().unwrap());
    let conflict_id = uuid::Uuid::new_v4().to_string();
    let mut conflict = plan.clone();
    conflict["id"] = json!(conflict_id);
    let conflict_path = fixture
        .state
        .join("update-plans")
        .join(format!("{conflict_id}.json"));
    fs::write(&conflict_path, serde_json::to_vec(&conflict).unwrap()).unwrap();
    let ambiguous = Command::new(&runtime)
        .arg("update")
        .env("AGENTLAW_HOME", &fixture.state)
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());
    let blocker: Value = serde_json::from_slice(&ambiguous.stdout).unwrap();
    assert_eq!(blocker["code"], "update_recovery", "{blocker}");
    fs::remove_file(&conflict_path).unwrap();
    assert_eq!(
        fs::read_dir(fixture.state.join("update-plans"))
            .unwrap()
            .count(),
        1
    );
    let mut active = McpClient::new(&fixture.helper, &fixture.state);
    assert_eq!(fixture.apply()["status"], "completed");
    assert!(active.child.try_wait().unwrap().is_none());
    fs::write(
        &instructions,
        fs::read_to_string(&instructions).unwrap().replace(
            "Use Agentlaw without waiting",
            "Disable Agentlaw without waiting",
        ),
    )
    .unwrap();
    assert_eq!(status()["status"], "incomplete");
}

#[test]
fn deferred_cleanup_resumes_the_same_finalizing_plan() {
    let fixture = Fixture::new();
    let worker = fixture.state.join("worker");
    fs::create_dir_all(&worker).unwrap();
    let guard = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(worker.join("model.lock"))
        .unwrap();
    guard.lock_exclusive().unwrap();
    let output = Command::new(&fixture.helper)
        .args([
            "update",
            "apply",
            &fixture.id,
            "--root",
            fixture.root.to_str().unwrap(),
        ])
        .env("AGENTLAW_HOME", &fixture.state)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["code"], "cleanup_incomplete", "{result}");
    let plan_path = fixture
        .state
        .join("update-plans")
        .join(format!("{}.json", fixture.id));
    let plan: Value = serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    assert_eq!(plan["phase"], "finalizing");
    assert!(plan["cleanup_plan_id"].is_string());
    assert_eq!(plan["cleanup_completed"], false);
    drop(guard);
    fs::remove_file(fixture.state.join("update-maintenance.json")).unwrap();
    let runtime = fixture.root.join("bin").join(binary_name());
    let preview = run(&runtime, &fixture.state, &["update"]);
    assert_eq!(preview["plan_id"], fixture.id);
    assert_eq!(preview["status"], "confirmation_required");
    let handoff = run(
        &runtime,
        &fixture.state,
        &["update", "--confirm-update", &fixture.id],
    );
    assert_eq!(handoff["status"], "handoff_ready");
    assert_eq!(fixture.apply()["status"], "completed");
}

#[test]
#[ignore = "requires AGENTLAW_TEST_STABLE_LAUNCHER pointing to a released launcher"]
fn released_stable_launcher_resumes_finalization_without_a_gate() {
    let stable = PathBuf::from(std::env::var("AGENTLAW_TEST_STABLE_LAUNCHER").unwrap());
    let fixture = Fixture::new();
    assert_eq!(fixture.apply()["status"], "completed");
    let path = fixture
        .state
        .join("update-plans")
        .join(format!("{}.json", fixture.id));
    let mut plan: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    plan["phase"] = json!("finalizing");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    assert!(!fixture.state.join("update-maintenance.json").exists());
    assert!(!fixture
        .root
        .join(format!(".update-{}", fixture.id))
        .exists());
    let command = fixture.root.join("command");
    fs::create_dir_all(&command).unwrap();
    let launcher = command.join(binary_name());
    fs::copy(stable, &launcher).unwrap();
    let before = hash(&launcher);
    let result = run(&launcher, &fixture.state, &["update"]);
    assert_eq!(result["status"], "installed", "{result}");
    assert_eq!(result["plan_id"], fixture.id);
    assert_eq!(hash(&launcher), before);
    assert_eq!(
        fs::read_dir(fixture.state.join("update-plans"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
#[ignore = "requires AGENTLAW_TEST_STABLE_LAUNCHER pointing to a released launcher"]
fn released_stable_launcher_rejects_a_changed_candidate_hash_before_apply() {
    let stable = PathBuf::from(std::env::var_os("AGENTLAW_TEST_STABLE_LAUNCHER").unwrap());
    let stable_before = hash(&stable);
    let fixture = Fixture::new();
    let command = fixture.root.join("command");
    fs::create_dir_all(&command).unwrap();
    let launcher = command.join(binary_name());
    fs::copy(&stable, &launcher).unwrap();

    // The prepared fixture follows the real offline handoff path and creates
    // its actual maintenance marker; only that owned marker is then damaged.
    let runtime = fixture.root.join("bin").join(binary_name());
    let handoff = run(
        &runtime,
        &fixture.state,
        &["update", "--confirm-update", &fixture.id],
    );
    assert_eq!(handoff["status"], "handoff_ready", "{handoff}");
    assert_eq!(handoff["plan_id"], fixture.id);
    let marker_path = fixture.state.join("update-maintenance.json");
    let mut marker: Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    assert_eq!(marker["plan_id"], handoff["plan_id"]);
    assert_eq!(marker["candidate"], handoff["candidate"]);
    assert_eq!(marker["candidate_sha256"], handoff["candidate_sha256"]);
    let candidate = PathBuf::from(marker["candidate"].as_str().unwrap());
    let candidate_before = hash(&candidate);
    assert_eq!(marker["candidate_sha256"], candidate_before);
    let mut wrong_hash = candidate_before.clone();
    wrong_hash.replace_range(
        0..1,
        if wrong_hash.starts_with('0') {
            "1"
        } else {
            "0"
        },
    );
    marker["candidate_sha256"] = json!(wrong_hash);
    fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    let before = snapshot(&fixture.root);

    let output = static_output(&launcher, &fixture.state, &fixture.root, &["update"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = cli_error(&output);
    assert_eq!(error["code"], "update_incomplete", "{error}");
    assert_eq!(
        error["message"], "The update candidate differs from the approved handoff.",
        "{error}"
    );
    assert_ne!(error["status"], "installed", "{error}");
    assert!(error.get("next_action").is_none(), "{error}");
    assert!(!String::from_utf8_lossy(&output.stdout)
        .to_ascii_lowercase()
        .contains("restart"));
    assert_eq!(snapshot(&fixture.root), before);
    assert_eq!(hash(&candidate), candidate_before);
    assert_eq!(hash(&launcher), stable_before);
    assert_eq!(hash(&stable), stable_before);
}

#[test]
fn runtime_update_status_requires_exact_plan_before_state_access() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    fs::write(&state, b"This state path must remain a file.\n").unwrap();
    let before = snapshot(temp.path());
    let binary = Path::new(env!("CARGO_BIN_EXE_agentlaw"));
    for args in [
        vec!["update", "status"],
        vec!["update", "status", "--root", temp.path().to_str().unwrap()],
    ] {
        let output = static_output(binary, &state, temp.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert_eq!(cli_error(&output)["code"], "invalid_arguments");
        assert_eq!(snapshot(temp.path()), before, "{args:?}");
    }
}

#[test]
fn runtime_update_hidden_legacy_wires_reach_unmanaged_domain_guard() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("unmanaged root");
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("config.json"), b"not valid configuration").unwrap();
    let before = snapshot(temp.path());
    let binary = Path::new(env!("CARGO_BIN_EXE_agentlaw"));
    let plan = "299d6f05-ac1c-477d-9c0a-62b1ffb87c78";
    for args in [
        vec!["update", "--confirm-update", plan],
        vec!["update", "apply", plan, "--root", root.to_str().unwrap()],
    ] {
        let output = static_output(binary, &state, temp.path(), &args);
        assert_eq!(cli_error(&output)["code"], "update_unmanaged", "{output:?}");
        assert_eq!(snapshot(temp.path()), before, "{args:?}");
    }
}

#[test]
fn source_runtime_update_rejects_unmanaged_and_foreign_managed_roots() {
    let binary = Path::new(env!("CARGO_BIN_EXE_agentlaw"));
    for managed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("isolated root");
        let state = root.join("state");
        fs::create_dir_all(state.join("update-plans")).unwrap();
        fs::write(state.join("config.json"), b"must not be loaded").unwrap();
        fs::write(
            state.join("update-plans/unreadable-plan.json"),
            b"must not be scanned",
        )
        .unwrap();
        if managed {
            fs::write(
                root.join(".agentlaw-layout"),
                b"agentlaw-managed-layout-v1\n",
            )
            .unwrap();
        }
        let before = snapshot(temp.path());
        let output = static_output(binary, &state, temp.path(), &["update"]);
        assert_eq!(cli_error(&output)["code"], "update_unmanaged", "{output:?}");
        assert_eq!(snapshot(temp.path()), before);
    }
}

#[test]
#[ignore = "requires AGENTLAW_TEST_STABLE_LAUNCHER pointing to a released launcher"]
fn released_stable_launcher_forwards_context_help_and_describe_without_state_effects() {
    // Read the supplied release only to copy/hash it; execute the isolated copy.
    let stable = PathBuf::from(std::env::var_os("AGENTLAW_TEST_STABLE_LAUNCHER").unwrap());
    let stable_before = hash(&stable);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("managed root with spaces");
    let state = root.join("state");
    let bin = root.join("bin");
    let command = root.join("command");
    for directory in [&state, &bin, &command] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    fs::write(state.join("config.json"), b"invalid configuration").unwrap();
    fs::write(state.join("machine.json"), b"invalid machine identity").unwrap();
    fs::write(state.join("update-maintenance.json"), b"invalid gate").unwrap();
    let runtime = bin.join(binary_name());
    fs::copy(env!("CARGO_BIN_EXE_agentlaw"), &runtime).unwrap();
    let launcher = command.join(binary_name());
    fs::copy(&stable, &launcher).unwrap();
    let before = snapshot(temp.path());
    let state_before = snapshot(&state);
    for (args, path, is_json) in [
        (
            &["sync", "resolve", "--help"][..],
            &["sync", "resolve"][..],
            false,
        ),
        (&["help", "update"][..], &["update"][..], false),
        (
            &["share", "import", "resolve", "-h"][..],
            &["share", "import", "resolve"][..],
            false,
        ),
        (&["describe", "update"][..], &["update"][..], true),
        (
            &["describe", "sync", "resolve"][..],
            &["sync", "resolve"][..],
            true,
        ),
        (
            &["help", "sync", "resolve", "--format", "json"][..],
            &["sync", "resolve"][..],
            true,
        ),
    ] {
        let direct = static_output(&runtime, &state, temp.path(), args);
        let forwarded = static_output(&launcher, &state, temp.path(), args);
        assert!(direct.status.success(), "{args:?}: {direct:?}");
        assert!(forwarded.status.success(), "{args:?}: {forwarded:?}");
        assert!(direct.stderr.is_empty(), "{direct:?}");
        assert!(forwarded.stderr.is_empty(), "{forwarded:?}");
        assert_eq!(forwarded.stdout, direct.stdout, "{args:?}");
        if is_json {
            let value: Value = serde_json::from_slice(&forwarded.stdout).unwrap();
            assert_eq!(value["name"], "agentlaw", "{value}");
            assert_eq!(value["path"], json!(path), "{value}");
            assert!(value["options"].is_array(), "{value}");
        } else {
            let text = std::str::from_utf8(&forwarded.stdout).unwrap();
            let usage = text.lines().find(|line| line.contains("Usage:")).unwrap();
            assert!(usage.contains(&format!("agentlaw {}", path.join(" "))));
        }
        assert_eq!(snapshot(&state), state_before, "{args:?}");
    }
    assert_eq!(snapshot(temp.path()), before);
    assert_eq!(hash(&launcher), stable_before);
    assert_eq!(hash(&stable), stable_before);
}

#[test]
fn harness_drift_stops_before_publication_and_reopens_old_startup() {
    let fixture = Fixture::new();
    let before = hash(&fixture.root.join("bin").join(binary_name()));
    let instructions = fixture.host.join("AGENTS.md");
    fs::write(&instructions, "user changed this instruction file\n").unwrap();
    let output = Command::new(&fixture.helper)
        .args([
            "update",
            "apply",
            &fixture.id,
            "--root",
            fixture.root.to_str().unwrap(),
        ])
        .env("AGENTLAW_HOME", &fixture.state)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["code"], "update_drift", "{result}");
    assert_eq!(hash(&fixture.root.join("bin").join(binary_name())), before);
    assert!(!fixture
        .root
        .join(format!(".bin-previous-{}", fixture.id))
        .exists());
    assert!(!fixture.state.join("update-maintenance.json").exists());
}
