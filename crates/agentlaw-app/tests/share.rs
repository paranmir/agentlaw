#[path = "../../../tests/support/owned_process.rs"]
mod owned_process;
#[path = "../../../tests/support/runtime_cleanup.rs"]
mod runtime_cleanup;

use owned_process::OwnedProcess;
use runtime_cleanup::RuntimeCleanup;
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn cli(
    state: &Path,
    args: &[&str],
    input: Option<&Value>,
) -> (std::process::ExitStatus, Value, Vec<u8>) {
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
        write!(child.stdin.take().unwrap(), "{input}").unwrap();
    }
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let mut child = OwnedProcess::frontend(child);
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "CLI timed out: {args:?}");
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = out.join().unwrap();
    let err = err.join().unwrap();
    let value = serde_json::from_slice(&out).unwrap_or_else(|_| {
        panic!(
            "invalid JSON: stdout={}, stderr={}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        )
    });
    (status, value, err)
}

fn ok(state: &Path, args: &[&str], input: Option<&Value>) -> Value {
    let (status, value, err) = cli(state, args, input);
    assert!(
        status.success(),
        "{args:?}: {value}; {}",
        String::from_utf8_lossy(&err)
    );
    value
}
fn call(state: &Path, input: Value) -> Value {
    ok(state, &["call", "--json", "-"], Some(&input))
}
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct Mcp {
    _child: OwnedProcess,
    input: std::process::ChildStdin,
    rx: std::sync::mpsc::Receiver<Value>,
}
impl Mcp {
    fn start(state: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agentlaw"))
            .env("AGENTLAW_HOME", state)
            .env_remove("AGENTLAW_ONNX_MODEL")
            .env_remove("AGENTLAW_TOKENIZER")
            .env_remove("AGENTLAW_ORT_LIBRARY")
            .args(["mcp", "serve", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let mut mcp = Self {
            _child: OwnedProcess::frontend(child),
            input,
            rx,
        };
        mcp.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}));
        assert!(mcp.reply().get("result").is_some());
        mcp.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        mcp
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn reply(&self) -> Value {
        self.rx
            .recv_timeout(Duration::from_secs(60))
            .expect("MCP response")
    }
    fn call(&mut self, id: u64, input: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"agentlaw","arguments":input}}));
        self.reply()
    }
}

