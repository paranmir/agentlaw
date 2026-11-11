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
    let deps = crate::ports::HostDeps {
        state: state.to_owned(),
        executable: std::env::current_exe().unwrap(),
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
