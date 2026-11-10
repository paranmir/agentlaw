use super::*;
use crate::prepare::{AuthoringPlan, ConnectPlan, ConnectPlanKind, ProcedurePlan};
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn procedure(machine: &str) -> Mutation {
    Mutation {
        unit: CurrentUnit {
            entity_id: id(),
            entity_type: "learned_procedure".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":machine},"recorded_at_ms":0,"name":"test","use_when":"test","evidence_memory_ids":[]}),
                    body: "instructions".into(),
                }],
            },
        },
        expected_versions: vec![],
        evidence: "test".into(),
    }
}

#[test]
fn procedure_authorization_is_consumed_and_publication_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let mut writer = crate::control::ControlWriter::open(&local).unwrap();
    let identity = StoreIdentity {
        root,
        local,
        user_id: id(),
        machine_id: id(),
        binding_id: id(),
        history_limit: 8192,
        response_limit: 65536,
    };
    let request = RequestControl::default();
    let mut ctx = LaneCtx {
        identity: &identity,
        source: &store,
        control: &mut writer,
        request: &request,
    };
    let token = uuid::Uuid::new_v4();
    apply(
        &mut ctx,
        Prepared::Authoring(AuthoringPlan {
            token,
            basis: "basis".into(),
            response: json!({"status":"authoring_required"}),
        }),
    )
    .unwrap();
    let mutation = Mutation {
        unit: CurrentUnit {
            entity_id: id(),
            entity_type: "learned_procedure".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":identity.machine_id},"recorded_at_ms":0,"name":"test","use_when":"test","evidence_memory_ids":[]}),
                    body: "instructions".into(),
                }],
            },
        },
        expected_versions: vec![],
        evidence: "test".into(),
    };
    let generation = store.generation().unwrap();
    let plan = ProcedurePlan {
        token: token.to_string(),
        operation_id: uuid::Uuid::new_v4(),
        generation,
        mutation: mutation.clone(),
        submission: "{}".into(),
    };
    let result = apply(&mut ctx, Prepared::Procedure(plan)).unwrap();
    assert!(matches!(result,Applied::Response(ref response) if response["status"]=="remembered"));
    assert!(store.read_procedure(&mutation.unit.entity_id).is_ok());
    let repeat = ProcedurePlan {
        token: token.to_string(),
        operation_id: uuid::Uuid::new_v4(),
        generation,
        mutation,
        submission: "{}".into(),
    };
    assert!(
        matches!(apply(&mut ctx,Prepared::Procedure(repeat)),Err(error) if error.code=="stale_authoring_ref")
    );
}

#[test]
fn cancellation_before_decision_preserves_source_and_retains_submission() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let mut writer = crate::control::ControlWriter::open(&local).unwrap();
    let identity = StoreIdentity {
        root,
        local,
        user_id: id(),
        machine_id: id(),
        binding_id: id(),
        history_limit: 8192,
        response_limit: 65536,
    };
    let normal = RequestControl::default();
    let token = uuid::Uuid::new_v4();
    let mut ctx = LaneCtx {
        identity: &identity,
        source: &store,
        control: &mut writer,
        request: &normal,
    };
    apply(
        &mut ctx,
        Prepared::Authoring(AuthoringPlan {
            token,
            basis: "basis".into(),
            response: json!({}),
        }),
    )
    .unwrap();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = cancel.clone();
    let request = RequestControl::new(cancel, move |phase| {
        if phase == "publishing" {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }
    });
    let mutation = procedure(&identity.machine_id);
    let entity = mutation.unit.entity_id.clone();
    let generation = store.generation().unwrap();
    let plan = ProcedurePlan {
        token: token.to_string(),
        operation_id: uuid::Uuid::new_v4(),
        generation,
        mutation,
        submission: "{}".into(),
    };
    let mut ctx = LaneCtx {
        identity: &identity,
        source: &store,
        control: &mut writer,
        request: &request,
    };
    let result = apply(&mut ctx, Prepared::Procedure(plan)).unwrap();
    assert!(matches!(result,Applied::Response(ref value) if value["code"]=="cancelled"));
    assert_eq!(store.generation().unwrap(), generation);
    assert!(matches!(
        store.read_procedure(&entity),
        Err(agentlaw_storage::Error::NotFound(_))
    ));
}

#[test]
fn decided_publication_recovers_even_if_request_was_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let mut writer = crate::control::ControlWriter::open(&local).unwrap();
    let identity = StoreIdentity {
        root,
        local,
        user_id: id(),
        machine_id: id(),
        binding_id: id(),
        history_limit: 8192,
        response_limit: 65536,
    };
    let mutation = procedure(&identity.machine_id);
    let entity = mutation.unit.entity_id.clone();
    assert!(matches!(
        store.publish_with_fault(
            &id(),
            vec![mutation],
            Some(agentlaw_storage::FaultPoint::Decision)
        ),
        Err(agentlaw_storage::Error::Interrupted(
            agentlaw_storage::FaultPoint::Decision
        ))
    ));
    let request = RequestControl::default();
    request
        .cancel
        .store(true, std::sync::atomic::Ordering::Release);
    let mut ctx = LaneCtx {
        identity: &identity,
        source: &store,
        control: &mut writer,
        request: &request,
    };
    crate::recovery::recover(&mut ctx).unwrap();
    assert!(store.read_procedure(&entity).is_ok());
}

#[test]
fn connect_rechecks_catalog_and_persists_association_before_follow_up() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let project = id();
    store.publish_catalog(&id(),&project,json!({"project_id":project,"name":"project","description":null,"aliases":[],"repository_observations":[],"catalog_revision":id()})).unwrap();
    let mut writer = crate::control::ControlWriter::open(&local).unwrap();
    let identity = StoreIdentity {
        root,
        local: local.clone(),
        user_id: id(),
        machine_id: id(),
        binding_id: id(),
        history_limit: 8192,
        response_limit: 65536,
    };
    let request = RequestControl::default();
    let mut ctx = LaneCtx {
        identity: &identity,
        source: &store,
        control: &mut writer,
        request: &request,
    };
    let path = dir.path().to_string_lossy().into_owned();
    let plan = ConnectPlan {
        kind: ConnectPlanKind::Select {
            path: path.clone(),
            project_id: project.clone(),
        },
        follow_up: Some(FollowUp {
            restore: true,
            recall_for: Some("context".into()),
        }),
    };
    let result = apply(&mut ctx, Prepared::Connect(plan)).unwrap();
    assert!(matches!(
        result,
        Applied::Connected {
            follow_up: Some(_),
            ..
        }
    ));
    let reader = crate::control::ControlReader::open(&local).unwrap();
    assert_eq!(
        reader
            .exact_association(&identity.binding_id, &path)
            .unwrap()
            .unwrap()
            .project_id,
        project
    );
}
