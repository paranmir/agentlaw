use super::*;

#[test]
fn missing_installation_is_not_created_by_diagnosis() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("absent");
    assert_eq!(
        doctor(&state, DoctorMode::Health).unwrap()["status"],
        "setup_required"
    );
    assert!(!state.exists());
}

#[test]
fn doctor_health_uses_replaceable_source_without_creating_state() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("absent");
    let result = doctor_with_health(&state, DoctorMode::Health, &|| {
        Ok(json!({"marker":"health-from-source"}))
    })
    .unwrap();
    assert_eq!(result["status"], "setup_required");
    assert_eq!(result["worker_runtime"]["marker"], "health-from-source");
    assert!(result["observed_at_ms"].as_u64().is_some_and(|v| v > 0));
    assert!(result.get("audit").is_none());
    assert!(!state.exists());
}

#[test]
fn doctor_preserves_result_when_health_source_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("absent");
    let result = doctor_with_health(&state, DoctorMode::Health, &|| {
        Err(DomainError::new(
            "health_unavailable",
            "health source failed",
        ))
    })
    .unwrap();
    assert_eq!(result["status"], "setup_required");
    assert_eq!(
        result["worker_runtime"]["code"],
        "worker_diagnosis_incomplete"
    );
    assert!(result["observed_at_ms"].as_u64().is_some_and(|v| v > 0));
    assert!(result.get("audit").is_none(), "{result}");
    assert!(!state.exists());
}
