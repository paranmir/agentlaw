//! Managed update subprocess tests use only temporary roots and owned processes.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::Duration,
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