struct Installation {
    state: PathBuf,
    source: PathBuf,
    _cleanup: RuntimeCleanup,
}
impl Installation {
    fn new(root: &Path, name: &str, bare: &Path) -> Self {
        let state = root.join(format!("{name}-state"));
        let source = root.join(format!("{name}-source"));
        let cleanup = RuntimeCleanup(state.clone());
        ok(
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
        git(&source, &["init", "--initial-branch=main"]);
        git(&source, &["config", "user.name", "Fixture Author"]);
        git(
            &source,
            &["config", "user.email", "fixture@example.invalid"],
        );
        git(
            &source,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        Self {
            state,
            source,
            _cleanup: cleanup,
        }
    }
    fn save(&self, text: &str) -> Value {
        call(
            &self.state,
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Observed fixture.","applies_to":["user"]}]}}),
        )
    }
}
fn bare(root: &Path) -> PathBuf {
    let bare = root.join("remote.git");
    std::fs::create_dir(&bare).unwrap();
    git(&bare, &["init", "--bare"]);
    bare
}
fn push(store: &Installation) -> Value {
    ok(&store.state, &["continuity", "save"], None);
    let review = ok(
        &store.state,
        &[
            "share",
            "inspect",
            "--remote",
            "origin",
            "--target-ref",
            "refs/heads/main",
        ],
        None,
    );
    let reference = review["review_ref"].as_str().unwrap();
    let pushed = ok(
        &store.state,
        &["share", "push", "--review", reference],
        None,
    );
    assert_eq!(pushed["commit_oid"], review["commit_oid"]);
    review
}
fn incoming(root: &Path, bare: &Path, text: &str) -> (Installation, String, String) {
    let remote = Installation::new(root, "remote", bare);
    let memory = remote.save(text);
    let id = memory["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let review = push(&remote);
    let commit = review["commit_oid"].as_str().unwrap().to_owned();
    (remote, id, commit)
}
fn prepare(store: &Installation, commit: &str) -> String {
    let fetched = ok(
        &store.state,
        &["share", "fetch", "--remote", "origin"],
        None,
    );
    assert_eq!(fetched["remote"], "origin");
    assert!(
        fetched["fetched_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry
                .as_str()
                .is_some_and(|line| line.split_whitespace().next() == Some(commit))),
        "{fetched}"
    );
    let prepared = ok(
        &store.state,
        &["share", "import", "prepare", "--commit", commit],
        None,
    );
    assert_eq!(prepared["status"], "review_required");
    prepared["import_ref"].as_str().unwrap().to_owned()
}
fn staged(
    store: &Installation,
    reference: &str,
    input: Value,
) -> (std::process::ExitStatus, Value, Vec<u8>) {
    cli(
        &store.state,
        &["share", "import", "call", "--ref", reference, "--json", "-"],
        Some(&input),
    )
}
fn source_bytes(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    files
}

#[test]
fn share_inspect_push_and_fetch_use_runtime_and_preserve_reviewed_commit() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let store = Installation::new(dir.path(), "active", &remote);
    store.save("A local observation for sharing.");
    ok(&store.state, &["continuity", "save"], None);
    let reviewed = ok(
        &store.state,
        &[
            "share",
            "inspect",
            "--remote",
            "origin",
            "--target-ref",
            "refs/heads/main",
        ],
        None,
    );
    assert_eq!(reviewed["remote"], "origin");
    assert_eq!(reviewed["target_ref"], "refs/heads/main");
    let commit = reviewed["commit_oid"].as_str().unwrap();
    let token = reviewed["review_ref"].as_str().unwrap();
    let pushed = ok(&store.state, &["share", "push", "--review", token], None);
    assert_eq!(pushed["commit_oid"], commit);
    assert_eq!(pushed["target_ref"], "refs/heads/main");
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), commit);
    let fetched = ok(
        &store.state,
        &["share", "fetch", "--remote", "origin"],
        None,
    );
    assert_eq!(fetched["remote"], "origin");
    assert!(fetched["fetch_ref"].is_string());
    assert!(fetched["fetched_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str().is_some_and(|line| line.contains(commit))));
    assert!(agentlaw_runtime::installation::owner::owner_alive(&store.state).unwrap());
}

#[test]
fn import_review_is_isolated_until_publish_and_then_visible_to_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    let (_remote, id, commit) = incoming(dir.path(), &remote, "Incoming record before review.");
    let before = source_bytes(&active.source);
    let reference = prepare(&active, &commit);
    let inspected = ok(
        &active.state,
        &["share", "import", "inspect", "--ref", &reference],
        None,
    );
    assert_eq!(inspected["import_ref"], reference);
    assert_eq!(inspected["status"], "review_required");
    let mut mcp = Mcp::start(&active.state);
    let active_before = mcp.call(2, json!({"action":"recall","recall":{"memory_ids":[id]}}));
    assert!(
        !active_before
            .to_string()
            .contains("Incoming record before review."),
        "{active_before}"
    );
    let (status, recalled, _) = staged(
        &active,
        &reference,
        json!({"action":"recall","recall":{"memory_ids":[id]}}),
    );
    assert!(status.success(), "{recalled}");
    assert_eq!(recalled["workspace"], "isolated_import_review");
    assert_eq!(recalled["active_memory_changed"], false);
    let parent = recalled["memories"][0]["current_heads"][0]["memory_ref"].clone();
    assert_eq!(parent["memory_id"], id);
    let (status, evolved, _) = staged(
        &active,
        &reference,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"evolve","parent_refs":[parent],"what_to_remember":"Incoming record after approved review.","evidence":"Reviewed staged content."}]}}),
    );
    assert!(status.success(), "{evolved}");
    assert_eq!(evolved["workspace"], "isolated_import_review");
    assert_eq!(evolved["active_memory_changed"], false);
    assert_eq!(source_bytes(&active.source), before);
    let still_active = mcp.call(3, json!({"action":"recall","recall":{"memory_ids":[id]}}));
    assert!(
        !still_active.to_string().contains("Incoming record"),
        "{still_active}"
    );
    let resolution = ok(
        &active.state,
        &["share", "import", "resolve", "--ref", &reference],
        None,
    );
    assert_eq!(
        resolution["ready_for_user_confirmation"], true,
        "{resolution}"
    );
    let token = resolution["resolution_token"].as_str().unwrap();
    let published = ok(
        &active.state,
        &[
            "share",
            "import",
            "publish",
            "--ref",
            &reference,
            "--resolution",
            token,
            "--user-confirmed",
        ],
        None,
    );
    assert_eq!(published["status"], "published_and_committed");
    assert!(published["commit_oid"].is_string());
    assert_eq!(published["pushed"], false);
    let after = mcp.call(4, json!({"action":"recall","recall":{"memory_ids":[id]}}));
    assert_eq!(after["result"]["isError"], false, "{after}");
    assert!(
        after
            .to_string()
            .contains("Incoming record after approved review."),
        "{after}"
    );
}

