//! Actual CLI and MCP subprocesses, not a mocked backend.
#[path = "../../../tests/support/owned_process.rs"]
mod owned_process;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Output, Stdio},
    time::Duration,
};

fn executable() -> &'static str {
    env!("CARGO_BIN_EXE_agentlaw")
}
fn command(state: &Path) -> Command {
    let mut cmd = Command::new(executable());
    cmd.env("AGENTLAW_HOME", state);
    for name in [
        "AGENTLAW_ONNX_MODEL",
        "AGENTLAW_TOKENIZER",
        "AGENTLAW_ORT_LIBRARY",
    ] {
        cmd.env_remove(name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd
}
fn run(state: &Path, args: &[&str], input: Option<Value>) -> (Output, Value) {
    let mut cmd = command(state);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(mut input) = input {
        if args == ["call", "--json", "-"] {
            if let Some(action) = input["action"].as_str().map(str::to_owned) {
                if input.get(&action).is_none() {
                    let mut fields = input.as_object().unwrap().clone();
                    fields.remove("action");
                    input = json!({"action":action});
                    input
                        .as_object_mut()
                        .unwrap()
                        .insert(action, Value::Object(fields));
                }
            }
        }
        write!(child.stdin.take().unwrap(), "{input}").unwrap();
    } else {
        drop(child.stdin.take());
    }
    // Drain both pipes while waiting: a full pipe must not masquerade as a
    // product hang. Bound the test and terminate only the command it owns.
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut child = Daemon::new(child);
    let (output_tx, output_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = output_tx.send(bytes);
    });
    let (error_tx, error_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        let _ = error_tx.send(bytes);
    });
    let until = std::time::Instant::now() + Duration::from_secs(180);
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break (status, false);
        }
        if std::time::Instant::now() >= until {
            child.stop();
            break (child.wait().unwrap(), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(child);
    let out = Output {
        status,
        stdout: output_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("CLI stdout did not close"),
        stderr: error_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("CLI stderr did not close"),
    };
    assert!(
        !timed_out,
        "Owned CLI command timed out: {args:?}; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("Invalid JSON output: {:?}", out));
    (out, value)
}
use owned_process::OwnedProcess as Daemon;
fn initialize(state: &Path, source: &Path) {
    let (out, value) = run(
        state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
        None,
    );
    assert!(out.status.success(), "{value}");
    assert_eq!(value["created"], true);
    assert!(source.join("format.md").is_file());
}

struct RuntimeCleanup(std::path::PathBuf);
impl Drop for RuntimeCleanup {
    fn drop(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(endpoint) = agentlaw_runtime::transport::read_endpoint(&self.0) {
                if let Ok(client) =
                    agentlaw_runtime::client::Client::connect(&self.0, &endpoint.installation_build)
                {
                    let _ = client.shutdown("test cleanup");
                }
            }
            if !agentlaw_runtime::installation::owner::owner_alive(&self.0).unwrap_or(false) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

struct McpFixture {
    _child: Daemon,
    input: std::process::ChildStdin,
    replies: std::sync::mpsc::Receiver<Value>,
}
impl McpFixture {
    fn start(state: &Path) -> Self {
        use std::io::BufRead;
        let mut child = command(state)
            .args(["mcp", "serve", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let child = Daemon::new(child);
        let (tx, replies) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            _child: child,
            input,
            replies,
        }
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn reply(&self) -> Value {
        self.replies
            .recv_timeout(Duration::from_secs(30))
            .expect("MCP response deadline")
    }
    fn initialize(&mut self) {
        self.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}));
        assert!(self.reply().get("result").is_some());
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    }
    fn call(&mut self, id: u64, arguments: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"agentlaw","arguments":arguments}}));
        self.reply()
    }
}

#[test]
fn cli_call_starts_runtime_and_returns_recall() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    initialize(&state, &temp.path().join("store"));
    let _cleanup = RuntimeCleanup(state.clone());
    assert!(!state.join("service/endpoint.json").exists());
    let (out, result) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({"action":"recall","recall":{"recall_for":"startup fixture"}})),
    );
    assert!(out.status.success(), "{result}");
    assert!(state.join("service/endpoint.json").exists());
    assert!(agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
}

#[test]
fn two_mcp_frontends_share_one_runtime_and_memory() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    initialize(&state, &temp.path().join("store"));
    let _cleanup = RuntimeCleanup(state.clone());
    let mut first = McpFixture::start(&state);
    let mut second = McpFixture::start(&state);
    first.initialize();
    second.initialize();
    let saved = first.call(2, json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"shared runtime marker","evidence":"fixture","applies_to":["user"]}]}}));
    assert_eq!(saved["result"]["isError"], false, "{saved}");
    let id = saved["result"]["structuredContent"]["results"][0]["memory_ref"]["memory_id"].clone();
    assert!(id.is_string(), "{saved}");
    let endpoint = agentlaw_runtime::transport::read_endpoint(&state).unwrap();
    let recalled = second.call(2, json!({"action":"recall","recall":{"memory_ids":[id]}}));
    assert_eq!(
        recalled["result"]["structuredContent"]["memories"][0]["current_heads"][0]
            ["what_to_remember"],
        "shared runtime marker",
        "{recalled}"
    );
    assert_eq!(
        agentlaw_runtime::transport::read_endpoint(&state)
            .unwrap()
            .pid,
        endpoint.pid
    );
    assert!(agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
    assert!(!state.join("worker/endpoint.json").exists());
}

#[test]
fn discovery_responds_before_runtime_is_ready_and_first_call_waits() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    initialize(&state, &temp.path().join("store"));
    let _cleanup = RuntimeCleanup(state.clone());
    let lock = agentlaw_runtime::installation::owner::OwnerLock::try_acquire(&state)
        .unwrap()
        .unwrap();
    let mut mcp = McpFixture::start(&state);
    let start = std::time::Instant::now();
    mcp.initialize();
    mcp.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let list = mcp
        .replies
        .recv_timeout(Duration::from_secs(3))
        .expect("tools/list waited for runtime");
    assert!(list["result"]["tools"].is_array(), "{list}");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(!state.join("service/endpoint.json").exists());
    drop(lock);
    let result = mcp.call(
        3,
        json!({"action":"recall","recall":{"recall_for":"startup fixture"}}),
    );
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert!(state.join("service/endpoint.json").exists());
}

