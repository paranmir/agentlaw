use super::*;
const ID: &str = "11111111-1111-4111-8111-111111111111";
#[test]
fn canonical_scope_string_roundtrip_and_invalid_combinations() {
    let a =
        scope_value(&json!({"scope":"project_machine","project_id":ID,"machine_id":ID})).unwrap();
    assert_eq!(a.scope, vec![ScopeKind::Project, ScopeKind::Machine]);
    assert_eq!(canonical_scope(&a).unwrap()["scope"], "project_machine");
    assert!(scope_value(&json!({"scope":"user","project_id":ID})).is_err());
    assert!(scope_value(&json!({"scope":"project"})).is_err());
    assert!(scope_value(&json!({"scope":["user"]})).is_err());
}
#[test]
fn missing_metadata_is_not_false_or_empty() {
    let mut h = Head {
        metadata: json!({"change_id":ID,"applicability":{"scope":"user"},"origin":{"machine_id":ID},"recorded_at_ms":0,"relations":[],"work_targets":[]}),
        body: "body".into(),
    };
    let unit = CurrentUnit {
        entity_id: ID.into(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![h.clone()],
        },
    };
    assert!(memory_from(&unit, &h).is_err());
    h.metadata["is_rule"] = json!(false);
    assert!(!memory_from(&unit, &h).unwrap().is_rule);
    h.metadata.as_object_mut().unwrap().remove("relations");
    assert!(memory_from(&unit, &h).is_err());
}