#[test]
fn staged_calls_reject_store_switch_and_sync_and_keep_delivery_and_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    let remote_store = Installation::new(dir.path(), "remote", &remote);
    let text = format!(
        "orchid isolated semantic fixture {}",
        "large body ".repeat(1600)
    );
    let written = remote_store.save(&text);
    let id = written["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let reviewed = push(&remote_store);
    let commit = reviewed["commit_oid"].as_str().unwrap();
    let reference = prepare(&active, commit);
    ok(
        &active.state,
        &["config", "set", "response_limit_bytes", "4096"],
        None,
    );
    let config = std::fs::read(active.state.join("config.json")).unwrap();
    // Existing rejection codes: a store switch is not an import-review request,
    // and sync needs the installation's sync adapter rather than a review workspace.
    for (input, code) in [
        (
            json!({"action":"connect_project_memory","connect_project_memory":{"project_path":dir.path(),"intent":"discover","memory_store_path":remote_store.source}}),
            "invalid_import_request",
        ),
        (
            json!({"action":"sync","sync":{"command":"status","operation_id":uuid::Uuid::new_v4().to_string()}}),
            "sync_requires_app_adapter",
        ),
    ] {
        let (status, value, _) = staged(&active, &reference, input);
        assert!(!status.success(), "{value}");
        assert_eq!(value["code"], code, "{value}");
        assert_eq!(
            std::fs::read(active.state.join("config.json")).unwrap(),
            config
        );
    }
    let (status, lexical, _) = staged(
        &active,
        &reference,
        json!({"action":"recall","recall":{"recall_for":"orchid isolated semantic fixture"}}),
    );
    assert!(status.success(), "{lexical}");
    assert_eq!(lexical["workspace"], "isolated_import_review");
    assert!(lexical.to_string().contains(id), "{lexical}");
    assert!(
        lexical.to_string().contains("semantic_channel_incomplete")
            || lexical.to_string().contains("semantic_unavailable"),
        "{lexical}"
    );
    let (status, large, _) = staged(
        &active,
        &reference,
        json!({"action":"recall","recall":{"memory_ids":[id]}}),
    );
    assert!(status.success(), "{large}");
    assert_eq!(large["code"], "complete_content_in_file", "{large}");
    let artifact = large["artifact"]["path"].as_str().unwrap();
    let contents = std::fs::read_to_string(artifact).unwrap();
    assert!(contents.contains(&text));
    assert!(!active.state.join("worker/endpoint.json").exists());
    #[cfg(windows)]
    {
        let output = Command::new("powershell").args(["-NoProfile", "-Command", "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*worker-daemon*' } | Select-Object -ExpandProperty CommandLine"]).output().unwrap();
        assert!(output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(active.state.to_str().unwrap()));
    }
}

