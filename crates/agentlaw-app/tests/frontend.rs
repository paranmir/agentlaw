//! Actual CLI and MCP subprocesses, not a mocked backend.
#[path = "../../../tests/support/owned_process.rs"]
mod owned_process;
#[path = "../../../tests/support/runtime_cleanup.rs"]
mod runtime_cleanup;
use runtime_cleanup::RuntimeCleanup;
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
    let out = run_command_output(state, args, input);
    let value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("Invalid JSON output: {:?}", out));
    (out, value)
}

fn run_command_output(state: &Path, args: &[&str], input: Option<Value>) -> Output {
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
    let mut child = Daemon::frontend(child);
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
    out
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
        let child = Daemon::frontend(child);
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &temp.path().join("store"));
    _initial_runtime_cleanup.shutdown();
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
fn frontend_pipes_reach_eof_while_runtime_remains_alive() {
    use std::sync::mpsc;
    use std::time::Instant;

    fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
            let _ = send.send(result);
        });
        receive
    }

    for frontend in ["cli", "mcp"] {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
        initialize(&state, &temp.path().join("store"));
        _initial_runtime_cleanup.shutdown();
        let _cleanup = RuntimeCleanup(state.clone());
        assert!(!state.join("service/endpoint.json").exists());
        let mut cmd = command(&state);
        if frontend == "cli" {
            cmd.args(["call", "--json", "-"]);
        } else {
            cmd.args(["mcp", "serve", "--stdio"]);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let stdout = drain(child.stdout.take().unwrap());
        let stderr = drain(child.stderr.take().unwrap());
        let mut child = Daemon::frontend(child);
        if frontend == "cli" {
            write!(
                input,
                "{}",
                json!({"action":"recall","recall":{"recall_for":"pipe eof fixture"}})
            )
            .unwrap();
        } else {
            for message in [
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall":{"recall_for":"pipe eof fixture"}}}}),
            ] {
                writeln!(input, "{message}").unwrap();
            }
        }
        drop(input);
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "{frontend} frontend did not exit"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            status.success(),
            "{frontend} exited unsuccessfully: {status}"
        );
        let endpoint =
            agentlaw_runtime::transport::read_endpoint(&state).expect("runtime endpoint");
        let mut processes = sysinfo::System::new();
        processes.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        assert!(
            processes
                .process(sysinfo::Pid::from_u32(endpoint.pid))
                .is_some(),
            "runtime process is not alive"
        );
        assert!(agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
        let output = stdout
            .recv_timeout(Duration::from_secs(5))
            .expect("stdout EOF deadline")
            .unwrap();
        stderr
            .recv_timeout(Duration::from_secs(5))
            .expect("stderr EOF deadline")
            .unwrap();
        assert!(!output.is_empty(), "{frontend} produced no response");
        processes.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        assert!(
            processes
                .process(sysinfo::Pid::from_u32(endpoint.pid))
                .is_some(),
            "runtime exited before EOF"
        );
        assert!(agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
    }
}

#[test]
fn two_mcp_frontends_share_one_runtime_and_memory() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &temp.path().join("store"));
    _initial_runtime_cleanup.shutdown();
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
}

#[test]
fn discovery_responds_before_runtime_is_ready_and_first_call_waits() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &temp.path().join("store"));
    _initial_runtime_cleanup.shutdown();
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &temp.path().join("store"));
    _initial_runtime_cleanup.shutdown();
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &temp.path().join("store"));
    _initial_runtime_cleanup.shutdown();
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
    let _seed_runtime_cleanup = RuntimeCleanup(temp.path().join("seed"));
    initialize(&temp.path().join("seed"), &source);
    _seed_runtime_cleanup.shutdown();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    for frontend in ["mcp", "cli"] {
        let state = temp.path().join(frontend);
        let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
            std::fs::canonicalize(&source).unwrap()
        );
    }
}

#[test]
fn runtime_file_delivery_reports_config_path_and_smaller_history_limit() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
        let bytes = std::fs::read(path).unwrap();
        let content: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            content["response_limit"]["config_file"],
            json!(state.join("config.json")),
            "{content}"
        );
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.matches("\"response_limit\"").count(), 1, "{content}");
        assert!(!text.contains("complete_content_in_file"), "{content}");
    }
}

#[test]
fn history_uses_smaller_limit_when_recall_remains_inline() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text.matches("\"response_limit\"").count(), 1, "{content}");
    assert!(!text.contains("complete_content_in_file"), "{content}");
}

#[test]
fn machine_inspect_without_identity_is_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &temp.path().join("store"));
    _initial_runtime_cleanup.shutdown();
    let (out, result) = run(
        &state,
        &["config", "set", "response_limit_bytes", "8192"],
        None,
    );
    assert!(out.status.success(), "{result}");
    _initial_runtime_cleanup.shutdown();
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let _replica_runtime_cleanup = RuntimeCleanup(replica.clone());
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let _runtime_cleanup = RuntimeCleanup(root.join("state"));
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let _first_runtime_cleanup = RuntimeCleanup(first.clone());
    let second = tmp.path().join("second");
    let _second_runtime_cleanup = RuntimeCleanup(second.clone());
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
fn fr_06_5_oversized_cli_recall_keeps_a_complete_frozen_version_after_later_evolution() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
        contextual_packet["rules"],
        json!([{"applicability":{"scope":["user"]},"items":[{"memory_id":reference["memory_id"],"what_to_remember":body}]}])
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

fn wait_for_recall_index(state: &Path, id: &str) {
    let config = agentlaw_app::config::load(state).unwrap().unwrap();
    let control = agentlaw_flows::control::ControlReader::open(config.runtime_root(state)).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (_, _, exact, _) = control.active_projection().unwrap().unwrap();
        let index = rusqlite::Connection::open_with_flags(
            exact,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let indexed: bool = index
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM exact_heads WHERE id = ?1)",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        if indexed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Memory was not indexed"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn recall_closure_failure_returns_only_common_error_fields() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("store");
    let _cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &source);
    let body = "Canonical recall body integrity marker.";
    let (out, saved) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({
            "action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed fixture","applies_to":["user"]}]}
        })),
    );
    assert!(out.status.success(), "{saved}");
    assert_eq!(saved["status"], "remembered", "{saved}");
    let id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();

    wait_for_recall_index(&state, id);

    let path = source
        .join("current/memory")
        .join(&id[..2])
        .join(format!("{id}.md"));
    let mut bytes = std::fs::read(&path).unwrap();
    let offset = bytes
        .windows(body.len())
        .position(|part| part == body.as_bytes())
        .unwrap();
    bytes[offset] = b'X';
    std::fs::write(&path, bytes).unwrap();

    let request = json!({"action":"recall","recall":{"memory_ids":[id]}});
    let mut mcp = McpFixture::start(&state);
    mcp.initialize();
    let rpc = mcp.call(2, request.clone());
    let (out, cli) = run(&state, &["call", "--json", "-"], Some(request));
    assert_eq!(rpc["result"]["isError"], true, "FR-06.4: {rpc}");
    assert!(!out.status.success(), "FR-06.4: {cli}");
    for error in [&cli, &rpc["result"]["structuredContent"]] {
        assert_eq!(error["code"], "source_corrupt", "FR-06.4: {error}");
        assert_eq!(error["retryable"], false);
        let reason = error["message"].as_str().unwrap().to_lowercase();
        assert!(
            ["integrity", "corrupt", "checksum", "hash"]
                .iter()
                .any(|term| reason.contains(term)),
            "{error}"
        );
        let action = error["next_action"].as_str().unwrap().to_lowercase();
        for term in ["preserve", "doctor --audit", "restor"] {
            assert!(action.contains(term), "{error}");
        }
        assert!(
            action.contains("recall") || action.contains("query"),
            "{error}"
        );
        assert_eq!(error.as_object().unwrap().len(), 4, "FR-06.4: {error}");
    }
}

#[cfg(windows)]
struct SourceReadDenial<'a> {
    path: &'a Path,
    principal: String,
    active: bool,
}

#[cfg(windows)]
impl<'a> SourceReadDenial<'a> {
    fn apply(path: &'a Path) -> Self {
        // Inherit the denial to child directories so catalog enumeration fails.
        let rights = if path.is_dir() { "(CI)(RX)" } else { "R" };
        Self::apply_rights(path, rights)
    }

    fn apply_rights(path: &'a Path, rights: &str) -> Self {
        let identity = std::process::Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .unwrap();
        assert!(identity.status.success(), "{identity:?}");
        let identity = String::from_utf8_lossy(&identity.stdout);
        let sid = identity
            .trim()
            .rsplit(',')
            .next()
            .unwrap()
            .trim_matches('"');
        assert!(sid.starts_with("S-1-"), "Missing current Windows user SID");
        let guard = Self {
            path,
            principal: format!("*{sid}"),
            active: true,
        };
        let output = std::process::Command::new("icacls")
            .arg(path)
            .args(["/deny", &format!("{}:{rights}", guard.principal)])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        guard
    }

    fn restore(&mut self) -> std::io::Result<()> {
        let output = std::process::Command::new("icacls")
            .arg(self.path)
            .args(["/remove:d", &self.principal])
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!("{output:?}")));
        }
        self.active = false;
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for SourceReadDenial<'_> {
    fn drop(&mut self) {
        if self.active {
            if let Err(error) = self.restore() {
                eprintln!("Failed to restore test source ACL: {error}");
            }
        }
    }
}

#[cfg(any(unix, windows))]
#[test]
fn recall_source_error_payloads_give_cause_specific_actions() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("store");
    let _cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &source);
    let (out, saved) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({
            "action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Readable canonical record","evidence":"Observed fixture","applies_to":["user"]}]}
        })),
    );
    assert!(out.status.success(), "{saved}");
    assert_eq!(saved["status"], "remembered", "{saved}");
    let id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    wait_for_recall_index(&state, id);
    let path = source
        .join("current/memory")
        .join(&id[..2])
        .join(format!("{id}.md"));
    #[cfg(unix)]
    let permissions = std::fs::metadata(&path).unwrap().permissions();
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0)).unwrap();
    #[cfg(windows)]
    let mut denial = SourceReadDenial::apply(&path);
    let readable = std::fs::File::open(&path);
    #[cfg(unix)]
    if readable.is_ok() {
        std::fs::set_permissions(&path, permissions).unwrap();
        panic!("FR-06.4: test precondition failed: this process bypasses file read permissions");
    }
    assert_eq!(
        readable.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let request = json!({"action":"recall","recall":{"memory_ids":[id]}});
    let (out, error) = run(&state, &["call", "--json", "-"], Some(request.clone()));
    #[cfg(unix)]
    std::fs::set_permissions(&path, permissions).unwrap();
    #[cfg(windows)]
    denial.restore().unwrap();
    assert!(!out.status.success(), "{error}");
    assert_eq!(error["code"], "source_operation_failed", "FR-06.4: {error}");
    assert_eq!(error["retryable"], false);
    assert_eq!(error.as_object().unwrap().len(), 4, "{error}");
    assert!(!error["message"].as_str().unwrap().is_empty());
    let action = error["next_action"].as_str().unwrap().to_lowercase();
    assert!(action.contains("permission"), "{error}");
    assert!(
        action.contains("recall") || action.contains("query"),
        "{error}"
    );
    let (out, recovered) = run(&state, &["call", "--json", "-"], Some(request));
    assert!(out.status.success(), "{recovered}");
    assert_eq!(
        recovered["memories"][0]["current_heads"][0]["what_to_remember"],
        "Readable canonical record"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn catalog_discovery_distinguishes_absence_from_access_denied() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("store");
    let _cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &source);
    let bound = temp.path().join("bound");
    let unbound = temp.path().join("unbound");
    std::fs::create_dir(&bound).unwrap();
    std::fs::create_dir(&unbound).unwrap();
    let call = |input| run(&state, &["call", "--json", "-"], Some(input));
    let discover = json!({"action":"connect_project_memory","connect_project_memory":{"intent":"discover","project_path":unbound}});
    let recall = json!({"action":"recall","recall":{"project_path":unbound,"recall_for":"Resume this project","include_active_tasks":true}});
    let catalog = source.join("catalog");
    let projects = catalog.join("projects");
    assert_eq!(
        std::fs::read_dir(&projects).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    let (discovery_status, empty_discovery) = call(discover.clone());
    let (out, result) = call(recall.clone());
    assert!(out.status.success(), "{result}");
    assert_eq!(result["status"], "needs_user_input");
    assert_eq!(result["code"], "project_connection_required");
    assert_eq!(result["candidates"], json!([]));
    let guidance = result["turn_instruction"].as_str().unwrap().to_lowercase();
    assert!(
        guidance.contains("project")
            && (guidance.contains("select")
                || guidance.contains("choose")
                || guidance.contains("connect")),
        "{result}"
    );

    let project_name = "Catalog project";
    let (out, connected) = call(
        json!({"action":"connect_project_memory","connect_project_memory":{"intent":"create","project_path":bound,"project_name":project_name}}),
    );
    assert!(out.status.success(), "{connected}");
    let id = connected["project_connection"]["project_id"]
        .as_str()
        .unwrap();
    {
        #[cfg(windows)]
        let mut access = SourceReadDenial::apply(&catalog);
        #[cfg(unix)]
        let restore = {
            use std::os::unix::fs::PermissionsExt;
            struct RestoreCatalog<'a>(&'a Path, std::fs::Permissions);
            impl Drop for RestoreCatalog<'_> {
                fn drop(&mut self) {
                    std::fs::set_permissions(self.0, self.1.clone())
                        .expect("Restore catalog permissions");
                }
            }
            let original = std::fs::metadata(&catalog).unwrap().permissions();
            std::fs::set_permissions(
                &catalog,
                std::fs::Permissions::from_mode(original.mode() & !0o111),
            )
            .unwrap();
            RestoreCatalog(&catalog, original)
        };
        let opening = std::fs::read_dir(&projects);
        assert!(
            matches!(opening, Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied),
            "FR-06.4: test precondition failed: catalog denial did not produce PermissionDenied"
        );
        for request in [discover.clone(), recall.clone()] {
            let (out, failure) = call(request);
            assert!(!out.status.success(), "{failure}");
            assert_eq!(
                failure["code"], "source_operation_failed",
                "FR-06.4: {failure}"
            );
            assert_eq!(failure["retryable"], false);
            assert!(failure.get("candidates").is_none());
            assert!(failure.get("partial_recall_error").is_none());
        }
        let output_path = temp.path().join("procedure-output.jsonl");
        for (operation, expected) in [
            ("search", "source_operation_failed"),
            ("list", "inventory_failed"),
        ] {
            let mut command = command(&state);
            command
                .args([
                    "learned-procedure",
                    operation,
                    "--project",
                    project_name,
                    "--output",
                ])
                .arg(&output_path);
            if operation == "search" {
                command.args(["--query", "release"]);
            }
            let out = command.output().unwrap();
            assert!(!out.status.success(), "{out:?}");
            let error: Value = serde_json::from_slice(&out.stderr).unwrap();
            assert_eq!(error["code"], expected, "FR-06.4 CT-04g: {error}");
            if operation == "search" {
                assert_eq!(error["retryable"], false);
                let action = error["next_action"].as_str().unwrap().to_lowercase();
                assert!(
                    action.contains("search")
                        && (action.contains("access") || action.contains("permission")),
                    "{error}"
                );
            }
        }
        #[cfg(windows)]
        access.restore().unwrap();
        #[cfg(unix)]
        drop(restore);
    }
    for request in [discover, recall] {
        let (out, restored) = call(request);
        assert!(out.status.success(), "{restored}");
        assert!(
            restored["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|candidate| candidate["project_id"] == id),
            "{restored}"
        );
    }
    assert!(discovery_status.status.success(), "{empty_discovery}");
    assert!(
        empty_discovery
            .get("candidates")
            .is_none_or(|candidates| candidates.as_array().is_some_and(Vec::is_empty)),
        "FR-03.2: {empty_discovery}"
    );
    assert!(empty_discovery["next_action"]
        .as_str()
        .is_some_and(|line| line.to_lowercase().contains("connect")));
}