#[test]
fn reconnects_within_same_mcp_call_after_owned_host_is_killed() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    initialize(&state, &temp.path().join("store"));
    let _cleanup = RuntimeCleanup(state.clone());
    let mut host = Daemon::new(
        command(&state)
            .arg("runtime-host")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !state.join("service/endpoint.json").exists() {
        assert!(host.is_running());
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut mcp = McpFixture::start(&state);
    mcp.initialize();
    let input = json!({"action":"recall","recall":{"recall_for":"reconnect fixture"}});
    let first = mcp.call(2, input.clone());
    assert_eq!(first["result"]["isError"], false, "{first}");
    let old = agentlaw_runtime::transport::read_endpoint(&state).unwrap();
    assert_eq!(old.pid, host.id());
    host.stop();
    let next = mcp.call(3, input);
    assert_eq!(next["result"]["isError"], false, "{next}");
    let new = agentlaw_runtime::transport::read_endpoint(&state).unwrap();
    assert_ne!(new.pid, old.pid);
}

#[test]
fn disconnect_after_mutation_body_helper() {
    let Some(state) = std::env::var_os("AGENTLAW_MUTATION_FIXTURE_STATE") else {
        return;
    };
    use agentlaw_runtime::{protocol::*, transport};
    use std::net::TcpListener;
    let state = std::path::PathBuf::from(state);
    let _lock = agentlaw_runtime::installation::owner::OwnerLock::try_acquire(&state)
        .unwrap()
        .unwrap();
    let control = TcpListener::bind("127.0.0.1:0").unwrap();
    let data = TcpListener::bind("127.0.0.1:0").unwrap();
    let build = agentlaw_runtime::installation::build_identity(Path::new(executable())).unwrap();
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let key =
        agentlaw_storage::binding_identity(&config.memory_store_path, &config.runtime_root(&state))
            .unwrap();
    let binding = BindingRef::new(&key, &config.user_id);
    transport::write_endpoint(
        &state,
        &transport::Endpoint {
            protocol: PROTOCOL_VERSION,
            installation_build: build,
            control: control.local_addr().unwrap(),
            data: data.local_addr().unwrap(),
            secret: "a".repeat(64),
            pid: std::process::id(),
        },
    )
    .unwrap();
    std::thread::spawn(move || {
        for stream in control.incoming() {
            let Ok(mut stream) = stream else { break };
            let Ok::<Hello, _>(_) = transport::receive(&mut stream, MAX_CONTROL_FRAME_BYTES) else {
                continue;
            };
            if transport::send(&mut stream, &HelloReply::Accepted, MAX_CONTROL_REPLY_BYTES).is_err()
            {
                continue;
            }
            let reply =
                match transport::receive::<ControlRequest>(&mut stream, MAX_CONTROL_FRAME_BYTES) {
                    Ok(ControlRequest::Lease { .. }) => ControlReply::LeaseRenewed,
                    Ok(ControlRequest::ResolveBinding) => ControlReply::Binding(binding),
                    _ => continue,
                };
            let _ = transport::send(&mut stream, &reply, MAX_CONTROL_REPLY_BYTES);
        }
    });
    let (mut stream, _) = data.accept().unwrap();
    let _: Hello = transport::receive(&mut stream, MAX_CONTROL_FRAME_BYTES).unwrap();
    transport::send(&mut stream, &HelloReply::Accepted, MAX_CONTROL_REPLY_BYTES).unwrap();
    let header: CallHeader = transport::receive(&mut stream, MAX_CONTROL_FRAME_BYTES).unwrap();
    assert_eq!(header.kind, RequestKind::Command);
    transport::send(
        &mut stream,
        &DataFrame::Proceed { id: header.id },
        MAX_CONTROL_REPLY_BYTES,
    )
    .unwrap();
    let mut body = vec![0; header.body_len as usize];
    stream.read_exact(&mut body).unwrap();
    std::fs::write(state.join("received-bodies"), b"1").unwrap();
    std::process::exit(0);
}

#[test]
fn mcp_does_not_resend_mutation_after_body_disconnect() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    initialize(&state, &temp.path().join("store"));
    let _cleanup = RuntimeCleanup(state.clone());
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "disconnect_after_mutation_body_helper",
            "--nocapture",
        ])
        .env("AGENTLAW_MUTATION_FIXTURE_STATE", &state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut helper = Daemon::new(child);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !state.join("service/endpoint.json").exists() {
        assert!(helper.is_running(), "protocol helper exited");
        assert!(
            std::time::Instant::now() < deadline,
            "protocol helper did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut mcp = McpFixture::start(&state);
    mcp.initialize();
    let result = mcp.call(2, json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"unknown outcome fixture","evidence":"fixture","applies_to":["user"]}]}}));
    assert_eq!(result["result"]["isError"], true, "{result}");
    assert_eq!(
        result["result"]["structuredContent"]["code"], "execution_outcome_unknown",
        "{result}"
    );
    assert_eq!(
        std::fs::read_to_string(state.join("received-bodies")).unwrap(),
        "1"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while helper.is_running() {
        assert!(
            std::time::Instant::now() < deadline,
            "protocol helper did not exit"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let recalled = mcp.call(
        3,
        json!({"action":"recall","recall":{"recall_for":"unknown outcome fixture"}}),
    );
    assert_eq!(recalled["result"]["isError"], false, "{recalled}");
    assert!(
        !recalled["result"]["structuredContent"]
            .to_string()
            .contains("unknown outcome fixture"),
        "{recalled}"
    );
}

#[test]
fn first_store_connection_works_from_mcp_and_cli() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("store");
    initialize(&temp.path().join("seed"), &source);
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    for frontend in ["mcp", "cli"] {
        let state = temp.path().join(frontend);
        let _cleanup = RuntimeCleanup(state.clone());
        assert!(!state.join("config.json").exists());
        let input = json!({"action":"connect_project_memory","connect_project_memory":{"project_path":project,"memory_store_path":source,"intent":"discover"}});
        let recall = json!({"action":"recall","recall":{"recall_for":"first connection fixture"}});
        if frontend == "mcp" {
            let mut mcp = McpFixture::start(&state);
            mcp.initialize();
            let result = mcp.call(2, input);
            assert_eq!(result["result"]["isError"], false, "{result}");
            assert!(result["result"]["structuredContent"].is_object());
            let result = mcp.call(3, recall);
            assert_eq!(result["result"]["isError"], false, "{result}");
        } else {
            let (out, result) = run(&state, &["call", "--json", "-"], Some(input));
            assert!(out.status.success(), "{result}");
            assert!(result.is_object());
            let (out, result) = run(&state, &["call", "--json", "-"], Some(recall));
            assert!(out.status.success(), "{result}");
        }
        assert_eq!(
            agentlaw_app::config::load(&state)
                .unwrap()
                .unwrap()
                .memory_store_path,
            source
        );
    }
}

#[test]
fn runtime_file_delivery_reports_config_path_and_smaller_history_limit() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    initialize(&state, &temp.path().join("store"));
    let _cleanup = RuntimeCleanup(state.clone());
    for (key, value) in [
        ("response_limit_bytes", "16384"),
        ("history.response_limit_bytes", "1024"),
    ] {
        let (out, result) = run(&state, &["config", "set", key, value], None);
        assert!(out.status.success(), "{result}");
    }
    let body = format!("required marker {}", "substantial body ".repeat(1200));
    let (out, saved) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"delivery fixture","applies_to":["user"],"is_rule":true}]}}),
        ),
    );
    assert!(out.status.success(), "{saved}");
    let id = saved["results"][0]["memory_ref"]["memory_id"].clone();
    assert!(id.is_string(), "{saved}");
    for request in [
        json!({"action":"recall","recall":{"memory_ids":[id]}}),
        json!({"action":"history","history":{"memory_id":id,"history_for":"required marker","context_layers":0,"max_matches":1}}),
    ] {
        let (out, envelope) = run(&state, &["call", "--json", "-"], Some(request));
        assert!(out.status.success(), "{envelope}");
        assert_eq!(envelope["code"], "complete_content_in_file", "{envelope}");
        let path = envelope["artifact"]["path"].as_str().unwrap();
        let content: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            content["response_limit"]["config_file"],
            json!(state.join("config.json")),
            "{content}"
        );
    }
}

