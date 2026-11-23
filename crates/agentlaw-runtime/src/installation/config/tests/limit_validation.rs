use super::*;

#[test]
fn delivery_ranges_are_checked_before_opening_setup_coordination() {
    assert_eq!(
        validate_limit("history.response_limit_bytes", "1").unwrap(),
        1
    );
    assert_eq!(
        validate_limit("response_limit_bytes", "4096").unwrap(),
        4096
    );
    let directory = tempfile::tempdir().unwrap();
    let absent_state = directory.path().join("absent-state");
    for (key, value) in [
        ("history.response_limit_bytes", "0"),
        ("response_limit_bytes", "4095"),
        ("response_limit_bytes", "not-a-number"),
        ("unknown", "8192"),
    ] {
        assert_eq!(
            set_limit(&absent_state, key, value).unwrap_err().code,
            "invalid_configuration"
        );
    }
    assert!(!absent_state.exists());
    assert!(!directory.path().join("setup.lock").exists());
}