#[test]
fn cli_sync_matches_mcp_errors_and_respects_delegation_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    let mut mcp = Mcp::start(&active.state);
    let operation = uuid::Uuid::new_v4().to_string();
    let policy = uuid::Uuid::new_v4().to_string();
    let request = uuid::Uuid::new_v4().to_string();
    let cases = [
        (
            vec!["sync", "status", "--operation", operation.as_str()],
            json!({"command":"status","operation_id":operation}),
        ),
        (
            vec![
                "sync",
                "start",
                "--policy",
                policy.as_str(),
                "--request-id",
                request.as_str(),
            ],
            json!({"command":"start","policy_id":policy,"request_id":request}),
        ),
    ];
    for (index, (args, sync)) in cases.into_iter().enumerate() {
        let (status, cli_result, _) = cli(&active.state, &args, None);
        let mcp_result = mcp.call(index as u64 + 2, json!({"action":"sync","sync":sync}));
        assert!(!status.success(), "{cli_result}");
        assert_eq!(mcp_result["result"]["isError"], true, "{mcp_result}");
        assert_eq!(
            cli_result, mcp_result["result"]["structuredContent"],
            "CLI and MCP sync errors differ"
        );
    }
    let solution = dir.path().join("solution.json");
    std::fs::write(
        &solution,
        serde_json::to_vec(
            &json!({"units":[],"redirects":[],"projects":[],"dependent_dispositions":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    for action in ["resolve", "resume", "hold", "cancel"] {
        let mut args = vec![
            "sync",
            action,
            "--operation",
            operation.as_str(),
            "--revision",
            "1",
            "--request-id",
            request.as_str(),
        ];
        if action == "resolve" {
            args.extend(["--solution", solution.to_str().unwrap()]);
        }
        let (status, value, _) = cli(&active.state, &args, None);
        assert!(!status.success(), "{action}: {value}");
        assert_eq!(
            value["code"], "sync_operation_not_found",
            "{action}: {value}"
        );
    }
    let proposed = ok(
        &active.state,
        &[
            "sync",
            "policy",
            "propose",
            "--remote",
            "origin",
            "--target-ref",
            "refs/heads/main",
        ],
        None,
    );
    assert_eq!(proposed["remote"], "origin");
    assert_eq!(proposed["target_ref"], "refs/heads/main");
    assert_eq!(proposed["enabled"], false);
    let path = dir.path().join("policy.json");
    std::fs::write(&path, serde_json::to_vec(&proposed).unwrap()).unwrap();
    let policy_path = active.state.join("policies").join(format!(
        "sync-{}.json",
        proposed["policy_id"].as_str().unwrap()
    ));
    let (status, refused, _) = cli(
        &active.state,
        &[
            "sync",
            "policy",
            "configure",
            "--file",
            path.to_str().unwrap(),
        ],
        None,
    );
    assert!(!status.success());
    assert_eq!(refused["code"], "delegation_confirmation_required");
    assert!(!policy_path.exists());
    let configured = ok(
        &active.state,
        &[
            "sync",
            "policy",
            "configure",
            "--file",
            path.to_str().unwrap(),
            "--confirm-delegation",
        ],
        None,
    );
    assert_eq!(configured["policy_id"], proposed["policy_id"]);
    assert!(policy_path.exists());
    let before = std::fs::read(&policy_path).unwrap();
    let (status, refused, _) = cli(
        &active.state,
        &[
            "sync",
            "accept-findings",
            "--operation",
            &operation,
            "--candidate",
            &"1".repeat(40),
            "--findings-digest",
            &"2".repeat(64),
        ],
        None,
    );
    assert!(!status.success());
    assert_eq!(refused["code"], "sharing_choice_required");
    assert_eq!(std::fs::read(&policy_path).unwrap(), before);
}

fn resolve(active: &Installation, reference: &str) -> String {
    let value = ok(
        &active.state,
        &["share", "import", "resolve", "--ref", reference],
        None,
    );
    assert_eq!(value["ready_for_user_confirmation"], true, "{value}");
    value["resolution_token"].as_str().unwrap().to_owned()
}

fn publish(
    active: &Installation,
    reference: &str,
    token: &str,
) -> (std::process::ExitStatus, Value, Vec<u8>) {
    cli(
        &active.state,
        &[
            "share",
            "import",
            "publish",
            "--ref",
            reference,
            "--resolution",
            token,
            "--user-confirmed",
        ],
        None,
    )
}

fn mcp_save(mcp: &mut Mcp, n: u64, text: &str) -> Value {
    let result = mcp.call(n, json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":text,"evidence":"Observed fixture.","applies_to":["user"]}]}}));
    assert_eq!(result["result"]["isError"], false, "{result}");
    result["result"]["structuredContent"].clone()
}

#[test]
fn import_publish_and_later_mcp_save_both_survive() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    let (_remote, incoming_id, commit) =
        incoming(dir.path(), &remote, "Imported independent observation.");
    let mut mcp = Mcp::start(&active.state);
    let reference = prepare(&active, &commit);
    let token = resolve(&active, &reference);
    let (status, published, _) = publish(&active, &reference, &token);
    assert!(status.success(), "{published}");
    assert_eq!(published["status"], "published_and_committed");
    let saved = mcp_save(&mut mcp, 2, "Saved after imported publication.");
    let saved_id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let recalled = mcp.call(
        3,
        json!({"action":"recall","recall":{"memory_ids":[incoming_id,saved_id]}}),
    );
    assert_eq!(recalled["result"]["isError"], false, "{recalled}");
    let text = recalled.to_string();
    assert!(text.contains("Imported independent observation."));
    assert!(text.contains("Saved after imported publication."));
}

#[test]
fn changed_active_basis_rejects_old_import_approval_and_keeps_mcp_save() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    let (_remote, _id, commit) = incoming(dir.path(), &remote, "Imported independent observation.");
    let reference = prepare(&active, &commit);
    let token = resolve(&active, &reference);
    let mut mcp = Mcp::start(&active.state);
    let saved = mcp_save(&mut mcp, 2, "Saved before stale publication.");
    let id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let before = source_bytes(&active.source);
    let (status, rejected, _) = publish(&active, &reference, &token);
    assert!(!status.success(), "{rejected}");
    assert!(
        matches!(
            rejected["code"].as_str(),
            Some("stale_import_review" | "stale_import_resolution")
        ),
        "{rejected}"
    );
    assert_eq!(source_bytes(&active.source), before);
    let recalled = mcp.call(3, json!({"action":"recall","recall":{"memory_ids":[id]}}));
    assert!(
        recalled
            .to_string()
            .contains("Saved before stale publication."),
        "{recalled}"
    );
}

#[test]
fn staged_edit_invalidates_old_resolution_and_new_token_publishes_it() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    let (_remote, id, commit) = incoming(dir.path(), &remote, "Review before staged edit.");
    let reference = prepare(&active, &commit);
    let old = resolve(&active, &reference);
    let (status, original, _) = staged(
        &active,
        &reference,
        json!({"action":"recall","recall":{"memory_ids":[id]}}),
    );
    assert!(status.success(), "{original}");
    let parent = original["memories"][0]["current_heads"][0]["memory_ref"].clone();
    let (status, edited, _) = staged(
        &active,
        &reference,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"evolve","parent_refs":[parent],"what_to_remember":"Reviewed staged generation.","evidence":"Explicit staged revision."}]}}),
    );
    assert!(status.success(), "{edited}");
    assert_eq!(edited["active_memory_changed"], false);
    let before = source_bytes(&active.source);
    let (status, stale, _) = publish(&active, &reference, &old);
    assert!(!status.success(), "{stale}");
    assert!(
        matches!(
            stale["code"].as_str(),
            Some("stale_import_resolution" | "stale_import_review")
        ),
        "{stale}"
    );
    assert_eq!(source_bytes(&active.source), before);
    let token = resolve(&active, &reference);
    assert_ne!(token, old);
    let (status, receipt, _) = publish(&active, &reference, &token);
    assert!(status.success(), "{receipt}");
    assert_eq!(receipt["status"], "published_and_committed");
    let recalled = call(
        &active.state,
        json!({"action":"recall","recall":{"memory_ids":[id]}}),
    );
    assert!(
        recalled.to_string().contains("Reviewed staged generation."),
        "{recalled}"
    );
}

