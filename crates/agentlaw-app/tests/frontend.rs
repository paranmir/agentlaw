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
    if let Some(input) = input {
        write!(child.stdin.take().unwrap(), "{input}").unwrap();
    } else {
        drop(child.stdin.take());
    }
    // Drain both pipes while waiting: a full pipe must not masquerade as a
    // product hang. Bound the test and terminate only the command it owns.
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
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
    let until = std::time::Instant::now() + Duration::from_secs(180);
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break (status, false);
        }
        if std::time::Instant::now() >= until {
            let _ = child.kill();
            break (child.wait().unwrap(), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = Output {
        status,
        stdout: output_reader.join().unwrap(),
        stderr: error_reader.join().unwrap(),
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
fn daemon(state: &Path) -> Daemon {
    let worker = state.join("worker");
    let child = command(state)
        .args(["worker-daemon", "--state-dir"])
        .arg(&worker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let guard = Daemon::new(child);
    // Shared CI runners can need longer than five seconds for process startup.
    // Return as soon as the endpoint exists; this is a deadline, not a delay.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if worker.join("endpoint.json").is_file() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("Owned test daemon failed to start within 30 seconds: {worker:?}");
}
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
    let assets = agentlaw_app::install::installed_assets(&state)
        .unwrap()
        .unwrap();
    let child = command(&state)
        .args(["worker-daemon", "--state-dir"])
        .arg(state.join("worker"))
        .arg("--model")
        .arg(&assets.onnx_model)
        .arg("--tokenizer")
        .arg(&assets.tokenizer_json)
        .arg("--ort-library")
        .arg(&assets.runtime_library)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon::new(child);
    for _ in 0..200 {
        if state.join("worker/endpoint.json").is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
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
            json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":body,"evidence":"Confirmed in the actual installed-model integration test.","applies_to":["user"]}]}),
        ),
    );
    assert!(out.status.success(), "{saved}");
    assert_eq!(saved["status"], "remembered");
    let id = saved["results"][0]["memory_ref"]["memory_id"].clone();
    let (out, recalled) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({"action":"recall","memory_ids":[id]})),
    );
    assert!(out.status.success(), "{recalled}");
    assert_eq!(
        recalled["memories"][0]["current_heads"][0]["what_to_remember"], body,
        "{recalled}"
    );
    let (out, search) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({"action":"recall","recall_for":"atomic publication of memory updates"})),
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
    writeln!(stdin,"{}",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","memory_ids":[id]}}})).unwrap();
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
    let replica_child = command(&replica)
        .args(["worker-daemon", "--state-dir"])
        .arg(replica.join("worker"))
        .arg("--model")
        .arg(&assets.onnx_model)
        .arg("--tokenizer")
        .arg(&assets.tokenizer_json)
        .arg("--ort-library")
        .arg(&assets.runtime_library)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _replica_daemon = Daemon::new(replica_child);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !replica.join("worker/endpoint.json").is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "replica broker startup timeout"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
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
        Some(json!({"action":"recall","memory_ids":[id]})),
    );
    assert_eq!(
        replica_exact["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
    let (out, replica_search) = run(
        &replica,
        &["call", "--json", "-"],
        Some(json!({"action":"recall","recall_for":"atomic publication of memory updates"})),
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
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["name"],
        "agentlaw"
    );
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
    let _daemon = daemon(&state);
    let first_body = "Use argument arrays when invoking PowerShell with paths containing spaces.";
    let current_body = "Use PowerShell argument arrays for paths containing spaces; never interpolate the path into a command string.";
    let (out, first) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":first_body,"evidence":"A quoted path failed in the original command.","applies_to":["user"]}]}),
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
            json!({"action":"remember_this","memories":[{"operation":"evolve","parent_refs":[original_ref],"what_to_remember":current_body,"evidence":"The user clarified that string interpolation is also unsafe."}]}),
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
            json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":current_body,"evidence":"Unresolved duplicate proposal: retain this evidence until the user decides.","applies_to":["user"]}]}),
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
            Some(json!({"action":"recall","memory_ids":[id]})),
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
                json!({"action":"remember_this","pending_action":"inspect","pending_batch_ref":pending_ref}),
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
    let (out, doctor) = run(&state, &["doctor"], None);
    assert!(out.status.success(), "{doctor}");
    assert_eq!(doctor["read_only"], true);
    assert_eq!(doctor["source"]["validated"], true);
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
    for action in ["doctor", "repair"] {
        let (out, error) = run(&state, &[action], None);
        assert!(!out.status.success(), "{action}: {error}");
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

    let doctor_output = invoke(&["doctor"]);
    let doctor: Value = serde_json::from_slice(&doctor_output.stdout).unwrap();
    assert!(doctor_output.status.success(), "{doctor}");
    assert_eq!(doctor["source"]["validated"], true);

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
    let _daemon = daemon(&state);
    let (out, written) = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({
            "action":"remember_this", "memories":[{"operation":"create", "what_to_remember":"Use PowerShell argument arrays for paths with spaces.", "evidence":"Verified in the shell.", "applies_to":["user"]}]
        })),
    );
    assert!(out.status.success(), "{written}");
    assert_eq!(written["status"], "remembered");
    let reference = &written["results"][0]["memory_ref"];
    let request = json!({"action":"recall", "memory_ids":[reference["memory_id"]]});
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"integration","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agentlaw","arguments":request}}),
    ];
    let mut child = command(&state)
        .args(["mcp", "serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for message in messages {
            writeln!(stdin, "{message}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines: Vec<Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(lines[2]["result"]["isError"], false);
    let recalled = &lines[2]["result"]["structuredContent"];
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
    let _daemon = daemon(&state);
    let (_, saved) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":"Store A contains this marker.","evidence":"Store switch fixture.","applies_to":["user"]}]}),
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
        writeln!(input,"{}",json!({"jsonrpc":"2.0","id":n,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","memory_ids":[id]}}})).unwrap();
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
    let _daemon = daemon(&state);
    let write = json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":"Use argument arrays for paths with spaces.","evidence":"User corrected command quoting.","applies_to":["user"]}]});
    let (_, first) = run(&state, &["call", "--json", "-"], Some(write.clone()));
    assert_eq!(first["status"], "remembered");
    let (_, pending) = run(&state, &["call", "--json", "-"], Some(write));
    assert_eq!(pending["status"], "resolution_required");
    let config = agentlaw_app::config::load(&state).unwrap().unwrap();
    let control = config.runtime_root(&state).join("control.sqlite");
    let backup = control.with_extension("held");
    std::fs::rename(&control, &backup).unwrap();
    let bytes = std::fs::read(&backup).unwrap();
    let selection = std::fs::read(state.join("config.json")).unwrap();
    let recall =
        json!({"action":"recall","memory_ids":[first["results"][0]["memory_ref"]["memory_id"]]});
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
            json!({"action":"remember_this","pending_action":"inspect","pending_batch_ref":pending["pending_batch_ref"]}),
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
    let _daemon = daemon(&state);
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
            json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":body,"evidence":"Verified command invocation.","applies_to":["user"]}]}),
        ),
    );
    assert_eq!(saved["status"], "remembered", "{saved}");
    let reference = saved["results"][0]["memory_ref"].clone();
    let request = json!({"action":"recall","memory_ids":[reference["memory_id"]]});
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
    let (_, changed) = run(
        &state,
        &["call", "--json", "-"],
        Some(
            json!({"action":"remember_this","memories":[{"operation":"evolve","parent_refs":[reference],"what_to_remember":"Current concise command guidance.","evidence":"User requested a coherent rewrite."}]}),
        ),
    );
    assert_eq!(changed["status"], "remembered", "{changed}");
    assert_eq!(
        std::fs::read(path).unwrap(),
        frozen,
        "later writes changed an already acquired artifact"
    );
    let (_, current) = run(&state, &["call", "--json", "-"], Some(request));
    assert_eq!(
        current["memories"][0]["current_heads"][0]["what_to_remember"],
        "Current concise command guidance."
    );
}
