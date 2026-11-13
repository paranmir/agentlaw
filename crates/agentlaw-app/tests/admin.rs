#[path = "../../../tests/support/owned_process.rs"]
mod owned_process;
#[path = "../../../tests/support/runtime_cleanup.rs"]
mod runtime_cleanup;

use owned_process::OwnedProcess;
use runtime_cleanup::RuntimeCleanup;
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

struct Mcp {
    _child: OwnedProcess,
    input: std::process::ChildStdin,
    replies: std::sync::mpsc::Receiver<serde_json::Value>,
}
impl Mcp {
    fn start(state: &Path) -> Self {
        use std::io::BufRead;
        let mut child = Command::new(env!("CARGO_BIN_EXE_agentlaw"))
            .env("AGENTLAW_HOME", state)
            .args(["mcp", "serve", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let child = OwnedProcess::frontend(child);
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
        let mut result = Self {
            _child: child,
            input,
            replies,
        };
        result.send(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}));
        assert!(result.reply().get("result").is_some());
        result.send(serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        result
    }
    fn send(&mut self, value: serde_json::Value) {
        use std::io::Write;
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn reply(&self) -> serde_json::Value {
        self.replies
            .recv_timeout(Duration::from_secs(60))
            .expect("MCP reply")
    }
    fn call(&mut self, id: u64, args: serde_json::Value) -> serde_json::Value {
        self.send(serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"agentlaw","arguments":args}}));
        self.reply()
    }
}

fn command(state: &Path, args: &[&str]) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    command_with_input(state, args, None)
}

fn command_with_input(
    state: &Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentlaw"))
        .env("AGENTLAW_HOME", state)
        .env_remove("AGENTLAW_ONNX_MODEL")
        .env_remove("AGENTLAW_TOKENIZER")
        .env_remove("AGENTLAW_ORT_LIBRARY")
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input).unwrap();
    }
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let mut child = OwnedProcess::frontend(child);
    let stdout = std::thread::spawn(move || {
        let mut b = Vec::new();
        out.read_to_end(&mut b).unwrap();
        b
    });
    let stderr = std::thread::spawn(move || {
        let mut b = Vec::new();
        err.read_to_end(&mut b).unwrap();
        b
    });
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "CLI timed out");
        std::thread::sleep(Duration::from_millis(5));
    };
    (status, stdout.join().unwrap(), stderr.join().unwrap())
}

#[test]
fn disconnect_after_admin_body_helper() {
    use agentlaw_runtime::{protocol::*, transport};
    use std::net::TcpListener;
    let Some(state) = std::env::var_os("AGENTLAW_ADMIN_FIXTURE_STATE") else {
        return;
    };
    let state = std::path::PathBuf::from(state);
    let _lock = agentlaw_runtime::installation::owner::OwnerLock::try_acquire(&state)
        .unwrap()
        .unwrap();
    let control = TcpListener::bind("127.0.0.1:0").unwrap();
    let data = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let key =
        agentlaw_storage::binding_identity(&config.memory_store_path, &config.runtime_root(&state))
            .unwrap();
    let binding = BindingRef::new(&key, &config.user_id);
    let build =
        agentlaw_runtime::installation::build_identity(Path::new(env!("CARGO_BIN_EXE_agentlaw")))
            .unwrap();
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
            if transport::receive::<Hello>(&mut stream, MAX_CONTROL_FRAME_BYTES).is_err() {
                continue;
            }
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
    data.set_nonblocking(true).unwrap();
    std::fs::write(state.join("helper-ready"), b"ready").unwrap();
    let mut count = 0;
    while !state.join("helper-stop").exists() {
        let (mut stream, _) = match data.accept() {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(e) => panic!("accept: {e}"),
        };
        let _: Hello = transport::receive(&mut stream, MAX_CONTROL_FRAME_BYTES).unwrap();
        transport::send(&mut stream, &HelloReply::Accepted, MAX_CONTROL_REPLY_BYTES).unwrap();
        let header: CallHeader = transport::receive(&mut stream, MAX_CONTROL_FRAME_BYTES).unwrap();
        assert_eq!(header.kind, RequestKind::Admin);
        assert_eq!(header.target, CallTarget::Store(binding));
        transport::send(
            &mut stream,
            &DataFrame::Proceed { id: header.id },
            MAX_CONTROL_REPLY_BYTES,
        )
        .unwrap();
        let mut body = vec![0; header.body_len as usize];
        stream.read_exact(&mut body).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body, serde_json::json!({"admin_command":{"repair":{}}}));
        count += 1;
        std::fs::write(state.join("received-bodies"), count.to_string()).unwrap();
        drop(stream);
    }
}