#[test]
fn recall_capacity_payload_instructs_retry_after_index_catch_up() {
    use agentlaw_flows::exact::{
        DeltaBudget, ExactIndexWriter, ExactSnapshot, ProjectionGenerations,
    };
    use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let source = store.owned_published_reader();
    let projection = temp.path().join("projection");
    let control = agentlaw_flows::RequestControl::default();
    let epoch = source.read_snapshot().unwrap().position().epoch.clone();
    let shadow = ExactIndexWriter::rebuild_shadow_in(&projection, &source, &control).unwrap();
    let generations = ProjectionGenerations::open_for_recovery(
        "bootstrap",
        &epoch,
        projection.join("exact-bootstrap.sqlite"),
        projection.join("lexical-bootstrap.sqlite"),
    )
    .unwrap();
    generations
        .commit(generations.stage(shadow).unwrap())
        .unwrap();
    let id = || uuid::Uuid::new_v4().to_string();
    store.publish(&id(), vec![Mutation {
        unit: CurrentUnit {
            entity_id: id(),
            entity_type: "memory".into(),
            state: UnitState::Live { heads: vec![Head {
                metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":id(),"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: "An unindexed published memory".into(),
            }] },
        },
        expected_versions: vec![],
        evidence: "Published fixture".into(),
    }]).unwrap();
    let guard = source.read_snapshot().unwrap();
    let budget = DeltaBudget {
        max_identities: 0,
        max_resident_bytes: 0,
    };
    let ordinary = ExactSnapshot::build(generations.pin().unwrap(), &guard, budget)
        .err()
        .unwrap();
    let generic = agentlaw_app::error_payload(&ordinary);
    assert_eq!(generic["code"], "capacity");
    assert_eq!(generic["retryable"], false);
    let generic_action = generic["next_action"].as_str().unwrap().to_lowercase();
    assert!(
        !generic_action.contains("index") && !generic_action.contains("recall"),
        "{generic}"
    );

    let recall = ExactSnapshot::build_for_recall(generations.pin().unwrap(), &guard, budget)
        .err()
        .unwrap();
    let payload = agentlaw_app::error_payload(&recall);
    assert_eq!(payload["code"], "capacity", "FR-06.4: {payload}");
    assert_eq!(payload["retryable"], true);
    assert_eq!(payload.as_object().unwrap().len(), 4);
    let action = payload["next_action"].as_str().unwrap().to_lowercase();
    assert!(
        action.contains("index") && action.contains("recall"),
        "{payload}"
    );
    assert!(
        action.contains("then") || action.contains("after") || action.contains("once"),
        "{payload}"
    );
}

#[test]
fn fr_10_4_failed_recall_file_is_a_tool_error() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let _cleanup = RuntimeCleanup(state.clone());
    initialize(&state, &temp.path().join("store"));
    let (out, config) = run(
        &state,
        &["config", "set", "response_limit_bytes", "4096"],
        None,
    );
    assert!(out.status.success(), "{config}");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let call = |request| run(&state, &["call", "--json", "-"], Some(request));
    let (out, connected) = call(
        json!({"action":"connect_project_memory","connect_project_memory":{"intent":"create","project_path":project,"project_name":"Release work"}}),
    );
    assert!(out.status.success(), "{connected}");
    let task_body = format!("# Objective\n{}\n# Current position\nReviewing\n# Resume point\nRun the checks\n# References\nDesign notes", "Verify the release. ".repeat(320));
    let (out, saved) = call(
        json!({"action":"remember_this","remember_this":{"project_path":project,"memories":[{"operation":"create","what_to_remember":task_body,"evidence":"Current release work","applies_to":["project"],"in_working_set":true}]}}),
    );
    assert!(out.status.success(), "{saved}");
    assert_eq!(saved["status"], "remembered", "{saved}");
    let task_id = &saved["results"][0]["memory_ref"]["memory_id"];
    assert!(task_id.is_string());
    let task = json!({"action":"recall","recall":{"project_path":project,"include_active_tasks":true,"recall_for":"Resume pending work"}});
    let (out, envelope) = call(task.clone());
    assert!(out.status.success(), "{envelope}");
    assert_eq!(envelope["code"], "complete_content_in_file");
    let artifact = Path::new(envelope["artifact"]["path"].as_str().unwrap());
    let directory = artifact.parent().unwrap();
    assert!(directory
        .canonicalize()
        .unwrap()
        .starts_with(temp.path().canonicalize().unwrap()));

    let (out, rule) = call(
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Check published policies. ".repeat(280),"evidence":"Agreed standing instruction","applies_to":["user"],"is_rule":true}]}}),
    );
    assert!(out.status.success(), "{rule}");
    assert_eq!(rule["status"], "remembered", "{rule}");
    std::fs::rename(directory, temp.path().join("saved-response-artifacts")).unwrap();
    std::fs::write(directory, "A file blocks recreation of this directory").unwrap();
    let ordinary = json!({"action":"recall","recall":{"recall_for":"Check unrelated work"}});

    let unbound = json!({"action":"recall","recall":{"project_path":temp.path().join("unbound"),"recall_for":"Check unrelated work","include_active_tasks":true}});
    let mut mcp = McpFixture::start(&state);
    mcp.initialize();
    let expected_text = |value: &Value| value.as_str().is_some_and(|text| !text.trim().is_empty());
    for (number, (request, expected_id)) in
        [(task, Some(task_id)), (ordinary, None), (unbound, None)]
            .into_iter()
            .enumerate()
    {
        let (out, cli) = call(request.clone());
        let rpc = mcp.call(2 + number as u64, request);
        assert!(!out.status.success(), "{cli}");
        assert_eq!(rpc["result"]["isError"], true, "{rpc}");
        for payload in [&cli, &rpc["result"]["structuredContent"]] {
            assert_eq!(payload["code"], "result_delivery_failed", "{payload}");
            assert!(expected_text(&payload["message"]));
            assert!(payload["retryable"].is_boolean());
            assert!(expected_text(&payload["next_action"]));
            for field in [
                "artifact",
                "memories",
                "rules",
                "active_task_handoff",
                "partial_recall",
            ] {
                assert!(payload.get(field).is_none(), "{payload}");
            }
            let errors: Vec<_> = payload["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|detail| detail.get("memory_id").is_some())
                .collect();
            if let Some(id) = expected_id {
                assert_eq!(errors.len(), 1, "{payload}");
                assert_eq!(&errors[0]["memory_id"], id);
                assert!(expected_text(&errors[0]["reason"]));
                assert!(
                    errors[0]["next_action"]
                        .as_str()
                        .is_some_and(|text| text.to_lowercase().contains("recall")),
                    "{payload}"
                );
            } else {
                assert!(errors.is_empty(), "{payload}");
            }
        }
    }
}

