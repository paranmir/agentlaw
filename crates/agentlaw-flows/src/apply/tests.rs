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
        config_file: Some(std::path::PathBuf::from("test-config.json")),
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
        config_file: Some(std::path::PathBuf::from("test-config.json")),
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
        config_file: Some(std::path::PathBuf::from("test-config.json")),
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
        config_file: Some(std::path::PathBuf::from("test-config.json")),
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

const PREFIX_PARENT: &str = "abcdef12-a000-4000-8000-000000000001";
const PREFIX_COMPETITOR: &str = "abcdef12-b000-4000-8000-000000000002";
const LIFECYCLE_BINDING: &str = "11111111-1111-4111-8111-111111111111";
const LIFECYCLE_MACHINE: &str = "22222222-2222-4222-8222-222222222222";

struct PrefixLifecycle {
    identity: StoreIdentity,
    store: Store,
    writer: crate::control::ControlWriter,
}

impl PrefixLifecycle {
    fn open(directory: &std::path::Path) -> Self {
        let root = directory.join("source");
        let local = directory.join("local");
        let store = Store::open(&root, local.join("canonical")).unwrap();
        let identity = StoreIdentity {
            root,
            local,
            user_id: "test".into(),
            machine_id: LIFECYCLE_MACHINE.into(),
            binding_id: LIFECYCLE_BINDING.into(),
            history_limit: 8192,
            response_limit: 65536,
            config_file: None,
        };
        let mut writer = crate::control::ControlWriter::open(&identity.local).unwrap();
        writer.bind_identity(&identity).unwrap();
        writer.finish_binding_initialization(&identity).unwrap();
        Self {
            identity,
            store,
            writer,
        }
    }

    fn seed(&self, identity: &str, body: &str) -> Value {
        seed_lifecycle_memory(&self.store, identity, body)
    }

    fn prefix(&self) -> SubmittedPrefix {
        serde_json::from_value(
            json!({"condition":{"kind":"Memory","prefix":"abcdef12","selected":PREFIX_PARENT},
            "path":"/procedure/evidence_memory_ids/0","provided":"abcdef12"}),
        )
        .unwrap()
    }

    fn authorized_procedure(&mut self) -> (String, Mutation) {
        let token = uuid::Uuid::new_v4();
        let request = RequestControl::default();
        self.apply(
            Prepared::Authoring(AuthoringPlan {
                token,
                basis: "reviewed evidence".into(),
                response: json!({"status":"authoring_required"}),
            }),
            &request,
        )
        .unwrap();
        let mut mutation = procedure(LIFECYCLE_MACHINE);
        let UnitState::Live { heads } = &mut mutation.unit.state else {
            unreachable!()
        };
        heads[0].metadata["evidence_memory_ids"] = json!([PREFIX_PARENT]);
        (token.to_string(), mutation)
    }

    fn procedure_plan(&self, token: &str, mutation: Mutation, operation: &str) -> Prepared {
        let plan = Prepared::Procedure(ProcedurePlan {
            token: token.into(),
            operation_id: uuid::Uuid::parse_str(operation).unwrap(),
            generation: self.store.generation().unwrap(),
            mutation,
            submission:
                json!({"procedure":{"operation":"create","evidence_memory_ids":["abcdef12"]}})
                    .to_string(),
        });
        Prepared::WithPrefixes(Box::new(plan), vec![self.prefix()])
    }

    fn apply(&mut self, plan: Prepared, request: &RequestControl) -> Result<Applied> {
        apply(
            &mut LaneCtx {
                identity: &self.identity,
                source: &self.store,
                control: &mut self.writer,
                request,
            },
            plan,
        )
    }

    fn recover(&mut self) {
        crate::recovery::recover(&mut LaneCtx {
            identity: &self.identity,
            source: &self.store,
            control: &mut self.writer,
            request: &RequestControl::default(),
        })
        .unwrap();
    }