#[test]
fn repair_does_not_resend_after_admin_body_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let cleanup = RuntimeCleanup(state.clone());
    let source = dir.path().join("source");
    let (status, out, err) = command(
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
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    cleanup.shutdown();
    let helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "disconnect_after_admin_body_helper",
            "--nocapture",
        ])
        .env("AGENTLAW_ADMIN_FIXTURE_STATE", &state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut helper = OwnedProcess::new(helper);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !state.join("helper-ready").exists() {
        assert!(helper.is_running(), "helper exited");
        assert!(Instant::now() < deadline, "helper did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (status, out, err) = command(&state, &["repair"]);
    assert!(!status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&out).unwrap_or_else(|_| serde_json::from_slice(&err).unwrap());
    assert_eq!(value["code"], "execution_outcome_unknown", "{value}");
    assert_eq!(
        std::fs::read_to_string(state.join("received-bodies")).unwrap(),
        "1"
    );
    std::fs::write(state.join("helper-stop"), b"stop").unwrap();
    assert!(helper.wait().unwrap().success());
}

#[test]
fn procedure_list_writes_new_output_and_preserves_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let _cleanup = RuntimeCleanup(state.clone());
    let source = dir.path().join("source");
    let (status, out, err) = command(
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
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let output = dir.path().join("inventory.jsonl");
    let (status, out, err) = command(
        &state,
        &[
            "learned-procedure",
            "list",
            "--output",
            output.to_str().unwrap(),
        ],
    );
    assert!(
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["procedure_count"], 0);
    assert_eq!(Path::new(result["output"].as_str().unwrap()), output);
    let original = std::fs::read(&output).unwrap();
    let (status, _, _) = command(
        &state,
        &[
            "learned-procedure",
            "list",
            "--output",
            output.to_str().unwrap(),
        ],
    );
    assert!(!status.success());
    assert_eq!(std::fs::read(&output).unwrap(), original);
    let inside = source.join("inventory.jsonl");
    let (status, _, _) = command(
        &state,
        &[
            "learned-procedure",
            "list",
            "--output",
            inside.to_str().unwrap(),
        ],
    );
    assert!(!status.success());
    assert!(!inside.exists());
    let missing = dir.path().join("missing").join("inventory.jsonl");
    let (status, _, _) = command(
        &state,
        &[
            "learned-procedure",
            "list",
            "--output",
            missing.to_str().unwrap(),
        ],
    );
    assert!(!status.success());
    assert!(!missing.exists());
    let blocked_parent = dir.path().join("not-a-directory");
    std::fs::write(&blocked_parent, b"preserve").unwrap();
    let blocked = blocked_parent.join("output.jsonl");
    let before: std::collections::BTreeSet<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    for args in [
        vec![
            "learned-procedure",
            "list",
            "--output",
            blocked.to_str().unwrap(),
        ],
        vec![
            "learned-procedure",
            "search",
            "--query",
            "fixture",
            "--output",
            blocked.to_str().unwrap(),
        ],
    ] {
        let (status, _, _) = command(&state, &args);
        assert!(!status.success());
        assert_eq!(std::fs::read(&blocked_parent).unwrap(), b"preserve");
    }
    let after: std::collections::BTreeSet<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
}

fn call(state: &Path, input: serde_json::Value) -> serde_json::Value {
    let input = input.to_string();
    let (status, out, err) =
        command_with_input(state, &["call", "--json", "-"], Some(input.as_bytes()));
    assert!(
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    serde_json::from_slice(&out).unwrap()
}

#[test]
fn runtime_authored_procedure_is_found_by_management_search() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let _cleanup = RuntimeCleanup(state.clone());
    let source = dir.path().join("source");
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let memory = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Migration fixture requires verification.","evidence":"Observed migration.","applies_to":["user"]}]}}),
    );
    let evidence = memory["results"][0]["memory_ref"]["memory_id"].clone();
    assert!(evidence.is_string(), "{memory}");
    let prepared = call(
        &state,
        json!({"action":"remember_this","remember_this":{"procedure":{"operation":"create","evidence_memory_ids":[evidence]}}}),
    );
    let authored = call(
        &state,
        json!({"action":"remember_this","remember_this":{"authoring_ref":prepared["authoring_ref"],"procedure":{"operation":"create","evidence_memory_ids":[evidence],"applies_to":["user"],"name":"Verify migration fixture","use_when":"Before migration","instructions":"Run checks.","evidence":"Observed migration."}}}),
    );
    let id = authored["procedure_ref"]["procedure_id"].as_str().unwrap();
    {
        use rusqlite::OptionalExtension;
        let config = agentlaw_app::config::load(&state).unwrap().unwrap();
        let local = config.runtime_root(&state);
        let control = rusqlite::Connection::open_with_flags(
            local.join("control.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let legacy: Option<String> = control
            .query_row(
                "SELECT value FROM settings WHERE key='exact_active_generation'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        if let Some(path) = legacy {
            let path = std::path::PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                local.join(path)
            };
            let index = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let exists: bool = index
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM exact_units WHERE id=?1)",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!exists, "legacy index contains the procedure");
        }
    }
    let (status, list_out, list_err) = command(&state, &["learned-procedure", "list"]);
    assert!(status.success(), "{}", String::from_utf8_lossy(&list_err));
    let list_text = String::from_utf8(list_out).unwrap();
    assert!(list_text
        .lines()
        .any(|line| serde_json::from_str::<serde_json::Value>(line)
            .is_ok_and(|row| row["procedure_id"] == id)));
    for format in ["jsonl", "table"] {
        let (status, out, err) = command(
            &state,
            &[
                "learned-procedure",
                "search",
                "--query",
                "migration fixture",
                "--scope",
                "user",
                "--format",
                format,
            ],
        );
        assert!(status.success(), "{}", String::from_utf8_lossy(&err));
        assert!(String::from_utf8_lossy(&err).contains("matched procedure IDs"));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains(id), "{text}");
        assert!(text.contains("Verify migration fixture"), "{text}");
        if format == "table" {
            assert!(text.contains("PROCEDURE ID"), "{text}");
        }
    }
    let output = dir.path().join("search.jsonl");
    let (status, out, err) = command(
        &state,
        &[
            "learned-procedure",
            "search",
            "--query",
            "migration fixture",
            "--output",
            output.to_str().unwrap(),
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(Path::new(result["output"].as_str().unwrap()), output);
    assert!(result["matched"].as_u64().unwrap() >= 1);
    assert!(result["shown"].as_u64().unwrap() >= 1);
    assert_eq!(result["complete"], true);
    assert!(std::fs::read_to_string(output).unwrap().contains(id));
    // The runtime searches its active projection; the legacy in-process path
    // would create the old settings-key index while searching.
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let local = config.runtime_root(&state);
    let control = rusqlite::Connection::open_with_flags(
        local.join("control.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let legacy: i64 = control
        .query_row(
            "SELECT count(*) FROM settings WHERE key='exact_active_generation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy, 0, "management search used the legacy index");
    assert!(!local.join("exact-v3.sqlite").exists());
}

#[test]
fn competing_procedure_heads_are_listed_and_searched() {
    use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let source = dir.path().join("source");
    let cleanup = RuntimeCleanup(state.clone());
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    cleanup.shutdown();
    assert!(!agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let local = config.runtime_root(&state);
    let id = uuid::Uuid::new_v4().to_string();
    let heads = ["Competing glacier alpha", "Competing glacier beta"].into_iter().map(|name| Head {
        metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"name":name,"use_when":"glacier fixture","evidence_memory_ids":[]}),
        body: "Review the relevant procedure.".into(),
    }).collect();
    {
        let store = Store::open_with_coordination(
            &source,
            local.join("canonical"),
            agentlaw_runtime::installation::config::coordination_root(&state),
        )
        .unwrap();
        store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit: CurrentUnit {
                        entity_type: "learned_procedure".into(),
                        entity_id: id.clone(),
                        state: UnitState::Live { heads },
                    },
                    expected_versions: vec![],
                    evidence: "Imported concurrent procedure revisions".into(),
                }],
            )
            .unwrap();
    }
    let (status, out, err) = command(&state, &["learned-procedure", "list"]);
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let out = String::from_utf8(out).unwrap();
    let err = String::from_utf8(err).unwrap();
    assert!(out.contains(&id));
    assert!(out.contains("Competing glacier alpha"));
    assert!(out.contains("Competing glacier beta"));
    assert!(err.contains("competing current heads"), "{err}");
    let (status, out, err) = command(
        &state,
        &["learned-procedure", "search", "--query", "glacier"],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains(&id));
    assert!(out.contains("Competing glacier alpha"));
    assert!(out.contains("Competing glacier beta"));
}

#[test]
fn procedure_search_filters_project_and_machine_scopes() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let source = dir.path().join("source");
    let _cleanup = RuntimeCleanup(state.clone());
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let project_path = dir.path().join("project");
    std::fs::create_dir(&project_path).unwrap();
    let project = call(
        &state,
        json!({"action":"connect_project_memory","connect_project_memory":{"intent":"create","project_path":project_path,"project_name":"Filter fixture"}}),
    );
    let project_id = project["project_connection"]["project_id"]
        .as_str()
        .unwrap();
    let (status, machine_out, _) = command(&state, &["machine", "inspect"]);
    assert!(status.success());
    let machine: serde_json::Value = serde_json::from_slice(&machine_out).unwrap();
    let machine_id = machine["machine_id"].as_str().unwrap();
    let evidence_result = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Orchid filter evidence.","evidence":"Observed fixture.","applies_to":["user"]}]}}),
    );
    let evidence = evidence_result["results"][0]["memory_ref"]["memory_id"].clone();
    for (scope, name) in [
        ("project", "Orchid project procedure"),
        ("machine", "Orchid machine procedure"),
    ] {
        let prepared = call(
            &state,
            json!({"action":"remember_this","remember_this":{"project_path":project_path,"procedure":{"operation":"create","evidence_memory_ids":[evidence]}}}),
        );
        let written = call(
            &state,
            json!({"action":"remember_this","remember_this":{"project_path":project_path,"authoring_ref":prepared["authoring_ref"],"procedure":{"operation":"create","evidence_memory_ids":[evidence],"applies_to":[scope],"name":name,"use_when":"Orchid filtering","instructions":"Verify selected scope.","evidence":"Observed fixture."}}}),
        );
        assert_eq!(written["status"], "remembered", "{written}");
    }
    let (status, out, err) = command(
        &state,
        &[
            "learned-procedure",
            "search",
            "--query",
            "orchid",
            "--scope",
            "project",
            "--project",
            project_id,
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Orchid project procedure"));
    assert!(!text.contains("Orchid machine procedure"));
    let (status, out, err) = command(
        &state,
        &[
            "learned-procedure",
            "search",
            "--query",
            "orchid",
            "--scope",
            "machine",
            "--machine",
            machine_id,
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Orchid machine procedure"));
    assert!(!text.contains("Orchid project procedure"));
}

fn source_bytes(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut result = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    result
}

#[test]
fn history_export_preserves_output_and_source_on_rejected_paths() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let source = dir.path().join("source");
    let _cleanup = RuntimeCleanup(state.clone());
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let memory = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"History export fixture.","evidence":"Observed fixture.","applies_to":["user"]}]}}),
    );
    let id = memory["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let output = dir.path().join("history.jsonl");
    let (status, out, err) = command(
        &state,
        &[
            "history",
            "export",
            "--memory-id",
            id,
            "--output",
            output.to_str().unwrap(),
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let receipt: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(Path::new(receipt["output"].as_str().unwrap()), output);
    assert_eq!(receipt["change_count"], 1);
    assert_eq!(receipt["complete"], true);
    assert_eq!(receipt["schema_version"], 1);
    let bytes = std::fs::read(&output).unwrap();
    let rows: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows[0].as_object().unwrap().len(), 7);
    assert_eq!(rows[0]["record_type"], "manifest");
    assert_eq!(rows[1]["record_type"], "change");
    assert!(rows[1].get("delta_base_id").is_none());
    let original = source_bytes(&source);
    let entries_before: std::collections::BTreeSet<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let (status, _, _) = command(
        &state,
        &[
            "history",
            "export",
            "--memory-id",
            id,
            "--output",
            output.to_str().unwrap(),
        ],
    );
    assert!(!status.success());
    assert_eq!(std::fs::read(&output).unwrap(), bytes);
    let inside = source.join("history.jsonl");
    let (status, _, _) = command(
        &state,
        &[
            "history",
            "export",
            "--memory-id",
            id,
            "--output",
            inside.to_str().unwrap(),
        ],
    );
    assert!(!status.success());
    assert!(!inside.exists());
    assert_eq!(source_bytes(&source), original);
    let entries_after: std::collections::BTreeSet<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries_before, entries_after);
    #[cfg(any(unix, windows))]
    {
        let alias = dir.path().join("source-alias");
        #[cfg(unix)]
        let link = std::os::unix::fs::symlink(&source, &alias);
        #[cfg(windows)]
        let link = std::os::windows::fs::symlink_dir(&source, &alias);
        match link {
            Ok(()) => {
                let path = alias.join("history.jsonl");
                let (status, _, _) = command(
                    &state,
                    &[
                        "history",
                        "export",
                        "--memory-id",
                        id,
                        "--output",
                        path.to_str().unwrap(),
                    ],
                );
                assert!(!status.success());
                assert!(!path.exists());
                assert_eq!(source_bytes(&source), original);
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::PermissionDenied
                    || e.raw_os_error() == Some(1314) =>
            {
                eprintln!("Skipping link test: {e}")
            }
            Err(e) => panic!("cannot create alias: {e}"),
        }
    }
}

#[test]
fn continuity_includes_mcp_save_and_reuses_unchanged_commit() {
    use serde_json::json;
    use std::io::{BufRead, Write};
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let source = dir.path().join("source");
    let _cleanup = RuntimeCleanup(state.clone());
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    // continuity save commits into the memory repository; the user owns its
    // initialization and author identity, as in the git_ops fixtures.
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "Agentlaw Test"],
        vec!["config", "user.email", "test@example.invalid"],
    ] {
        let git = Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            git.status.success(),
            "{}",
            String::from_utf8_lossy(&git.stderr)
        );
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentlaw"))
        .env("AGENTLAW_HOME", &state)
        .args(["mcp", "serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let _mcp = OwnedProcess::frontend(child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while output.read_line(&mut line).unwrap() != 0 {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
            line.clear();
        }
    });
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).unwrap();
    assert!(rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .get("result")
        .is_some());
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    let text = "Continuity saves this MCP observation.";
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Observed fixture.","applies_to":["user"]}]}}}})).unwrap();
    input.flush().unwrap();
    let saved = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(saved["result"]["isError"], false, "{saved}");
    let (status, out, err) = command(&state, &["continuity", "save"]);
    assert!(
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let commit = receipt["commit_oid"].as_str().unwrap();
    let git = Command::new("git")
        .args(["-C", source.to_str().unwrap(), "grep", "-F", text, commit])
        .output()
        .unwrap();
    assert!(
        git.status.success(),
        "{}",
        String::from_utf8_lossy(&git.stderr)
    );
    let (status, out, err) = command(&state, &["continuity", "save"]);
    assert!(
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let next: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(next["commit_oid"], commit);
}

#[test]
fn repair_starts_runtime_for_corrupted_source_and_preserves_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let source = dir.path().join("source");
    let cleanup = RuntimeCleanup(state.clone());
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    cleanup.shutdown();
    assert!(!agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
    let format = source.join("format.md");
    std::fs::write(&format, b"corrupted source bytes").unwrap();
    let before = source_bytes(&source);
    let (status, out, err) = command(&state, &["repair"]);
    assert!(!status.success());
    let result: serde_json::Value =
        serde_json::from_slice(&out).unwrap_or_else(|_| serde_json::from_slice(&err).unwrap());
    assert_eq!(result["code"], "source_corrupt", "{result}");
    assert!(agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
    let endpoint = agentlaw_runtime::transport::read_endpoint(&state).unwrap();
    assert_ne!(endpoint.pid, std::process::id());
    assert_eq!(source_bytes(&source), before);
}

#[test]
fn repair_replaces_damaged_active_projection_and_preserves_source_and_pending() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let source = dir.path().join("source");
    let cleanup = RuntimeCleanup(state.clone());
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let text = "Index repair fixture retains this unique memory.";
    let saved = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Observed fixture.","applies_to":["user"]}]}}),
    );
    let id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let pending = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Unresolved duplicate fixture.","applies_to":["user"]}]}}),
    );
    assert_eq!(pending["status"], "resolution_required", "{pending}");
    let pending_ref = pending["pending_batch_ref"].clone();
    let inspect = || {
        call(
            &state,
            json!({"action":"remember_this","remember_this":{"pending_action":"inspect","pending_batch_ref":pending_ref}}),
        )
    };
    let pending_before = inspect();
    let history = |name: &str| {
        let path = dir.path().join(name);
        let (status, out, err) = command(
            &state,
            &[
                "history",
                "export",
                "--memory-id",
                id,
                "--output",
                path.to_str().unwrap(),
            ],
        );
        assert!(
            status.success(),
            "{} {}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        let mut rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        rows[0].as_object_mut().unwrap().remove("exported_at");
        rows
    };
    let history_before = history("before-history.jsonl");
    let source_before = source_bytes(&source);
    cleanup.shutdown();
    assert!(!agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let local = config.runtime_root(&state);
    let projection = || -> serde_json::Value {
        let db = rusqlite::Connection::open_with_flags(
            local.join("control.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let value: String = db
            .query_row(
                "SELECT value FROM settings WHERE key='active_projection'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&value).unwrap()
    };
    let old = projection();
    let old_path = std::path::PathBuf::from(old["exact_file"].as_str().unwrap());
    std::fs::write(&old_path, b"corrupt exact projection").unwrap();
    let (status, out, err) = command(&state, &["repair"]);
    assert!(
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(result["original_memory_preserved"], true);
    assert_eq!(result["derived"]["lexical_complete"], true);
    assert_eq!(result["derived"]["semantic_complete"], false);
    let new = projection();
    assert_ne!(new["id"], old["id"]);
    let db = rusqlite::Connection::open_with_flags(
        new["exact_file"].as_str().unwrap(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM exact_units WHERE id=?1)",
            [id],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    let recalled = call(
        &state,
        json!({"action":"recall","recall":{"memory_ids":[id]}}),
    );
    assert_eq!(
        recalled["memories"][0]["current_heads"][0]["what_to_remember"],
        text
    );
    assert_eq!(source_bytes(&source), source_before);
    assert_eq!(inspect()["proposals"], pending_before["proposals"]);
    assert_eq!(history("after-history.jsonl"), history_before);
}

#[test]
fn repair_recovers_damaged_journal_in_running_runtime() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let source = dir.path().join("source");
    let cleanup = RuntimeCleanup(state.clone());
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let text = "Journal recovery preserves this decision.";
    let saved = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Observed fixture.","applies_to":["user"]}]}}),
    );
    let id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let pending = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Pending duplicate fixture.","applies_to":["user"]}]}}),
    );
    assert_eq!(pending["status"], "resolution_required", "{pending}");
    let pending_ref = pending["pending_batch_ref"].clone();
    let pending_before = call(
        &state,
        json!({"action":"remember_this","remember_this":{"pending_action":"inspect","pending_batch_ref":pending_ref}}),
    );
    let original = source_bytes(&source);
    cleanup.shutdown();
    assert!(!agentlaw_runtime::installation::owner::owner_alive(&state).unwrap());
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let canonical = config.runtime_root(&state).join("canonical");
    {
        let journal = rusqlite::Connection::open_with_flags(
            canonical.join("journal.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut statement = journal
            .prepare("SELECT operation_id, sequence FROM publications ORDER BY sequence")
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .unwrap();
        let records: Vec<_> = rows.map(Result::unwrap).collect();
        assert!(!records.is_empty(), "missing publication receipts");
        for (index, (operation, sequence)) in records.iter().enumerate() {
            let bytes: [u8; 8] = sequence.as_slice().try_into().unwrap();
            assert_eq!(
                u64::from_be_bytes(bytes),
                index as u64 + 1,
                "nonconsecutive publications"
            );
            assert!(
                canonical
                    .join("recovery")
                    .join(operation)
                    .join("published")
                    .is_file(),
                "missing durable receipt"
            );
        }
    }
    std::fs::write(
        canonical.join("journal.sqlite"),
        b"damaged original sqlite bytes",
    )
    .unwrap();
    let mut mcp = Mcp::start(&state);
    let failed = mcp.call(2, json!({"action":"recall","recall":{"memory_ids":[id]}}));
    assert_eq!(failed["result"]["isError"], true, "{failed}");
    let endpoint = agentlaw_runtime::transport::read_endpoint(&state).unwrap();
    let (status, out, err) = command(&state, &["repair"]);
    assert!(
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let repaired: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(repaired["original_memory_preserved"], true);
    assert_eq!(
        agentlaw_runtime::transport::read_endpoint(&state)
            .unwrap()
            .pid,
        endpoint.pid
    );
    let recalled = mcp.call(3, json!({"action":"recall","recall":{"memory_ids":[id]}}));
    assert_eq!(recalled["result"]["isError"], false, "{recalled}");
    assert_eq!(
        recalled["result"]["structuredContent"]["memories"][0]["current_heads"][0]
            ["what_to_remember"],
        text
    );
    let inspected = mcp.call(4, json!({"action":"remember_this","remember_this":{"pending_action":"inspect","pending_batch_ref":pending_ref}}));
    assert_eq!(
        inspected["result"]["structuredContent"]["proposals"],
        pending_before["proposals"]
    );
    assert_eq!(source_bytes(&source), original);
}

#[test]
fn repair_completes_while_mcp_recalls_preserve_memory_and_pending() {
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let source = dir.path().join("source");
    let _cleanup = RuntimeCleanup(state.clone());
    let (status, _, err) = command(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
    );
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    let text = "Concurrent recall preserves this observation.";
    let saved = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Observed fixture.","applies_to":["user"]}]}}),
    );
    let id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let pending = call(
        &state,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Pending duplicate fixture.","applies_to":["user"]}]}}),
    );
    assert_eq!(pending["status"], "resolution_required", "{pending}");
    let pending_ref = pending["pending_batch_ref"].clone();
    let pending_before = call(
        &state,
        json!({"action":"remember_this","remember_this":{"pending_action":"inspect","pending_batch_ref":pending_ref}}),
    );
    let source_before = source_bytes(&source);
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let worker = {
        let stop = stop.clone();
        let id = id.clone();
        let state = state.clone();
        std::thread::spawn(move || {
            let mut mcp = Mcp::start(&state);
            let mut sequence = 2;
            while !stop.load(Ordering::Acquire) {
                let response = mcp.call(
                    sequence,
                    json!({"action":"recall","recall":{"memory_ids":[id]}}),
                );
                if tx.send(response).is_err() {
                    break;
                }
                sequence += 1;
            }
        })
    };
    let check = |response: serde_json::Value| {
        assert_eq!(response["result"]["isError"], false, "{response}");
        assert_eq!(
            response["result"]["structuredContent"]["memories"][0]["current_heads"][0]
                ["what_to_remember"],
            text,
            "{response}"
        );
    };
    check(
        rx.recv_timeout(Duration::from_secs(60))
            .expect("first recall"),
    );
    let (repair_tx, repair_rx) = mpsc::channel();
    let repair = {
        let state = state.clone();
        std::thread::spawn(move || {
            repair_tx.send(command(&state, &["repair"])).unwrap();
        })
    };
    let mut observed = 0;
    let deadline = Instant::now() + Duration::from_secs(180);
    let (status, out, err) = loop {
        while let Ok(response) = rx.try_recv() {
            check(response);
            observed += 1;
        }
        if let Ok(result) = repair_rx.try_recv() {
            break result;
        }
        assert!(Instant::now() < deadline, "repair timed out");
        std::thread::sleep(Duration::from_millis(5));
    };
    stop.store(true, Ordering::Release);
    worker.join().unwrap();
    for response in rx.try_iter() {
        check(response);
        observed += 1;
    }
    repair.join().unwrap();
    assert!(observed > 0, "no recall was observed during repair");
    assert!(
        status.success(),
        "{} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(result["status"], "repaired");
    assert_eq!(result["original_memory_preserved"], true);
    assert_eq!(result["retained_proposals_preserved"], true);
    assert_eq!(source_bytes(&source), source_before);
    let pending_after = call(
        &state,
        json!({"action":"remember_this","remember_this":{"pending_action":"inspect","pending_batch_ref":pending_ref}}),
    );
    assert_eq!(pending_after["proposals"], pending_before["proposals"]);
}