#[test]
fn history_uses_smaller_limit_when_recall_remains_inline() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    initialize(&state, &temp.path().join("store"));
    let _cleanup = RuntimeCleanup(state.clone());
    for (key, value) in [
        ("response_limit_bytes", "65536"),
        ("history.response_limit_bytes", "1024"),
    ] {
        let (out, result) = run(&state, &["config", "set", key, value], None);
        assert!(out.status.success(), "{result}");
    }
    let body = format!("history limit marker {}", "bounded text ".repeat(350));
    let (out, saved) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"history limit fixture","applies_to":["user"]}]}}),
        ),
    );
    assert!(out.status.success(), "{saved}");
    let id = saved["results"][0]["memory_ref"]["memory_id"].clone();
    assert!(id.is_string(), "{saved}");
    let (out, recall) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({"action":"recall","recall":{"memory_ids":[id]}})),
    );
    assert!(out.status.success(), "{recall}");
    assert_ne!(recall["code"], "complete_content_in_file");
    assert_eq!(
        recall["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
    let (out, history) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"history","history":{"memory_id":id,"history_for":"history limit marker","context_layers":0,"max_matches":1}}),
        ),
    );
    assert!(out.status.success(), "{history}");
    assert_eq!(history["code"], "complete_content_in_file", "{history}");
    assert_eq!(history["response_limit_bytes"], 1024);
    let bytes = std::fs::read(history["artifact"]["path"].as_str().unwrap()).unwrap();
    assert!(bytes.len() > 1024 && bytes.len() < 65536);
    let content: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        content["response_limit"]["config_file"],
        json!(state.join("config.json"))
    );
}

#[test]
fn machine_inspect_without_identity_is_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let (out, value) = run(&state, &["machine", "inspect"], None);
    assert!(out.status.success(), "{value}");
    assert!(value["machine_id"].is_null(), "{value}");
    assert!(value["display_name"].is_null(), "{value}");
    assert!(!state.join("machine.json").exists());
}

#[test]
fn config_get_without_runtime_reads_file_without_starting_host() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    initialize(&state, &temp.path().join("store"));
    let (out, result) = run(
        &state,
        &["config", "set", "response_limit_bytes", "8192"],
        None,
    );
    assert!(out.status.success(), "{result}");
    assert!(!state.join("service/endpoint.json").exists());
    let (out, result) = run(&state, &["config", "get", "response_limit_bytes"], None);
    assert!(out.status.success(), "{result}");
    assert_eq!(result["value"], 8192);
    assert!(!state.join("service/endpoint.json").exists());
    assert!(!agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
}