#[test]
fn staged_pending_survives_runtime_restart() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    let (_remote, _id, commit) = incoming(dir.path(), &remote, "Staged pending duplicate.");
    let reference = prepare(&active, &commit);
    let before = source_bytes(&active.source);
    let (status, pending, _) = staged(
        &active,
        &reference,
        json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Staged pending duplicate.","evidence":"Unresolved fixture.","applies_to":["user"]}]}}),
    );
    assert!(status.success(), "{pending}");
    assert_eq!(pending["status"], "resolution_required", "{pending}");
    let pending_ref = pending["pending_batch_ref"].clone();
    active._cleanup.shutdown();
    assert!(!agentlaw_runtime::installation::owner::owner_alive(&active.state).unwrap());
    let (status, inspected, _) = staged(
        &active,
        &reference,
        json!({"action":"remember_this","remember_this":{"pending_action":"inspect","pending_batch_ref":pending_ref}}),
    );
    assert!(status.success(), "{inspected}");
    assert_eq!(inspected["workspace"], "isolated_import_review");
    assert_eq!(
        inspected["proposals"][0]["what_to_remember"],
        "Staged pending duplicate."
    );
    assert_eq!(source_bytes(&active.source), before);
}

fn wait_file(path: &Path, timeout: Duration) {
    let until = Instant::now() + timeout;
    while !path.exists() {
        assert!(
            Instant::now() < until,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn import_progress_protocol_helper() {
    use agentlaw_runtime::{protocol::*, transport};
    use std::net::TcpListener;
    let Some(state) = std::env::var_os("SHARE_PROGRESS_FIXTURE") else {
        return;
    };
    let state = PathBuf::from(state);
    let _owner = agentlaw_runtime::installation::owner::OwnerLock::try_acquire(&state)
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
            stream.set_nonblocking(false).unwrap();
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
    std::fs::write(state.join("progress-ready"), b"ready").unwrap();
    let (mut stream, _) = data.accept().unwrap();
    stream.set_nonblocking(false).unwrap();
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
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body["admin_command"]["import_prepare"]["commit"],
        "1".repeat(40)
    );
    for (phase, gate) in [
        ("receiving objects", "progress-next"),
        ("validating review", "progress-finish"),
    ] {
        transport::send(
            &mut stream,
            &DataFrame::Progress {
                id: header.id,
                phase: phase.into(),
            },
            MAX_RESULT_FRAME_BYTES,
        )
        .unwrap();
        std::fs::write(state.join(format!("{gate}.sent")), b"sent").unwrap();
        wait_file(&state.join(gate), Duration::from_secs(30));
    }
    transport::send(
        &mut stream,
        &DataFrame::Completed {
            id: header.id,
            result: json!({"status":"review_required","import_ref":"fixture"}),
        },
        MAX_RESULT_FRAME_BYTES,
    )
    .unwrap();
}

#[test]
fn import_prepare_progress_repeats_on_stderr_and_final_json_is_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    active._cleanup.shutdown();
    let helper = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "import_progress_protocol_helper", "--nocapture"])
        .env("SHARE_PROGRESS_FIXTURE", &active.state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut helper = OwnedProcess::new(helper);
    wait_file(
        &active.state.join("progress-ready"),
        Duration::from_secs(10),
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentlaw"))
        .env("AGENTLAW_HOME", &active.state)
        .args(["share", "import", "prepare", "--commit", &"1".repeat(40)])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut child = OwnedProcess::frontend(child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stream = stdout;
        stream.read_to_end(&mut bytes).unwrap();
        out_tx.send(bytes).unwrap();
    });
    wait_file(
        &active.state.join("progress-next.sent"),
        Duration::from_secs(10),
    );
    std::fs::write(active.state.join("progress-next"), b"next").unwrap();
    wait_file(
        &active.state.join("progress-finish.sent"),
        Duration::from_secs(10),
    );
    let until = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let line = rx
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .expect("repeated progress on stderr");
        if line.contains("validating review") && line.chars().any(|c| c.is_ascii_digit()) {
            seen.push(line);
        }
    }
    std::fs::write(active.state.join("progress-finish"), b"finish").unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    assert!(helper.wait().unwrap().success());
    let stdout = out_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let text = std::str::from_utf8(&stdout).unwrap();
    let value: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        value,
        json!({"status":"review_required","import_ref":"fixture"})
    );
}