#[test]
fn fr_06_5_aggregate_contextual_rules_and_required_reference_survive_response_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let _initial_runtime_cleanup = RuntimeCleanup(state.clone());
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
    let (_, target) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Traceability notes for release verification","evidence":"Candidate fixture","applies_to":["user"]}]}}),
        ),
    );
    assert_eq!(target["status"], "remembered", "{target}");
    let target_id = &target["results"][0]["memory_ref"]["memory_id"];
    let mut source_ids = Vec::new();
    for candidate_body in ["unrelated task alpha", "unrelated task bravo"] {
        let (_, saved) = run(
            &state,
            &["call", "--json", "-"],
            Some(
                json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":candidate_body,"evidence":"Candidate fixture","applies_to":["user"],"related_memory_ids":[target_id]}]}}),
            ),
        );
        assert_eq!(saved["status"], "remembered", "{saved}");
        source_ids.push(saved["results"][0]["memory_ref"]["memory_id"].clone());
    }
    let assert_candidate_paths = |content: &Value| {
        let candidates = content["candidates"].as_array().unwrap();
        let expected_sources: Vec<_> = candidates
            .iter()
            .filter_map(|candidate| {
                let id = &candidate["memory_id"];
                source_ids.contains(id).then_some(id.clone())
            })
            .collect();
        assert_eq!(expected_sources.len(), 2);
        for candidate in candidates
            .iter()
            .filter(|candidate| source_ids.contains(&candidate["memory_id"]))
        {
            let paths = candidate["retrieval_paths"].as_array().unwrap();
            assert_eq!(paths.len(), 1);
            assert_eq!(paths[0]["via"], json!(["lexical"]));
            assert!(paths[0].get("source_memory_ids").is_none());
        }
        let target = candidates
            .iter()
            .find(|candidate| candidate["memory_id"] == *target_id)
            .unwrap();
        let paths = target["retrieval_paths"].as_array().unwrap();
        let related: Vec<_> = paths
            .iter()
            .filter(|path| path["via"] == json!(["related_memory"]))
            .collect();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0]["clue"], *target_id);
        assert_eq!(related[0]["source_memory_ids"], json!(expected_sources));
        assert!(related[0].get("source_memory_id").is_none());
    };
    let (_, response) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"recall","recall":{"recall_for":"unrelated task","memory_candidate_limit":3}}),
        ),
    );
    assert_eq!(response["code"], "complete_content_in_file", "{response}");
    assert_eq!(response["content_read"], false);
    let bytes = std::fs::read(response["artifact"]["path"].as_str().unwrap()).unwrap();
    assert_eq!(response["artifact"]["bytes"], bytes.len());
    let packet: Value = serde_json::from_slice(&bytes).unwrap();
    let assert_mandatory = |packet: &Value| {
        let memories = packet["memories"].as_array().unwrap();
        assert_eq!(memories.len(), 1);
        let expected = &reference["results"][0]["memory_ref"];
        assert_eq!(memories[0]["memory_id"], expected["memory_id"]);
        assert_eq!(memories[0]["current_heads"][0]["memory_ref"], *expected);
        assert_eq!(
            memories[0]["current_heads"][0]["what_to_remember"],
            reference_body
        );
        let groups = packet["rules"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["applicability"], json!({"scope":["user"]}));
        let items = groups[0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        for (saved, body) in [(&first, &first_body), (&second, &second_body)] {
            assert!(items.contains(&json!({"memory_id":saved["results"][0]["memory_ref"]["memory_id"],"what_to_remember":body})));
        }
    };
    assert_mandatory(&packet);
    assert!(response["next_action"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert_eq!(
        packet["candidate_counts"]["memories"],
        json!({"matched":3,"shown":3})
    );
    assert_candidate_paths(&packet);
    assert!(packet
        .get("undelivered_required")
        .is_none_or(|v| v.as_array().unwrap().is_empty()));
    let (_, unbound) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"recall","recall":{"project_path":"C:/work/unbound-aggregate","recall_for":"unrelated task","include_active_tasks":true,"memory_candidate_limit":3}}),
        ),
    );
    assert_eq!(unbound["code"], "complete_content_in_file", "{unbound}");
    let unbound_packet: Value = serde_json::from_slice(
        &std::fs::read(unbound["artifact"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(unbound_packet["status"], "needs_user_input");
    assert_eq!(unbound_packet["code"], "project_connection_required");
    assert_ne!(
        unbound_packet["partial_recall"]["code"], "complete_content_in_file",
        "partial recall must be inside the one delivered file"
    );
    assert_candidate_paths(&unbound_packet["partial_recall"]);
    assert_mandatory(&unbound_packet["partial_recall"]);
    assert_eq!(unbound["content_read"], false);
    assert!(unbound["next_action"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(unbound_packet["turn_instruction"]
        .as_str()
        .unwrap()
        .contains("Project rules, active Tasks and work targets have not been checked"));
}

struct IdentityFrontend {
    runtime: RuntimeCleanup,
    temp: tempfile::TempDir,
    state: std::path::PathBuf,
    source: std::path::PathBuf,
}

impl IdentityFrontend {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let source = temp.path().join("store");
        let runtime = RuntimeCleanup(state.clone());
        initialize(&state, &source);
        Self {
            runtime,
            temp,
            state,
            source,
        }
    }

    fn call(&self, action: &str, fields: Value) -> (Output, Value) {
        run(
            &self.state,
            &["call", "--json", "-"],
            Some(json!({"action": action, (action): fields})),
        )
    }

    fn success(&self, action: &str, fields: Value) -> Value {
        let (out, result) = self.call(action, fields);
        assert!(out.status.success(), "{action}: {result}");
        result
    }

    fn memory(&self, body: &str, scope: &str, rule: bool) -> Value {
        self.success(
            "remember_this",
            json!({"memories":[{
                "operation":"create", "what_to_remember":body, "applies_to":[scope],
                "evidence":"Observed", "is_rule":rule
            }]}),
        )["results"][0]["memory_ref"]
            .clone()
    }

    fn procedure(&self, memory_id: &Value) -> Value {
        let mut proposal = json!({"operation":"create","evidence_memory_ids":[memory_id]});
        let prepared = self.success("remember_this", json!({"procedure":proposal}));
        proposal["applies_to"] = json!(["machine"]);
        proposal["name"] = json!("Verify deployment");
        proposal["use_when"] = json!("During deployment");
        proposal["instructions"] = json!("Check the deployment results.");
        proposal["evidence"] = json!("Verified in deployment");
        self.success(
            "remember_this",
            json!({"procedure":proposal,"authoring_ref":prepared["authoring_ref"]}),
        )["procedure_ref"]
            .clone()
    }

    fn current_path(&self, reference: &Value) -> std::path::PathBuf {
        let id = reference["memory_id"].as_str().unwrap();
        self.source
            .join("current/memory")
            .join(&id[..2])
            .join(format!("{id}.md"))
    }
}

#[test]
fn fr_01_5_mixed_recall_resolves_memory_and_procedure_prefixes() {
    // ID-01
    let f = IdentityFrontend::new();
    let memory = f.memory("Check the deployment logs", "user", false);
    let other = f.memory("Record deployment time", "user", false);
    let rule = f.memory("Follow the release checklist", "user", true);
    let procedure = f.procedure(&memory["memory_id"]);
    wait_for_recall_index(&f.state, rule["memory_id"].as_str().unwrap());
    let mut request = json!({"recall_for":"deployment","memory_ids":[memory["memory_id"],other["memory_id"]],"procedure_ids":[procedure["procedure_id"]]});
    let full = f.success("recall", request.clone());
    request["memory_ids"][0] = json!(&memory["memory_id"].as_str().unwrap()[..8]);
    request["procedure_ids"][0] = json!(&procedure["procedure_id"].as_str().unwrap()[..8]);
    let short = f.success("recall", request);
    for field in [
        "memories",
        "learned_procedures",
        "rules",
        "candidate_counts",
    ] {
        assert_eq!(short[field], full[field], "FR-01.5 ID-01: {field}");
    }
    assert!(short["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["current_heads"][0]["memory_ref"] == memory
            && row["current_heads"][0]["what_to_remember"] == "Check the deployment logs"));
    assert!(short["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["current_heads"][0]["memory_ref"] == other));
    assert!(short["learned_procedures"]
        .to_string()
        .contains(procedure["procedure_id"].as_str().unwrap()));
    assert!(short["learned_procedures"]
        .to_string()
        .contains("Check the deployment results."));
    assert!(short["rules"]
        .to_string()
        .contains(rule["memory_id"].as_str().unwrap()));
}

#[test]
fn fr_01_5_uppercase_prefix_retrieves_memory_outside_project_scope() {
    // ID-02
    let f = IdentityFrontend::new();
    let project = f.temp.path().join("release");
    std::fs::create_dir(&project).unwrap();
    f.success(
        "connect_project_memory",
        json!({"intent":"create","project_path":project,"project_name":"Release"}),
    );
    let saved = f.success("remember_this", json!({"project_path":project,"memories":[{
        "operation":"create","what_to_remember":"Release verification","evidence":"Observed","applies_to":["project"]
    }]}));
    let reference = &saved["results"][0]["memory_ref"];
    let id = reference["memory_id"].as_str().unwrap();
    let full = f.success("recall", json!({"project_path":project,"memory_ids":[id]}));
    let prefix = id.replace('-', "")[..9].to_uppercase();
    let outside = f.success("recall", json!({"memory_ids":[prefix]}));
    assert_eq!(outside["memories"], full["memories"], "FR-01.5 ID-02");
    assert_eq!(
        outside["memories"][0]["current_heads"][0]["memory_ref"],
        *reference
    );
    assert!(outside.get("candidate_counts").is_none());
}

fn assert_id_recall_error(
    out: &Output,
    error: &Value,
    code: &str,
    path: &str,
    value: &str,
) -> String {
    assert!(!out.status.success(), "FR-01.5: {error}");
    assert_eq!(error["code"], code, "{error}");
    assert_eq!(error["retryable"], false);
    let details = error["errors"].as_array().unwrap();
    assert!(
        details
            .iter()
            .any(|entry| entry["path"] == path && entry["provided_value"] == value),
        "{error}"
    );
    for field in [
        "status",
        "memories",
        "learned_procedures",
        "rules",
        "candidate_counts",
        "artifact",
    ] {
        assert!(error.get(field).is_none(), "{error}");
    }
    error["next_action"].as_str().unwrap().to_lowercase()
}

#[test]
fn fr_01_5_malformed_identity_inputs_identify_the_submitted_value() {
    // ID-03
    let f = IdentityFrontend::new();
    for value in [
        "abcdef1",
        "0123456789abcdef0123456789abcdef0",
        "abcdef1g",
        "abcd-ef12",
        "abcdef12-3456-1345-8234-abcdef123456",
        "abcdef12-3456-4345-1234-abcdef123456",
    ] {
        let (out, error) = f.call("recall", json!({"memory_ids":[value]}));
        let action = assert_id_recall_error(&out, &error, "invalid_input", "/memory_ids/0", value);
        assert!(
            action.contains("uuid") || action.contains("prefix"),
            "{error}"
        );
    }
}

#[test]
fn fr_01_5_unmatched_prefix_rejects_the_whole_mixed_recall() {
    // ID-04
    let f = IdentityFrontend::new();
    let existing = f.memory("Deployment notes", "user", false);
    let id = existing["memory_id"].as_str().unwrap();
    let absent = if id.starts_with("aaaaaaaa") {
        "bbbbbbbb"
    } else {
        "aaaaaaaa"
    };
    let (out, error) = f.call(
        "recall",
        json!({"recall_for":"deployment","memory_ids":[id,absent]}),
    );
    let action = assert_id_recall_error(&out, &error, "not_found", "/memory_ids/1", absent);
    assert!(
        action.contains("id")
            && (action.contains("lookup")
                || action.contains("look up")
                || action.contains("search")
                || action.contains("recall")
                || action.contains("find")),
        "{error}"
    );
}

#[test]
fn fr_01_5_history_compact_uuid_selects_the_same_versions() {
    // ID-07
    let f = IdentityFrontend::new();
    let memory = f.memory("Record deployment time", "user", false);
    let procedure = f.procedure(&memory["memory_id"]);
    for (field, reference) in [("memory_id", memory), ("procedure_id", procedure)] {
        let id = reference[field].as_str().unwrap();
        let mut request = json!({(field): id, "context_layers": 2});
        let canonical = f.success("history", request.clone());
        request[field] = json!(id.replace('-', ""));
        let compact = f.success("history", request);
        assert_eq!(compact, canonical, "FR-01.5 ID-07: {field}");
        assert!(!canonical["latest_changes"].as_array().unwrap().is_empty());
    }
}

#[test]
fn fr_01_5_missing_canonical_id_preserves_the_selected_body() {
    // ID-08
    let f = IdentityFrontend::new();
    let reference = f.memory("Record deployment time", "user", false);
    let missing = if reference["memory_id"] == "11111111-1111-4111-8111-111111111111" {
        "22222222-2222-4222-8222-222222222222"
    } else {
        "11111111-1111-4111-8111-111111111111"
    };
    let result = f.success(
        "recall",
        json!({"memory_ids":[reference["memory_id"],missing]}),
    );
    assert_eq!(
        result["code"], "exact_selection_incomplete",
        "FR-01.5 ID-08"
    );
    assert_eq!(
        result["missing_ids"],
        json!({"memory_ids":[missing],"procedure_ids":[]})
    );
    assert_eq!(
        result["memories"][0]["current_heads"][0]["memory_ref"],
        reference
    );
    assert_eq!(
        result["memories"][0]["current_heads"][0]["what_to_remember"],
        "Record deployment time"
    );
}

#[test]
fn fr_01_6_missing_evidence_identifies_required_field_on_cli_and_mcp() {
    // ER-04
    let f = IdentityFrontend::new();
    let reference = f.memory("Original deployment notes", "user", false);
    let fields = json!({"memories":[{"operation":"evolve","parent_refs":[reference],"what_to_remember":"Revision"}]});
    let (out, cli) = f.call("remember_this", fields.clone());
    assert!(!out.status.success(), "FR-01.6 ER-04: {cli}");
    let mut mcp = McpFixture::start(&f.state);
    mcp.initialize();
    let rpc = mcp.call(2, json!({"action":"remember_this","remember_this":fields}));
    assert_eq!(rpc["result"]["isError"], true, "{rpc}");
    for error in [&cli, &rpc["result"]["structuredContent"]] {
        let detail = error["errors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == "/memories/0/evidence")
            .unwrap_or_else(|| panic!("{error}"));
        assert!(detail.get("provided_value").is_none(), "{detail}");
        assert!(
            detail.to_string().to_lowercase().contains("required"),
            "{detail}"
        );
    }
    let current = f.success("recall", json!({"memory_ids":[reference["memory_id"]]}));
    assert_eq!(
        current["memories"][0]["current_heads"][0]["memory_ref"],
        reference
    );
}

#[test]
fn fr_01_6_audit_preserves_truncated_canonical_memory() {
    // ER-09
    let f = IdentityFrontend::new();
    let body = "Check the length of the published deployment record.";
    let reference = f.memory(body, "user", false);
    let path = f.current_path(&reference);
    let original = std::fs::read(&path).unwrap();
    let start = original
        .windows(body.len())
        .position(|bytes| bytes == body.as_bytes())
        .unwrap();
    let footer = b"\n<!-- /agentlaw-record-v1 -->\n";
    let footer_start = start + body.len();
    assert_eq!(&original[footer_start..footer_start + footer.len()], footer);
    for length in [footer_start + footer.len() / 2, start + body.len() / 2] {
        let truncated = &original[..length];
        std::fs::write(&path, truncated).unwrap();
        let error = failed_source_audit(&f.state);
        let message = error["message"].as_str().unwrap().to_lowercase();
        assert!(
            ["unexpected eof", "unexpected end", "end of file", "truncat"]
                .iter()
                .any(|cause| message.contains(cause)),
            "{error}"
        );
        assert_eq!(error["code"], "source_corrupt", "{error}");
        assert_eq!(error["retryable"], false);
        let action = error["next_action"].as_str().unwrap().to_lowercase();
        assert!(
            action.contains("preserv") && action.contains("restor") && action.contains("cop"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), truncated);
    }
}

#[test]
fn fr_01_6_unparseable_installation_config_names_the_configuration() {
    // ER-11
    let f = IdentityFrontend::new();
    f.runtime.shutdown();
    let config = f.state.join("config.json");
    assert!(config.is_file());
    std::fs::write(&config, b"{\"response_limit_bytes\":").unwrap();
    let (out, error) = run(&f.state, &["config", "get", "response_limit_bytes"], None);
    assert!(!out.status.success(), "FR-01.6 ER-11: {error}");
    assert_eq!(error["code"], "invalid_configuration", "{error}");
    assert_configuration_path(&error, &config);
    assert_eq!(
        std::fs::read(config).unwrap(),
        b"{\"response_limit_bytes\":"
    );
}

fn assert_configuration_path(error: &Value, config: &Path) {
    let message = error["message"].as_str().unwrap();
    assert!(message.contains(config.to_str().unwrap()), "{error}");
}

fn failed_source_audit(state: &Path) -> Value {
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };
    let started = now_ms();
    let out = run_command_output(state, &["doctor", "--audit"], None);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let error: Value = serde_json::from_slice(&out.stderr)
        .unwrap_or_else(|error| panic!("Invalid audit stderr: {error}; {out:?}"));
    let observed = error["observed_at_ms"].as_u64().unwrap();
    assert!((started..=now_ms()).contains(&observed), "{error}");
    let health = &error["worker_runtime"];
    assert_eq!(health["status"], "running", "{health}");
    assert!(
        health["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|component| component["kind"] == "host" && component["state"] == "ready"),
        "{health}"
    );
    assert!(health["activity"].is_object(), "{health}");
    error
}

fn frontend_with_colliding_identities() -> (IdentityFrontend, [(&'static str, &'static str); 2]) {
    use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let source = temp.path().join("store");
    let _runtime = RuntimeCleanup(state.clone());
    let store = Store::open(&source, temp.path().join("publisher")).unwrap();
    let machine = uuid::Uuid::new_v4().to_string();
    let entries = [
        (
            "abcdef12-a000-4000-8000-000000000001",
            "Check deployment logs",
        ),
        (
            "abcdef12-b000-4000-8000-000000000002",
            "Review office supplies",
        ),
    ];
    store.publish(&uuid::Uuid::new_v4().to_string(), entries.iter().map(|(id, body)| Mutation {
        unit: CurrentUnit {
            entity_id: (*id).into(), entity_type:"memory".into(),
            state: UnitState::Live { heads: vec![Head {
                metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},
                    "origin":{"machine_id":machine,"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: (*body).into(),
            }] },
        },
        expected_versions:vec![], evidence:"Observed test publication".into(),
    }).collect()).unwrap();
    drop(store);
    let (out, connected) = run(
        &state,
        &["store", "connect", "--path", source.to_str().unwrap()],
        None,
    );
    assert!(out.status.success(), "{connected}");
    (
        IdentityFrontend {
            runtime: _runtime,
            temp,
            state,
            source,
        },
        entries,
    )
}

#[test]
fn fr_01_5_ambiguous_prefix_is_not_narrowed_by_search_matches() {
    // ID-05
    let (f, entries) = frontend_with_colliding_identities();
    let call = |fields| f.call("recall", fields);
    let query = "deployment";
    wait_for_recall_index(&f.state, entries[0].0);
    let (out, error) = call(json!({"recall_for":query,"memory_ids":["abcdef12"]}));
    let action = assert_id_recall_error(&out, &error, "ambiguous", "/memory_ids/0", "abcdef12");
    assert!(
        action.contains("longer") || action.contains("full"),
        "FR-01.5 ID-05: {error}"
    );
    for (id, body) in entries {
        let (out, result) = call(json!({"memory_ids":[id.replace('-', "")[..9]]}));
        assert!(out.status.success(), "{result}");
        assert_eq!(
            result["memories"][0]["current_heads"][0]["memory_ref"]["memory_id"],
            id
        );
        assert_eq!(
            result["memories"][0]["current_heads"][0]["what_to_remember"],
            body
        );
    }
}

#[cfg(unix)]
struct DeniedRead<'a> {
    path: &'a Path,
    original: std::fs::Permissions,
}

#[test]
fn duplicate_identity_text_reports_the_second_input() {
    // ID-17
    let f = IdentityFrontend::new();
    let (out, error) = f.call("recall", json!({"memory_ids":["abcdef12","abcdef12"]}));
    let correction =
        assert_id_recall_error(&out, &error, "invalid_input", "/memory_ids/1", "abcdef12");
    assert!(
        correction.contains("duplicate")
            || correction.contains("unique")
            || correction.contains("once"),
        "{error}"
    );
}

#[test]
fn unbound_project_reports_ambiguous_explicit_prefix() {
    // ID-18
    let (f, _) = frontend_with_colliding_identities();
    let path = f.temp.path().join("unconnected-project");
    std::fs::create_dir(&path).unwrap();
    let (out, error) = f.call(
        "recall",
        json!({
            "project_path":path, "recall_for":"deployment", "memory_ids":["abcdef12"]
        }),
    );
    assert_id_recall_error(&out, &error, "ambiguous", "/memory_ids/0", "abcdef12");
    for field in [
        "partial_recall",
        "partial_recall_error",
        "project_candidates",
    ] {
        assert!(error.get(field).is_none(), "{error}");
    }
}

#[cfg(unix)]
impl<'a> DeniedRead<'a> {
    fn apply(path: &'a Path) -> Self {
        Self::apply_mode(path, 0)
    }

    fn apply_mode(path: &'a Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let original = std::fs::metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        Self { path, original }
    }
}

#[cfg(unix)]
impl Drop for DeniedRead<'_> {
    fn drop(&mut self) {
        std::fs::set_permissions(self.path, self.original.clone()).unwrap();
    }
}

#[cfg(any(unix, windows))]
#[test]
fn fr_01_6_audit_reports_unreadable_source_without_repairing_it() {
    // ER-10
    let f = IdentityFrontend::new();
    let reference = f.memory("Verify deployment", "user", false);
    let path = f.current_path(&reference);
    let bytes = std::fs::read(&path).unwrap();
    let (error, cause) = {
        #[cfg(unix)]
        let _denial = DeniedRead::apply(&path);
        #[cfg(windows)]
        let _denial = SourceReadDenial::apply(&path);
        let cause = std::fs::File::open(&path).unwrap_err();
        assert_eq!(cause.kind(), std::io::ErrorKind::PermissionDenied);
        (failed_source_audit(&f.state), cause.to_string())
    };
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(error["code"], "diagnostic_io", "{error}");
    assert!(
        error["message"].as_str().unwrap().contains(&cause),
        "{error}"
    );
    assert_eq!(error["retryable"], false);
    let action = error["next_action"].as_str().unwrap().to_lowercase();
    assert!(
        action.contains("access") || action.contains("permission"),
        "{error}"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn fr_01_6_inaccessible_configuration_reports_io_and_preserves_selection() {
    // ER-12
    let f = IdentityFrontend::new();
    f.runtime.shutdown();
    let config = f.state.join("config.json");
    let selected = std::fs::read(&config).unwrap();
    let args = ["config", "get", "response_limit_bytes"];
    let before = run(&f.state, &args, None);
    assert!(before.0.status.success(), "{}", before.1);
    let error = {
        #[cfg(unix)]
        let _denial = DeniedRead::apply(&config);
        #[cfg(windows)]
        let _file_denial = SourceReadDenial::apply(&config);
        assert_eq!(
            std::fs::File::open(&config).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let (out, error) = run(&f.state, &args, None);
        assert!(!out.status.success(), "FR-01.6 ER-12: {error}");
        let mut mcp = McpFixture::start(&f.state);
        mcp.initialize();
        let rpc = mcp.call(
            2,
            json!({"action":"recall","recall":{"recall_for":"Deployment"}}),
        );
        assert_eq!(rpc["result"]["isError"], true, "{rpc}");
        [error, rpc["result"]["structuredContent"].clone()]
    };
    assert_eq!(std::fs::read(&config).unwrap(), selected);
    f.runtime.shutdown();
    let (out, restored) = run(&f.state, &args, None);
    assert!(out.status.success(), "{restored}");
    assert_eq!(restored, before.1);
    for error in error {
        assert_eq!(error["code"], "configuration_io", "{error}");
        assert_configuration_path(&error, &config);
        let action = error["next_action"].as_str().unwrap().to_lowercase();
        assert!(
            action.contains("permission") || action.contains("access"),
            "{error}"
        );
    }
}

#[test]
fn fr_01_6_failed_delivery_lists_only_fully_selected_tasks() {
    // ER-07 and ER-08
    let f = IdentityFrontend::new();
    let (out, config) = run(
        &f.state,
        &["config", "set", "response_limit_bytes", "4096"],
        None,
    );
    assert!(out.status.success(), "{config}");
    let project = f.temp.path().join("release");
    std::fs::create_dir(&project).unwrap();
    f.success(
        "connect_project_memory",
        json!({"intent":"create","project_path":project,"project_name":"Release"}),
    );
    let proposals: Vec<_> = (0..30).map(|number| {
        let objective = format!("Release check {number}. ").repeat(40);
        let body = format!("# Objective\n{objective}\n# Current position\nReviewing\n# Resume point\nRun the checks\n# References\n{}", "Design notes. ".repeat(450));
        json!({"operation":"create","what_to_remember":body,"evidence":"Release review","applies_to":["project"],"in_working_set":true})
    }).collect();
    let mut ids = Vec::new();
    for chunk in proposals.chunks(4) {
        let saved = f.success(
            "remember_this",
            json!({"project_path":project,"memories":chunk}),
        );
        assert_eq!(saved["status"], "remembered", "{saved}");
        ids.extend(
            saved["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["memory_ref"]["memory_id"].clone()),
        );
    }
    f.memory(
        &"Verify the signed deployment record. ".repeat(220),
        "user",
        true,
    );
    let selected = json!({"project_path":project,"include_active_tasks":true,"memory_ids":[ids[0]],"recall_for":"release check","memory_candidate_limit":30});
    let choices = json!({"project_path":project,"include_active_tasks":true,"recall_for":"unrelated subject","memory_candidate_limit":30});
    let mut directory = None;
    for request in [&selected, &choices] {
        let result = f.success("recall", request.clone());
        assert_eq!(
            result["code"], "complete_content_in_file",
            "FR-01.6 ER-07/ER-08: {result}"
        );
        let path = Path::new(result["artifact"]["path"].as_str().unwrap());
        let full: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(full["active_task_count"], ids.len());
        assert!(full.get("active_task_handoff").is_none(), "{full}");
        let candidates = full["candidates"].as_array().unwrap();
        assert!(
            candidates
                .iter()
                .any(|entry| entry["memory_id"] == ids[1] && entry.get("objective").is_some()),
            "{full}"
        );
        if request == &selected {
            assert!(full["memories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["memory_id"] == ids[0]));
        } else {
            assert!(
                full["memories"].as_array().is_none_or(Vec::is_empty),
                "{full}"
            );
        }
        directory = Some(path.parent().unwrap().to_path_buf());
    }
    let directory = directory.unwrap();
    assert!(directory
        .canonicalize()
        .unwrap()
        .starts_with(f.temp.path().canonicalize().unwrap()));
    std::fs::rename(&directory, f.temp.path().join("successful-artifacts")).unwrap();
    std::fs::write(
        &directory,
        "A file blocks recreation of the result directory",
    )
    .unwrap();
    let mut mcp = McpFixture::start(&f.state);
    mcp.initialize();
    for (number, (request, expected)) in [(selected, Some(&ids[0])), (choices, None)]
        .into_iter()
        .enumerate()
    {
        let (out, cli) = f.call("recall", request.clone());
        assert!(!out.status.success(), "{cli}");
        let rpc = mcp.call(
            2 + number as u64,
            json!({"action":"recall","recall":request}),
        );
        assert_eq!(rpc["result"]["isError"], true, "{rpc}");
        for payload in [&cli, &rpc["result"]["structuredContent"]] {
            assert_eq!(
                payload["code"], "result_delivery_failed",
                "FR-01.6 ER-07/ER-08: {payload}"
            );
            assert!(payload.get("artifact").is_none(), "{payload}");
            if let Some(id) = expected {
                let errors = payload["errors"].as_array().unwrap();
                assert_eq!(errors.len(), 1, "{payload}");
                assert_eq!(&errors[0]["memory_id"], id);
                assert!(errors[0]["reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()));
                assert!(errors[0]["next_action"]
                    .as_str()
                    .is_some_and(|action| action.to_lowercase().contains("recall")));
            } else {
                assert!(
                    payload["errors"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .all(|entry| entry.get("memory_id").is_none()),
                    "{payload}"
                );
            }
        }
    }
}

#[test]
fn fr_01_6_failed_write_delivery_leaves_the_committed_version_visible() {
    // ER-13
    assert_write_delivery_failure(false);
    assert_write_delivery_failure(true);
}

fn assert_write_delivery_failure(registry: bool) {
    let f = IdentityFrontend::new();
    let (out, config) = run(
        &f.state,
        &["config", "set", "response_limit_bytes", "4096"],
        None,
    );
    assert!(out.status.success(), "{config}");
    let original = f.memory(&"Original release checklist. ".repeat(300), "user", false);
    let id = &original["memory_id"];
    let directory = f.state.join("response-artifacts");
    let blocked = if registry {
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("response-registry.sqlite");
        std::fs::create_dir(&path).unwrap();
        path
    } else {
        if directory.is_dir() {
            std::fs::rename(&directory, f.temp.path().join("saved-artifacts")).unwrap();
        }
        std::fs::write(
            &directory,
            "A file blocks recreation of the result directory",
        )
        .unwrap();
        directory
    };
    let cause = if registry {
        rusqlite::Connection::open(blocked.canonicalize().unwrap())
            .unwrap_err()
            .to_string()
    } else {
        std::fs::create_dir_all(&blocked).unwrap_err().to_string()
    };

    let mut proposals: Vec<_> = (0..35)
        .map(|index| {
            json!({
                "operation":"create","what_to_remember":format!("Release checkpoint {index}"),
                "evidence":"Observed", "applies_to":["user"]
            })
        })
        .collect();
    proposals.insert(0, json!({
        "operation":"evolve","parent_refs":[original],
        "what_to_remember":"Updated release checklist", "evidence":"Verified the updated checklist"
    }));
    let (out, error) = f.call("remember_this", json!({"memories":proposals}));
    let current = f.success("recall", json!({"memory_ids":[id]}));
    let heads = current["memories"][0]["current_heads"].as_array().unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0]["what_to_remember"], "Updated release checklist");
    assert_ne!(heads[0]["memory_ref"], original);
    if registry {
        std::fs::remove_dir(&blocked).unwrap();
    } else {
        std::fs::remove_file(&blocked).unwrap();
    }
    let history = f.success("history", json!({"memory_id":id,"context_layers":2}));
    let history: Value = if let Some(path) = history["artifact"]["path"].as_str() {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    } else {
        history
    };
    let changes = history["latest_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2, "{history}");
    assert_eq!(
        changes
            .iter()
            .filter(|change| change["evidence"] == "Verified the updated checklist")
            .count(),
        1,
        "{history}"
    );

    assert!(!out.status.success(), "FR-01.6 ER-13: {error}");
    assert_eq!(error["code"], "result_delivery_failed", "{error}");
    assert!(error.get("artifact").is_none());
    let diagnostic = error["message"].as_str().unwrap();
    assert!(diagnostic.contains(&cause), "Expected {cause:?} in {error}");
    assert!(
        diagnostic.contains(blocked.file_name().unwrap().to_str().unwrap()),
        "{error}"
    );
    let diagnostic = diagnostic.to_lowercase();
    if registry {
        assert!(
            diagnostic.contains("open") || diagnostic.contains("insert"),
            "{error}"
        );
    } else {
        assert!(diagnostic.contains("creat"), "{error}");
    }
    let action = error["next_action"].as_str().unwrap().to_lowercase();
    assert!(
        (action.contains("check") || action.contains("confirm"))
            && (action.contains("current") || action.contains("already")),
        "{error}"
    );
}

#[cfg(any(unix, windows))]
fn inspect_publication(f: &IdentityFrontend, operation: &Value) -> (Output, Value) {
    f.call(
        "remember_this",
        json!({"pending_action":"inspect","publication_ref":operation}),
    )
}

#[cfg(any(unix, windows))]
fn pending_prefix_evolution(
    f: &IdentityFrontend,
    verify_response: bool,
) -> (Value, Value, std::path::PathBuf) {
    let reference = f.memory("Preserve the verified release notes.", "user", false);
    let id = reference["memory_id"].as_str().unwrap();
    let local = agentlaw_app::config::load(&f.state)
        .unwrap()
        .unwrap()
        .runtime_root(&f.state);
    let recovery = local.join("canonical/recovery");
    assert!(recovery.is_dir(), "{recovery:?}");
    let operation = {
        #[cfg(unix)]
        let _denial = DeniedRead::apply_mode(&recovery, 0o500);
        #[cfg(windows)]
        let _denial = SourceReadDenial::apply_rights(&recovery, "W");
        assert_eq!(
            std::fs::create_dir(recovery.join("permission-probe"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let result = f.call("remember_this", json!({"memories":[{
            "operation":"evolve","parent_refs":[{"memory_id":&id[..8],"observed_version":reference["observed_version"]}],
            "what_to_remember":"Preserve the signed release notes.","evidence":"Signature verified"
        }]}));
        let (out, error) = result;
        assert!(!out.status.success(), "{error}");
        let db = rusqlite::Connection::open_with_flags(
            local.join("control.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let recorded: String = db
            .query_row("SELECT operation FROM prefix_executions", [], |row| {
                row.get(0)
            })
            .unwrap();
        let operation = json!(recorded);
        if verify_response {
            assert_eq!(
                error["publication_state"], "before_decision",
                "FR-01.6 ER-20: {error}"
            );
            assert_eq!(error["publication_ref"], operation, "{error}");
            assert!(uuid::Uuid::parse_str(&recorded).is_ok());
            assert!(
                error["next_action"]
                    .as_str()
                    .unwrap()
                    .to_lowercase()
                    .contains("inspect"),
                "{error}"
            );
        }
        assert!(!recovery.join(&recorded).exists());
        let current = f.success("recall", json!({"memory_ids":[id]}));
        assert_eq!(
            current["memories"][0]["current_heads"][0]["memory_ref"],
            reference
        );
        operation
    };
    (reference, operation, recovery)
}

#[cfg(any(unix, windows))]
#[test]
fn predecision_io_returns_readable_publication_reference() {
    // ER-20
    let f = IdentityFrontend::new();
    let (reference, operation, recovery) = pending_prefix_evolution(&f, true);
    let probe = tempfile::tempdir_in(&recovery).unwrap();
    probe.close().unwrap();
    let source = f.current_path(&reference);
    let original = std::fs::read(&source).unwrap();
    let operation_dir = recovery.join(operation.as_str().unwrap());
    let (out, inspected) = inspect_publication(&f, &operation);
    assert!(out.status.success(), "{inspected}");
    assert_eq!(inspected["publication_ref"], operation);
    assert_eq!(
        inspected["publication_state"], "before_decision",
        "{inspected}"
    );
    assert!(!operation_dir.join("decision").exists());
    assert!(!operation_dir.join("published").exists());
    assert_eq!(std::fs::read(source).unwrap(), original);
    let db = rusqlite::Connection::open_with_flags(
        recovery
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("control.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let retained: String = db
        .query_row("SELECT operation FROM prefix_executions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(operation, retained);
    drop(db);
    f.runtime.shutdown();
    let found = f.success("recall", json!({"memory_ids":[reference["memory_id"]]}));
    assert_eq!(
        found["memories"][0]["current_heads"][0]["what_to_remember"],
        "Preserve the signed release notes."
    );
    let (out, inspected) = inspect_publication(&f, &operation);
    assert!(out.status.success(), "{inspected}");
    assert_eq!(inspected["publication_ref"], operation);
    assert_eq!(inspected["publication_state"], "published", "{inspected}");
}

#[cfg(any(unix, windows))]
#[test]
fn inaccessible_decision_keeps_intent_and_operation_reference() {
    // ER-21
    let f = IdentityFrontend::new();
    use agentlaw_storage::{Error, FaultPoint, Store};
    let (_, operation, recovery) = pending_prefix_evolution(&f, false);
    let canonical = recovery.parent().unwrap();
    let operation_id = operation.as_str().unwrap();
    let db = rusqlite::Connection::open_with_flags(
        canonical.parent().unwrap().join("control.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let payload: String = db
        .query_row(
            "SELECT payload FROM prefix_executions WHERE operation=?1",
            [operation_id],
            |row| row.get(0),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    let mutations =
        serde_json::from_value(payload["execution"]["Remember"]["retained"]["mutations"].clone())
            .unwrap();
    let store =
        Store::open_with_coordination(&f.source, canonical, f.state.join("source-coordination"))
            .unwrap();
    let fault = store.publish_with_fault(operation_id, mutations, Some(FaultPoint::Decision));
    assert!(
        matches!(fault, Err(Error::Interrupted(FaultPoint::Decision))),
        "{fault:?}"
    );
    let decision = recovery.join(operation_id).join("decision");
    {
        #[cfg(unix)]
        let _denial = DeniedRead::apply(&decision);
        #[cfg(windows)]
        let _denial = SourceReadDenial::apply(&decision);
        assert_eq!(
            std::fs::File::open(&decision).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let (_, observed) = inspect_publication(&f, &operation);
        assert_eq!(
            observed["publication_state"], "decision_unknown",
            "FR-01.6 ER-21: {observed}"
        );
        assert_eq!(observed["publication_ref"], operation);
        let action = observed["next_action"].as_str().unwrap().to_lowercase();
        assert!(
            action.contains("inspect")
                && (action.contains("access") || action.contains("permission")),
            "{observed}"
        );
        let recorded: String = db
            .query_row("SELECT operation FROM prefix_executions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(operation, recorded);
    }
    let (out, inspected) = inspect_publication(&f, &operation);
    assert!(out.status.success(), "{inspected}");
    assert_eq!(inspected["publication_ref"], operation);
    assert_eq!(inspected["publication_state"], "decided", "{inspected}");
}

const RESTART_DIR_ENV: &str = "AGENTLAW_TEST_PREFIX_RESTART_DIRECTORY";
const RESTART_PHASE_ENV: &str = "AGENTLAW_TEST_PREFIX_RESTART_PHASE";
const RESTART_PARENT: &str = "abcdef12-a000-4000-8000-000000000001";
const RESTART_COMPETITOR: &str = "abcdef12-b000-4000-8000-000000000002";
const RESTART_BINDING: &str = "11111111-1111-4111-8111-111111111111";
const RESTART_MACHINE: &str = "22222222-2222-4222-8222-222222222222";

struct RestartFixture {
    store: agentlaw_storage::Store,
    identity: agentlaw_flows::store::StoreIdentity,
}

impl RestartFixture {
    fn open(directory: &Path) -> Self {
        let root = directory.join("source");
        let local = directory.join("local");
        let store = agentlaw_storage::Store::open(&root, local.join("canonical")).unwrap();
        Self {
            store,
            identity: agentlaw_flows::store::StoreIdentity {
                root: root.canonicalize().unwrap(),
                local,
                user_id: "test".into(),
                machine_id: RESTART_MACHINE.into(),
                binding_id: RESTART_BINDING.into(),
                history_limit: 8192,
                response_limit: 65536,
                config_file: None,
            },
        }
    }

    fn writer(&self) -> agentlaw_flows::control::ControlWriter {
        let mut writer =
            agentlaw_flows::control::ControlWriter::open(&self.identity.local).unwrap();
        writer.bind_identity(&self.identity).unwrap();
        writer
            .finish_binding_initialization(&self.identity)
            .unwrap();
        writer
    }

    fn reader(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.identity.local.join("control.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn seed(&self, entity: &str, body: &str) {
        use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
        self.store.publish(&uuid::Uuid::new_v4().to_string(), vec![Mutation {
            unit:CurrentUnit {entity_id:entity.into(),entity_type:"memory".into(),state:UnitState::Live {
                heads:vec![Head {metadata:json!({"change_id":uuid::Uuid::new_v4().to_string(),
                    "applicability":{"scope":"user"},"origin":{"machine_id":RESTART_MACHINE,"user_id":"test"},
                    "recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),body:body.into()}],
            }}, expected_versions:vec![],evidence:"Verified fixture publication".into(),
        }]).unwrap();
    }

    fn prepare(&self, fields: Value) -> agentlaw_flows::prepare::Prepared {
        use agentlaw_flows::{
            control::ControlReader,
            exact::{DeltaBudget, ExactIndexWriter, ExactSnapshot, ProjectionGenerations},
            history_cache::HistoryCacheGuards,
            prepare,
            store::ReadCtx,
            RequestControl,
        };
        let source = self.store.owned_published_reader();
        let reader = ControlReader::open(&self.identity.local).unwrap();
        let control = RequestControl::default();
        let cache = HistoryCacheGuards::new();
        let projection = self.identity.local.join("prefix-test-projection");
        let shadow = ExactIndexWriter::rebuild_shadow_in(&projection, &source, &control).unwrap();
        let guard = source.read_snapshot().unwrap();
        let registry = ProjectionGenerations::open_for_recovery(
            "bootstrap",
            &guard.position().epoch,
            projection.join("exact-bootstrap.sqlite"),
            projection.join("lexical-bootstrap.sqlite"),
        )
        .unwrap();
        registry.commit(registry.stage(shadow).unwrap()).unwrap();
        let mut exact =
            ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
        drop(guard);
        let ctx = ReadCtx {
            identity: &self.identity,
            source: &source,
            control: &reader,
            semantic: &RestartSemantic,
            history_semantic: &RestartSemantic,
            history_cache: &cache,
            request: &control,
        };
        prepare::prepare(&ctx, &mut exact, serde_json::from_value(fields).unwrap()).unwrap()
    }

    fn recover(&self) {
        use agentlaw_flows::{recovery::recover, store::LaneCtx, RequestControl};
        recover(&mut LaneCtx {
            identity: &self.identity,
            source: &self.store,
            control: &mut self.writer(),
            request: &RequestControl::default(),
        })
        .unwrap();
    }
}

struct RestartSemantic;
impl agentlaw_flows::semantic::SemanticPort for RestartSemantic {
    fn embed_prefix(
        &self,
        _: &str,
        _: &agentlaw_flows::RequestControl,
    ) -> Result<agentlaw_flows::semantic::PrefixEmbedding, agentlaw_flows::semantic::SemanticError>
    {
        Err(agentlaw_flows::semantic::SemanticError::Unavailable(
            "Model omitted in test fixture".into(),
        ))
    }
    fn plan_pieces(
        &self,
        _: &[&str],
        _: &agentlaw_flows::RequestControl,
    ) -> Result<agentlaw_flows::semantic::PiecePlan, agentlaw_flows::semantic::SemanticError> {
        Err(agentlaw_flows::semantic::SemanticError::Unavailable(
            "Model omitted in test fixture".into(),
        ))
    }
    fn embed_planned(
        &self,
        _: &agentlaw_flows::semantic::PiecePlan,
        _: usize,
        _: &agentlaw_flows::semantic::PieceVectorReuse,
        _: &agentlaw_flows::RequestControl,
    ) -> Result<Vec<agentlaw_flows::semantic::QueryVector>, agentlaw_flows::semantic::SemanticError>
    {
        Err(agentlaw_flows::semantic::SemanticError::Unavailable(
            "Model omitted in test fixture".into(),
        ))
    }
    fn search_available(
        &self,
        _: agentlaw_flows::semantic::SemanticQuery<'_>,
        _: &agentlaw_flows::RequestControl,
    ) -> Result<agentlaw_flows::semantic::SemanticCandidates, agentlaw_flows::semantic::SemanticError>
    {
        Err(agentlaw_flows::semantic::SemanticError::Unavailable(
            "Model omitted in test fixture".into(),
        ))
    }
}
impl agentlaw_flows::semantic::HistorySemanticPort for RestartSemantic {
    fn offer(
        &self,
        _: agentlaw_flows::semantic::HistoryIngestHint,
    ) -> agentlaw_flows::semantic::OfferResult {
        agentlaw_flows::semantic::OfferResult::Accepted
    }
    fn search_available(
        &self,
        _: &str,
        _: &str,
        _: &agentlaw_flows::semantic::QueryVector,
        _: &[String],
        _: usize,
        _: &agentlaw_flows::RequestControl,
    ) -> Result<agentlaw_flows::semantic::SemanticCandidates, agentlaw_flows::semantic::SemanticError>
    {
        Err(agentlaw_flows::semantic::SemanticError::Unavailable(
            "Model omitted in test fixture".into(),
        ))
    }
}

fn pause_restart_child(directory: &Path) -> ! {
    std::fs::write(directory.join("checkpoint-reached"), b"ready").unwrap();
    loop {
        std::thread::park();
    }
}

mod execution_delete_pause {
    use rusqlite::ffi;
    use std::{
        ffi::{c_char, c_void, CStr},
        sync::{
            atomic::{AtomicBool, Ordering},
            OnceLock,
        },
    };

    static DIRECTORY: OnceLock<std::path::PathBuf> = OnceLock::new();
    static ARMED: AtomicBool = AtomicBool::new(false);
    static UPDATED: AtomicBool = AtomicBool::new(false);

    pub fn install(directory: &std::path::Path) {
        DIRECTORY.set(directory.to_path_buf()).unwrap();
        // SQLite installs these observers only in the test-owned child.
        assert_eq!(
            unsafe { ffi::sqlite3_auto_extension(Some(install_connection)) },
            ffi::SQLITE_OK
        );
    }

    pub fn arm() {
        ARMED.store(true, Ordering::SeqCst);
    }

    unsafe extern "C" fn install_connection(
        db: *mut ffi::sqlite3,
        _: *mut *mut c_char,
        _: *const ffi::sqlite3_api_routines,
    ) -> i32 {
        unsafe {
            ffi::sqlite3_update_hook(db, Some(updated), std::ptr::null_mut());
            ffi::sqlite3_set_authorizer(db, Some(authorize), std::ptr::null_mut());
        }
        ffi::SQLITE_OK
    }

    unsafe extern "C" fn updated(
        _: *mut c_void,
        _: i32,
        _: *const c_char,
        table: *const c_char,
        _: i64,
    ) {
        if ARMED.load(Ordering::SeqCst) && !table.is_null() {
            let table = unsafe { CStr::from_ptr(table) }.to_bytes();
            if table == b"pending" || table == b"authoring" {
                UPDATED.store(true, Ordering::SeqCst);
            }
        }
    }

    unsafe extern "C" fn authorize(
        _: *mut c_void,
        action: i32,
        table: *const c_char,
        _: *const c_char,
        _: *const c_char,
        _: *const c_char,
    ) -> i32 {
        if action == ffi::SQLITE_DELETE
            && UPDATED.load(Ordering::SeqCst)
            && !table.is_null()
            && unsafe { CStr::from_ptr(table) }.to_bytes() == b"prefix_executions"
        {
            super::pause_restart_child(DIRECTORY.get().unwrap());
        }
        ffi::SQLITE_OK
    }
}

fn run_prefix_restart_child() -> bool {
    use agentlaw_flows::{
        apply::{self, Applied},
        store::LaneCtx,
        RequestControl,
    };
    let Some(directory) = std::env::var_os(RESTART_DIR_ENV).map(std::path::PathBuf::from) else {
        return false;
    };
    let checkpoint = std::env::var(RESTART_PHASE_ENV).unwrap();
    if checkpoint.starts_with("commit") {
        execution_delete_pause::install(&directory);
    }
    let fixture = RestartFixture::open(&directory);
    let mut writer = fixture.writer();
    let mut lane = LaneCtx {
        identity: &fixture.identity,
        source: &fixture.store,
        control: &mut writer,
        request: &RequestControl::default(),
    };
    let plan = if checkpoint == "commit-memory" {
        let parent = fixture.store.read_current(RESTART_PARENT).unwrap();
        let version = &parent.references().unwrap()[0].observed_version;
        fixture.prepare(json!({"memories":[{"operation":"evolve","parent_refs":[{"memory_id":"abcdef12","observed_version":version}],
            "what_to_remember":"Restore the verified release archive before starting.","evidence":"Recovery verified"}]}))
    } else {
        let prepared = fixture.prepare(
            json!({"procedure":{"operation":"create","evidence_memory_ids":[RESTART_PARENT]}}),
        );
        let Applied::Response(context) = apply::apply(&mut lane, prepared).unwrap() else {
            panic!("Expected authoring context");
        };
        let token = &context["authoring_ref"];
        assert!(token.is_string(), "{context}");
        std::fs::write(directory.join("authoring-token"), token.as_str().unwrap()).unwrap();
        fixture.prepare(json!({"authoring_ref":token,"procedure":{"operation":"create","applies_to":["machine"],
            "evidence_memory_ids":["abcdef12"],"name":"Restore verified release archive","use_when":"After a failed deployment",
            "instructions":"Restore the verified archive before starting.","evidence":"Recovery verified"}}))
    };
    let interrupt_after_decision = checkpoint == "decided";
    let decision_directory = directory.clone();
    let request = RequestControl::new(
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        move |phase| match (checkpoint.as_str(), phase) {
            ("commit-memory" | "commit-procedure", "published") => execution_delete_pause::arm(),
            ("undecided", "publishing") => pause_restart_child(&directory),
            _ => {}
        },
    );
    lane.request = &request;
    let outcome = apply::apply(&mut lane, plan);
    if interrupt_after_decision {
        assert!(
            outcome.is_err(),
            "Conditional fault hook did not interrupt apply"
        );
        let (operation, _) = restart_operation(&fixture);
        assert_decided_without_receipt(&fixture, &operation);
        pause_restart_child(&decision_directory);
    }
    panic!(
        "Publication checkpoint was never reached: {:?}",
        outcome.err()
    );
}

fn stop_prefix_child(directory: &Path, test: &str, checkpoint: &str) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env(RESTART_DIR_ENV, directory)
        .env(RESTART_PHASE_ENV, checkpoint);
    if checkpoint == "decided" {
        command.env("AGENTLAW_TEST_CONDITIONAL_PUBLISH_FAULT", "decision");
    }
    let child = command.spawn().unwrap();
    let mut child = Daemon::new(child);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !directory.join("checkpoint-reached").is_file() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "{checkpoint}: child exited before its checkpoint"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "{checkpoint}: publication checkpoint timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    child.stop();
}

fn restart_operation(f: &RestartFixture) -> (String, String) {
    let (operation, payload): (String, String) = f
        .reader()
        .query_row(
            "SELECT operation, payload FROM prefix_executions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let data: Value = serde_json::from_str(&payload).unwrap();
    let entity = data["execution"]["Procedure"]["mutation"]["unit"]["entity_id"]
        .as_str()
        .unwrap_or(RESTART_PARENT);
    (operation, entity.into())
}

fn assert_decided_without_receipt(f: &RestartFixture, operation: &str) {
    let receipt = restart_receipt(f, operation);
    assert!(receipt.with_file_name("decision").is_file(), "{operation}");
    assert!(!receipt.exists(), "{operation}");
}

fn restart_receipt(f: &RestartFixture, operation: &str) -> std::path::PathBuf {
    f.identity
        .local
        .join("canonical/recovery")
        .join(operation)
        .join("published")
}

fn assert_retired_execution(f: &RestartFixture) {
    let executions: u64 = f
        .reader()
        .query_row("SELECT count(*) FROM prefix_executions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(executions, 0);
}

fn authoring_consumed(f: &RestartFixture, dir: &Path) -> bool {
    let token = std::fs::read_to_string(dir.join("authoring-token")).unwrap();
    f.reader()
        .query_row(
            "SELECT used FROM authoring WHERE token=?1",
            [token],
            |row| row.get(0),
        )
        .unwrap()
}

fn recovered_procedure_result(f: &RestartFixture, dir: &Path) -> Option<Value> {
    let token = std::fs::read_to_string(dir.join("authoring-token")).unwrap();
    let reader = agentlaw_flows::control::ControlReader::open(&f.identity.local).unwrap();
    reader
        .authoring_result(&f.identity.binding_id, &token)
        .unwrap()
        .and_then(|(_, _, result)| result)
        .map(|result| serde_json::from_str(&result).unwrap())
}

#[test]
fn interrupted_control_commit_finalizes_receipt_atomically() {
    // ER-17
    if run_prefix_restart_child() {
        unreachable!();
    }
    for (checkpoint, procedure) in [("commit-memory", false), ("commit-procedure", true)] {
        let dir = tempfile::tempdir().unwrap();
        let f = RestartFixture::open(dir.path());
        f.seed(RESTART_PARENT, "Preserve the original release archive.");
        stop_prefix_child(
            dir.path(),
            "interrupted_control_commit_finalizes_receipt_atomically",
            checkpoint,
        );
        let (operation, entity) = restart_operation(&f);
        let receipt = restart_receipt(&f, &operation);
        let original_receipt = std::fs::read(&receipt).unwrap();
        let generation = f.store.generation().unwrap();
        if procedure {
            assert!(!authoring_consumed(&f, dir.path()));
            assert!(recovered_procedure_result(&f, dir.path()).is_none());
            assert_eq!(f.store.history(&entity).unwrap().len(), 1);
        } else {
            let visible: u64 = f
                .reader()
                .query_row(
                    "SELECT count(*) FROM pending WHERE status='published'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(visible, 0);
            assert_eq!(f.store.history(&entity).unwrap().len(), 2);
        }
        f.recover();
        assert_retired_execution(&f);
        assert_eq!(std::fs::read(receipt).unwrap(), original_receipt);
        assert_eq!(f.store.generation().unwrap(), generation);
        if procedure {
            assert!(authoring_consumed(&f, dir.path()));
            let result = recovered_procedure_result(&f, dir.path()).unwrap();
            let heads = f
                .store
                .read_procedure(&entity)
                .unwrap()
                .references()
                .unwrap();
            assert_eq!(
                result["procedure_ref"],
                json!({"procedure_id":entity,"observed_version":heads[0].observed_version})
            );
            assert_eq!(f.store.history(&entity).unwrap().len(), 1);
        } else {
            let result: String = f
                .reader()
                .query_row(
                    "SELECT result FROM pending WHERE status='published'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let result: Value = serde_json::from_str(&result).unwrap();
            assert_eq!(result["status"], "remembered");
            let heads = f.store.read_current(&entity).unwrap().references().unwrap();
            assert_eq!(
                result["results"][0]["memory_ref"]["observed_version"],
                heads[0].observed_version
            );
            assert_eq!(f.store.history(&entity).unwrap().len(), 2);
        }
    }
}

#[test]
fn restart_completes_decided_prefix_publication_once() {
    // ER-18
    if run_prefix_restart_child() {
        unreachable!();
    }
    let dir = tempfile::tempdir().unwrap();
    let f = RestartFixture::open(dir.path());
    f.seed(RESTART_PARENT, "Preserve the original release archive.");
    stop_prefix_child(
        dir.path(),
        "restart_completes_decided_prefix_publication_once",
        "decided",
    );
    let (operation, entity) = restart_operation(&f);
    let receipt = restart_receipt(&f, &operation);
    assert_decided_without_receipt(&f, &operation);
    assert!(!authoring_consumed(&f, dir.path()));
    f.recover();
    assert_retired_execution(&f);
    assert!(authoring_consumed(&f, dir.path()));
    let generation = f.store.generation().unwrap();
    let references = f
        .store
        .read_procedure(&entity)
        .unwrap()
        .references()
        .unwrap();
    assert_eq!(references.len(), 1);
    let result = recovered_procedure_result(&f, dir.path()).unwrap();
    assert_eq!(
        result["procedure_ref"],
        json!({"procedure_id":entity,"observed_version":references[0].observed_version})
    );
    let published_receipt = std::fs::read(&receipt).unwrap();
    f.recover();
    assert_eq!(std::fs::read(receipt).unwrap(), published_receipt);
    assert_eq!(f.store.generation().unwrap(), generation, "{operation}");
    assert_eq!(
        f.store
            .read_procedure(&entity)
            .unwrap()
            .references()
            .unwrap(),
        references
    );
    assert_eq!(f.store.history(&entity).unwrap().len(), 1);
}

#[test]
fn restart_rechecks_undecided_prefix_without_publishing() {
    // ER-27
    if run_prefix_restart_child() {
        unreachable!();
    }
    let dir = tempfile::tempdir().unwrap();
    let f = RestartFixture::open(dir.path());
    f.seed(RESTART_PARENT, "Preserve the original release archive.");
    let original = f
        .store
        .read_current(RESTART_PARENT)
        .unwrap()
        .references()
        .unwrap();
    stop_prefix_child(
        dir.path(),
        "restart_rechecks_undecided_prefix_without_publishing",
        "undecided",
    );
    let (operation, entity) = restart_operation(&f);
    let receipt = restart_receipt(&f, &operation);
    assert!(!receipt.exists());
    assert!(!authoring_consumed(&f, dir.path()));
    f.seed(RESTART_COMPETITOR, "Verified a separate archive.");
    let generation = f.store.generation().unwrap();
    f.recover();
    assert!(!receipt.exists());
    assert_retired_execution(&f);
    assert!(!authoring_consumed(&f, dir.path()));
    assert_eq!(f.store.generation().unwrap(), generation);
    assert!(matches!(
        f.store.read_procedure(&entity),
        Err(agentlaw_storage::Error::NotFound(_))
    ));
    assert_eq!(
        f.store
            .read_current(RESTART_PARENT)
            .unwrap()
            .references()
            .unwrap(),
        original
    );
    assert_eq!(
        f.store.read_current(RESTART_COMPETITOR).unwrap().heads()[0].body,
        "Verified a separate archive."
    );
}

#[cfg(any(unix, windows))]
fn publish_competing_version(
    f: &IdentityFrontend,
    recovery: &Path,
    reference: &Value,
    collide: bool,
) -> Value {
    use agentlaw_storage::{Mutation, Store, UnitState};
    let canonical = recovery.parent().unwrap();
    let store =
        Store::open_with_coordination(&f.source, canonical, f.state.join("source-coordination"))
            .unwrap();
    let id = reference["memory_id"].as_str().unwrap();
    let mut unit = store.read_current(id).unwrap();
    let versions = if collide {
        let mut competitor = format!("{}-0000-4000-8000-000000000001", &id[..8]);
        if competitor == id {
            competitor.replace_range(35.., "2");
        }
        unit.entity_id = competitor;
        vec![]
    } else {
        vec![reference["observed_version"].as_str().unwrap().to_owned()]
    };
    let UnitState::Live { heads } = &mut unit.state else {
        panic!("Expected live memory");
    };
    heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    heads[0].body = if collide {
        "Check a separate release archive.".into()
    } else {
        "The other writer verified the latest release archive.".into()
    };
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit,
                expected_versions: versions,
                evidence: "Independent verification".into(),
            }],
        )
        .unwrap();
    serde_json::to_value(&receipt.references[0]).unwrap()
}

#[cfg(any(unix, windows))]
fn assert_settled_without_publication(recovery: &Path, operation: &Value) {
    let operation = operation.as_str().unwrap();
    let directory = recovery.join(operation);
    assert!(!directory.join("decision").exists());
    assert!(!directory.join("published").exists());
    let db = rusqlite::Connection::open_with_flags(
        recovery
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("control.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let active: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM prefix_executions WHERE operation=?1)",
            [operation],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!active);
}

#[cfg(any(unix, windows))]
#[test]
fn restart_preserves_rejection_for_original_publication_reference() {
    // ER-28
    let f = IdentityFrontend::new();
    let (reference, operation, recovery) = pending_prefix_evolution(&f, true);
    f.runtime.shutdown();
    let competing = publish_competing_version(&f, &recovery, &reference, true);
    let before = std::fs::read(f.current_path(&reference)).unwrap();
    for _ in 0..2 {
        let (out, inspected) = inspect_publication(&f, &operation);
        assert!(out.status.success(), "FR-01.6 ER-28: {inspected}");
        assert_eq!(inspected["publication_ref"], operation);
        assert_eq!(inspected["publication_state"], "rejected", "{inspected}");
        assert_eq!(inspected["rejection_code"], "ambiguous");
        assert!(inspected.get("pending_batch_ref").is_none());
        let guidance = inspected["next_action"].as_str().unwrap().to_lowercase();
        assert!(
            guidance.contains("longer")
                || guidance.contains("full")
                || guidance.contains("canonical"),
            "{inspected}"
        );
    }
    assert_settled_without_publication(&recovery, &operation);
    assert_eq!(std::fs::read(f.current_path(&reference)).unwrap(), before);
    let current = f.success("recall", json!({"memory_ids":[competing["memory_id"]]}));
    assert_eq!(
        current["memories"][0]["current_heads"][0]["memory_ref"],
        competing
    );
}

#[cfg(any(unix, windows))]
#[test]
fn restart_returns_review_batch_for_original_publication_reference() {
    // ER-29
    let f = IdentityFrontend::new();
    let (reference, operation, recovery) = pending_prefix_evolution(&f, true);
    f.runtime.shutdown();
    let competing = publish_competing_version(&f, &recovery, &reference, false);
    let path = f.current_path(&reference);
    let concurrent_bytes = std::fs::read(&path).unwrap();
    let inspect_review = || {
        let (out, inspected) = inspect_publication(&f, &operation);
        assert!(out.status.success(), "FR-01.6 ER-29: {inspected}");
        assert_eq!(inspected["publication_ref"], operation);
        assert_eq!(
            inspected["publication_state"], "review_required",
            "{inspected}"
        );
        assert!(inspected.get("rejection_code").is_none());
        let pending = &inspected["pending_batch_ref"];
        assert!(
            pending["pending_batch_id"].is_string() && pending["observed_version"].is_string(),
            "{inspected}"
        );
        pending.clone()
    };
    let pending = inspect_review();
    let proposal = f.success(
        "remember_this",
        json!({"pending_action":"inspect","pending_batch_ref":pending}),
    );
    assert_eq!(proposal["pending_batch_ref"], pending);
    assert_eq!(
        proposal["proposals"][0]["what_to_remember"],
        "Preserve the signed release notes."
    );
    let issue = proposal["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["current_states"].is_array())
        .unwrap_or_else(|| panic!("{proposal}"));
    let heads = issue["current_states"][0]["current_heads"]
        .as_array()
        .unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0]["memory_ref"], competing);
    assert_eq!(
        heads[0]["what_to_remember"],
        "The other writer verified the latest release archive."
    );
    f.runtime.shutdown();
    assert_eq!(inspect_review(), pending);
    assert_settled_without_publication(&recovery, &operation);
    assert_eq!(std::fs::read(&path).unwrap(), concurrent_bytes);
    let resolution_ref = proposal["resolution_ref"]
        .as_str()
        .filter(|reference| !reference.is_empty())
        .expect("ER-29: retained review must provide a resolution reference");
    let continued = f.success(
        "remember_this",
        json!({"pending_action":"continue","pending_batch_ref":pending,"resolution_ref":resolution_ref}),
    );
    assert_eq!(continued["status"], "resolution_required", "{continued}");
    assert_eq!(
        continued["pending_batch_ref"]["pending_batch_id"],
        pending["pending_batch_id"]
    );
    assert_ne!(
        continued["pending_batch_ref"]["observed_version"],
        pending["observed_version"]
    );
    assert_eq!(
        inspect_review()["pending_batch_id"],
        pending["pending_batch_id"]
    );
    assert_eq!(std::fs::read(path).unwrap(), concurrent_bytes);
}

#[cfg(any(unix, windows))]
fn with_head_reread_fault<T>(
    parent: &Path,
    before_next_publication: impl Fn() + Send + Sync + 'static,
    run: impl FnOnce(&agentlaw_flows::RequestControl) -> T,
) -> (T, String, bool) {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    };
    enum FileAccess {
        Deny(mpsc::SyncSender<String>),
        Restore(mpsc::SyncSender<()>),
    }
    std::thread::scope(|scope| {
        let (commands, worker) = mpsc::channel();
        scope.spawn(move || {
            let mut denied = None;
            for command in worker {
                match command {
                    FileAccess::Deny(ack) => {
                        assert!(denied.is_none());
                        #[cfg(unix)]
                        {
                            denied = Some(DeniedRead::apply(parent));
                        }
                        #[cfg(windows)]
                        {
                            denied = Some(SourceReadDenial::apply(parent));
                        }
                        let cause = std::fs::File::open(parent).unwrap_err();
                        assert_eq!(cause.kind(), std::io::ErrorKind::PermissionDenied);
                        ack.send(cause.to_string()).unwrap();
                    }
                    FileAccess::Restore(ack) => {
                        drop(denied.take());
                        assert!(std::fs::File::open(parent).is_ok());
                        ack.send(()).unwrap();
                    }
                }
            }
        });
        let cause = Arc::new(Mutex::new(None));
        let recorded = cause.clone();
        let continued = Arc::new(AtomicBool::new(false));
        let reached_next = continued.clone();
        let control =
            agentlaw_flows::RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
                if phase == "rereading_changed_heads" {
                    let (ack, completed) = mpsc::sync_channel(0);
                    commands.send(FileAccess::Deny(ack)).unwrap();
                    *recorded.lock().unwrap() = Some(completed.recv().unwrap());
                } else if phase == "publishing"
                    && recorded.lock().unwrap().is_some()
                    && !reached_next.swap(true, Ordering::SeqCst)
                {
                    before_next_publication();
                    let (ack, completed) = mpsc::sync_channel(0);
                    commands.send(FileAccess::Restore(ack)).unwrap();
                    completed.recv().unwrap();
                }
            });
        let result = run(&control);
        drop(control);
        let cause = cause
            .lock()
            .unwrap()
            .take()
            .expect("ER-31: heads-change reread checkpoint was not reached");
        (result, cause, continued.load(Ordering::SeqCst))
    })
}

#[cfg(any(unix, windows))]
fn inspect_restart_fixture(f: &RestartFixture, fields: Value) -> Value {
    use agentlaw_flows::{
        control::ControlReader, history_cache::HistoryCacheGuards, read, store::ReadCtx,
        RequestControl,
    };
    let source = f.store.owned_published_reader();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &RestartSemantic,
        history_semantic: &RestartSemantic,
        history_cache: &HistoryCacheGuards::new(),
        request: &RequestControl::default(),
    };
    read::pending_inspect(&ctx, &serde_json::from_value(fields).unwrap()).unwrap()
}

#[cfg(any(unix, windows))]
fn recovery_control_snapshot(local: &Path, operation: &str, token: &str) -> Value {
    let db = rusqlite::Connection::open_with_flags(
        local.join("control.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let payload: Option<String> = db
        .query_row(
            "SELECT (SELECT payload FROM prefix_executions WHERE binding=?1 AND operation=?2)",
            [RESTART_BINDING, operation],
            |row| row.get(0),
        )
        .unwrap();
    let (pending, outcome, used): (u64, bool, bool) = db
        .query_row(
            "SELECT (SELECT count(*) FROM pending WHERE binding=?1),
            EXISTS(SELECT 1 FROM prefix_execution_outcomes WHERE binding=?1 AND operation=?2),
            (SELECT used FROM authoring WHERE token=?3)",
            [RESTART_BINDING, operation, token],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let directory = local.join("canonical/recovery").join(operation);
    json!({"execution":payload,"pending":pending,"terminal_outcome":outcome,"token_used":used,
        "decision":directory.join("decision").exists(),"receipt":directory.join("published").exists()})
}

#[cfg(any(unix, windows))]
#[test]
fn failed_parent_head_reread_preserves_reference_and_other_executions_recover() {
    // ER-31
    use agentlaw_flows::{
        apply::{self, Applied},
        recovery,
        store::LaneCtx,
        RequestControl,
    };
    use agentlaw_storage::Mutation;
    const INDEPENDENT: &str = "fedcba98-a000-4000-8000-000000000001";
    const PROPOSAL: &str = "Preserve the signed release notes.";
    const COMPETING: &str = "Another writer verified the release notes.";
    let dir = tempfile::tempdir().unwrap();
    let f = RestartFixture::open(dir.path());
    let mut writer = f.writer();
    f.seed(RESTART_PARENT, "Preserve the verified release notes.");
    f.seed(INDEPENDENT, "Review the deployment manifest.");
    let apply =
        |writer: &mut agentlaw_flows::control::ControlWriter, plan, request: &RequestControl| {
            apply::apply(
                &mut LaneCtx {
                    identity: &f.identity,
                    source: &f.store,
                    control: writer,
                    request,
                },
                plan,
            )
        };
    let prepared =
        f.prepare(json!({"procedure":{"operation":"create","evidence_memory_ids":[INDEPENDENT]}}));
    let Applied::Response(authoring) =
        apply(&mut writer, prepared, &RequestControl::default()).unwrap()
    else {
        panic!("Expected a retained authoring token");
    };
    let token = authoring["authoring_ref"].as_str().unwrap().to_owned();
    let prepare_evolution = |id: &str, body| {
        let version = f.store.read_current(id).unwrap().references().unwrap()[0]
            .observed_version
            .clone();
        f.prepare(json!({"memories":[{"operation":"evolve","parent_refs":[{"memory_id":&id[..8],"observed_version":version}],
            "what_to_remember":body,"evidence":"Signed notes inspected"}]}))
    };
    let stale_plan = prepare_evolution(RESTART_PARENT, PROPOSAL);
    let mut parent = f.store.read_current(RESTART_PARENT).unwrap();
    let prior = parent.references().unwrap();
    let agentlaw_storage::UnitState::Live { heads } = &mut parent.state else {
        panic!("Expected current memory");
    };
    heads[0].body = COMPETING.into();
    heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    let concurrent = f
        .store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit: parent,
                expected_versions: vec![prior[0].observed_version.clone()],
                evidence: "Concurrent verification".into(),
            }],
        )
        .unwrap();
    let concurrent_ref = serde_json::to_value(&concurrent.references[0]).unwrap();
    let parent_file = f
        .identity
        .root
        .join("current/memory")
        .join(&RESTART_PARENT[..2])
        .join(format!("{RESTART_PARENT}.md"));
    let current_bytes = std::fs::read(&parent_file).unwrap();
    let (failed, cause, _) = with_head_reread_fault(
        &parent_file,
        || {},
        |request| apply(&mut writer, stale_plan, request),
    );
    let error = failed
        .err()
        .expect("FR-01.6 ER-31: current heads could not be read");
    let operation: String = f
        .reader()
        .query_row("SELECT operation FROM prefix_executions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let output = agentlaw_app::error_payload(&error);
    assert_eq!(output["publication_ref"], operation, "{output}");
    assert_eq!(output["publication_state"], "before_decision", "{output}");
    assert!(
        output["message"].as_str().unwrap().contains(&cause),
        "{output}"
    );
    assert!(
        output["next_action"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("inspect"),
        "{output}"
    );
    let blocked = recovery_control_snapshot(&f.identity.local, &operation, &token);
    assert!(blocked["execution"].is_string());
    assert_eq!(blocked["pending"], 0);
    for field in ["terminal_outcome", "token_used", "decision", "receipt"] {
        assert_eq!(blocked[field], false, "{blocked}");
    }
    assert_eq!(std::fs::read(&parent_file).unwrap(), current_bytes);
    let independent = prepare_evolution(INDEPENDENT, "Review the signed deployment manifest.");
    let recovery_dir = f.identity.local.join("canonical/recovery");
    {
        #[cfg(unix)]
        let _write_denial = DeniedRead::apply_mode(&recovery_dir, 0o500);
        #[cfg(windows)]
        let _write_denial = SourceReadDenial::apply_rights(&recovery_dir, "W");
        assert!(apply(&mut writer, independent, &RequestControl::default()).is_err());
    }
    let independent_operation: String = f
        .reader()
        .query_row(
            "SELECT operation FROM prefix_executions WHERE operation<>?1",
            [&operation],
            |row| row.get(0),
        )
        .unwrap();
    drop(writer);

    // Fix the test database's traversal order so the failure precedes the independent execution.
    let db = rusqlite::Connection::open(f.identity.local.join("control.sqlite")).unwrap();
    uuid::Uuid::parse_str(&operation).unwrap();
    db.execute_batch(&format!(
        "CREATE INDEX fixture_recovery_order
        ON prefix_executions(binding, (operation <> '{operation}'), operation, payload)"
    ))
    .unwrap();
    let first: String = db
        .query_row(
            "SELECT operation,payload FROM prefix_executions WHERE binding=?1",
            [RESTART_BINDING],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        first, operation,
        "FR-01.6 ER-31: fixture must replay the failing execution first"
    );
    drop(db);

    let restarted = RestartFixture::open(dir.path());
    let observe_local = restarted.identity.local.clone();
    let observe_operation = operation.clone();
    let (recovery_result, _, proceeded) = with_head_reread_fault(
        &parent_file,
        move || {
            assert_eq!(
                recovery_control_snapshot(&observe_local, &observe_operation, &token),
                blocked
            );
        },
        |request| {
            recovery::recover(&mut LaneCtx {
                identity: &restarted.identity,
                source: &restarted.store,
                control: &mut restarted.writer(),
                request,
            })
        },
    );
    recovery_result.unwrap();
    assert!(
        proceeded,
        "FR-01.6 ER-31: recovery stopped at the failed reread"
    );
    assert_eq!(std::fs::read(&parent_file).unwrap(), current_bytes);
    let receipt_path = restart_receipt(&restarted, &independent_operation);
    let receipt = std::fs::read(&receipt_path).unwrap();
    let published = restarted
        .store
        .read_current(INDEPENDENT)
        .unwrap()
        .references()
        .unwrap();
    assert_eq!(restarted.store.history(INDEPENDENT).unwrap().len(), 2);
    assert_eq!(
        inspect_restart_fixture(
            &restarted,
            json!({"pending_action":"inspect","publication_ref":independent_operation})
        )["publication_state"],
        "published"
    );

    let next = RestartFixture::open(dir.path());
    next.recover();
    let review = inspect_restart_fixture(
        &next,
        json!({"pending_action":"inspect","publication_ref":operation}),
    );
    assert_eq!(review["publication_state"], "review_required", "{review}");
    assert_eq!(review["publication_ref"], operation);
    let retained = inspect_restart_fixture(
        &next,
        json!({"pending_action":"inspect","pending_batch_ref":review["pending_batch_ref"]}),
    );
    assert_eq!(retained["proposals"][0]["what_to_remember"], PROPOSAL);
    let issue = retained["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["current_states"].is_array())
        .unwrap();
    let heads = issue["current_states"][0]["current_heads"]
        .as_array()
        .unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0]["memory_ref"], concurrent_ref);
    assert_eq!(heads[0]["what_to_remember"], COMPETING);
    assert_retired_execution(&next);
    assert_eq!(std::fs::read(parent_file).unwrap(), current_bytes);
    assert_eq!(std::fs::read(receipt_path).unwrap(), receipt);
    assert_eq!(
        next.store
            .read_current(INDEPENDENT)
            .unwrap()
            .references()
            .unwrap(),
        published
    );
    assert_eq!(next.store.history(INDEPENDENT).unwrap().len(), 2);
}

#[cfg(any(unix, windows))]
#[test]
fn cancelled_changed_head_reread_records_rejection_before_restart() {
    // ER-32
    use agentlaw_contracts::{Request, Result};
    use agentlaw_flows::{
        apply::{self, Applied},
        store::LaneCtx,
        RequestControl,
    };
    use agentlaw_storage::Mutation;
    use std::{
        io::{BufRead, BufReader},
        net::{Shutdown, TcpListener, TcpStream},
        sync::{atomic::Ordering, mpsc, Mutex},
        time::Instant,
    };

    const WAIT: Duration = Duration::from_secs(15);

    struct PublicationBackend {
        directory: std::path::PathBuf,
        resume_publication: Option<mpsc::Receiver<()>>,
        finished: mpsc::Sender<()>,
    }

    impl agentlaw_app::Backend for PublicationBackend {
        fn call(&mut self, _: Request) -> Result<Value> {
            unreachable!("MCP serves with request control")
        }

        fn call_with_control(
            &mut self,
            request: Request,
            transport: RequestControl,
        ) -> Result<Value> {
            let Request::RememberThis(request) = request else {
                panic!("Expected the prefix publication request");
            };
            let fixture = RestartFixture::open(&self.directory);
            let mut writer = fixture.writer();
            let plan = fixture.prepare(serde_json::to_value(request).unwrap());
            let resume = Mutex::new(self.resume_publication.take().unwrap());
            let control = RequestControl::new(transport.cancel.clone(), move |phase| {
                transport.phase(phase);
                if phase == "publishing" {
                    resume
                        .lock()
                        .unwrap()
                        .recv_timeout(WAIT)
                        .expect("Competing writer did not finish");
                } else if phase == "rereading_changed_heads" {
                    let deadline = Instant::now() + WAIT;
                    while !transport.cancel.load(Ordering::Acquire) {
                        assert!(
                            Instant::now() < deadline,
                            "MCP transport did not receive cancellation"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            });
            let result = apply::apply(
                &mut LaneCtx {
                    identity: &fixture.identity,
                    source: &fixture.store,
                    control: &mut writer,
                    request: &control,
                },
                plan,
            );
            self.finished.send(()).unwrap();
            match result? {
                Applied::Response(value) => Ok(value),
                _ => panic!("Expected a publication response"),
            }
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let fixture = RestartFixture::open(dir.path());
    fixture.seed(RESTART_PARENT, "Preserve the verified release notes.");
    let parent = fixture.store.read_current(RESTART_PARENT).unwrap();
    let version = parent.references().unwrap()[0].observed_version.clone();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (resume_publication, resume) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let backend = PublicationBackend {
        directory: dir.path().to_path_buf(),
        resume_publication: Some(resume),
        finished,
    };
    let (operation, concurrent, current_bytes) = std::thread::scope(|scope| {
        let server = scope.spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            agentlaw_app::transport::serve(
                BufReader::new(stream.try_clone().unwrap()),
                stream,
                backend,
            )
            .unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(WAIT)).unwrap();
        client.set_write_timeout(Some(WAIT)).unwrap();
        let mut input = BufReader::new(client.try_clone().unwrap());
        let receive = |input: &mut BufReader<TcpStream>| {
            let mut line = String::new();
            assert_ne!(
                input.read_line(&mut line).unwrap(),
                0,
                "MCP closed before sending a response"
            );
            serde_json::from_str::<Value>(&line).unwrap()
        };
        let wait_for = |input: &mut BufReader<TcpStream>, phase: &str| loop {
            let msg = receive(input);
            assert!(msg.get("id").is_none(), "Expected progress {phase}: {msg}");
            if msg["method"] == "notifications/progress" && msg["params"]["message"] == phase {
                assert_eq!(msg["params"]["progressToken"], "publication");
                break;
            }
        };
        writeln!(client, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).unwrap();
        assert_eq!(receive(&mut input)["id"], 1);
        writeln!(
            client,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        writeln!(client, "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"agentlaw","_meta":{"progressToken":"publication"},
            "arguments":{"action":"remember_this","remember_this":{"memories":[{
                "operation":"evolve","parent_refs":[{"memory_id":&RESTART_PARENT[..8],"observed_version":version}],
                "what_to_remember":"Preserve the signed release notes.","evidence":"Signature verified"
            }]}}
        }})).unwrap();
        wait_for(&mut input, "publishing");

        let mut changed = parent;
        let agentlaw_storage::UnitState::Live { heads } = &mut changed.state else {
            panic!("Expected current memory");
        };
        heads[0].body = "The other writer verified the latest release archive.".into();
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
        let concurrent = fixture
            .store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit: changed,
                    expected_versions: vec![version],
                    evidence: "Independent release verification".into(),
                }],
            )
            .unwrap()
            .references;
        let source_path = fixture
            .identity
            .root
            .join("current/memory")
            .join(&RESTART_PARENT[..2])
            .join(format!("{RESTART_PARENT}.md"));
        let current_bytes = std::fs::read(source_path).unwrap();
        resume_publication.send(()).unwrap();
        wait_for(&mut input, "rereading_changed_heads");
        let (operation, entity) = restart_operation(&fixture);
        assert_eq!(entity, RESTART_PARENT);
        writeln!(
            client,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":2}})
        )
        .unwrap();
        completion
            .recv_timeout(WAIT)
            .expect("Publication did not finish after MCP cancellation");
        client.shutdown(Shutdown::Write).unwrap();
        server.join().unwrap();
        (operation, concurrent, current_bytes)
    });
    let check_outcome = |fixture: &RestartFixture| {
        let outcome = inspect_restart_fixture(
            fixture,
            json!({"pending_action":"inspect","publication_ref":operation}),
        );
        assert_eq!(outcome["publication_ref"], operation, "{outcome}");
        assert_eq!(
            outcome["publication_state"], "rejected",
            "FR-01.6 ER-32: {outcome}"
        );
        assert_eq!(outcome["rejection_code"], "cancelled", "{outcome}");
        assert!(outcome.get("pending_batch_ref").is_none(), "{outcome}");
        assert_retired_execution(fixture);
        assert!(!restart_receipt(fixture, &operation).exists());
        let pending: u64 = fixture
            .reader()
            .query_row("SELECT count(*) FROM pending", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pending, 0);
        let current = fixture
            .store
            .read_current(RESTART_PARENT)
            .unwrap()
            .references()
            .unwrap();
        assert_eq!(current, concurrent);
        assert_eq!(fixture.store.history(RESTART_PARENT).unwrap().len(), 2);
        let path = fixture
            .identity
            .root
            .join("current/memory")
            .join(&RESTART_PARENT[..2])
            .join(format!("{RESTART_PARENT}.md"));
        assert_eq!(std::fs::read(path).unwrap(), current_bytes);
        outcome
    };
    let outcome = check_outcome(&fixture);
    drop(fixture);
    let restarted = RestartFixture::open(dir.path());
    restarted.recover();
    assert_eq!(check_outcome(&restarted), outcome);
}