    fn operations(&self) -> Vec<String> {
        let mut query = self
            .writer
            .connection
            .prepare("SELECT operation FROM prefix_executions ORDER BY operation")
            .unwrap();
        query
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
}

fn seed_lifecycle_memory(store: &Store, identity: &str, body: &str) -> Value {
    let mutation = Mutation {
        unit: CurrentUnit {
            entity_id: identity.into(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":id(),"applicability":{"scope":"user"},
                    "origin":{"machine_id":LIFECYCLE_MACHINE,"user_id":"test"},
                    "recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: body.into(),
                }],
            },
        },
        expected_versions: vec![],
        evidence: "Observed test publication".into(),
    };
    let receipt = store.publish(&id(), vec![mutation]).unwrap();
    serde_json::to_value(&receipt.references[0]).unwrap()
}

#[test]
fn concurrent_subject_returns_the_original_operation() {
    // ER-22
    use std::sync::{atomic::AtomicBool, Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    let mut f = PrefixLifecycle::open(dir.path());
    f.seed(PREFIX_PARENT, "Verified backup restoration.");
    let (token, mutation) = f.authorized_procedure();
    let entity = mutation.unit.entity_id.clone();
    let operation = id();
    let first = f.procedure_plan(&token, mutation, &operation);
    let second_mutation = procedure(LIFECYCLE_MACHINE);
    let second_entity = second_mutation.unit.entity_id.clone();
    let second = f.procedure_plan(&token, second_mutation, &id());
    let location = dir.path().to_path_buf();
    let observed = Arc::new(Mutex::new(None));
    let capture = observed.clone();
    let input = Mutex::new(Some(second));
    let request = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "publishing" {
            let mut concurrent = PrefixLifecycle::open(&location);
            let error = concurrent
                .apply(
                    input.lock().unwrap().take().unwrap(),
                    &RequestControl::default(),
                )
                .err()
                .expect("the first execution still owns this authoring reference");
            *capture.lock().unwrap() = Some((error, concurrent.operations()));
        }
    });
    let response = f.apply(first, &request).unwrap();
    assert!(matches!(response, Applied::Response(value) if value["status"] == "remembered"));
    let (error, operations) = observed.lock().unwrap().take().unwrap();
    assert_eq!(
        error.code, "publication_in_progress",
        "FR-01.6 ER-22: {error:?}"
    );
    let details = error.details.as_ref().unwrap();
    assert_eq!(details["publication_ref"], operation, "{error:?}");
    assert!(error
        .next_action
        .unwrap()
        .to_lowercase()
        .contains("inspect"));
    assert_eq!(operations, vec![operation]);
    assert!(f.operations().is_empty());
    assert_eq!(f.store.history(&entity).unwrap().len(), 1);
    assert!(matches!(
        f.store.read_procedure(&second_entity),
        Err(agentlaw_storage::Error::NotFound(_))
    ));
}

