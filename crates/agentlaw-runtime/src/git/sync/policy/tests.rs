use super::*;

fn policy() -> Value {
    json!({
        "policy_id":"c62664c6-0988-43f5-878c-d2d14f59d210",
        "version":1,
        "enabled":true,
        "source":"unavailable-source",
        "control":"unavailable-control",
        "remote":"origin",
        "fetch_endpoint":"unavailable-remote",
        "push_endpoint":"unavailable-remote",
        "target_ref":"refs/heads/main",
        "allowed_operations":["memory"],
        "allowed_scopes":["user"],
        "push_permission":true
    })
}

#[test]
fn prepared_policy_requires_confirmation_before_decoding() {
    assert_eq!(
        prepare_policy_configuration("not JSON", false)
            .unwrap_err()
            .code,
        "delegation_confirmation_required"
    );
}

#[test]
fn prepares_existing_static_policy_contract_without_source_access() {
    let prepared = prepare_policy_configuration(&policy().to_string(), true).unwrap();
    assert!(prepared.0.enabled);
    assert_eq!(prepared.0.version, 1);
}

#[test]
fn prepared_policy_reuses_existing_constraint_errors() {
    for (field, value, expected) in [
        ("version", json!(0), "sync_policy_binding"),
        (
            "target_ref",
            json!("refs/tags/main"),
            "sync_destination_changed",
        ),
        (
            "allowed_operations",
            json!(["unknown"]),
            "invalid_policy_scope",
        ),
        ("allowed_scopes", json!(["unknown"]), "invalid_policy_scope"),
    ] {
        let mut input = policy();
        input[field] = value;
        assert_eq!(
            prepare_policy_configuration(&input.to_string(), true)
                .unwrap_err()
                .code,
            expected
        );
    }
}

#[test]
fn prepared_policy_preserves_decode_error_and_rejects_duplicate_fields() {
    for input in [
        "{}".to_owned(),
        policy()
            .to_string()
            .replacen("\"version\":1", "\"version\":1,\"version\":2", 1),
    ] {
        assert_eq!(
            prepare_policy_configuration(&input, true).unwrap_err().code,
            "git_io"
        );
    }
}