#[test]
#[ignore = "requires explicitly provisioned real Granite QDQ and ONNX Runtime artifacts"]
fn installed_real_model_cli_mcp_and_codex_config_smoke() {
    use sha2::{Digest, Sha256};
    fn asset(name: &str) -> Value {
        let path =
            std::path::PathBuf::from(std::env::var_os(name).expect("explicit smoke artifact"));
        let mut reader = std::fs::File::open(&path).unwrap();
        let mut hash = Sha256::new();
        let mut bytes = [0u8; 65536];
        loop {
            let n = std::io::Read::read(&mut reader, &mut bytes).unwrap();
            if n == 0 {
                break;
            }
            hash.update(&bytes[..n]);
        }
        json!({"path":path,"sha256":format!("{:x}",hash.finalize())})
    }
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let host = temp.path().join("codex");
    let source = temp.path().join("source");
    let manifest = temp.path().join("model.json");
    std::fs::write(&manifest,serde_json::to_vec(&json!({"model_build_id":"granite-r2-256-portable-qdq-v1","onnx_model":asset("AGENTLAW_SMOKE_MODEL"),"tokenizer_json":asset("AGENTLAW_SMOKE_TOKENIZER"),"runtime_library":asset("AGENTLAW_SMOKE_ORT")})).unwrap()).unwrap();
    let (out, installed) = run(
        &state,
        &[
            "install",
            "--harness",
            "codex",
            "--harness-dir",
            host.to_str().unwrap(),
            "--model-manifest",
            manifest.to_str().unwrap(),
            "--confirm-install",
        ],
        None,
    );
    assert!(out.status.success(), "{installed}");
    assert_eq!(installed["semantic_model_configured"], true);
    let _runtime = RuntimeCleanup(state.clone());
    let (out, connected) = run(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
        None,
    );
    assert!(
        out.status.success(),
        "{} / {}",
        connected,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        connected["derived_build"]["semantic_complete"], true,
        "{connected}"
    );
    let body = "Publish related memory changes atomically. Keep the previous source visible until every required review is completed.";
    let (out, saved) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"Confirmed in the actual installed-model integration test.","applies_to":["user"]}]}}),
        ),
    );
    assert!(out.status.success(), "{saved}");
    assert_eq!(saved["status"], "remembered");
    let id = saved["results"][0]["memory_ref"]["memory_id"].clone();
    let (out, recalled) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({"action":"recall","recall":{"memory_ids":[id]}})),
    );
    assert!(out.status.success(), "{recalled}");
    assert_eq!(
        recalled["memories"][0]["current_heads"][0]["what_to_remember"], body,
        "{recalled}"
    );
    let (out, search) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"recall","recall":{"recall_for":"atomic publication of memory updates"}}),
        ),
    );
    assert!(out.status.success(), "{search}");
    assert!(
        !search["candidates"].as_array().unwrap().is_empty()
            || !search["memories"].as_array().unwrap().is_empty(),
        "{search}"
    );
    assert!(
        !search.to_string().contains("semantic_unavailable")
            && !search.to_string().contains("semantic_channel_incomplete"),
        "{search}"
    );
    let (out, history) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"history","memory_id":id,"history_for":"atomic publication memory","context_layers":0,"max_matches":1}),
        ),
    );
    assert!(out.status.success(), "{history}");
    assert!(history.get("diagnostics").is_none(), "{history}");
    assert_eq!(
        history["match_windows"][0]["changes"][0]["matched"], true,
        "{history}"
    );
    // Actual MCP transport sees the same installed source, with no synthetic backend.
    let mut mcp = command(&state)
        .args(["mcp", "serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = mcp.stdin.take().unwrap();
    writeln!(stdin,"{}",json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"actual-model-smoke","version":"1"}}})).unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(stdin,"{}",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall":{"memory_ids":[id]}}}})).unwrap();
    drop(stdin);
    let output = mcp.wait_with_output().unwrap();
    assert!(output.status.success());
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let reply = replies.iter().find(|r| r["id"] == 2).unwrap();
    assert_eq!(
        reply["result"]["structuredContent"]["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
    // Transfer only canonical Markdown through an actual local Git clone. A
    // brand-new installation has neither the publication ledger nor vector DB.
    let empty_git_home = temp.path().join("empty-git-home");
    std::fs::create_dir(&empty_git_home).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        let out = Command::new("git")
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", empty_git_home.join("absent-config"))
            .env("GIT_TEMPLATE_DIR", &empty_git_home)
            .args([
                "-c",
                "user.name=Agentlaw isolated test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "Git fixture failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&source, &["init", "--quiet"]);
    git(&source, &["add", "--all"]);
    git(
        &source,
        &["commit", "--quiet", "-m", "Canonical transfer fixture"],
    );
    let cloned = temp.path().join("cloned-source");
    git(
        temp.path(),
        &[
            "clone",
            "--quiet",
            "--no-local",
            source.to_str().unwrap(),
            cloned.to_str().unwrap(),
        ],
    );
    let replica = temp.path().join("new-installation");
    std::fs::create_dir(&replica).unwrap();
    // Reuse provisioned, immutable assets; no user installation and no network download.
    std::fs::copy(
        state.join("model-assets.json"),
        replica.join("model-assets.json"),
    )
    .unwrap();
    let _replica_runtime = RuntimeCleanup(replica.clone());
    let (out, connected) = run(
        &replica,
        &["store", "connect", "--path", cloned.to_str().unwrap()],
        None,
    );
    assert!(out.status.success(), "{connected}");
    assert_eq!(
        connected["derived_build"]["semantic_complete"], true,
        "{connected}"
    );
    let (_, replica_exact) = run(
        &replica,
        &["call", "--json", "-"],
        Some(json!({"action":"recall","recall":{"memory_ids":[id]}})),
    );
    assert_eq!(
        replica_exact["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
    let (out, replica_search) = run(
        &replica,
        &["call", "--json", "-"],
        Some(
            json!({"action":"recall","recall":{"recall_for":"atomic publication of memory updates"}}),
        ),
    );
    assert!(out.status.success(), "{replica_search}");
    let has_id = replica_search["memories"]
        .as_array()
        .is_some_and(|items| items.iter().any(|m| m["memory_id"] == id))
        || replica_search["candidates"]
            .as_array()
            .is_some_and(|items| items.iter().any(|m| m["memory_id"] == id));
    assert!(has_id, "cloned memory was not searchable: {replica_search}");
    assert!(
        !replica_search
            .to_string()
            .contains("semantic_channel_incomplete"),
        "{replica_search}"
    );
    let original_machine: Value =
        serde_json::from_slice(&std::fs::read(state.join("machine.json")).unwrap()).unwrap();
    let replica_machine: Value =
        serde_json::from_slice(&std::fs::read(replica.join("machine.json")).unwrap()).unwrap();
    assert_ne!(
        original_machine["machine_id"],
        replica_machine["machine_id"]
    );
    assert!(!cloned.join("control.sqlite").exists());
    assert!(!cloned.join("broker.sqlite").exists());
    // This only asks the installed Codex binary to parse an isolated profile.
    // It sends no model request and never touches the active user's auth/config.
    if let Some(exe) = std::env::var_os("AGENTLAW_SMOKE_CODEX") {
        let out = Command::new(exe)
            .env("CODEX_HOME", &host)
            .current_dir(temp.path())
            .args(["mcp", "get", "agentlaw", "--json"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let parsed: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(parsed["name"], "agentlaw");
    }
}

#[test]
fn isolated_install_is_explicit_repeatable_and_preserves_harness_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("설치 state");
    let host = tmp.path().join("codex profile");
    let (_, proposal) = run(
        &state,
        &[
            "install",
            "--harness",
            "codex",
            "--harness-dir",
            host.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(proposal["status"], "confirmation_required");
    assert!(!state.exists());
    assert!(!host.exists());
    std::fs::create_dir_all(&host).unwrap();
    std::fs::write(
        host.join("config.toml"),
        "model = 'user-choice'\n[mcp_servers.other]\ncommand = 'other-tool'\n",
    )
    .unwrap();
    std::fs::write(
        host.join("AGENTS.override.md"),
        "# Existing override\nKeep this user rule.\n",
    )
    .unwrap();
    let args = [
        "install",
        "--harness",
        "codex",
        "--harness-dir",
        host.to_str().unwrap(),
        "--confirm-install",
    ];
    let (out, installed) = run(&state, &args, None);
    assert!(out.status.success(), "{installed}");
    assert_eq!(installed["status"], "installed");
    let first = std::fs::read(state.join("machine.json")).unwrap();
    let config = std::fs::read_to_string(host.join("config.toml")).unwrap();
    let doc = config.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(doc["model"].as_str(), Some("user-choice"));
    assert_eq!(
        doc["mcp_servers"]["other"]["command"].as_str(),
        Some("other-tool")
    );
    let instructions = std::fs::read_to_string(host.join("AGENTS.override.md")).unwrap();
    assert!(instructions.starts_with("# Existing override"));
    assert!(!instructions.contains("{AGENTLAW_CLI_PATH}"));
    assert!(instructions.contains("check for a matching active Task before creating one"));
    assert!(instructions.contains("emerging evidence or similarity to prior work"));
    assert!(!host.join("AGENTS.md").exists());
    // Unrelated user edits after installation remain intact through an update.
    std::fs::write(
        host.join("config.toml"),
        config.replace("user-choice", "user-new-choice"),
    )
    .unwrap();
    let (out, again) = run(&state, &args, None);
    assert!(out.status.success(), "{again}");
    assert!(std::fs::read_to_string(host.join("config.toml"))
        .unwrap()
        .contains("user-new-choice"));
    assert_eq!(std::fs::read(state.join("machine.json")).unwrap(), first);
    assert_eq!(
        std::fs::read_to_string(host.join("AGENTS.override.md"))
            .unwrap()
            .matches("managed bootstrap: begin")
            .count(),
        1
    );
    let output = Command::new(installed["executable"].as_str().unwrap())
        .arg("schema")
        .output()
        .unwrap();
    assert!(output.status.success());
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["name"], "agentlaw");
    assert!(schema["description"]
        .as_str()
        .unwrap()
        .contains("At a new work request or resumption"));
    assert!(schema["description"]
        .as_str()
        .unwrap()
        .contains("match by objective and scope before creating one"));
    let omp = tmp.path().join("omp profile");
    std::fs::create_dir(&omp).unwrap();
    std::fs::write(
        omp.join("mcp.json"),
        "{\"mcpServers\":{\"other\":{\"command\":\"keep\"}}}",
    )
    .unwrap();
    let (out, installed) = run(
        &state,
        &[
            "install",
            "--harness",
            "oh-my-pi",
            "--harness-dir",
            omp.to_str().unwrap(),
            "--confirm-install",
        ],
        None,
    );
    assert!(out.status.success(), "{installed}");
    let json: Value =
        serde_json::from_str(&std::fs::read_to_string(omp.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(json["mcpServers"]["other"]["command"], "keep");
    assert_eq!(json["mcpServers"]["agentlaw"]["timeout"], 0);
}

#[test]
fn diagnostics_and_limits_are_explicit_and_do_not_reset_memory() {
    use rusqlite::OptionalExtension;
    fn source_bytes(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        let mut directories = vec![root.to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    directories.push(entry.path());
                } else {
                    files.insert(
                        entry.path().strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        files
    }
    fn delivered(value: Value) -> Value {
        if value["code"] == "complete_content_in_file" {
            serde_json::from_slice(
                &std::fs::read(value["artifact"]["path"].as_str().unwrap()).unwrap(),
            )
            .unwrap()
        } else {
            value
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let source = tmp.path().join("source");
    initialize(&state, &source);
    let _runtime = RuntimeCleanup(state.clone());
    let first_body = "Use argument arrays when invoking PowerShell with paths containing spaces.";
    let current_body = "Use PowerShell argument arrays for paths containing spaces; never interpolate the path into a command string.";
    let (out, first) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":first_body,"evidence":"A quoted path failed in the original command.","applies_to":["user"]}]}}),
        ),
    );
    assert!(out.status.success(), "{first}");
    assert_eq!(first["status"], "remembered", "{first}");
    let original_ref = first["results"][0]["memory_ref"].clone();
    let id = original_ref["memory_id"].as_str().unwrap();
    let (out, evolved) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"evolve","parent_refs":[original_ref],"what_to_remember":current_body,"evidence":"The user clarified that string interpolation is also unsafe."}]}}),
        ),
    );
    assert!(out.status.success(), "{evolved}");
    assert_eq!(evolved["status"], "remembered", "{evolved}");
    let current_ref = evolved["results"][0]["memory_ref"].clone();
    assert_ne!(original_ref, current_ref);
    let (out, pending) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":current_body,"evidence":"Unresolved duplicate proposal: retain this evidence until the user decides.","applies_to":["user"]}]}}),
        ),
    );
    assert!(out.status.success(), "{pending}");
    assert_eq!(pending["status"], "resolution_required", "{pending}");
    let pending_ref = pending["pending_batch_ref"].clone();
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let local = config.runtime_root(&state);
    let pending_row = || {
        let db = rusqlite::Connection::open_with_flags(
            local.join("control.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT id,version,binding,payload,status,result,discard_reason FROM pending WHERE id=?1",
            [pending_ref["pending_batch_id"].as_str().unwrap()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .unwrap()
    };
    let retained_pending = pending_row();
    assert_eq!(retained_pending.4, "pending");
    let canonical = source_bytes(&source);
    assert!(canonical.keys().any(|p| p.starts_with("current")));
    assert!(canonical.keys().any(|p| p.starts_with("history")));
    let export = |name: &str| {
        let path = tmp.path().join(name);
        let (out, receipt) = run(
            &state,
            &[
                "history",
                "export",
                "--memory-id",
                id,
                "--output",
                path.to_str().unwrap(),
            ],
            None,
        );
        assert!(out.status.success(), "{receipt}");
        assert_eq!(receipt["change_count"], 2, "{receipt}");
        let mut records: Vec<Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        // Export time describes this derived artifact, not a source change.
        assert!(records[0]["exported_at"].is_string());
        records[0].as_object_mut().unwrap().remove("exported_at");
        records
    };
    let history = export("before.jsonl");
    let records = &history;
    assert!(records[1]["markdown_diff"]
        .as_str()
        .unwrap()
        .contains(first_body));
    assert!(records[2]["markdown_diff"]
        .as_str()
        .unwrap()
        .contains(current_body));
    assert_eq!(records[2]["parent_change_ids"][0], records[1]["change_id"]);
    let verify_content = || {
        assert_eq!(
            source_bytes(&source),
            canonical,
            "canonical file set or bytes changed"
        );
        assert_eq!(
            pending_row(),
            retained_pending,
            "unpublished proposal authority changed"
        );
        let (out, recalled) = run(
            &state,
            &["call", "--json", "-"],
            Some(json!({"action":"recall","recall":{"memory_ids":[id]}})),
        );
        assert!(out.status.success(), "{recalled}");
        let recalled = delivered(recalled);
        assert_eq!(
            recalled["memories"][0]["current_heads"][0]["what_to_remember"],
            current_body
        );
        assert_eq!(
            recalled["memories"][0]["current_heads"][0]["memory_ref"],
            current_ref
        );
        let (out, inspected) = run(
            &state,
            &["call", "--json", "-"],
            Some(
                json!({"action":"remember_this","remember_this":{"pending_action":"inspect","pending_batch_ref":pending_ref}}),
            ),
        );
        assert!(out.status.success(), "{inspected}");
        let inspected = delivered(inspected);
        assert_eq!(inspected["proposals"][0]["what_to_remember"], current_body);
        assert_eq!(
            inspected["proposals"][0]["evidence"],
            "Unresolved duplicate proposal: retain this evidence until the user decides."
        );
        assert_eq!(pending_row(), retained_pending);
    };
    verify_content();
    let (out, changed) = run(
        &state,
        &["config", "set", "response_limit_bytes", "4096"],
        None,
    );
    assert!(out.status.success(), "{changed}");
    let original = std::fs::read(state.join("config.json")).unwrap();
    let (out, invalid) = run(
        &state,
        &["config", "set", "response_limit_bytes", "0"],
        None,
    );
    assert!(!out.status.success());
    assert_eq!(invalid["code"], "invalid_configuration");
    assert_eq!(std::fs::read(state.join("config.json")).unwrap(), original);
    let (out, doctor) = run(&state, &["doctor", "--audit"], None);
    assert!(out.status.success(), "{doctor}");
    assert_eq!(doctor["read_only"], true);
    assert_eq!(doctor["audit"]["status"], "passed", "{doctor}");
    assert_eq!(doctor["audit"]["source"]["validated"], true);
    verify_content();
    assert_eq!(export("after-doctor.jsonl"), history);
    // Damage only a disposable exact index, then require repair to install a
    // real replacement generation. A success flag alone is not preservation evidence.
    let active_index = {
        let db = rusqlite::Connection::open_with_flags(
            local.join("control.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT value FROM settings WHERE key='exact_active_generation'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
        .unwrap_or_else(|| "exact-v3.sqlite".into())
    };
    let damaged = local.join(&active_index);
    let old_index =
        rusqlite::Connection::open_with_flags(&damaged, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        old_index
            .query_row("SELECT COUNT(*) FROM exact_units WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    drop(old_index);
    std::fs::write(&damaged, b"deliberately damaged disposable exact index").unwrap();
    let corrupt_index =
        rusqlite::Connection::open_with_flags(&damaged, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert!(
        corrupt_index
            .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .is_err(),
        "fixture must damage the effective SQLite index, not just an unused file"
    );
    drop(corrupt_index);
    let (out, repaired) = run(&state, &["repair"], None);
    assert!(out.status.success(), "{repaired}");
    assert_eq!(repaired["original_memory_preserved"], true);
    assert_eq!(repaired["derived"]["lexical_complete"], true);
    assert_eq!(
        repaired["derived"]["semantic_complete"], false,
        "no model was configured"
    );
    let replacement = {
        let db = rusqlite::Connection::open_with_flags(
            local.join("control.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT value FROM settings WHERE key='exact_active_generation'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    };
    assert_ne!(replacement, active_index);
    let index = rusqlite::Connection::open_with_flags(
        local.join(replacement),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        index
            .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        index
            .query_row("SELECT COUNT(*) FROM exact_units WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    drop(index);
    verify_content();
    assert_eq!(export("after-repair.jsonl"), history);
    // Corrupt source is not a repair invitation to reset authoritative content.
    let format = std::fs::read(source.join("format.md")).unwrap();
    std::fs::write(source.join("format.md"), b"invalid fixture format").unwrap();
    let invalid_source = source_bytes(&source);
    for args in [&["doctor", "--audit"][..], &["repair"][..]] {
        let action = args.join(" ");
        let (out, result) = run(&state, args, None);
        if args[0] == "doctor" {
            assert!(out.status.success(), "{action}: {result}");
            assert_eq!(result["audit"]["status"], "failed", "{action}: {result}");
        } else {
            assert!(!out.status.success(), "{action}: {result}");
        }
        assert_eq!(
            source_bytes(&source),
            invalid_source,
            "{action} modified a corrupt source"
        );
        assert_eq!(
            pending_row(),
            retained_pending,
            "{action} modified an unpublished proposal"
        );
        assert_eq!(std::fs::read(state.join("config.json")).unwrap(), original);
    }
    std::fs::write(source.join("format.md"), format).unwrap();
    verify_content();
    let (_, identity) = run(&state, &["machine", "inspect"], None);
    let (out, named) = run(&state, &["machine", "name", "--value", "개발 노트북"], None);
    assert!(out.status.success(), "{named}");
    assert_eq!(identity["machine_id"], named["machine_id"]);
}

#[test]
fn installed_layout_proposes_memory_beside_private_state() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Agentlaw");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let state = root.join("state");
    let (out, proposal) = run(&state, &["store", "propose-location"], None);
    assert!(out.status.success());
    assert_eq!(
        proposal["proposed_path"],
        root.join("memory").to_string_lossy().as_ref()
    );
    assert!(!state.exists());
    assert!(!root.join("memory").exists());
}

#[test]
fn managed_install_cli_create_and_doctor_use_installation_local_coordination() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("custom-installation");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let installed = bin.join(if cfg!(windows) {
        "agentlaw.exe"
    } else {
        "agentlaw"
    });
    std::fs::copy(executable(), &installed).unwrap();

    let profile = tmp.path().join("isolated-profile");
    std::fs::create_dir_all(&profile).unwrap();
    let local_app_data = tmp.path().join("unrelated-local-app-data");
    let xdg_state = tmp.path().join("unrelated-xdg-state");
    let invoke = |args: &[&str]| {
        let mut cmd = Command::new(&installed);
        cmd.args(args)
            .env_remove("AGENTLAW_HOME")
            .env("LOCALAPPDATA", &local_app_data)
            .env("XDG_STATE_HOME", &xdg_state);
        #[cfg(windows)]
        cmd.env("USERPROFILE", &profile);
        #[cfg(not(windows))]
        cmd.env("HOME", &profile);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        cmd.output().unwrap()
    };

    let source = root.join("memory");
    let create = invoke(&[
        "store",
        "create",
        "--path",
        source.to_str().unwrap(),
        "--confirm-create",
    ]);
    let created: Value = serde_json::from_slice(&create.stdout).unwrap();
    assert!(create.status.success(), "{created}");
    assert_eq!(created["created"], true);

    let doctor_output = invoke(&["doctor", "--audit"]);
    let doctor: Value = serde_json::from_slice(&doctor_output.stdout).unwrap();
    assert!(doctor_output.status.success(), "{doctor}");
    assert_eq!(doctor["audit"]["source"]["validated"], true);

    let selected_registry = root.join("state").join("source-coordination");
    assert!(
        selected_registry.is_dir(),
        "create and doctor must use the selected managed installation's state registry"
    );
    assert!(
        std::fs::read_dir(&selected_registry)
            .unwrap()
            .next()
            .is_some(),
        "the source binding should be recorded in the managed registry"
    );
    assert!(
        !profile
            .join("Agentlaw")
            .join("source-coordination")
            .exists(),
        "coordination must not be pinned to a profile-wide Agentlaw installation"
    );
    assert!(
        !local_app_data
            .join("Agentlaw")
            .join("source-coordination")
            .exists(),
        "coordination must not fall back to LOCALAPPDATA"
    );
    assert!(
        !xdg_state
            .join("Agentlaw")
            .join("source-coordination")
            .exists(),
        "coordination must not fall back to XDG_STATE_HOME"
    );
}

#[cfg(windows)]
#[test]
fn installed_binary_discovers_its_root_without_a_user_name_or_environment_override() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("custom-root");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let installed = bin.join("agentlaw.exe");
    std::fs::copy(executable(), &installed).unwrap();
    let output = Command::new(&installed)
        .args(["store", "propose-location"])
        .env_remove("AGENTLAW_HOME")
        .env("LOCALAPPDATA", tmp.path().join("unrelated-appdata"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let proposal: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        proposal["proposed_path"],
        root.join("memory").to_string_lossy().as_ref()
    );
    assert!(!root.join("state").exists());
    assert!(!root.join("memory").exists());
}

#[cfg(windows)]
#[test]
fn unmanaged_binary_uses_profile_managed_root_but_never_appdata_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    let appdata = tmp.path().join("unrelated-appdata");
    let request = || {
        Command::new(executable())
            .args(["store", "propose-location"])
            .env_remove("AGENTLAW_HOME")
            .env("USERPROFILE", &profile)
            .env("LOCALAPPDATA", &appdata)
            .output()
            .unwrap()
    };
    let missing = request();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stdout).contains("configuration_required"));
    assert!(!appdata.exists());

    let root = profile.join("Agentlaw");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let installed = request();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let proposal: Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(
        proposal["proposed_path"],
        root.join("memory").to_string_lossy().as_ref()
    );
    assert!(!appdata.exists());
}

#[cfg(windows)]
#[test]
fn damaged_managed_layout_does_not_fall_back_to_appdata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("damaged-root");
    let bin = root.join("bin");
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(root.join(".agentlaw-layout"), b"future-unknown-layout\n").unwrap();
    let installed = bin.join("agentlaw.exe");
    std::fs::copy(executable(), &installed).unwrap();
    let output = Command::new(&installed)
        .args(["store", "propose-location"])
        .env_remove("AGENTLAW_HOME")
        .env("LOCALAPPDATA", tmp.path().join("unrelated-appdata"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stdout);
    assert!(error.contains("invalid_installation"), "{error}");
    assert!(!tmp.path().join("unrelated-appdata").exists());

    std::fs::remove_file(root.join(".agentlaw-layout")).unwrap();
    let missing = Command::new(&installed)
        .args(["store", "propose-location"])
        .env_remove("AGENTLAW_HOME")
        .env("LOCALAPPDATA", tmp.path().join("unrelated-appdata"))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stdout).contains("invalid_installation"));

    let version_dir = root.join("state").join("versions").join("test-build");
    std::fs::create_dir_all(&version_dir).unwrap();
    let versioned = version_dir.join("agentlaw.exe");
    std::fs::copy(executable(), &versioned).unwrap();
    let version_output = Command::new(&versioned)
        .args(["store", "propose-location"])
        .env_remove("AGENTLAW_HOME")
        .env("LOCALAPPDATA", tmp.path().join("unrelated-appdata"))
        .output()
        .unwrap();
    assert!(!version_output.status.success());
    assert!(String::from_utf8_lossy(&version_output.stdout).contains("invalid_installation"));
}

#[test]
fn setup_is_explicit_and_does_not_select_foreign_data() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let (_, proposed) = run(&state, &["store", "propose-location"], None);
    assert_eq!(proposed["created"], false);
    assert_eq!(
        proposed["proposed_path"],
        state.join("memory").to_string_lossy().as_ref()
    );
    assert!(!state.exists());
    let foreign = tmp.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    let (out, error) = run(
        &state,
        &["store", "connect", "--path", foreign.to_str().unwrap()],
        None,
    );
    assert!(!out.status.success());
    assert_eq!(error["code"], "invalid_store");
    assert!(!state.join("config.json").exists());
    assert_eq!(std::fs::read_dir(&foreign).unwrap().count(), 0);
    let source = tmp.path().join("source");
    initialize(&state, &source);
    let (out, same) = run(
        &state,
        &["store", "connect", "--path", source.to_str().unwrap()],
        None,
    );
    assert!(out.status.success());
    assert_eq!(same["changed"], false);
    let before = std::fs::read(state.join("config.json")).unwrap();
    let (out, error) = run(
        &state,
        &["store", "connect", "--path", foreign.to_str().unwrap()],
        None,
    );
    assert!(!out.status.success());
    assert_eq!(error["code"], "invalid_store");
    assert_eq!(std::fs::read(state.join("config.json")).unwrap(), before);
    std::fs::write(source.join("format.md"), "broken format in test fixture").unwrap();
    let (out, error) = run(
        &state,
        &["store", "connect", "--path", source.to_str().unwrap()],
        None,
    );
    assert!(!out.status.success());
    assert_eq!(error["code"], "connected_store_unavailable");
    assert_eq!(std::fs::read(state.join("config.json")).unwrap(), before);
}

#[test]
fn real_cli_publication_is_visible_to_new_mcp_process() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let source = tmp.path().join("source");
    initialize(&state, &source);
    let _runtime = RuntimeCleanup(state.clone());
    let (out, written) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({
            "action":"remember_this", "remember_this":{"memories":[{"operation":"create", "what_to_remember":"Use PowerShell argument arrays for paths with spaces.", "evidence":"Verified in the shell.", "applies_to":["user"]}]}
        })),
    );
    assert!(out.status.success(), "{written}");
    assert_eq!(written["status"], "remembered");
    let reference = &written["results"][0]["memory_ref"];
    let request = json!({"action":"recall", "recall":{"memory_ids":[reference["memory_id"]]}});
    let mut mcp = McpFixture::start(&state);
    mcp.initialize();
    mcp.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert_eq!(mcp.reply()["result"]["tools"].as_array().unwrap().len(), 1);
    let response = mcp.call(3, request);
    assert_eq!(response["result"]["isError"], false);
    let recalled = &response["result"]["structuredContent"];
    assert_eq!(
        recalled["memories"][0]["current_heads"][0]["memory_ref"],
        *reference
    );
    assert_eq!(
        recalled["memories"][0]["current_heads"][0]["what_to_remember"],
        "Use PowerShell argument arrays for paths with spaces."
    );
    assert!(recalled.get("candidate_counts").is_none());
}

#[test]
fn clone_connect_validates_source_and_keeps_machine_state_local() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    let source = tmp.path().join("source");
    initialize(&first, &source);
    let cloned = tmp.path().join("cloned");
    std::fs::create_dir(&cloned).unwrap();
    for name in ["format.md", ".gitattributes"] {
        std::fs::copy(source.join(name), cloned.join(name)).unwrap();
    }
    let (out, connected) = run(
        &second,
        &["store", "connect", "--path", cloned.to_str().unwrap()],
        None,
    );
    assert!(out.status.success(), "{connected}");
    assert_eq!(connected["connected"], true);
    assert!(second.join("runtime/control.sqlite").is_file());
    assert!(!source.join("control.sqlite").exists());
}

#[test]
fn existing_frontend_switches_next_request_and_old_store_is_retained() {
    use std::io::{BufRead, BufReader};
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let a = tmp.path().join("a");
    initialize(&state, &a);
    let machine_before = std::fs::read(state.join("machine.json")).unwrap();
    let _runtime = RuntimeCleanup(state.clone());
    let (_, saved) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Store A contains this marker.","evidence":"Store switch fixture.","applies_to":["user"]}]}}),
        ),
    );
    let id = saved["results"][0]["memory_ref"]["memory_id"].clone();
    assert!(id.is_string(), "{saved}");
    let mut child = command(&state)
        .args(["mcp", "serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut request = |message: Value| -> Value {
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    request(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{}}}),
    );
    // This notification has no response; release the closure's temporary borrow.
    drop(request);
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    let mut request = |n: u64| -> Value {
        writeln!(input,"{}",json!({"jsonrpc":"2.0","id":n,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall":{"memory_ids":[id]}}}})).unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    assert_eq!(
        request(2)["result"]["structuredContent"]["memories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // A failed B preparation must not poison an already-open MCP frontend's A.
    let invalid_b = tmp.path().join("invalid-b");
    std::fs::create_dir(&invalid_b).unwrap();
    std::fs::write(invalid_b.join("format.md"), "not a memory-store format").unwrap();
    let selection_before = std::fs::read(state.join("config.json")).unwrap();
    let (failed, failure) = run(
        &state,
        &["store", "connect", "--path", invalid_b.to_str().unwrap()],
        None,
    );
    assert!(!failed.status.success(), "{failure}");
    assert_eq!(failure["code"], "store_validation_failed");
    assert_eq!(
        std::fs::read(state.join("config.json")).unwrap(),
        selection_before
    );
    assert_eq!(
        request(20)["result"]["structuredContent"]["memories"][0]["current_heads"][0]
            ["what_to_remember"],
        "Store A contains this marker."
    );
    let b = tmp.path().join("b");
    initialize(&state, &b);
    let response = request(3);
    assert_eq!(
        response["result"]["structuredContent"]["missing_ids"]["memory_ids"][0], id,
        "{response}"
    );
    let (out, switched) = run(
        &state,
        &["store", "connect", "--path", a.to_str().unwrap()],
        None,
    );
    assert!(out.status.success(), "{switched}");
    assert_eq!(
        request(4)["result"]["structuredContent"]["memories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        std::fs::read(state.join("machine.json")).unwrap(),
        machine_before
    );
    drop(request);
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn missing_proposal_authority_is_not_recreated_by_call_reconnect_or_repair() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let source = tmp.path().join("source");
    initialize(&state, &source);
    let _runtime = RuntimeCleanup(state.clone());
    let write = json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Use argument arrays for paths with spaces.","evidence":"User corrected command quoting.","applies_to":["user"]}]}});
    let (_, first) = run(&state, &["call", "--json", "-"], Some(write.clone()));
    assert_eq!(first["status"], "remembered");
    let (_, pending) = run(&state, &["call", "--json", "-"], Some(write));
    assert_eq!(pending["status"], "resolution_required");
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let control = config.runtime_root(&state).join("control.sqlite");
    let backup = control.with_extension("held");
    drop(_runtime);
    let _runtime = RuntimeCleanup(state.clone());
    std::fs::rename(&control, &backup).unwrap();
    let bytes = std::fs::read(&backup).unwrap();
    let selection = std::fs::read(state.join("config.json")).unwrap();
    let recall = json!({"action":"recall","recall":{"memory_ids":[first["results"][0]["memory_ref"]["memory_id"]]}});
    let (out, error) = run(&state, &["call", "--json", "-"], Some(recall.clone()));
    assert!(!out.status.success());
    assert_eq!(error["code"], "control_backup_required");
    for args in [
        vec!["store", "connect", "--path", source.to_str().unwrap()],
        vec!["repair"],
    ] {
        let (out, error) = run(&state, &args, None);
        assert!(!out.status.success(), "{error}");
        assert_eq!(error["code"], "control_backup_required");
        assert!(
            !control.exists(),
            "authoritative local proposals were silently replaced"
        );
    }
    assert_eq!(std::fs::read(&backup).unwrap(), bytes);
    assert_eq!(std::fs::read(state.join("config.json")).unwrap(), selection);
    // Restoring the original authority must recover both published and pending content.
    drop(_runtime);
    let _runtime = RuntimeCleanup(state.clone());
    std::fs::rename(&backup, &control).unwrap();
    let (_, restored) = run(&state, &["call", "--json", "-"], Some(recall));
    assert_eq!(
        restored["memories"][0]["current_heads"][0]["what_to_remember"],
        "Use argument arrays for paths with spaces."
    );
    let (_, inspected) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"pending_action":"inspect","pending_batch_ref":pending["pending_batch_ref"]}}),
        ),
    );
    assert_eq!(
        inspected["proposals"][0]["what_to_remember"],
        "Use argument arrays for paths with spaces."
    );
}

#[test]
fn oversized_cli_recall_keeps_a_complete_frozen_version_after_later_evolution() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let source = tmp.path().join("source");
    initialize(&state, &source);
    let _runtime = RuntimeCleanup(state.clone());
    let (out, config) = run(
        &state,
        &["config", "set", "response_limit_bytes", "4096"],
        None,
    );
    assert!(out.status.success(), "{config}");
    // Unicode, CRLF and Markdown fences exercise lossless transport, not relevance quality.
    let body = format!(
        "# 경로 기록\r\n{}\r\n```powershell\r\n& $python @args\r\n```\r\nEND-OF-MEMORY",
        "경로에 공백이 있으면 인수 배열을 쓴다.\r\n".repeat(300)
    );
    let (_, saved) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"Verified command invocation.","applies_to":["user"],"is_rule":true}]}}),
        ),
    );
    assert_eq!(saved["status"], "remembered", "{saved}");
    let reference = saved["results"][0]["memory_ref"].clone();
    let request = json!({"action":"recall","recall":{"memory_ids":[reference["memory_id"]]}});
    let (out, oversized) = run(&state, &["call", "--json", "-"], Some(request.clone()));
    assert!(out.status.success(), "{oversized}");
    assert_eq!(oversized["code"], "complete_content_in_file");
    assert_eq!(oversized["content_read"], false);
    assert!(oversized["next_action"].as_str().unwrap().contains("NOT"));
    let path = Path::new(oversized["artifact"]["path"].as_str().unwrap());
    assert!(path.is_absolute());
    let frozen = std::fs::read(path).unwrap();
    let packet: Value = serde_json::from_slice(&frozen).unwrap();
    assert_eq!(
        packet["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
    assert_eq!(
        packet["memories"][0]["current_heads"][0]["memory_ref"],
        reference
    );
    assert_eq!(oversized["artifact"]["bytes"], frozen.len());
    let (_, contextual) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"recall","recall":{"recall_for":"unrelated ordinary work","memory_candidate_limit":1}}),
        ),
    );
    assert_eq!(
        contextual["code"], "complete_content_in_file",
        "{contextual}"
    );
    assert_eq!(contextual["content_read"], false);
    let contextual_packet: Value = serde_json::from_slice(
        &std::fs::read(contextual["artifact"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        contextual_packet["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
    assert_eq!(
        contextual_packet["memories"][0]["current_heads"][0]["memory_ref"],
        saved["results"][0]["memory_ref"]
    );
    let (_, changed) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"evolve","parent_refs":[reference],"what_to_remember":"Current concise command guidance.","evidence":"User requested a coherent rewrite."}]}}),
        ),
    );
    assert_eq!(changed["status"], "remembered", "{changed}");
    assert_eq!(
        std::fs::read(path).unwrap(),
        frozen,
        "later writes changed an already acquired artifact"
    );
    assert_eq!(
        contextual_packet,
        serde_json::from_slice::<Value>(
            &std::fs::read(contextual["artifact"]["path"].as_str().unwrap()).unwrap()
        )
        .unwrap()
    );
    let (_, current) = run(&state, &["call", "--json", "-"], Some(request));
    assert_eq!(
        current["memories"][0]["current_heads"][0]["what_to_remember"],
        "Current concise command guidance."
    );
}