#[test]
fn receipt_precedes_prefix_recheck_after_restart() {
    // ER-19
    const CHILD: &str = "AGENTLAW_TEST_RECEIPT_RESTART";
    if let Some(directory) = std::env::var_os(CHILD) {
        let directory = std::path::PathBuf::from(directory);
        let input: Value =
            serde_json::from_slice(&std::fs::read(directory.join("child-request.json")).unwrap())
                .unwrap();
        let mut f = PrefixLifecycle::open(&directory);
        let plan = f.procedure_plan(
            input["token"].as_str().unwrap(),
            serde_json::from_value(input["mutation"].clone()).unwrap(),
            input["operation"].as_str().unwrap(),
        );
        let request = RequestControl::new(
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            |phase| {
                if phase == "published" {
                    std::process::exit(47);
                }
            },
        );
        let _ = f.apply(plan, &request);
        panic!("Publication checkpoint not reached");
    }

    let dir = tempfile::tempdir().unwrap();
    let mut f = PrefixLifecycle::open(dir.path());
    f.seed(PREFIX_PARENT, "Verified backup restoration.");
    let (token, mutation) = f.authorized_procedure();
    let entity = mutation.unit.entity_id.clone();
    let operation = id();
    std::fs::write(
        dir.path().join("child-request.json"),
        json!({
            "token":token,"mutation":mutation,"operation":operation
        })
        .to_string(),
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "apply::tests::receipt_precedes_prefix_recheck_after_restart",
        ])
        .env(CHILD, dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(47), "{output:?}");
    assert_eq!(f.operations(), vec![operation]);
    let published = f
        .store
        .read_procedure(&entity)
        .unwrap()
        .references()
        .unwrap();
    f.seed(PREFIX_COMPETITOR, "An independent backup check.");
    let generation = f.store.generation().unwrap();
    f.recover();
    assert!(f.operations().is_empty());
    assert_eq!(f.store.generation().unwrap(), generation);
    assert_eq!(
        f.store
            .read_procedure(&entity)
            .unwrap()
            .references()
            .unwrap(),
        published
    );
    assert_eq!(f.store.history(&entity).unwrap().len(), 1);
    assert_eq!(
        f.store.read_current(PREFIX_COMPETITOR).unwrap().heads()[0].body,
        "An independent backup check."
    );
    let reader = crate::control::ControlReader::open(&f.identity.local).unwrap();
    let (status, _, result) = reader
        .authoring_result(LIFECYCLE_BINDING, &token)
        .unwrap()
        .unwrap();
    assert_eq!(status, "published");
    let result: Value = serde_json::from_str(&result.unwrap()).unwrap();
    assert_eq!(
        result["procedure_ref"],
        json!({"procedure_id":entity,"observed_version":published[0].observed_version})
    );
    let used: bool = f
        .writer
        .connection
        .query_row(
            "SELECT used FROM authoring WHERE token=?1",
            [token],
            |row| row.get(0),
        )
        .unwrap();
    assert!(used);
}

