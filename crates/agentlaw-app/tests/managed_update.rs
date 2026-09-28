//! Managed update subprocess tests use only temporary roots and owned processes.
#[path = "../../../tests/support/owned_process.rs"]
mod owned_process;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
    let output = Command::new(binary)
        .args(args)
        .env("AGENTLAW_HOME", state)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("Invalid CLI output: {:?}", output));
    assert!(output.status.success(), "{value}");
    value
}

fn mcp_recall(binary: &Path, state: &Path) -> Vec<Value> {
    let mut child = Command::new(binary)
        .args(["mcp", "serve", "--stdio"])
        .env("AGENTLAW_HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"managed-update-test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentlaw",
            "arguments":{"action":"recall","recall":{"recall_for":"general preferences"}}}}),
    ] {
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    host: PathBuf,
    helper: PathBuf,
    id: String,
    receipt: PathBuf,
}
impl Fixture {
    fn new() -> Self {
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
        fs::copy(source, &old).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&old)
            .unwrap()
            .write_all(b"previous-version-overlay")
            .unwrap();
        #[cfg(target_os = "macos")]
        assert!(Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&old)
            .status()
            .unwrap()
            .success());
        fs::write(bin.join(worker_name()), b"old-worker").unwrap();
        fs::write(bin.join("LICENSE.agentlaw"), b"old-license").unwrap();
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
        let helper_dir = staged.join("helper");
        fs::create_dir_all(&unpacked).unwrap();
        fs::create_dir_all(&helper_dir).unwrap();
        fs::copy(source, unpacked.join(binary_name())).unwrap();
        fs::write(unpacked.join(worker_name()), b"new-worker").unwrap();
        fs::write(unpacked.join("LICENSE.agentlaw"), b"new-license").unwrap();
        let helper = helper_dir.join(binary_name());
        fs::copy(source, &helper).unwrap();
        let hashes = json!({
            binary_name(): hash(&unpacked.join(binary_name())),
            worker_name(): hash(&unpacked.join(worker_name())),
            "LICENSE.agentlaw": hash(&unpacked.join("LICENSE.agentlaw"))
        });
        let plan = json!({
            "id":id,"root":root,"tag":format!("v{}",env!("CARGO_PKG_VERSION")),
            "archive": if cfg!(windows) { "agentlaw-x86_64-pc-windows-msvc.zip" } else if cfg!(target_os="macos") {
                if cfg!(target_arch="aarch64") { "agentlaw-aarch64-apple-darwin.tar.gz" } else { "agentlaw-x86_64-apple-darwin.tar.gz" }
            } else { "agentlaw-x86_64-unknown-linux-gnu.tar.gz" },
            "archive_digest":"a".repeat(64),"bin_before":hash(&old),
            "bin_before_hashes":{
                binary_name():hash(&old),worker_name():hash(&bin.join(worker_name())),
                "LICENSE.agentlaw":hash(&bin.join("LICENSE.agentlaw"))
            },
            "registrations":[{"harness":"codex","directory":host,
                "config_path":config,"instructions_path":instructions,"receipt_path":receipt,
                "config_before":fs::read_to_string(&config).unwrap(),
                "instructions_before":fs::read_to_string(&instructions).unwrap(),
                "receipt_before":fs::read_to_string(&receipt).unwrap()}],
            "phase":"prepared","helper_digest":hash(&helper),"bundle_hashes":hashes
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
            receipt,
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
    fn plan_path(&self) -> PathBuf {
        self.state
            .join("update-plans")
            .join(format!("{}.json", self.id))
    }
    fn set_phase(&self, phase: &str) {
        let path = self.plan_path();
        let mut plan: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        plan["phase"] = json!(phase);
        fs::write(path, serde_json::to_vec(&plan).unwrap()).unwrap();
    }
}

#[test]
fn updater_waits_for_owned_broker_then_refreshes_pinned_harness_and_preserves_state() {
    let fixture = Fixture::new();
    let before_receipt: Value =
        serde_json::from_slice(&fs::read(&fixture.receipt).unwrap()).unwrap();
    let machine_before = fs::read(fixture.state.join("machine.json")).unwrap();
    let sentinel = fixture.state.join("pending-user-data");
    fs::write(&sentinel, b"unchanged memory and model state").unwrap();
    fs::create_dir_all(fixture.root.join("models")).unwrap();
    fs::write(fixture.root.join("models/test-model"), b"model sentinel").unwrap();
    let worker_state = fixture.state.join("owned-worker");
    let broker = Command::new(fixture.root.join("bin").join(binary_name()))
        .args([
            "worker-daemon",
            "--state-dir",
            worker_state.to_str().unwrap(),
        ])
        .env("AGENTLAW_HOME", &fixture.state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let broker = owned_process::OwnedProcess::new(broker);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !worker_state.join("endpoint.json").is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(worker_state.join("endpoint.json").is_file());
    let blocked = fixture.apply();
    assert_eq!(blocked["status"], "incomplete");
    assert_eq!(blocked["inspection"]["status"], "pending_exit");
    drop(broker);
    let complete = fixture.apply();
    assert_eq!(complete["status"], "completed");
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        b"unchanged memory and model state"
    );
    assert_eq!(
        fs::read(fixture.state.join("machine.json")).unwrap(),
        machine_before
    );
    assert_eq!(
        fs::read(fixture.root.join("models/test-model")).unwrap(),
        b"model sentinel"
    );
    let after_receipt: Value =
        serde_json::from_slice(&fs::read(&fixture.receipt).unwrap()).unwrap();
    assert_ne!(before_receipt["executable"], after_receipt["executable"]);
    assert_eq!(
        fs::read(fixture.root.join("bin").join(worker_name())).unwrap(),
        b"new-worker"
    );
    assert!(fixture.root.join(".bin-previous").is_dir());
    let pinned = PathBuf::from(after_receipt["executable"].as_str().unwrap());
    assert!(Command::new(&pinned)
        .arg("--version")
        .output()
        .unwrap()
        .status
        .success());
    assert!(fixture.host.join("AGENTS.md").is_file());
    let worker_state = fixture.state.join("worker");
    let worker = Command::new(fixture.root.join("bin").join(binary_name()))
        .args([
            "worker-daemon",
            "--state-dir",
            worker_state.to_str().unwrap(),
        ])
        .env("AGENTLAW_HOME", &fixture.state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let worker = owned_process::OwnedProcess::new(worker);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !worker_state.join("endpoint.json").is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(worker_state.join("endpoint.json").is_file());
    let memory = fixture.root.join("memory");
    assert_eq!(
        run(
            &fixture.root.join("bin").join(binary_name()),
            &fixture.state,
            &[
                "store",
                "create",
                "--path",
                memory.to_str().unwrap(),
                "--confirm-create"
            ],
        )["created"],
        true
    );
    let verified_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        fixture.state.join("update-check.json"),
        serde_json::to_vec(
            &json!({"last_success":verified_at,"retry_after":null,"latest_tag":"v99.0.0"}),
        )
        .unwrap(),
    )
    .unwrap();
    for binary in [&fixture.root.join("bin").join(binary_name()), &pinned] {
        let replies = mcp_recall(binary, &fixture.state);
        assert_eq!(
            replies[0]["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        let recall = &replies[1]["result"];
        assert_eq!(recall["isError"], false, "{recall}");
        assert!(
            recall["structuredContent"].get("memories").is_some(),
            "{recall}"
        );
        assert_eq!(
            recall["structuredContent"]["update_notice"]["latest_version"],
            "v99.0.0"
        );
        let text: Value =
            serde_json::from_str(recall["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, recall["structuredContent"]);
    }
    drop(worker);
}

#[test]
fn interrupted_bundle_move_and_publish_resume_the_same_plan() {
    for phase in ["moving_old_bundle", "publishing_new_bundle"] {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.root.join("memory")).unwrap();
        fs::write(fixture.root.join("memory/source.md"), b"memory sentinel").unwrap();
        fs::rename(fixture.root.join("bin"), fixture.root.join(".bin-previous")).unwrap();
        if phase == "publishing_new_bundle" {
            fs::rename(
                fixture
                    .root
                    .join(format!(".update-{}", fixture.id))
                    .join("unpacked"),
                fixture.root.join("bin"),
            )
            .unwrap();
        }
        fixture.set_phase(phase);
        let result = fixture.apply();
        assert_eq!(result["status"], "completed", "phase={phase}: {result}");
        let receipt: Value = serde_json::from_slice(&fs::read(&fixture.receipt).unwrap()).unwrap();
        assert!(Path::new(receipt["executable"].as_str().unwrap()).is_file());
        assert!(fixture.root.join(".bin-previous").is_dir());
        assert_eq!(
            fs::read(fixture.root.join("memory/source.md")).unwrap(),
            b"memory sentinel"
        );
    }
}

#[test]
fn external_harness_edit_after_preview_stops_before_swap() {
    let fixture = Fixture::new();
    let config = fixture.host.join("config.toml");
    fs::OpenOptions::new()
        .append(true)
        .open(&config)
        .unwrap()
        .write_all(b"\n# user changed this registration\n")
        .unwrap();
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
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["code"], "update_drift");
    assert!(fixture.root.join("bin").is_dir());
    assert!(!fixture.root.join(".bin-previous").exists());
}

#[test]
fn interrupted_registration_journal_recovers_only_pinned_targets() {
    let fixture = Fixture::new();
    let plan_path = fixture.plan_path();
    let mut plan: Value = serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    let old = plan["registrations"][0].clone();
    let config = fixture.host.join("config.toml");
    let instructions = fixture.host.join("AGENTS.md");
    let installed = run(
        &fixture.helper,
        &fixture.state,
        &[
            "install",
            "--harness",
            "codex",
            "--harness-dir",
            fixture.host.to_str().unwrap(),
            "--confirm-install",
        ],
    );
    assert_eq!(installed["status"], "installed");
    let after_config = fs::read_to_string(&config).unwrap();
    let after_instructions = fs::read_to_string(&instructions).unwrap();
    let after_receipt = fs::read_to_string(&fixture.receipt).unwrap();
    fs::write(&config, old["config_before"].as_str().unwrap()).unwrap();
    fs::write(&instructions, old["instructions_before"].as_str().unwrap()).unwrap();
    fs::write(&fixture.receipt, old["receipt_before"].as_str().unwrap()).unwrap();
    plan["registrations"][0]["config_after"] = json!(after_config.clone());
    plan["registrations"][0]["instructions_after"] = json!(after_instructions.clone());
    plan["registrations"][0]["receipt_after"] = json!(after_receipt.clone());
    plan["phase"] = json!("refreshing_registrations");
    fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
    let journal = json!({"targets":[
        {"path":config,"before":old["config_before"],"after":after_config},
        {"path":instructions,"before":old["instructions_before"],"after":after_instructions},
        {"path":fixture.receipt,"before":old["receipt_before"],"after":after_receipt}
    ]});
    fs::write(
        fixture.state.join("install-pending.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    fs::rename(fixture.root.join("bin"), fixture.root.join(".bin-previous")).unwrap();
    fs::rename(
        fixture
            .root
            .join(format!(".update-{}", fixture.id))
            .join("unpacked"),
        fixture.root.join("bin"),
    )
    .unwrap();
    let result = fixture.apply();
    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(fs::read_to_string(&fixture.receipt).unwrap(), after_receipt);
    assert!(!fixture.state.join("install-pending.json").exists());
}