#[test]
fn aggregate_contextual_rules_and_required_reference_survive_response_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let source = tmp.path().join("source");
    initialize(&state, &source);
    let _runtime = RuntimeCleanup(state.clone());
    let (out, _) = run(
        &state,
        &["config", "set", "response_limit_bytes", "4096"],
        None,
    );
    assert!(out.status.success());
    let reference_body = format!("Reference: {}", "R".repeat(1000));
    let (_, reference) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":reference_body,"evidence":"Fixture","applies_to":["user"]}]}}),
        ),
    );
    assert_eq!(reference["status"], "remembered", "{reference}");
    let reference_id = reference["results"][0]["memory_ref"]["memory_id"].clone();
    let first_body = format!("First rule: {}", "A".repeat(1800));
    let (_, first) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":first_body,"evidence":"Fixture","applies_to":["user"],"is_rule":true,"required_memory_ids":[reference_id]}]}}),
        ),
    );
    assert_eq!(first["status"], "remembered", "{first}");
    let second_body = format!("Second rule: {}", "B".repeat(1800));
    let (_, second) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":second_body,"evidence":"Fixture","applies_to":["user"],"is_rule":true}]}}),
        ),
    );
    assert_eq!(second["status"], "remembered", "{second}");
    for candidate_body in ["unrelated task alpha", "unrelated task bravo"] {
        let (_, saved) = run(
            &state,
            &["call", "--json", "-"],
            Some(
                json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":candidate_body,"evidence":"Candidate fixture","applies_to":["user"]}]}}),
            ),
        );
        assert_eq!(saved["status"], "remembered", "{saved}");
    }
    let (_, response) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"recall","recall":{"recall_for":"unrelated task","memory_candidate_limit":1}}),
        ),
    );
    assert_eq!(response["code"], "complete_content_in_file", "{response}");
    assert_eq!(response["content_read"], false);
    let bytes = std::fs::read(response["artifact"]["path"].as_str().unwrap()).unwrap();
    assert_eq!(response["artifact"]["bytes"], bytes.len());
    let packet: Value = serde_json::from_slice(&bytes).unwrap();
    let memories = packet["memories"].as_array().unwrap();
    for (saved, body) in [
        (&reference, &reference_body),
        (&first, &first_body),
        (&second, &second_body),
    ] {
        let expected = &saved["results"][0]["memory_ref"];
        assert!(memories
            .iter()
            .any(|m| m["memory_id"] == expected["memory_id"]
                && m["current_heads"][0]["memory_ref"] == *expected
                && m["current_heads"][0]["what_to_remember"] == *body));
    }
    assert_eq!(memories.len(), 3);
    assert_eq!(
        packet["candidate_counts"]["memories"],
        json!({"matched":2,"shown":1})
    );
    assert_eq!(
        packet["candidates"][0]["retrieval_paths"][0]["via"],
        json!(["lexical"])
    );
    assert_eq!(
        packet["candidates"][0]["retrieval_paths"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(packet
        .get("undelivered_required")
        .is_none_or(|v| v.as_array().unwrap().is_empty()));
    let (_, unbound) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"recall","recall":{"project_path":"C:/work/unbound-aggregate","recall_for":"unrelated task","include_active_tasks":true}}),
        ),
    );
    assert_eq!(unbound["code"], "complete_content_in_file", "{unbound}");
    let unbound_packet: Value = serde_json::from_slice(
        &std::fs::read(unbound["artifact"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(unbound_packet["status"], "needs_user_input");
    assert_eq!(unbound_packet["code"], "project_connection_required");
    for candidate in unbound_packet["partial_recall"]["candidates"]
        .as_array()
        .unwrap()
    {
        assert_eq!(candidate["retrieval_paths"][0]["via"], json!(["lexical"]));
    }
    assert_eq!(
        unbound_packet["partial_recall"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        unbound_packet["partial_recall"]["memories"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(unbound_packet["turn_instruction"]
        .as_str()
        .unwrap()
        .contains("Project rules, active Tasks and work targets have not been checked"));
}
