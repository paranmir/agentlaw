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
        assets: None,
        installation_build: "test-build".into(),
        start_gate: Arc::new(|| Err(DomainError::new("update_maintenance", "updating"))),
    };
    let outcome = run(deps);
    assert!(matches!(outcome, Ok(RunExit::StartBlocked(ref e)) if e.code == "update_maintenance"));
    assert!(!dir.path().join("service/endpoint.json").exists());
    assert!(OwnerLock::try_acquire(dir.path()).unwrap().is_some());
}