#[test]
fn prefix_rejection_removes_only_the_rejected_execution() {
    // ER-16
    use crate::prepare::{PendingExpectation, RememberPlan};
    use crate::write::PendingBatch;
    use std::sync::{atomic::AtomicBool, Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    let mut f = PrefixLifecycle::open(dir.path());
    let reference = f.seed(PREFIX_PARENT, "Verified backup restoration.");
    let context = RequestContext {
        store_binding_id: LIFECYCLE_BINDING.into(),
        user_id: "test".into(),
        machine_id: LIFECYCLE_MACHINE.into(),
        project_id: None,
        connection_version: "1".into(),
    };
    let body = "Restore verified backups before opening the service.";
    let proposal = serde_json::from_value(json!({"operation":"evolve","parent_refs":[reference],
        "what_to_remember":body,"evidence":"Observed recovery"}))
    .unwrap();
    let batch = PendingBatch::new(&context, vec![proposal]);
    let pending_ref = batch.reference.clone();
    let mut retained = Retained {
        diagnostics: vec![],
        batch,
        context,
        issues: vec![
            json!({"code":"stale_memory_ref","proposal_index":0,"path":"/memories/0/parent_refs","message":"Review current heads."}),
        ],
        basis: String::new(),
        resolution_ref: Some(id()),
        review_refs: vec![],
        approved_review_refs: vec![],
        overlap_approved: false,
        operation_id: None,
        mutations: vec![],
        expected_generation: None,
        publication_basis: None,
    };
    let parked = f
        .apply(
            Prepared::Remember(RememberPlan {
                retained: retained.clone(),
                expectation: PendingExpectation::New,
                review_basis: None,
            }),
            &RequestControl::default(),
        )
        .unwrap();
    assert!(matches!(parked, Applied::Response(value) if value["status"] == "resolution_required"));
    let inspect = |fixture: &PrefixLifecycle| {
        crate::control::ControlReader::open(&fixture.identity.local)
            .unwrap()
            .pending_inspect(LIFECYCLE_BINDING, &pending_ref)
            .unwrap()
    };
    let before = inspect(&f);
    assert_eq!(before["proposals"].as_array().unwrap().len(), 1);
    let original = f.store.read_current(PREFIX_PARENT).unwrap();
    let parents = original.references().unwrap();
    let mut unit = original.clone();
    unit.state = UnitState::Live {
        heads: vec![Head {
            metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":LIFECYCLE_MACHINE,"user_id":"test"},
            "recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
            body: body.into(),
        }],
    };
    let versions: Vec<_> = parents
        .iter()
        .map(|parent| parent.observed_version.clone())
        .collect();
    let source = f.store.owned_published_reader();
    retained.issues.clear();
    retained.operation_id = Some(id());
    let rejected_operation = retained.operation_id.clone().unwrap();
    retained.batch.reference.observed_version = id();
    retained.expected_generation = Some(f.store.generation().unwrap());
    retained.publication_basis = Some(agentlaw_storage::ReadSet {
        source_position: source.read_snapshot().unwrap().position().clone(),
        observed: [(PREFIX_PARENT.into(), versions.clone())].into(),
        absent: vec![],
        reverse_required: Default::default(),
        watched_scopes: vec![],
    });
    retained.mutations = vec![Mutation {
        unit,
        expected_versions: versions,
        evidence: "Observed recovery".into(),
    }];
    let prefix: SubmittedPrefix = serde_json::from_value(json!({
        "condition":{"kind":"Memory","prefix":"abcdef12","selected":PREFIX_PARENT},
        "path":"/memories/0/parent_refs/0/memory_id","provided":"abcdef12"
    }))
    .unwrap();
    let plan = Prepared::WithPrefixes(
        Box::new(Prepared::Remember(RememberPlan {
            retained,
            expectation: PendingExpectation::Version(pending_ref.observed_version.clone()),
            review_basis: None,
        })),
        vec![prefix],
    );

    let unrelated_parent = "12abcdef-a000-4000-8000-000000000003";
    f.seed(unrelated_parent, "Check the restored archive.");
    let (token, mut unrelated_mutation) = f.authorized_procedure();
    let UnitState::Live { heads } = &mut unrelated_mutation.unit.state else {
        unreachable!()
    };
    heads[0].metadata["evidence_memory_ids"] = json!([unrelated_parent]);
    let unrelated_operation = id();
    let unrelated_prefix: SubmittedPrefix = serde_json::from_value(json!({
        "condition":{"kind":"Memory","prefix":"12abcdef","selected":unrelated_parent},
        "path":"/procedure/evidence_memory_ids/0","provided":"12abcdef"
    }))
    .unwrap();
    let unrelated = Prepared::WithPrefixes(
        Box::new(Prepared::Procedure(ProcedurePlan {
            token,
            operation_id: uuid::Uuid::parse_str(&unrelated_operation).unwrap(),
            generation: f.store.generation().unwrap(),
            mutation: unrelated_mutation,
            submission: "{}".into(),
        })),
        vec![unrelated_prefix],
    );
    let location = dir.path().to_path_buf();
    let competing_location = location.clone();
    let competing_publish = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "publishing" {
            let writer = PrefixLifecycle::open(&competing_location);
            writer.seed(PREFIX_COMPETITOR, "Concurrent backup observation.");
        }
    });
    let nested = Mutex::new(Some(plan));
    let observed = Arc::new(Mutex::new(None));
    let capture = observed.clone();
    let request = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "publishing" {
            let mut nested_writer = PrefixLifecycle::open(&location);
            let rejected =
                nested_writer.apply(nested.lock().unwrap().take().unwrap(), &competing_publish);
            *capture.lock().unwrap() = Some((rejected.err(), nested_writer.operations()));
        }
    });
    let _ = f.apply(unrelated, &request);
    let (error, executions) = observed.lock().unwrap().take().unwrap();
    assert_eq!(
        error
            .expect("FR-01.6 ER-16: ambiguity must reject publication")
            .code,
        "ambiguous"
    );
    assert_eq!(executions, vec![unrelated_operation]);
    assert_eq!(inspect(&f), before);
    assert_eq!(
        f.store
            .read_current(PREFIX_PARENT)
            .unwrap()
            .references()
            .unwrap(),
        parents
    );
    assert_eq!(
        f.store.read_current(PREFIX_COMPETITOR).unwrap().heads()[0].body,
        "Concurrent backup observation."
    );
    assert_eq!(f.store.history(PREFIX_PARENT).unwrap().len(), 1);
    assert!(!f
        .identity
        .local
        .join("canonical/recovery")
        .join(rejected_operation)
        .join("published")
        .exists());
}