struct ReleaseHook(PathBuf);
impl Drop for ReleaseHook {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"release");
    }
}

fn install_blocking_hook(remote: &Path, root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let started = root.join("hook-started");
    let release = root.join("hook-release");
    let finished = root.join("hook-finished");
    let quote = |path: &Path| {
        format!(
            "'{}'",
            path.to_string_lossy()
                .replace('\\', "/")
                .replace('\'', "'\\''")
        )
    };
    let script = format!("#!/bin/sh\nprintf 'started\\n' >> {}\nwhile [ ! -f {} ]; do sleep 0.05; done\nprintf 'finished\\n' >> {}\n", quote(&started), quote(&release), quote(&finished));
    let hook = remote.join("hooks/pre-receive");
    std::fs::write(&hook, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (started, release, finished)
}

#[cfg(windows)]
fn terminate_owned_runtime(pid: u32) {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
    };
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        assert!(!handle.is_null());
        assert_ne!(TerminateProcess(handle, 1), 0);
        CloseHandle(handle);
    }
}

fn remote_git_processes(remote: &Path) -> Vec<String> {
    #[cfg(windows)]
    {
        let output = Command::new("powershell").args(["-NoProfile", "-Command", "$p = $env:AGENTLAW_TEST_REMOTE; Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^git(?:-.*)?\\.exe$' -and $_.CommandLine -and $_.CommandLine.Contains($p) } | ForEach-Object { $_.ProcessId }"])
            .env("AGENTLAW_TEST_REMOTE", remote).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }
    #[cfg(unix)]
    {
        let output = Command::new("ps")
            .args(["-eo", "pid=,comm=,args="])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                let mut fields = line.split_whitespace();
                let _ = fields.next();
                fields
                    .next()
                    .is_some_and(|name| name == "git" || name.starts_with("git-"))
                    && line.contains(remote.to_str().unwrap())
            })
            .map(str::to_owned)
            .collect()
    }
}
#[cfg(unix)]
fn terminate_owned_runtime(pid: u32) {
    assert!(Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap()
        .success());
}

