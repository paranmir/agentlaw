use super::*;
use crate::installation::owner::OwnerLock;
use agentlaw_contracts::DomainError;
use std::sync::Arc;

#[test]
fn start_gate_error_is_returned_by_host_without_publishing_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let deps = crate::ports::HostDeps {
        state: dir.path().to_owned(),
        executable: std::env::current_exe().unwrap(),
        assets: Arc::new(|| Ok(None)),
        installation_build: "test-build".into(),
        start_gate: Arc::new(|| Err(DomainError::new("update_maintenance", "updating"))),
    };
    let outcome = run(deps);
    assert!(matches!(outcome, Ok(RunExit::StartBlocked(ref e)) if e.code == "update_maintenance"));
    assert!(!dir.path().join("service/endpoint.json").exists());
    assert!(OwnerLock::try_acquire(dir.path()).unwrap().is_some());
}

struct RunningHost {
    state: std::path::PathBuf,
    thread: Option<std::thread::JoinHandle<crate::ports::RuntimeResult<RunExit>>>,
}

impl Drop for RunningHost {
    fn drop(&mut self) {
        if let Ok(client) = crate::client::Client::connect(&self.state, "model-test") {
            let _ = client.shutdown("test cleanup");
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !predicate() {
        assert!(
            std::time::Instant::now() < deadline,
            "host fixture timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn start_with_assets(
    state: &std::path::Path,
    assets: Arc<dyn Fn() -> Result<Option<agentlaw_worker::ModelAssets>, String> + Send + Sync>,
) -> RunningHost {
    start_with_executable(state, assets, std::env::current_exe().unwrap())
}

fn start_with_executable(
    state: &std::path::Path,
    assets: Arc<dyn Fn() -> Result<Option<agentlaw_worker::ModelAssets>, String> + Send + Sync>,
    executable: std::path::PathBuf,
) -> RunningHost {
    std::fs::create_dir_all(state.join("worker")).unwrap();
    let deps = crate::ports::HostDeps {
        state: state.to_owned(),
        executable,
        assets,
        installation_build: "model-test".into(),
        start_gate: Arc::new(|| Ok(())),
    };
    let thread = std::thread::spawn(move || run(deps));
    let host = RunningHost {
        state: state.to_owned(),
        thread: Some(thread),
    };
    wait_until(|| {
        matches!(
            crate::client::probe(state, "model-test", std::time::Duration::from_millis(100)),
            crate::client::Probe::Running(_)
        )
    });
    host
}

#[test]
fn model_asset_error_is_visible_and_clears_when_unconfigured() {
    use crate::client::Probe;
    use crate::ports::{ComponentKind, ComponentState};
    use crate::test_support::installation::TestInstallation;
    use std::sync::atomic::{AtomicBool, Ordering};
    let installation = TestInstallation::new();
    installation.select_store("a").unwrap();
    let repaired = Arc::new(AtomicBool::new(false));
    let flag = repaired.clone();
    let _host = start_with_assets(
        installation.state(),
        Arc::new(move || {
            if flag.load(Ordering::SeqCst) {
                Ok(None)
            } else {
                Err("fixture model missing".into())
            }
        }),
    );
    let client = crate::client::Client::connect(installation.state(), "model-test").unwrap();
    let binding = client.resolve_binding().unwrap();
    let request = agentlaw_contracts::wire::ValidatedRequest::parse_public(
        &serde_json::json!({"action":"recall","recall":{"recall_for":"lexical marker"}})
            .to_string(),
    )
    .unwrap();
    assert!(client
        .call(
            crate::protocol::CallTarget::Store(binding),
            request,
            &agentlaw_contracts::RequestControl::default()
        )
        .is_ok());
    wait_until(|| {
        match crate::client::probe(
            installation.state(),
            "model-test",
            std::time::Duration::from_millis(100),
        ) {
            Probe::Running(snapshot) => snapshot.components.iter().any(|(id, status)| {
                id.kind == ComponentKind::Model
                    && matches!(
                        status.state,
                        ComponentState::Failed | ComponentState::Degraded
                    )
                    && status
                        .reason
                        .as_deref()
                        .is_some_and(|s| s.contains("fixture model missing"))
            }),
            _ => false,
        }
    });
    repaired.store(true, Ordering::SeqCst);
    wait_until(|| {
        match crate::client::probe(
            installation.state(),
            "model-test",
            std::time::Duration::from_millis(100),
        ) {
            Probe::Running(snapshot) => !snapshot.components.iter().any(|(id, status)| {
                id.kind == ComponentKind::Model
                    && status
                        .reason
                        .as_deref()
                        .is_some_and(|s| s.contains("fixture model missing"))
            }),
            _ => false,
        }
    });
}

#[test]
fn unconfigured_model_has_no_model_diagnostic() {
    use crate::client::Probe;
    use crate::ports::{ComponentKind, ComponentState};
    let dir = tempfile::tempdir().unwrap();
    let _host = start_with_assets(dir.path(), Arc::new(|| Ok(None)));
    match crate::client::probe(
        dir.path(),
        "model-test",
        std::time::Duration::from_millis(100),
    ) {
        Probe::Running(snapshot) => assert!(!snapshot.components.iter().any(|(id, status)| id
            .kind
            == ComponentKind::Model
            && matches!(
                status.state,
                ComponentState::Failed | ComponentState::Degraded
            ))),
        _ => panic!("host not running"),
    }
}

#[test]
fn missing_assets_are_retried_and_restored_assets_make_the_model_ready_automatically() {
    use crate::ports::{ComponentKind, ComponentState};
    use crate::test_support::{model_assets, model_child_executable, TestInstallation};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let installation = TestInstallation::new();
    installation.select_store("recovering").unwrap();
    let assets_root = installation.state().join("fixture-assets");
    let assets = model_assets(&assets_root);
    let repaired = Arc::new(AtomicBool::new(false));
    let flag = repaired.clone();
    let (attempt_tx, attempt_rx) = mpsc::channel();
    let _host = start_with_executable(
        installation.state(),
        Arc::new(move || {
            attempt_tx.send(()).unwrap();
            if flag.load(Ordering::Acquire) {
                Ok(Some(assets.clone()))
            } else {
                Err("fixture model assets missing".into())
            }
        }),
        model_child_executable(),
    );
    // A live client lease retains demand throughout resolver failure and recovery.
    let _client = crate::client::Client::connect(installation.state(), "model-test").unwrap();
    attempt_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    wait_until(|| {
        matches!(crate::client::probe(installation.state(), "model-test", std::time::Duration::from_millis(100)),
        crate::client::Probe::Running(snapshot) if snapshot.components.iter().any(|(id, status)| id.kind == ComponentKind::Model &&
            status.reason.as_deref().is_some_and(|reason| reason.contains("fixture model assets missing"))))
    });
    attempt_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("resolver must retry without a frontend request");
    repaired.store(true, Ordering::Release);
    wait_until(|| {
        matches!(crate::client::probe(installation.state(), "model-test", std::time::Duration::from_millis(100)),
        crate::client::Probe::Running(snapshot) if snapshot.components.iter().any(|(id, status)| id.kind == ComponentKind::Model &&
            status.state == ComponentState::Ready && status.reason.is_none()))
    });
}

#[test]
fn idle_host_releases_ownership_checkpoints_vectors_and_resumes_only_uncommitted_indexing() {
    use crate::ports::{ComponentKind, ComponentState};
    use crate::test_support::{
        model_assets, model_child_executable, model_transcript, TestInstallation,
    };
    use std::time::{Duration, Instant};
    let installation = TestInstallation::new();
    installation.select_store("idle-resume").unwrap();
    let assets_root = installation.state().join("fixture-assets");
    let assets = model_assets(&assets_root);
    let executable = model_child_executable();
    let clock = Arc::new(std::sync::Mutex::new(Instant::now()));
    std::fs::create_dir_all(installation.state().join("worker")).unwrap();
    let deps = crate::ports::HostDeps {
        state: installation.state().to_owned(),
        executable: executable.clone(),
        assets: Arc::new({
            let assets = assets.clone();
            move || Ok(Some(assets.clone()))
        }),
        installation_build: "model-test".into(),
        start_gate: Arc::new(|| Ok(())),
    };
    let worker_clock = clock.clone();
    let thread = std::thread::spawn(move || {
        run_with_clock(deps, Arc::new(move || *worker_clock.lock().unwrap()))
    });
    let mut host = RunningHost {
        state: installation.state().to_owned(),
        thread: Some(thread),
    };
    wait_until(|| {
        matches!(
            crate::client::probe(
                installation.state(),
                "model-test",
                Duration::from_millis(100)
            ),
            crate::client::Probe::Running(_)
        )
    });
    let client = crate::client::Client::connect(installation.state(), "model-test").unwrap();
    let binding = client.resolve_binding().unwrap();
    let remember = |client: &crate::client::Client, body: &str, scope: &str| {
        let request = agentlaw_contracts::wire::ValidatedRequest::parse_public(&serde_json::json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed","applies_to":[scope]}]}}).to_string()).unwrap();
        client
            .call(
                crate::protocol::CallTarget::Store(binding.clone()),
                request,
                &agentlaw_contracts::RequestControl::default(),
            )
            .unwrap()
    };
    let first_body = "This committed section must survive idle exit without another embedding.";
    assert_eq!(
        remember(&client, first_body, "user")["status"],
        "remembered"
    );
    wait_until(|| {
        let request = agentlaw_contracts::wire::ValidatedRequest::parse_public(&serde_json::json!({"action":"recall","recall":{"recall_for":"idle checkpoint coverage"}}).to_string()).unwrap();
        let result = client
            .call(
                crate::protocol::CallTarget::Store(binding.clone()),
                request,
                &agentlaw_contracts::RequestControl::default(),
            )
            .unwrap();
        result["semantic_complete"] != false
    });
    let committed_requests = model_transcript(&assets_root, "requests.jsonl")
        .iter()
        .filter(|v| v.as_str().is_some_and(|s| s.contains(first_body)))
        .count();
    assert!(committed_requests > 0);
    // A temporary asset outage leaves a real persisted backlog behind the committed section.
    std::fs::write(assets_root.join("offline"), b"temporarily unavailable").unwrap();
    let second_body = "A later unindexed section must resume when the runtime starts again.";
    let second = remember(&client, second_body, "machine");
    assert_eq!(second["status"], "remembered");
    wait_until(|| {
        client
            .health()
            .unwrap()
            .components
            .iter()
            .any(|(id, status)| {
                id.kind == ComponentKind::Model
                    && status
                        .reason
                        .as_deref()
                        .is_some_and(|r| r.contains("fixture model assets missing"))
            })
    });
    let old_endpoint = std::fs::read(installation.state().join("service/endpoint.json")).unwrap();
    drop(client);
    *clock.lock().unwrap() +=
        lifecycle::LEASE_TTL + lifecycle::DEFAULT_IDLE_TIMEOUT + Duration::from_secs(1);
    wait_until(|| host.thread.as_ref().unwrap().is_finished());
    assert!(matches!(
        host.thread.take().unwrap().join().unwrap().unwrap(),
        RunExit::Stopped
    ));
    assert!(!installation.state().join("service/endpoint.json").exists());
    let owner = OwnerLock::try_acquire(installation.state())
        .unwrap()
        .expect("idle runtime retained owner lock");
    drop(owner);
    fn vector_paths(root: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        if !root.exists() {
            return;
        }
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                vector_paths(&path, found);
            } else if path.file_name().is_some_and(|n| n == "vector.sqlite") {
                found.push(path);
            }
        }
    }
    let mut vectors = vec![];
    vector_paths(&installation.state().join("worker/indices"), &mut vectors);
    assert!(!vectors.is_empty(), "no committed semantic files");
    for path in &vectors {
        let wal = std::path::PathBuf::from(format!("{}-wal", path.display()));
        assert!(
            std::fs::metadata(wal).map_or(true, |m| m.len() == 0),
            "semantic WAL was not checkpointed after idle exit"
        );
    }
    std::fs::rename(assets_root.join("offline"), assets_root.join("restored")).unwrap();
    let _replacement = start_with_executable(
        installation.state(),
        Arc::new(move || Ok(Some(assets.clone()))),
        executable,
    );
    let client = crate::client::Client::connect(installation.state(), "model-test").unwrap();
    let replacement_endpoint =
        std::fs::read(installation.state().join("service/endpoint.json")).unwrap();
    assert_ne!(old_endpoint, replacement_endpoint);
    let binding = client.resolve_binding().unwrap();
    wait_until(|| {
        client
            .health()
            .unwrap()
            .components
            .iter()
            .any(|(id, status)| {
                id.kind == ComponentKind::Semantic && status.state == ComponentState::Ready
            })
    });
    let request = agentlaw_contracts::wire::ValidatedRequest::parse_public(
        &serde_json::json!({"action":"recall","recall":{"recall_for":"later unindexed section"}})
            .to_string(),
    )
    .unwrap();
    let result = client
        .call(
            crate::protocol::CallTarget::Store(binding),
            request,
            &agentlaw_contracts::RequestControl::default(),
        )
        .unwrap();
    assert_ne!(result["semantic_complete"], false, "{result}");
    let resumed_requests = model_transcript(&assets_root, "requests.jsonl");
    assert!(resumed_requests
        .iter()
        .any(|v| v.as_str().is_some_and(|s| s == second_body)));
    assert_eq!(
        resumed_requests
            .iter()
            .filter(|v| v.as_str().is_some_and(|s| s.contains(first_body)))
            .count(),
        committed_requests,
        "restart asked the model again for a committed section"
    );
}