#[test]
fn terminal_outcome_retention_is_bounded_per_binding() {
    // ER-30
    use crate::{
        prepare::{PendingExpectation, RememberPlan},
        write::PendingBatch,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut f = PrefixLifecycle::open(dir.path());
    f.seed(PREFIX_PARENT, "Verified backup restoration.");
    let (token, mutation) = f.authorized_procedure();
    let newest = id();
    let plan = f.procedure_plan(&token, mutation, &newest);
    let context = RequestContext {
        store_binding_id: LIFECYCLE_BINDING.into(),
        user_id: "test".into(),
        machine_id: LIFECYCLE_MACHINE.into(),
        project_id: None,
        connection_version: "1".into(),
    };
    let draft = serde_json::from_value(json!({"operation":"create","applies_to":["user"],
        "what_to_remember":"Review the verified release archive.","evidence":"Observed restoration"})).unwrap();
    let batch = PendingBatch::new(&context, vec![draft]);
    let reference = batch.reference.clone();
    f.apply(Prepared::Remember(RememberPlan {
        retained:Retained {
            diagnostics:vec![],batch,context,
            issues:vec![json!({"code":"source_changed","message":"Review the changed source before applying."})],
            basis:String::new(),resolution_ref:Some(id()),review_refs:vec![],approved_review_refs:vec![],
            overlap_approved:false,operation_id:None,mutations:vec![],expected_generation:None,publication_basis:None,
        },expectation:PendingExpectation::New,review_basis:None,
    }), &RequestControl::default()).unwrap();
    let oldest = id();
    let foreign_binding = id();
    let foreign_operation = id();
    let ordinary: Vec<_> = (0..4095).map(|_| id()).collect();
    // Install the already settled history. The next outcome and eviction go through apply.
    let tx = f.writer.connection.transaction().unwrap();
    tx.execute("INSERT INTO prefix_execution_outcomes(binding,operation,state,pending_batch_id,observed_version)
        VALUES (?1,?2,'review_required',?3,?4)", params![LIFECYCLE_BINDING,oldest,reference.pending_batch_id,reference.observed_version]).unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO prefix_execution_outcomes(binding,operation,state,rejection_code)
            VALUES (?1,?2,'rejected','ambiguous')",
            )
            .unwrap();
        insert
            .execute(params![foreign_binding, foreign_operation])
            .unwrap();
        for operation in &ordinary {
            insert
                .execute(params![LIFECYCLE_BINDING, operation])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    let before = inspect_terminal_outcome(&f, LIFECYCLE_BINDING, &oldest);
    assert_eq!(before["publication_state"], "review_required", "{before}");
    assert_eq!(
        before["pending_batch_ref"],
        serde_json::to_value(&reference).unwrap()
    );
    let foreign = inspect_terminal_outcome(&f, &foreign_binding, &foreign_operation);
    assert_eq!(foreign["publication_state"], "rejected");
    assert_eq!(
        inspect_terminal_outcome(&f, LIFECYCLE_BINDING, &foreign_operation)["publication_state"],
        "decision_unknown"
    );
    f.seed(PREFIX_COMPETITOR, "An independent backup observation.");
    let error = f.apply(plan, &RequestControl::default()).err().unwrap();
    assert_eq!(error.code, "ambiguous", "FR-01.6 ER-30: {error:?}");
    let oldest_after = inspect_terminal_outcome(&f, LIFECYCLE_BINDING, &oldest);
    assert_eq!(
        oldest_after["publication_state"], "decision_unknown",
        "{oldest_after}"
    );
    assert!(oldest_after.get("pending_batch_ref").is_none());
    let guidance = oldest_after["next_action"].as_str().unwrap().to_lowercase();
    assert!(
        guidance.contains("inspect") || guidance.contains(&oldest),
        "{oldest_after}"
    );
    if guidance.contains("resubmit") || guidance.contains("submit again") {
        assert!(
            guidance.contains("do not")
                || guidance.contains("don't")
                || guidance.contains("before"),
            "{oldest_after}"
        );
    }
    assert_eq!(
        inspect_terminal_outcome(&f, &foreign_binding, &foreign_operation),
        foreign
    );
    let newest_outcome = inspect_terminal_outcome(&f, LIFECYCLE_BINDING, &newest);
    assert_eq!(newest_outcome["publication_state"], "rejected");
    assert_eq!(newest_outcome["rejection_code"], "ambiguous");
    let mut stmt = f.writer.connection.prepare("SELECT operation FROM prefix_execution_outcomes WHERE binding=?1 ORDER BY completion_seq").unwrap();
    let records: Vec<String> = stmt
        .query_map([LIFECYCLE_BINDING], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        records,
        ordinary.into_iter().chain([newest]).collect::<Vec<_>>()
    );
    let reader = crate::control::ControlReader::open(&f.identity.local).unwrap();
    let pending = reader
        .pending_inspect(LIFECYCLE_BINDING, &reference)
        .unwrap();
    assert_eq!(
        pending["pending_batch_ref"],
        serde_json::to_value(&reference).unwrap()
    );
    assert_eq!(
        pending["proposals"][0]["what_to_remember"],
        "Review the verified release archive."
    );
}

fn inspect_terminal_outcome(f: &PrefixLifecycle, binding: &str, operation: &str) -> Value {
    use crate::{history_cache::HistoryCacheGuards, semantic::*, store::ReadCtx};
    struct NoSemantics;
    impl SemanticPort for NoSemantics {
        fn embed_prefix(
            &self,
            _: &str,
            _: &RequestControl,
        ) -> std::result::Result<PrefixEmbedding, SemanticError> {
            panic!("Publication inspection does not run embeddings")
        }
        fn plan_pieces(
            &self,
            _: &[&str],
            _: &RequestControl,
        ) -> std::result::Result<PiecePlan, SemanticError> {
            unreachable!()
        }
        fn embed_planned(
            &self,
            _: &PiecePlan,
            _: usize,
            _: &PieceVectorReuse,
            _: &RequestControl,
        ) -> std::result::Result<Vec<QueryVector>, SemanticError> {
            unreachable!()
        }
        fn search_available(
            &self,
            _: SemanticQuery<'_>,
            _: &RequestControl,
        ) -> std::result::Result<SemanticCandidates, SemanticError> {
            unreachable!()
        }
    }
    impl HistorySemanticPort for NoSemantics {
        fn offer(&self, _: HistoryIngestHint) -> OfferResult {
            unreachable!()
        }
        fn search_available(
            &self,
            _: &str,
            _: &str,
            _: &QueryVector,
            _: &[String],
            _: usize,
            _: &RequestControl,
        ) -> std::result::Result<SemanticCandidates, SemanticError> {
            unreachable!()
        }
    }
    let identity = StoreIdentity {
        root: f.identity.root.clone(),
        local: f.identity.local.clone(),
        user_id: f.identity.user_id.clone(),
        machine_id: f.identity.machine_id.clone(),
        binding_id: binding.into(),
        history_limit: f.identity.history_limit,
        response_limit: f.identity.response_limit,
        config_file: None,
    };
    let ctx = ReadCtx {
        identity: &identity,
        source: &f.store.owned_published_reader(),
        control: &crate::control::ControlReader::open(&identity.local).unwrap(),
        semantic: &NoSemantics,
        history_semantic: &NoSemantics,
        history_cache: &HistoryCacheGuards::new(),
        request: &RequestControl::default(),
    };
    let request =
        serde_json::from_value(json!({"pending_action":"inspect","publication_ref":operation}))
            .unwrap();
    crate::read::pending_inspect(&ctx, &request).unwrap()
}