fn blocked_push(shutdown: bool) {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare(dir.path());
    let active = Installation::new(dir.path(), "active", &remote);
    active.save("Push interruption fixture.");
    ok(&active.state, &["continuity", "save"], None);
    let review = ok(
        &active.state,
        &[
            "share",
            "inspect",
            "--remote",
            "origin",
            "--target-ref",
            "refs/heads/main",
        ],
        None,
    );
    let review_ref = review["review_ref"].as_str().unwrap().to_owned();
    let (started, release, finished) = install_blocking_hook(&remote, dir.path());
    let _release = ReleaseHook(release.clone());
    let endpoint = agentlaw_runtime::transport::read_endpoint(&active.state).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let state = active.state.clone();
        std::thread::spawn(move || {
            tx.send(cli(
                &state,
                &["share", "push", "--review", &review_ref],
                None,
            ))
            .unwrap();
        })
    };
    wait_file(&started, Duration::from_secs(30));
    if shutdown {
        assert!(matches!(
            agentlaw_runtime::client::shutdown_installed(&active.state, "test shutdown").unwrap(),
            agentlaw_runtime::client::ShutdownState::Stopping
        ));
        assert!(rx.try_recv().is_err(), "push ended before hook released");
    } else {
        assert_eq!(
            agentlaw_runtime::transport::read_endpoint(&active.state)
                .unwrap()
                .pid,
            endpoint.pid
        );
        terminate_owned_runtime(endpoint.pid);
    }
    std::fs::write(&release, b"release").unwrap();
    let (status, result, _) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("push result");
    worker.join().unwrap();
    wait_file(&finished, Duration::from_secs(30));
    if shutdown {
        assert!(status.success(), "{result}");
        assert_eq!(result["commit_oid"], review["commit_oid"]);
    } else {
        assert!(!status.success(), "{result}");
        assert_eq!(result["code"], "execution_outcome_unknown", "{result}");
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while agentlaw_runtime::installation::owner::owner_alive(&active.state).unwrap() {
        assert!(Instant::now() < deadline, "runtime did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let children = remote_git_processes(&remote);
        if children.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Git processes remain: {children:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(started).unwrap().lines().count(), 1);
    assert_eq!(
        std::fs::read_to_string(finished).unwrap().lines().count(),
        1
    );
}

#[test]
fn accepted_push_finishes_during_runtime_shutdown() {
    blocked_push(true);
}

#[test]
fn terminated_runtime_makes_push_outcome_unknown_without_retry() {
    blocked_push(false);
}
