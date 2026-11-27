use super::*;
use crate::test_support::{FakeIndex, FakeModel, FakeSemantic};
use agentlaw_flows::{
    control::ControlReader,
    exact::{ExactIndex, ProjectionGenerations},
    history_cache::HistoryCacheGuards,
    store::StoreIdentity,
    RecallRequest, RequestControl,
};
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::PathBuf;

struct Fixture {
    dir: tempfile::TempDir,
    _journal: agentlaw_storage::JournalWriter,
    store: Store,
    registry: ProjectionGenerations,
    identity: StoreIdentity,
    semantic: FakeSemantic,
}
impl Fixture {
    fn new(limit: usize) -> Self {
        let (dir, mut identity) = crate::test_support::store_service::fixture();
        identity.user_id = "user".into();
        identity.response_limit = limit;
        let root = identity.root.clone();
        let local = identity.local.clone();
        let dependencies = crate::test_support::store_service::deps(dir.path());
        let coordination = dependencies.coordination.clone();
        let indexer = dependencies.indexer.clone();
        let machine_id = identity.machine_id.clone();
        let binding_id = identity.binding_id.clone();
        let service = crate::store::service::StoreService::open(
            identity,
            dependencies,
            &RequestControl::default(),
        )
        .unwrap();
        service.close();
        assert!(indexer.shutdown(std::time::Duration::from_secs(10)));
        let store =
            Store::open_with_coordination(&root, local.join("canonical"), coordination).unwrap();
        let position = store
            .owned_published_reader()
            .read_snapshot()
            .unwrap()
            .position()
            .clone();
        let exact = dir.path().join("exact-test.sqlite");
        let lexical = dir.path().join("lexical-test.sqlite");
        drop(ExactIndex::open(&exact).unwrap());
        drop(agentlaw_search::SearchIndex::open(&lexical).unwrap());
        let db = Connection::open(&exact).unwrap();
        db.execute(
            "INSERT INTO exact_position VALUES(1,?1,?2)",
            params![position.epoch, position.sequence.to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO exact_lexical VALUES(1,'lexical-test.sqlite')",
            [],
        )
        .unwrap();
        drop(db);
        let registry =
            ProjectionGenerations::open("test", &position.epoch, exact, lexical).unwrap();
        let identity = StoreIdentity {
            root: std::fs::canonicalize(root).unwrap(),
            local,
            user_id: "user".into(),
            machine_id: machine_id.clone(),
            binding_id: binding_id.clone(),
            history_limit: 8192,
            response_limit: limit,
            config_file: Some(std::path::PathBuf::from("test-config.json")),
        };
        Self {
            dir,
            _journal: store.hold_journal_writer().unwrap(),
            store,
            registry,
            identity,
            semantic: FakeSemantic::new(),
        }
    }
    fn publish(&self, id: &str, body: &str, rule: bool, relations: Value, expected: Vec<String>) {
        let unit = CurrentUnit {
            entity_id: id.into(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":self.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":rule,"relations":relations,"work_targets":[]}),
                    body: body.into(),
                }],
            },
        };
        self.store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit,
                    expected_versions: expected,
                    evidence: "observed".into(),
                }],
            )
            .unwrap();
    }
    fn query(&self, request: Value, control: &RequestControl) -> agentlaw_flows::Result<Value> {
        self.query_with_semantic(request, control, &self.semantic)
    }
    fn query_with_semantic(
        &self,
        request: Value,
        control: &RequestControl,
        semantic: &dyn agentlaw_flows::semantic::SemanticPort,
    ) -> agentlaw_flows::Result<Value> {
        let source = self.store.owned_published_reader();
        let reader = ControlReader::open(&self.identity.local).unwrap();
        let cache = HistoryCacheGuards::new();
        let ctx = ReadCtx {
            identity: &self.identity,
            source: &source,
            control: &reader,
            semantic,
            history_semantic: &self.semantic,
            history_cache: &cache,
            request: control,
        };
        let pinned = self.registry.pin()?;
        let guard = source.read_snapshot().unwrap();
        let mut exact = ExactSnapshot::build_for_recall(pinned, &guard, DeltaBudget::default())?;
        drop(guard);
        let request: RecallRequest = serde_json::from_value(request).unwrap();
        agentlaw_flows::read::recall(&ctx, &mut exact, &request)
    }
    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

#[test]
fn fr_06_5_lagging_projection_includes_fresh_lexical_and_mandatory_closure() {
    let f = Fixture::new(65536);
    let rule = Fixture::id();
    let required = Fixture::id();
    let selected = Fixture::id();
    f.publish(
        &rule,
        "A standing rule outside the query ranking",
        true,
        json!([]),
        vec![],
    );
    f.publish(
        &required,
        "Always include this dependency.",
        false,
        json!([]),
        vec![],
    );
    f.publish(
        &selected,
        "freshuniquetoken current evidence",
        false,
        json!([{"kind":"required","target_memory_id":required}]),
        vec![],
    );
    let result = f.query(json!({"recall_for":"freshuniquetoken","memory_ids":[selected],"memory_candidate_limit":1}), &RequestControl::default()).unwrap();
    let memories = result["memories"].as_array().unwrap();
    for id in [&required, &selected] {
        assert!(memories.iter().any(|v| v["memory_id"] == *id));
    }
    assert_eq!(
        result["rules"],
        json!([{"applicability":{"scope":["user"]},"items":[{"memory_id":rule,"what_to_remember":"A standing rule outside the query ranking"}]}])
    );
}

#[test]
fn semantic_loading_busy_unavailable_and_partial_keep_lexical_results() {
    let f = Fixture::new(65536);
    let id = Fixture::id();
    f.publish(
        &id,
        "needleforsemantic availability",
        false,
        json!([]),
        vec![],
    );
    let position = f
        .store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let complete = f
        .query(
            json!({"recall_for":"needleforsemantic"}),
            &RequestControl::default(),
        )
        .unwrap();
    assert!(complete.get("semantic_complete").is_none());
    for (model, index) in [
        (FakeModel::Loading, FakeIndex::Complete),
        (
            FakeModel::Unavailable("offline".into()),
            FakeIndex::Complete,
        ),
        (FakeModel::Ready, FakeIndex::Busy),
        (FakeModel::Ready, FakeIndex::Failed("failure".into())),
        (FakeModel::Ready, FakeIndex::Unavailable("offline".into())),
        (
            FakeModel::Ready,
            FakeIndex::Partial {
                available: position.clone(),
            },
        ),
    ] {
        f.semantic.set_model(model);
        f.semantic.set_index(index);
        let result = f
            .query(
                json!({"recall_for":"needleforsemantic"}),
                &RequestControl::default(),
            )
            .unwrap();
        assert_eq!(result["semantic_complete"], false);
        let present = result["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["memory_id"] == id)
            || result["memories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["memory_id"] == id);
        assert!(present);
        assert!(result["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "semantic_channel_incomplete"));
    }
}

#[test]
fn changed_delta_shadows_obsolete_lexical_body() {
    let f = Fixture::new(65536);
    let id = Fixture::id();
    f.publish(&id, "olduniquetoken", false, json!([]), vec![]);
    let old = f.store.read_current(&id).unwrap();
    let versions = old
        .references()
        .unwrap()
        .into_iter()
        .map(|r| r.observed_version)
        .collect();
    f.publish(&id, "newuniquetoken", false, json!([]), versions);
    let stale = f
        .query(
            json!({"recall_for":"olduniquetoken"}),
            &RequestControl::default(),
        )
        .unwrap();
    assert!(!stale["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["memory_id"] == id));
    let fresh = f
        .query(
            json!({"recall_for":"newuniquetoken"}),
            &RequestControl::default(),
        )
        .unwrap();
    assert!(
        fresh["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["memory_id"] == id)
            || fresh["memories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["memory_id"] == id)
    );
}

#[test]
fn cancelled_read_returns_error() {
    let f = Fixture::new(65536);
    let control = RequestControl::default();
    control
        .cancel
        .store(true, std::sync::atomic::Ordering::Release);
    assert_eq!(
        f.query(json!({"recall_for":"anything"}), &control)
            .unwrap_err()
            .code,
        "cancelled"
    );
}

#[test]
fn oversized_mandatory_body_uses_file_and_small_reads_leave_files_unchanged() {
    let f = Fixture::new(4096);
    let id = Fixture::id();
    f.publish(&id, &"body ".repeat(1200), true, json!([]), vec![]);
    let result = f
        .query(json!({"memory_ids":[id]}), &RequestControl::default())
        .unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    let path = PathBuf::from(result["artifact"]["path"].as_str().unwrap());
    let content: Value = serde_json::from_reader(std::fs::File::open(path).unwrap()).unwrap();
    assert_eq!(
        content["memories"][0]["current_heads"][0]["what_to_remember"]
            .as_str()
            .unwrap()
            .len(),
        6000
    );
    assert!(!f
        .identity
        .local
        .join("response-artifacts/response-registry.sqlite")
        .exists());
}

#[test]
fn ordinary_reads_do_not_change_persistent_files() {
    use crate::test_support::write_audit::{assert_read_openers, WriteAudit};
    let f = Fixture::new(65536);
    let id = Fixture::id();
    f.publish(
        &id,
        "ordinary read has a lexical needle",
        false,
        json!([]),
        vec![],
    );
    // The store owner keeps its database connections open throughout requests.
    let owners: Vec<_> = [
        f.dir.path().join("exact-test.sqlite"),
        f.dir.path().join("lexical-test.sqlite"),
        f.identity.local.join("canonical/journal.sqlite"),
    ]
    .into_iter()
    .map(|path| {
        let db = Connection::open(path).unwrap();
        db.execute_batch("BEGIN; SELECT count(*) FROM sqlite_schema;")
            .unwrap();
        db
    })
    .collect();
    assert_read_openers(
        &f.identity.local.join("control.sqlite"),
        &f.dir.path().join("exact-test.sqlite"),
        &f.dir.path().join("lexical-test.sqlite"),
    );
    // File comparison cannot count rolled-back write transactions or files created and removed between snapshots.
    let before = WriteAudit::capture(f.dir.path()).unwrap();
    for _ in 0..3 {
        f.query(
            json!({"recall_for":"lexical needle"}),
            &RequestControl::default(),
        )
        .unwrap();
    }
    assert_eq!(WriteAudit::capture(f.dir.path()).unwrap(), before);
    drop(owners);
}

#[test]
fn fr_06_5_candidate_items_are_omitted_as_whole_items_before_mandatory_content() {
    let f = Fixture::new(4096);
    let rule = Fixture::id();
    f.publish(
        &rule,
        "The standing rule must remain complete.",
        true,
        json!([]),
        vec![],
    );
    for n in 0..12 {
        let id = Fixture::id();
        let body = format!("sharedqueryterm {n} {}", "detail ".repeat(80));
        f.publish(&id, &body, false, json!([]), vec![]);
    }
    f.semantic.set_model(FakeModel::Loading);
    let result = f
        .query(
            json!({"recall_for":"sharedqueryterm", "memory_candidate_limit":12}),
            &RequestControl::default(),
        )
        .unwrap();
    assert!(result.get("artifact").is_none());
    assert!(serde_json::to_vec(&result).unwrap().len() <= 4096);
    assert_eq!(
        result["rules"][0]["items"][0]["what_to_remember"],
        "The standing rule must remain complete."
    );
    let counts = &result["candidate_counts"]["memories"];
    assert_eq!(counts["matched"], 12);
    assert_eq!(
        counts["shown"].as_u64().unwrap() as usize,
        result["candidates"].as_array().unwrap().len()
    );
    assert!(counts["shown"].as_u64().unwrap() < 12);
}

#[test]
fn work_target_matching_is_case_sensitive_and_required_ignores_candidate_limit() {
    let f = Fixture::new(65536);
    let path = f.dir.path().join("project");
    std::fs::create_dir(&path).unwrap();
    let command = json!({"action":"connect_project_memory","connect_project_memory":{"project_path":path.to_str().unwrap(),"intent":"create","project_name":"Example"}});
    let connected = f
        .call(agentlaw_flows::parse_request(&command.to_string()).unwrap())
        .unwrap();
    let project = connected["project_connection"]["project_id"]
        .as_str()
        .unwrap();
    let id = Fixture::id();
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"project","project_id":project},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[{"project_id":project,"path":"Src/pay/main.rs","kind":"file","reading":"required"}]}),
                body: "The required work target.".into(),
            }],
        },
    };
    f.store
        .publish(
            &Fixture::id(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "observed".into(),
            }],
        )
        .unwrap();
    let base = json!({"project_path":path.to_str().unwrap(),"recall_for":"unrelated","memory_candidate_limit":1,"work_targets":[{"path":"Src/pay","kind":"directory"}]});
    let result = f.query(base.clone(), &RequestControl::default()).unwrap();
    assert!(result["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["memory_id"] == id));
    let mut wrong = base;
    wrong["work_targets"][0]["path"] = json!("src/pay");
    let result = f.query(wrong, &RequestControl::default()).unwrap();
    assert!(!result["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["memory_id"] == id));
}

#[test]
fn consolidation_redirect_shadows_both_old_lexical_records() {
    let f = Fixture::new(65536);
    let make = |body: &str| json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"observed","applies_to":["user"]}]}});
    let one = f
        .call(agentlaw_flows::parse_request(&make("firstobsoleteword").to_string()).unwrap())
        .unwrap()["results"][0]["memory_ref"]
        .clone();
    let two = f
        .call(agentlaw_flows::parse_request(&make("secondobsoleteword").to_string()).unwrap())
        .unwrap()["results"][0]["memory_ref"]
        .clone();
    let merge = json!({"action":"remember_this","remember_this":{"memories":[{"operation":"consolidate","consolidation_refs":[one,two],"what_to_remember":"mergedcurrentword","evidence":"observed"}]}});
    let result = f
        .call(agentlaw_flows::parse_request(&merge.to_string()).unwrap())
        .unwrap();
    assert_eq!(result["status"], "remembered");
    let original_ids = [
        one["memory_id"].as_str().unwrap(),
        two["memory_id"].as_str().unwrap(),
    ];
    for old in ["firstobsoleteword", "secondobsoleteword"] {
        let result = f
            .query(json!({"recall_for":old}), &RequestControl::default())
            .unwrap();
        assert!(!result["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| original_ids.contains(&v["memory_id"].as_str().unwrap_or(""))));
    }
    let redirected = f
        .query(
            json!({"memory_ids":[one["memory_id"]]}),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(
        redirected["memories"][0]["current_heads"][0]["what_to_remember"],
        "mergedcurrentword"
    );
}

#[test]
fn coordinator_returns_capacity_without_a_partial_result() {
    use crate::{
        ports::{HealthSink, RuntimeResult},
        projection::{
            ActiveGeneration, ActivePointerSink, ProjectionConfig, ProjectionHandle,
            RetiredFileSink,
        },
    };
    use std::{sync::Arc, time::Duration};
    struct Sink;
    impl ActivePointerSink for Sink {
        fn persist_active(&self, _: &str, _: &ActiveGeneration) -> RuntimeResult<()> {
            Ok(())
        }
    }
    impl RetiredFileSink for Sink {
        fn retire(&self, _: &str, _: &[PathBuf]) -> RuntimeResult<()> {
            Ok(())
        }
    }
    let f = Fixture::new(65536);
    let source = f.store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = ActiveGeneration {
        id: "test".into(),
        epoch: position.epoch,
        exact_file: f.dir.path().join("exact-test.sqlite"),
        lexical_file: f.dir.path().join("lexical-test.sqlite"),
    };
    let sink = Arc::new(Sink);
    let projection = ProjectionHandle::start(
        f.identity.binding_id.clone(),
        source.clone(),
        active,
        sink.clone(),
        sink,
        HealthSink::default(),
        ProjectionConfig::new(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while projection.status().through.is_none() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    f.publish(&Fixture::id(), "first delta", false, json!([]), vec![]);
    f.publish(&Fixture::id(), "second delta", false, json!([]), vec![]);
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &f.semantic,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let coordinator = ReadCoordinator::new(
        projection.clone(),
        DeltaBudget {
            max_identities: 1,
            ..DeltaBudget::default()
        },
    );
    let request = serde_json::from_value(json!({"recall_for":"delta"})).unwrap();
    let error = coordinator
        .run_query(&ctx, QueryRequest::Recall(request))
        .unwrap_err();
    assert_eq!(error.code, "capacity");
    assert!(
        error.retryable,
        "FR-06.4: capacity is retryable after indexing"
    );
    let target = source.read_snapshot().unwrap().position().clone();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while projection
        .status()
        .through
        .as_ref()
        .is_none_or(|p| p.sequence < target.sequence)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "capacity must wake the maintainer"
        );
        std::thread::yield_now();
    }
    assert!(projection.shutdown(Duration::from_secs(5)));
}

#[test]
fn large_closure_spools_are_removed_after_file_delivery() {
    use agentlaw_flows::exact::ExactIndexWriter;
    let f = Fixture::new(65536);
    let first = Fixture::id();
    let second = Fixture::id();
    let body = "x".repeat(33 * 1024 * 1024);
    f.publish(&first, &body, false, json!([]), vec![]);
    f.publish(&second, &body, false, json!([]), vec![]);
    drop(body);
    let source = f.store.owned_published_reader();
    let mut writer = ExactIndexWriter::open(f.dir.path().join("exact-test.sqlite")).unwrap();
    writer
        .synchronize(&source, &RequestControl::default())
        .unwrap();
    drop(writer);
    let result = f
        .query(
            json!({"memory_ids":[first,second]}),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    let path = PathBuf::from(result["artifact"]["path"].as_str().unwrap());
    assert!(std::fs::metadata(path).unwrap().len() > 64 * 1024 * 1024);
    let spool_root = f.identity.local.join("canonical/acquisitions");
    if spool_root.exists() {
        assert_eq!(std::fs::read_dir(spool_root).unwrap().count(), 0);
    }
    let artifacts = std::fs::read_dir(f.identity.local.join("response-artifacts")).unwrap();
    assert!(artifacts.into_iter().all(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "json")
    }));
}

fn assert_task_handoff(result: &Value, id: &str, heads: Value) {
    assert_eq!(result["active_task_count"], 1, "{result}");
    assert_eq!(
        result["active_task_handoff"],
        json!({"memory_id":id,"current_heads":heads})
    );
    assert!(
        result["memories"].as_array().unwrap().is_empty(),
        "{result}"
    );
    let instruction = result["task_instruction"].as_str().unwrap();
    for term in ["memory_ids", "references", "full", "edit", "reconcil"] {
        assert!(instruction.to_lowercase().contains(term), "{instruction}");
    }
}

#[test]
fn fr_10_4_active_task_sections_remain_complete_when_other_candidates_are_present() {
    let mut f = Fixture::new(65536);
    let path = f.dir.path().join("task-project");
    std::fs::create_dir(&path).unwrap();
    let command = json!({"action":"connect_project_memory","connect_project_memory":{"project_path":path.to_str().unwrap(),"intent":"create","project_name":"Task project"}});
    let connected = f
        .call(agentlaw_flows::parse_request(&command.to_string()).unwrap())
        .unwrap();
    assert!(connected["project_connection"]["project_id"].is_string());
    let required = Fixture::id();
    f.publish(
        &required,
        "Dependency for full Task review",
        false,
        json!([]),
        vec![],
    );
    let body = "# Objective\nFinish implementation\n## Objective # #\nVerify the handoff\n# Current position\nHalfway through\n# Resume point\nRun verification\n# References\nNone\n";
    let create = json!({"action":"remember_this","remember_this":{"project_path":path,"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed task handoff","applies_to":["project"],"in_working_set":true,"required_memory_ids":[required]}]}});
    let saved = f
        .call(agentlaw_flows::parse_request(&create.to_string()).unwrap())
        .unwrap();
    assert_eq!(saved["status"], "remembered", "{saved}");
    let id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for body in ["discoverable context alpha", "discoverable context bravo"] {
        f.publish(&Fixture::id(), body, false, json!([]), vec![]);
    }
    let result = f.query(json!({"project_path":path.to_str().unwrap(),"recall_for":"discoverable", "include_active_tasks":true, "memory_candidate_limit":1}), &RequestControl::default()).unwrap();
    assert_eq!(
        result["candidate_counts"]["memories"],
        json!({"matched":2,"shown":1})
    );
    let expected = json!([{"objective":"Finish implementation\n## Objective # #\nVerify the handoff\n","current_position":"Halfway through\n","resume_point":"Run verification\n"}]);
    assert!(result.get("artifact").is_none(), "{result}");
    assert!(
        !result["undelivered_required"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| item["memory_id"] == id),
        "{result}"
    );
    assert_task_handoff(&result, &id, expected.clone());
    assert!(result["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["memory_id"] != id));
    let matched = f.query(json!({"project_path":path,"recall_for":"Finish implementation","include_active_tasks":true,"memory_candidate_limit":1}), &RequestControl::default()).unwrap();
    assert_task_handoff(&matched, &id, expected);
    assert_eq!(
        matched["candidate_counts"]["memories"],
        json!({"matched":0,"shown":0})
    );

    let exact = f
        .query(json!({"memory_ids":[id]}), &RequestControl::default())
        .unwrap();
    let task = exact["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["memory_id"] == id)
        .unwrap();
    let head = &task["current_heads"][0];
    assert_eq!(head["what_to_remember"], body);
    assert_eq!(head["required_memory_ids"], json!([required]));
    assert!(exact["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["memory_id"] == required
            && m["current_heads"][0]["what_to_remember"] == "Dependency for full Task review"));
    let reference = &head["memory_ref"];
    assert_eq!(
        reference,
        &json!(f.store.read_current(&id).unwrap().references().unwrap()[0])
    );
    let change = json!({"action":"remember_this","remember_this":{"project_path":path,"memories":[{"operation":"evolve","parent_refs":[reference],"in_working_set":true,"evidence":"Confirmed handoff after reading the full current head"}]}});
    let revised = f
        .call(agentlaw_flows::parse_request(&change.to_string()).unwrap())
        .unwrap();
    assert_eq!(revised["status"], "remembered", "{revised}");
    assert_eq!(f.store.read_current(&id).unwrap().heads()[0].body, body);
    for index in 0..12 {
        f.publish(
            &Fixture::id(),
            &format!("budgetfixture {index} {}", "padding ".repeat(72)),
            false,
            json!([]),
            vec![],
        );
    }
    f.semantic.set_model(FakeModel::Loading);
    let request = json!({"project_path":path,"recall_for":"budgetfixture","include_active_tasks":true,"memory_candidate_limit":12});
    let unlimited = f
        .query(request.clone(), &RequestControl::default())
        .unwrap();
    assert_eq!(
        unlimited["candidate_counts"]["memories"],
        json!({"matched":12,"shown":12})
    );
    f.identity.response_limit = 4096;
    let bounded = f
        .query(request.clone(), &RequestControl::default())
        .unwrap();
    assert_eq!(
        bounded["active_task_handoff"],
        unlimited["active_task_handoff"]
    );
    assert_eq!(bounded["task_instruction"], unlimited["task_instruction"]);
    assert!(serde_json::to_vec(&bounded).unwrap().len() <= 4096);
    let count = &bounded["candidate_counts"]["memories"];
    let shown = bounded["candidates"].as_array().unwrap().len();
    assert_eq!(count, &json!({"matched":12,"shown":shown}));
    assert!(shown > 0 && shown < 12);
    let limit = &bounded["response_limit"];
    assert_eq!(limit["applied_bytes"], 4096);
    assert_eq!(limit["config_key"], "response_limit_bytes");
    assert_eq!(limit["config_file"], json!(f.identity.config_file));
    let notice = limit["user_notice"].as_str().unwrap().to_lowercase();
    assert!(
        notice.contains("search") || notice.contains("show") || notice.contains("display"),
        "{notice}"
    );

    let mut bare = bounded.clone();
    bare.as_object_mut().unwrap().remove("response_limit");
    bare["candidates"] = json!([]);
    bare["candidate_counts"]["memories"]["shown"] = json!(0);
    let tight_limit = serde_json::to_vec(&bare).unwrap().len();
    f.identity.response_limit = tight_limit;
    let envelope = f.query(request, &RequestControl::default()).unwrap();
    assert_eq!(envelope["code"], "complete_content_in_file");
    assert_eq!(envelope["content_read"], false);
    let file = std::fs::read_to_string(envelope["artifact"]["path"].as_str().unwrap()).unwrap();
    let content: Value = serde_json::from_str(&file).unwrap();
    assert_eq!(
        content["active_task_handoff"],
        bounded["active_task_handoff"]
    );
    assert_eq!(content["task_instruction"], bounded["task_instruction"]);
    assert_eq!(content["response_limit"]["applied_bytes"], tight_limit);
    assert_eq!(
        content["response_limit"]["config_file"],
        json!(f.identity.config_file)
    );
    assert!(content["response_limit"]["user_notice"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert_eq!(file.matches("\"response_limit\"").count(), 1);
}

#[test]
fn discover_reports_candidates_without_binding_the_folder() {
    let f = Fixture::new(65536);
    let existing = f.dir.path().join("existing-project");
    let other = f.dir.path().join("other-project");
    std::fs::create_dir(&existing).unwrap();
    std::fs::create_dir(&other).unwrap();
    let command = json!({"action":"connect_project_memory","connect_project_memory":{"project_path":existing.to_str().unwrap(),"intent":"create","project_name":"Catalog example"}});
    let connected = f
        .call(agentlaw_flows::parse_request(&command.to_string()).unwrap())
        .unwrap();
    let project = connected["project_connection"]["project_id"]
        .as_str()
        .unwrap();
    let source = f.store.owned_published_reader();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &f.semantic,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let mut request: agentlaw_contracts::ConnectRequest = serde_json::from_value(
        json!({"project_path":other.to_str().unwrap(), "clues":{"name":"Catalog"}}),
    )
    .unwrap();
    let result = agentlaw_flows::read::discover(&ctx, &request).unwrap();
    assert_eq!(result["code"], "project_connection_required");
    assert_eq!(result["candidates"][0]["project_id"], project);
    request.restore_context = Some(true);
    assert_eq!(
        agentlaw_flows::read::discover(&ctx, &request)
            .unwrap_err()
            .code,
        "query_classification_error"
    );
    assert!(reader
        .exact_association(&f.identity.binding_id, other.to_str().unwrap())
        .unwrap()
        .is_none());
}

#[test]
fn fr_06_5_publication_during_query_preparation_is_in_the_final_rule_snapshot() {
    let f = Fixture::new(65536);
    let rule = Fixture::id();
    let change = Fixture::id();
    let machine = f.identity.machine_id.clone();
    let store = &f.store;
    let (start_tx, start_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let gate = QueryGate {
        fake: f.semantic.clone(),
        on_embed: true,
        fired: Default::default(),
        started: start_tx,
        proceed: std::sync::Mutex::new(done_rx),
    };
    std::thread::scope(|scope| {
        let published_rule = rule.clone();
        let writer = scope.spawn(move || {
            start_rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
            let unit = CurrentUnit { entity_id:published_rule, entity_type:"memory".into(), state:UnitState::Live { heads:vec![Head { metadata:json!({"change_id":change,"applicability":{"scope":"user"},"origin":{"machine_id":machine,"user_id":"user"},"recorded_at_ms":0,"is_rule":true,"relations":[],"work_targets":[]}), body:"The new rule during preparation.".into() }] } };
            store.publish(&uuid::Uuid::new_v4().to_string(), vec![Mutation { unit, expected_versions:vec![], evidence:"observed".into() }]).unwrap();
            done_tx.send(()).unwrap();
        });
        let result = f
            .query_with_semantic(
                json!({"recall_for":"unrelated"}),
                &RequestControl::default(),
                &gate,
            )
            .unwrap();
        writer.join().unwrap();
        let found = result["rules"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|memory| memory["memory_id"] == rule)
            .unwrap();
        assert_eq!(
            found["what_to_remember"],
            "The new rule during preparation."
        );
    });
}
#[test]
fn multiple_procedure_heads_are_compact_rows_counted_as_one_candidate() {
    let f = Fixture::new(65536);
    f.semantic.set_model(FakeModel::Loading);
    let id = Fixture::id();
    let head = |name: &str| Head {
        metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"user"},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"name":name,"use_when":"procedureclue","evidence_memory_ids":[]}),
        body: "Follow the documented steps.".into(),
    };
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "learned_procedure".into(),
        state: UnitState::Live {
            heads: vec![head("First"), head("Second")],
        },
    };
    f.store
        .publish(
            &Fixture::id(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "observed".into(),
            }],
        )
        .unwrap();
    let result = f
        .query(
            json!({"recall_for":"procedureclue", "procedure_candidate_limit":1}),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(
        result["candidate_counts"]["learned_procedures"]["matched"],
        1
    );
    assert_eq!(result["candidate_counts"]["learned_procedures"]["shown"], 1);
    let rows = result["learned_procedures"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|row| row["procedure_id"] == id && row.get("current_heads").is_none()));
    assert!(result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| diagnostic["code"] == "procedure_head_reconciliation_required"));
}

#[test]
fn fr_06_5_restore_uses_saved_project_after_the_folder_is_reconnected() {
    use crate::{
        ports::{HealthSink, RuntimeResult},
        projection::{
            ActiveGeneration, ActivePointerSink, ProjectionConfig, ProjectionHandle,
            RetiredFileSink,
        },
    };
    use std::{sync::Arc, time::Duration};
    struct Sink;
    impl ActivePointerSink for Sink {
        fn persist_active(&self, _: &str, _: &ActiveGeneration) -> RuntimeResult<()> {
            Ok(())
        }
    }
    impl RetiredFileSink for Sink {
        fn retire(&self, _: &str, _: &[PathBuf]) -> RuntimeResult<()> {
            Ok(())
        }
    }
    let f = Fixture::new(65536);
    let folder_a = f.dir.path().join("project-a");
    let folder_b = f.dir.path().join("project-b");
    std::fs::create_dir(&folder_a).unwrap();
    std::fs::create_dir(&folder_b).unwrap();
    let create = |path: &std::path::Path, name: &str| json!({"action":"connect_project_memory","connect_project_memory":{"project_path":path.to_str().unwrap(),"intent":"create","project_name":name}});
    let a = f
        .call(agentlaw_flows::parse_request(&create(&folder_a, "A").to_string()).unwrap())
        .unwrap()["project_connection"]["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = f
        .call(agentlaw_flows::parse_request(&create(&folder_b, "B").to_string()).unwrap())
        .unwrap()["project_connection"]["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (project, body) in [(&a, "Project A rule"), (&b, "Project B rule")] {
        let unit = CurrentUnit {
            entity_id: Fixture::id(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"project","project_id":project},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":true,"relations":[],"work_targets":[]}),
                    body: body.into(),
                }],
            },
        };
        f.store
            .publish(
                &Fixture::id(),
                vec![Mutation {
                    unit,
                    expected_versions: vec![],
                    evidence: "observed".into(),
                }],
            )
            .unwrap();
    }
    let reconnect = json!({"action":"connect_project_memory","connect_project_memory":{"project_path":folder_a.to_str().unwrap(),"intent":"connect","project_id":b}});
    f.call(agentlaw_flows::parse_request(&reconnect.to_string()).unwrap())
        .unwrap();
    let source = f.store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = ActiveGeneration {
        id: "test".into(),
        epoch: position.epoch,
        exact_file: f.dir.path().join("exact-test.sqlite"),
        lexical_file: f.dir.path().join("lexical-test.sqlite"),
    };
    let sink = Arc::new(Sink);
    let projection = ProjectionHandle::start(
        f.identity.binding_id.clone(),
        source.clone(),
        active,
        sink.clone(),
        sink,
        HealthSink::default(),
        ProjectionConfig::new(),
    )
    .unwrap();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    assert_eq!(
        reader
            .exact_association(&f.identity.binding_id, folder_a.to_str().unwrap())
            .unwrap()
            .unwrap()
            .project_id,
        b
    );
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    f.semantic.set_model(FakeModel::Loading);
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &f.semantic,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let coordinator = ReadCoordinator::new(projection.clone(), DeltaBudget::default());
    let request: RecallRequest = serde_json::from_value(json!({"project_path":folder_a.to_str().unwrap(),"recall_for":"resume","restore_context":true})).unwrap();
    let result = coordinator
        .run_recall_for_project(&ctx, request.clone(), &a)
        .unwrap();
    let groups = result["rules"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0]["applicability"],
        json!({"scope":["project"],"project_id":a})
    );
    let items = groups[0]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["what_to_remember"], "Project A rule");
    assert_eq!(
        coordinator
            .run_recall_for_project(&ctx, request, &Fixture::id())
            .unwrap_err()
            .code,
        "project_not_found"
    );
    assert!(projection.shutdown(Duration::from_secs(5)));
}

fn publish_small(store: &Store, machine: &str, body: &str) {
    let unit = CurrentUnit {
        entity_id: Fixture::id(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"user"},"origin":{"machine_id":machine,"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: body.into(),
            }],
        },
    };
    store
        .publish(
            &Fixture::id(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "observed".into(),
            }],
        )
        .unwrap();
}
struct ReadTestSink;
impl crate::projection::ActivePointerSink for ReadTestSink {
    fn persist_active(
        &self,
        _: &str,
        _: &crate::projection::ActiveGeneration,
    ) -> crate::ports::RuntimeResult<()> {
        Ok(())
    }
}
impl crate::projection::RetiredFileSink for ReadTestSink {
    fn retire(&self, _: &str, _: &[PathBuf]) -> crate::ports::RuntimeResult<()> {
        Ok(())
    }
}
fn start_read_projection(f: &Fixture) -> crate::projection::ProjectionHandle {
    use crate::projection::{ActiveGeneration, ProjectionConfig, ProjectionHandle};
    let source = f.store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = ActiveGeneration {
        id: "test".into(),
        epoch: position.epoch,
        exact_file: f.dir.path().join("exact-test.sqlite"),
        lexical_file: f.dir.path().join("lexical-test.sqlite"),
    };
    let sink = std::sync::Arc::new(ReadTestSink);
    let projection = ProjectionHandle::start(
        f.identity.binding_id.clone(),
        source,
        active,
        sink.clone(),
        sink,
        crate::ports::HealthSink::default(),
        ProjectionConfig::new(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while projection.status().through.is_none() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    projection
}
struct QueryGate {
    fake: FakeSemantic,
    on_embed: bool,
    fired: std::sync::atomic::AtomicBool,
    started: std::sync::mpsc::SyncSender<()>,
    proceed: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl QueryGate {
    fn pause(&self, on_embed: bool) {
        if self.on_embed == on_embed && !self.fired.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.started.send(()).unwrap();
            self.proceed
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap();
        }
    }
}
impl agentlaw_flows::semantic::SemanticPort for QueryGate {
    fn embed_prefix(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> Result<agentlaw_flows::semantic::PrefixEmbedding, agentlaw_flows::semantic::SemanticError>
    {
        self.pause(true);
        agentlaw_flows::semantic::SemanticPort::embed_prefix(&self.fake, text, control)
    }
    fn search_available(
        &self,
        query: agentlaw_flows::semantic::SemanticQuery<'_>,
        control: &RequestControl,
    ) -> Result<agentlaw_flows::semantic::SemanticCandidates, agentlaw_flows::semantic::SemanticError>
    {
        self.pause(false);
        agentlaw_flows::semantic::SemanticPort::search_available(&self.fake, query, control)
    }

    fn plan_pieces(
        &self,
        _: &[&str],
        _: &RequestControl,
    ) -> std::result::Result<
        agentlaw_flows::semantic::PiecePlan,
        agentlaw_flows::semantic::SemanticError,
    > {
        Err(agentlaw_flows::semantic::SemanticError::Unavailable(
            "fixture has no piece planner".into(),
        ))
    }
    fn embed_planned(
        &self,
        _: &agentlaw_flows::semantic::PiecePlan,
        _: usize,
        _: &agentlaw_flows::semantic::PieceVectorReuse,
        _: &RequestControl,
    ) -> std::result::Result<
        Vec<agentlaw_flows::semantic::QueryVector>,
        agentlaw_flows::semantic::SemanticError,
    > {
        Err(agentlaw_flows::semantic::SemanticError::Unavailable(
            "fixture has no piece embeddings".into(),
        ))
    }
}

#[test]
fn final_delta_uses_the_callers_budget_after_a_concurrent_publication() {
    use std::time::Duration;
    let f = Fixture::new(65536);
    let projection = start_read_projection(&f);
    let source = f.store.owned_published_reader();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let (start_tx, start_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let gate = QueryGate {
        fake: f.semantic.clone(),
        on_embed: false,
        fired: Default::default(),
        started: start_tx,
        proceed: std::sync::Mutex::new(done_rx),
    };
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &gate,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let coordinator = ReadCoordinator::new(
        projection.clone(),
        DeltaBudget {
            max_identities: 1,
            ..DeltaBudget::default()
        },
    );
    let store = &f.store;
    let machine = f.identity.machine_id.clone();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            start_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            publish_small(store, &machine, "new first");
            publish_small(store, &machine, "new second");
            done_tx.send(()).unwrap();
        });
        let request = serde_json::from_value(json!({"recall_for":"query"})).unwrap();
        let error = coordinator
            .run_query(&ctx, QueryRequest::Recall(request))
            .unwrap_err();
        assert_eq!(error.code, "capacity");
        assert!(
            error.retryable,
            "FR-06.4: capacity is retryable after indexing"
        );
        writer.join().unwrap();
    });
    assert!(projection.shutdown(Duration::from_secs(10)));
}

#[test]
fn embedding_finishes_before_projection_is_pinned() {
    use std::time::{Duration, Instant};
    let f = Fixture::new(65536);
    let projection = start_read_projection(&f);
    let source = f.store.owned_published_reader();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let (start_tx, start_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let gate = QueryGate {
        fake: f.semantic.clone(),
        on_embed: true,
        fired: Default::default(),
        started: start_tx,
        proceed: std::sync::Mutex::new(done_rx),
    };
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &gate,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let coordinator = ReadCoordinator::new(
        projection.clone(),
        DeltaBudget {
            max_identities: 1,
            ..DeltaBudget::default()
        },
    );
    let store = &f.store;
    let machine = f.identity.machine_id.clone();
    let worker_projection = projection.clone();
    let worker_source = source.clone();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            start_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            publish_small(store, &machine, "one");
            publish_small(store, &machine, "two");
            let target = worker_source.read_snapshot().unwrap().position().sequence;
            worker_projection.wake();
            let deadline = Instant::now() + Duration::from_secs(20);
            while worker_projection
                .status()
                .through
                .as_ref()
                .is_none_or(|p| p.sequence < target)
            {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            done_tx.send(()).unwrap();
        });
        let request = serde_json::from_value(json!({"recall_for":"query"})).unwrap();
        assert!(coordinator
            .run_query(&ctx, QueryRequest::Recall(request))
            .is_ok());
        writer.join().unwrap();
    });
    assert!(projection.shutdown(Duration::from_secs(10)));
}

#[test]
fn fr_06_5_automatically_applied_rules_are_minimal_but_explicit_and_required_rules_are_full() {
    let f = Fixture::new(65536);
    let auto = Fixture::id();
    let explicit = Fixture::id();
    let required = Fixture::id();
    let root = Fixture::id();
    let multi = Fixture::id();
    let target = Fixture::id();
    let active = Fixture::id();
    let folder = f.dir.path().join("rule-task-project");
    std::fs::create_dir(&folder).unwrap();
    let connected =
        f.connect(json!({"project_path":folder,"intent":"create","project_name":"Rule priority"}));
    let project = &connected["project_connection"]["project_id"];
    let task_body = "# Objective\nVerify constraints\n# Current position\nReady\n# Resume point\nInspect required context\n# References\nPublished constraints";
    for (id, body, working, targets) in [
        (
            &target,
            "Required work target rule",
            false,
            json!([{"project_id":project,"path":"src/task.rs","kind":"file","reading":"required"}]),
        ),
        (&active, task_body, true, json!([])),
    ] {
        let unit = CurrentUnit {
            entity_id: id.clone(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"project","project_id":project},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":true,"in_working_set":working,"relations":[],"work_targets":targets}),
                    body: body.into(),
                }],
            },
        };
        f.store
            .publish(
                &Fixture::id(),
                vec![Mutation {
                    unit,
                    expected_versions: vec![],
                    evidence: "Established instruction".into(),
                }],
            )
            .unwrap();
    }
    f.publish(&auto, "Automatic standing rule", true, json!([]), vec![]);
    f.publish(&explicit, "Explicit standing rule", true, json!([]), vec![]);
    f.publish(&required, "Required standing rule", true, json!([]), vec![]);
    f.publish(
        &root,
        "Explicit dependency source",
        false,
        json!([{"kind":"required","target_memory_id":required}]),
        vec![],
    );
    let make_head = |text: &str| Head {
        metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"user"},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":true,"relations":[],"work_targets":[]}),
        body: text.into(),
    };
    let unit = CurrentUnit {
        entity_id: multi.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![make_head("First rule head"), make_head("Second rule head")],
        },
    };
    f.store
        .publish(
            &Fixture::id(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "observed".into(),
            }],
        )
        .unwrap();
    let result = f
        .query(
            json!({"recall_for":"unrelated", "memory_ids":[explicit,root],"project_path":folder,"include_active_tasks":true,"work_targets":[{"path":"src/task.rs","kind":"file"}]}),
            &RequestControl::default(),
        )
        .unwrap();
    let entries = result["memories"].as_array().unwrap();
    let head = |id: &str| {
        entries.iter().find(|m| m["memory_id"] == id).unwrap()["current_heads"][0].clone()
    };
    assert_eq!(
        result["rules"],
        json!([{"applicability":{"scope":["user"]},"items":[{"memory_id":auto,"what_to_remember":"Automatic standing rule"}]}])
    );
    assert_eq!(
        entries.iter().filter(|m| m["memory_id"] == multi).count(),
        1
    );
    assert_eq!(
        entries.iter().find(|m| m["memory_id"] == multi).unwrap()["current_heads"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(entries.iter().all(|m| m["memory_id"] != auto));
    for id in [&explicit, &required, &multi, &target, &active] {
        assert_eq!(entries.iter().filter(|m| m["memory_id"] == *id).count(), 1);
        let value = head(id);
        assert!(value.get("memory_ref").is_some());
        assert!(value.get("origin").is_some());
    }
    assert_eq!(result["active_task_count"], 1);
    assert!(result.get("active_task_handoff").is_none(), "{result}");
    assert_eq!(head(&active)["what_to_remember"], task_body);
    let winner = "Exact uniquely searchable rule";
    let winner_id = Fixture::id();
    f.publish(&winner_id, winner, true, json!([]), vec![]);
    let discovered = f
        .query(json!({"recall_for":winner}), &RequestControl::default())
        .unwrap();
    let items = discovered["rules"][0]["items"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|item| item["memory_id"] == winner_id)
            .collect::<Vec<_>>(),
        [&json!({"memory_id":winner_id,"what_to_remember":winner})]
    );
    for field in ["memories", "candidates"] {
        assert!(!discovered[field]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| row["memory_id"] == winner_id));
    }
    let exact = f
        .query(
            json!({"memory_ids":[winner_id]}),
            &RequestControl::default(),
        )
        .unwrap();
    assert!(exact.get("rules").is_none());
    assert_eq!(exact["memories"].as_array().unwrap().len(), 1);
    assert_eq!(exact["memories"][0]["memory_id"], winner_id);
    let heads = exact["memories"][0]["current_heads"].as_array().unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0]["what_to_remember"], winner);
    assert_eq!(
        heads[0]["memory_ref"],
        json!(
            f.store
                .read_current(&winner_id)
                .unwrap()
                .references()
                .unwrap()[0]
        )
    );
}

#[test]
fn fr_10_4_task_heads_are_fitted_together_and_counted_once() {
    let f = Fixture::new(4096);
    let folder = f.dir.path().join("group-project");
    std::fs::create_dir(&folder).unwrap();
    let command = json!({"action":"connect_project_memory","connect_project_memory":{"project_path":folder.to_str().unwrap(),"intent":"create","project_name":"Grouping"}});
    let project = f
        .call(agentlaw_flows::parse_request(&command.to_string()).unwrap())
        .unwrap()["project_connection"]["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = Fixture::id();
    let required = Fixture::id();
    f.publish(
        &required,
        "Required handoff source",
        false,
        json!([]),
        vec![],
    );
    let head = |ch: char| {
        Head {
        metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"project","project_id":project},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"in_working_set":true,"relations":[{"kind":"required","target_memory_id":required}],"work_targets":[]}),
        body: format!(
            "# Objective\n{}\n# Current position\nworking\n# Resume point\nnext\n# References\nnone",
            ch.to_string().repeat(2100)
        ),
    }
    };
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![head('a'), head('b')],
        },
    };
    f.store
        .publish(
            &Fixture::id(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "observed".into(),
            }],
        )
        .unwrap();
    let result = f.query(json!({"project_path":folder.to_str().unwrap(),"recall_for":"unrelated", "include_active_tasks":true, "memory_candidate_limit":1}), &RequestControl::default()).unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    assert_eq!(result["content_read"], false);
    let content: Value = serde_json::from_reader(
        std::fs::File::open(result["artifact"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let current = f.store.read_current(&id).unwrap();
    let expected_heads: Vec<_> = current.heads().iter().map(|head| {
        let objective = head.body.lines().nth(1).unwrap();
        assert!(objective == "a".repeat(2100) || objective == "b".repeat(2100));
        json!({"objective":format!("{objective}\n"),"current_position":"working\n","resume_point":"next\n"})
    }).collect();
    assert_task_handoff(&content, &id, json!(expected_heads));
    assert_eq!(
        content["candidate_counts"]["memories"],
        json!({"matched":0,"shown":0})
    );
    assert!(result["next_action"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));

    let exact = f
        .query(json!({"memory_ids":[id]}), &RequestControl::default())
        .unwrap();
    assert_eq!(exact["content_read"], false);
    let full: Value = serde_json::from_reader(
        std::fs::File::open(exact["artifact"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let memories = full["memories"].as_array().unwrap();
    let task = memories
        .iter()
        .find(|memory| memory["memory_id"] == id)
        .unwrap();
    let dependency = memories
        .iter()
        .find(|memory| memory["memory_id"] == required)
        .unwrap();
    assert_eq!(
        dependency["current_heads"][0]["what_to_remember"],
        "Required handoff source"
    );
    let heads = task["current_heads"].as_array().unwrap();
    assert_eq!(heads.len(), 2);
    for ((actual, stored), reference) in heads
        .iter()
        .zip(current.heads())
        .zip(current.references().unwrap())
    {
        assert_eq!(actual["what_to_remember"], stored.body);
        assert_eq!(actual["memory_ref"], json!(reference));
        assert_eq!(actual["required_memory_ids"], json!([required]));
    }

    let artifact_dir = f.identity.local.join("response-artifacts");
    std::fs::rename(&artifact_dir, f.identity.local.join("saved-artifacts")).unwrap();
    std::fs::write(&artifact_dir, "An ordinary file blocks directory creation").unwrap();
    let failed = f.query(json!({"project_path":folder,"recall_for":"unrelated","include_active_tasks":true,"memory_candidate_limit":1}), &RequestControl::default()).unwrap_err();
    assert_eq!(failed.code, "result_delivery_failed");
    let details = failed.details.unwrap();
    let errors = details.as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["memory_id"], id);
    assert!(errors[0]["reason"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(errors[0]["next_action"]
        .as_str()
        .is_some_and(|text| text.to_lowercase().contains("recall")));
}

#[test]
fn fr_10_4_spooled_task_with_small_selected_sections_stays_inline() {
    use agentlaw_flows::exact::ExactIndexWriter;
    let f = Fixture::new(65536);
    let folder = f.dir.path().join("spooled-project");
    std::fs::create_dir(&folder).unwrap();
    let command = json!({"action":"connect_project_memory","connect_project_memory":{"project_path":folder.to_str().unwrap(),"intent":"create","project_name":"Spool"}});
    let project = f
        .call(agentlaw_flows::parse_request(&command.to_string()).unwrap())
        .unwrap()["project_connection"]["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = Fixture::id();
    let references = "x".repeat(33 * 1024 * 1024);
    let head = || {
        Head {
        metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"project","project_id":project},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"in_working_set":true,"relations":[],"work_targets":[]}),
        body: format!(
            "# Objective\nsmall\n# Current position\nsmall\n# Resume point\nsmall\n# References\n{references}"
        ),
    }
    };
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![head(), head()],
        },
    };
    let expected_refs = unit.references().unwrap();
    f.store
        .publish(
            &Fixture::id(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "observed".into(),
            }],
        )
        .unwrap();
    let source = f.store.owned_published_reader();
    let synchronize = || {
        let mut index = ExactIndexWriter::open(f.dir.path().join("exact-test.sqlite")).unwrap();
        index
            .synchronize(&source, &RequestControl::default())
            .unwrap();
    };
    let request =
        json!({"project_path":folder,"recall_for":"unrelated","include_active_tasks":true});
    let error = f
        .query(request.clone(), &RequestControl::default())
        .unwrap_err();
    assert_eq!(error.code, "capacity");
    assert!(
        error.retryable,
        "FR-06.4: capacity is retryable after indexing"
    );
    assert!(!error.message.is_empty());
    synchronize();

    let single = f.query(request, &RequestControl::default()).unwrap();
    assert!(single.get("artifact").is_none(), "{single}");
    let sections =
        json!({"objective":"small\n","current_position":"small\n","resume_point":"small\n"});
    assert_task_handoff(&single, &id, json!([sections, sections]));
    assert_eq!(
        single["candidate_counts"]["memories"],
        json!({"matched":0,"shown":0})
    );
    let exact = f
        .query(json!({"memory_ids":[id]}), &RequestControl::default())
        .unwrap();
    assert_eq!(exact["code"], "complete_content_in_file");
    assert_eq!(exact["content_read"], false);
    let file = std::fs::File::open(exact["artifact"]["path"].as_str().unwrap()).unwrap();
    let content: Value = serde_json::from_reader(std::io::BufReader::new(file)).unwrap();
    assert_eq!(content["memories"][0]["memory_id"], id);
    let heads = content["memories"][0]["current_heads"].as_array().unwrap();
    assert_eq!(heads.len(), 2);
    let full_body = format!("# Objective\nsmall\n# Current position\nsmall\n# Resume point\nsmall\n# References\n{references}");
    for (head, reference) in heads.iter().zip(expected_refs) {
        assert_eq!(head["what_to_remember"].as_str(), Some(full_body.as_str()));
        assert_eq!(head["memory_ref"], json!(reference));
    }
    drop(content);
    let other = CurrentUnit {
        entity_id: Fixture::id(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"project","project_id":project},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"in_working_set":true,"relations":[],"work_targets":[]}),
                body: "# Objective\nOther task\n# Current position\nReady\n# Resume point\nContinue\n# References\nNone".into(),
            }],
        },
    };
    f.store
        .publish(
            &Fixture::id(),
            vec![Mutation {
                unit: other,
                expected_versions: vec![],
                evidence: "other active task".into(),
            }],
        )
        .unwrap();
    drop(references);
    synchronize();
    let result = f.query(json!({"project_path":folder.to_str().unwrap(),"recall_for":"unrelated", "include_active_tasks":true}), &RequestControl::default()).unwrap();
    assert!(result.get("artifact").is_none());
    assert_eq!(result["active_task_count"], 2);
    assert!(result.get("active_task_handoff").is_none());
    assert_eq!(result["candidate_counts"]["memories"]["matched"], 2);
    let task_rows: Vec<_> = result["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["memory_id"] == id)
        .collect();
    assert_eq!(task_rows.len(), 2);
    assert!(task_rows.iter().all(|row| row["objective"] == "small\n"));
    assert_eq!(result["candidate_counts"]["memories"]["shown"], 2);
    let spool_root = f.identity.local.join("canonical/acquisitions");
    if spool_root.exists() {
        assert_eq!(std::fs::read_dir(spool_root).unwrap().count(), 0);
    }
}

#[test]
fn local_epoch_read_failure_preserves_canonical_source() {
    for missing in [true, false] {
        let f = Fixture::new(65536);
        let id = Fixture::id();
        let body = "Canonical content survives local epoch failure";
        f.publish(&id, body, false, json!([]), vec![]);
        let projection = start_read_projection(&f);
        let coordinator = ReadCoordinator::new(projection.clone(), DeltaBudget::default());
        let source = f.store.owned_published_reader();
        let reader = ControlReader::open(&f.identity.local).unwrap();
        let cache = HistoryCacheGuards::new();
        let control = RequestControl::default();
        let ctx = ReadCtx {
            identity: &f.identity,
            source: &source,
            control: &reader,
            semantic: &f.semantic,
            history_semantic: &f.semantic,
            history_cache: &cache,
            request: &control,
        };
        let request = || {
            QueryRequest::Recall(RecallRequest {
                memory_ids: Some(vec![id.clone()]),
                ..Default::default()
            })
        };
        let canonical = f
            .identity
            .root
            .join("current/memory")
            .join(&id[..2])
            .join(format!("{id}.md"));
        let original = std::fs::read(&canonical).unwrap();
        let epoch = f.identity.local.join("canonical/source-epoch");
        let saved = f.dir.path().join("saved-source-epoch");
        let original_epoch = std::fs::read(&epoch).unwrap();
        if missing {
            std::fs::rename(&epoch, &saved).unwrap();
        } else {
            std::fs::write(&epoch, b"invalid JSON epoch").unwrap();
        }
        let observed = coordinator.run_query(&ctx, request());
        if missing {
            std::fs::rename(saved, epoch).unwrap();
        } else {
            std::fs::write(epoch, original_epoch).unwrap();
        }
        let error = observed.unwrap_err();
        assert_eq!(
            error.code, "recall_epoch_unavailable",
            "FR-06.4: missing={missing}, {error:?}"
        );
        assert!(error.retryable);
        assert!(error.details.is_none());
        let action = error.next_action.unwrap().to_lowercase();
        assert!(
            action.contains("local") && action.contains("epoch") && action.contains("recall"),
            "{action}"
        );
        assert!(
            !action.contains("restore the canonical") && !action.contains("recreate the epoch"),
            "{action}"
        );
        assert_eq!(std::fs::read(canonical).unwrap(), original);
        assert_eq!(
            coordinator.run_query(&ctx, request()).unwrap()["memories"][0]["current_heads"][0]
                ["what_to_remember"],
            body
        );
        assert!(projection.shutdown(std::time::Duration::from_secs(10)));
    }
}

#[test]
fn delta_errors_follow_the_requested_operation() {
    use crate::protocol::InventoryFilter;

    for corrupt in [false, true] {
        for operation in ["prepare", "procedure", "recall"] {
            let f = Fixture::new(65536);
            let projection = start_read_projection(&f);
            let id = Fixture::id();
            let body = "Unindexed delta";
            f.publish(&id, body, false, json!([]), vec![]);
            if corrupt {
                let path = f
                    .identity
                    .root
                    .join("current/memory")
                    .join(&id[..2])
                    .join(format!("{id}.md"));
                let mut bytes = std::fs::read(&path).unwrap();
                let offset = bytes
                    .windows(body.len())
                    .position(|part| part == body.as_bytes())
                    .unwrap();
                bytes[offset] = b'X';
                std::fs::write(path, bytes).unwrap();
            }
            let budget = if corrupt {
                DeltaBudget::default()
            } else {
                DeltaBudget {
                    max_identities: 0,
                    max_resident_bytes: 0,
                }
            };
            let coordinator = ReadCoordinator::new(projection.clone(), budget);
            let source = f.store.owned_published_reader();
            let reader = ControlReader::open(&f.identity.local).unwrap();
            let cache = HistoryCacheGuards::new();
            let control = RequestControl::default();
            let ctx = ReadCtx {
                identity: &f.identity,
                source: &source,
                control: &reader,
                semantic: &f.semantic,
                history_semantic: &f.semantic,
                history_cache: &cache,
                request: &control,
            };
            let error = match operation {
                "prepare" => coordinator.run_prepare(&ctx, serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"New memory proposal","evidence":"Observed","applies_to":["user"]}]})).unwrap()).err().unwrap(),
                "procedure" => coordinator.search_procedures(&ctx, "procedure", &InventoryFilter { scope:None, project:None, machine:None, table:false }, 1).unwrap_err(),
                _ => coordinator.run_query(&ctx, QueryRequest::Recall(RecallRequest {recall_for:Some("unindexed delta".into()), ..Default::default()})).unwrap_err(),
            };
            let expected = if corrupt {
                "source_corrupt"
            } else {
                "capacity"
            };
            assert_eq!(
                error.code, expected,
                "FR-06.4 CT-04e: {operation}, {error:?}"
            );
            assert_eq!(error.retryable, !corrupt && operation == "recall");
            if operation == "recall" {
                let action = error.next_action.unwrap().to_lowercase();
                assert!(
                    action.contains(if corrupt { "doctor --audit" } else { "index" }),
                    "{action}"
                );
                assert!(
                    action.contains("recall") || action.contains("query"),
                    "{action}"
                );
            } else {
                let action = error
                    .next_action
                    .expect("CT-04e: operation recovery guidance")
                    .to_lowercase();
                if corrupt {
                    assert!(
                        action.contains("preserv")
                            && action.contains("doctor --audit")
                            && action.contains("restor"),
                        "{action}"
                    );
                } else {
                    assert!(action.contains("index"), "{action}");
                    let operation_terms = if operation == "prepare" {
                        ["prepar", "remember_this"]
                    } else {
                        ["procedure", "search"]
                    };
                    assert!(
                        operation_terms.iter().any(|term| action.contains(term)),
                        "{action}"
                    );
                }
            }
            assert!(projection.shutdown(std::time::Duration::from_secs(10)));
        }
    }
}

#[test]
fn query_detecting_changed_source_epoch_wakes_idle_recovery() {
    use std::time::{Duration, Instant};
    let f = Fixture::new(65536);
    let projection = start_read_projection(&f);
    let other = tempfile::tempdir().unwrap();
    let other_store = Store::open(other.path().join("source"), other.path().join("local")).unwrap();
    let replacement = other_store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .epoch
        .clone();
    std::fs::copy(
        other.path().join("local/source-epoch"),
        f.identity.local.join("canonical/source-epoch"),
    )
    .unwrap();
    let source = f.store.owned_published_reader();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &f.semantic,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let coordinator = ReadCoordinator::new(projection.clone(), DeltaBudget::default());
    let request = serde_json::from_value(json!({})).unwrap();
    let error = coordinator
        .run_query(&ctx, QueryRequest::Recall(request))
        .unwrap_err();
    assert_eq!(error.code, "source_epoch_changed");
    let deadline = Instant::now() + Duration::from_secs(20);
    while projection
        .status()
        .through
        .as_ref()
        .is_none_or(|p| p.epoch != replacement)
    {
        assert!(
            Instant::now() < deadline,
            "query must wake source-epoch recovery"
        );
        std::thread::yield_now();
    }
    assert!(projection.shutdown(Duration::from_secs(10)));
}

#[test]
fn fr_10_4_active_task_published_during_embedding_is_in_the_final_recall() {
    use std::time::Duration;
    let f = Fixture::new(65536);
    let path = f.dir.path().join("concurrent-task-project");
    std::fs::create_dir(&path).unwrap();
    let connected =
        f.connect(json!({"project_path":path,"intent":"create","project_name":"Concurrent task"}));
    let project = connected["project_connection"]["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = Fixture::id();
    let change = Fixture::id();
    let body = "# Objective\nShip the feature\n# Current position\nImplementation complete\n# Resume point\nReview verification\n# References\nDesign document\n";
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":change,"applicability":{"scope":"project","project_id":project},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"in_working_set":true,"relations":[],"work_targets":[]}),
                body: body.into(),
            }],
        },
    };
    let expected_reference = unit.references().unwrap()[0].clone();
    let projection = start_read_projection(&f);
    let source = f.store.owned_published_reader();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let (start_tx, start_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let gate = QueryGate {
        fake: f.semantic.clone(),
        on_embed: true,
        fired: Default::default(),
        started: start_tx,
        proceed: std::sync::Mutex::new(done_rx),
    };
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &gate,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let coordinator = ReadCoordinator::new(
        projection.clone(),
        DeltaBudget {
            max_identities: 1,
            ..DeltaBudget::default()
        },
    );
    let store = &f.store;
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            start_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            store
                .publish(
                    &Fixture::id(),
                    vec![Mutation {
                        unit,
                        expected_versions: vec![],
                        evidence: "Another session published a Task".into(),
                    }],
                )
                .unwrap();
            done_tx.send(()).unwrap();
        });
        let request = serde_json::from_value(json!({"project_path":path,"recall_for":"unrelated search","include_active_tasks":true})).unwrap();
        let result = coordinator
            .run_query(&ctx, QueryRequest::Recall(request))
            .unwrap();
        assert_task_handoff(
            &result,
            &id,
            json!([{
                "objective":"Ship the feature\n",
                "current_position":"Implementation complete\n",
                "resume_point":"Review verification\n"
            }]),
        );
        let exact = f
            .query(json!({"memory_ids":[id]}), &RequestControl::default())
            .unwrap();
        assert_eq!(
            exact["memories"][0]["current_heads"][0]["what_to_remember"],
            body
        );
        assert_eq!(
            exact["memories"][0]["current_heads"][0]["memory_ref"],
            json!(expected_reference)
        );
        writer.join().unwrap();
    });
    assert!(projection.shutdown(Duration::from_secs(10)));
}

#[test]
fn damaged_vector_file_keeps_lexical_source_control_and_other_binding_reads_available() {
    use crate::test_support::{model_assets, model_child_executable};
    use crate::{
        host::supervision::Supervisor,
        model::{run_model_component, ModelHandle},
        ports::{ComponentId, ComponentKind, ComponentState, HealthSink},
        semantic::{SemanticConfig, SemanticIndexer, SemanticSearch},
    };
    use agentlaw_flows::semantic::SemanticPort;
    use agentlaw_worker::supervisor::ModelRequestIds;
    use std::time::{Duration, Instant};
    let f = Fixture::new(65536);
    let other = Fixture::new(65536);
    let id = Fixture::id();
    f.publish(
        &id,
        "Corruption leaves this lexical marker readable.",
        false,
        json!([]),
        vec![],
    );
    other.publish(
        &Fixture::id(),
        "Another binding remains semantically searchable.",
        false,
        json!([]),
        vec![],
    );
    let asset_root = f.dir.path().join("assets");
    let assets = model_assets(&asset_root);
    let executable = model_child_executable();
    let config = SemanticConfig::new(f.dir.path().join("vectors"), "test".into());
    let health = HealthSink::default();
    let model = ModelHandle::new();
    let ids = ModelRequestIds::new();
    let indexer = SemanticIndexer::start(config.clone(), ids.clone(), health.clone()).unwrap();
    let wake = model.clone();
    indexer.set_model_waker(std::sync::Arc::new(move || wake.wake()));
    let supervisor = Supervisor::new(health.clone());
    std::fs::create_dir_all(f.dir.path().join("worker")).unwrap();
    let start_model = |supervisor: &Supervisor, indexer: &SemanticIndexer, ids: ModelRequestIds| {
        let worker = indexer.clone();
        let model = model.clone();
        let health = health.clone();
        let runtime = agentlaw_worker::RuntimeConfig {
            state_dir: f.dir.path().join("worker"),
            executable: executable.clone(),
            model: Some(assets.clone()),
        };
        supervisor
            .spawn(
                ComponentId {
                    binding: None,
                    kind: ComponentKind::Model,
                },
                move |stop| {
                    run_model_component(
                        runtime.clone(),
                        std::sync::Arc::new(worker.clone()),
                        ids.clone(),
                        health.clone(),
                        model.clone(),
                        stop,
                    )
                },
            )
            .unwrap();
    };
    start_model(&supervisor, &indexer, ids);
    indexer
        .open_binding(&f.identity.binding_id, f.store.owned_published_reader())
        .unwrap();
    let wait_complete = |indexer: &SemanticIndexer, binding: &str| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !indexer.progress(binding).is_some_and(|p| p.complete) {
            assert!(Instant::now() < deadline, "{:?}", indexer.progress(binding));
            std::thread::yield_now();
        }
    };
    wait_complete(&indexer, &f.identity.binding_id);
    let vector_file = std::fs::read_dir(&config.indices_root)
        .unwrap()
        .map(|e| e.unwrap().path().join("vector.sqlite"))
        .find(|p| p.exists())
        .unwrap();
    indexer
        .open_binding(
            &other.identity.binding_id,
            other.store.owned_published_reader(),
        )
        .unwrap();
    wait_complete(&indexer, &other.identity.binding_id);
    supervisor.shutdown();
    assert!(indexer.shutdown(Duration::from_secs(10)));
    drop(indexer);
    // Damage the existing vectors B-tree after all writers and readers have closed.
    {
        use std::io::{Seek, SeekFrom, Write};
        let db =
            Connection::open_with_flags(&vector_file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let page: u64 = db
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name='vectors'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let size: u64 = db.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        drop(db);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&vector_file)
            .unwrap();
        file.seek(SeekFrom::Start((page - 1) * size)).unwrap();
        file.write_all(&[0]).unwrap();
        file.sync_all().unwrap();
    }
    let image = || {
        ["", "-wal"]
            .into_iter()
            .map(|suffix| {
                let path = PathBuf::from(format!("{}{suffix}", vector_file.display()));
                let bytes = match std::fs::read(&path) {
                    Ok(b) => Some(b),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => panic!("{e}"),
                };
                (path, bytes)
            })
            .collect::<Vec<_>>()
    };
    let damaged = image();
    let ids = ModelRequestIds::new();
    let indexer = SemanticIndexer::start(config, ids.clone(), health.clone()).unwrap();
    let wake = model.clone();
    indexer.set_model_waker(std::sync::Arc::new(move || wake.wake()));
    model.set_warm(true);
    let supervisor = Supervisor::new(health.clone());
    start_model(&supervisor, &indexer, ids);
    indexer
        .open_binding(&f.identity.binding_id, f.store.owned_published_reader())
        .unwrap();
    indexer
        .open_binding(
            &other.identity.binding_id,
            other.store.owned_published_reader(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while model.snapshot().state != agentlaw_worker::WorkerState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let search = SemanticSearch::new(indexer.clone(), model.clone());
    let vector = search
        .embed_prefix(
            "The model still embeds queries.",
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(vector.vector.values.len(), 256);
    let exact = f
        .query_with_semantic(
            json!({"memory_ids":[id]}),
            &RequestControl::default(),
            &search,
        )
        .unwrap();
    assert_eq!(
        exact["memories"][0]["current_heads"][0]["what_to_remember"],
        "Corruption leaves this lexical marker readable."
    );
    let lexical = f
        .query_with_semantic(
            json!({"recall_for":"lexical marker"}),
            &RequestControl::default(),
            &search,
        )
        .unwrap();
    assert_eq!(lexical["semantic_complete"], false, "{lexical}");
    assert!(
        lexical["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["memory_id"] == id),
        "{lexical}"
    );
    assert!(f.store.owned_published_reader().read_snapshot().is_ok());
    assert!(ControlReader::open(&f.identity.local)
        .unwrap()
        .pending_notices(&f.identity.binding_id, None)
        .is_ok());
    wait_complete(&indexer, &other.identity.binding_id);
    let healthy = other
        .query_with_semantic(
            json!({"recall_for":"semantically searchable"}),
            &RequestControl::default(),
            &search,
        )
        .unwrap();
    assert_ne!(healthy["semantic_complete"], false, "{healthy}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snapshot = health.snapshot();
        if snapshot.iter().any(|(component, status)| {
            component.binding.as_deref() == Some(f.identity.binding_id.as_str())
                && component.kind == ComponentKind::Semantic
                && status.state == ComponentState::Degraded
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "semantic health did not degrade for binding {}: {snapshot:?}",
            f.identity.binding_id
        );
        std::thread::yield_now();
    }
    assert_eq!(
        image(),
        damaged,
        "reads changed the damaged vector DB or WAL"
    );
    supervisor.shutdown();
    assert!(indexer.shutdown(Duration::from_secs(10)));
    assert_eq!(image(), damaged);
}

impl Fixture {
    fn connect(&self, request: Value) -> Value {
        self.call(agentlaw_flows::Request::ConnectProjectMemory(
            serde_json::from_value(request).unwrap(),
        ))
        .unwrap()
    }
    fn call(&self, request: agentlaw_flows::Request) -> agentlaw_flows::Result<Value> {
        let identity = StoreIdentity {
            root: self.identity.root.clone(),
            local: self.identity.local.clone(),
            user_id: self.identity.user_id.clone(),
            machine_id: self.identity.machine_id.clone(),
            binding_id: self.identity.binding_id.clone(),
            history_limit: self.identity.history_limit,
            response_limit: 65536,
            config_file: self.identity.config_file.clone(),
        };
        let deps = crate::test_support::store_service::deps(self.dir.path());
        let indexer = deps.indexer.clone();
        let service =
            crate::store::service::StoreService::open(identity, deps, &RequestControl::default())
                .unwrap();
        let control = RequestControl::default();
        let value = match request {
            agentlaw_flows::Request::ConnectProjectMemory(request) => {
                service.command(crate::protocol::CommandRequest::Connect(request), &control)
            }
            agentlaw_flows::Request::RememberThis(request) => {
                service.command(crate::protocol::CommandRequest::Remember(request), &control)
            }
            _ => unreachable!("fixture only prepares writes and connections"),
        };
        service.close();
        assert!(indexer.shutdown(std::time::Duration::from_secs(10)));
        value
    }
}

#[test]
fn recall_source_errors_preserve_cause_across_acquisition_paths() {
    use agentlaw_flows::exact::ExactIndexWriter;
    use std::io::{Read, Seek, SeekFrom, Write};

    let marker = "Canonical body to check.";
    for file_backed in [false, true] {
        let f = Fixture::new(65536);
        let id = Fixture::id();
        let body = if file_backed {
            format!("{marker}\n{}", "x".repeat(65 * 1024 * 1024))
        } else {
            marker.to_owned()
        };
        f.publish(&id, &body, false, json!([]), vec![]);
        let request = if file_backed {
            json!({"memory_ids":[id]})
        } else {
            json!({"recall_for":marker})
        };
        let query = || f.query(request.clone(), &RequestControl::default());
        if file_backed {
            let source = f.store.owned_published_reader();
            let mut index = ExactIndexWriter::open(f.dir.path().join("exact-test.sqlite")).unwrap();
            index
                .synchronize(&source, &RequestControl::default())
                .unwrap();
            drop(index);
            let delivered = query().unwrap();
            assert_eq!(delivered["code"], "complete_content_in_file");
            assert_eq!(delivered["content_read"], false);
            assert!(delivered["artifact"]["bytes"].as_u64().unwrap() > body.len() as u64);
        }
        let current = f
            .identity
            .root
            .join("current/memory")
            .join(&id[..2])
            .join(format!("{id}.md"));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(current)
            .unwrap();
        let mut prefix = Vec::new();
        (&mut file).take(4096).read_to_end(&mut prefix).unwrap();
        let offset = prefix
            .windows(marker.len())
            .position(|bytes| bytes == marker.as_bytes())
            .unwrap();
        file.seek(SeekFrom::Start(offset as u64)).unwrap();
        file.write_all(b"X").unwrap();
        drop(file);

        let error = query().unwrap_err();
        assert_eq!(
            error.code, "source_corrupt",
            "FR-06.4: file-backed={file_backed}, {error:?}"
        );
        assert!(!error.retryable);
        assert!(error.details.is_none(), "{error:?}");
    }
}

#[test]
fn fr_06_5_exact_current_larger_than_owned_budget_is_delivered_as_whole_file() {
    use agentlaw_storage::codec;
    use sha2::{Digest, Sha256};
    use std::io::{BufRead, Cursor, Write};
    let f = Fixture::new(65536);
    let root = &f.identity.root;
    let id = uuid::Uuid::new_v4().to_string();
    let change = uuid::Uuid::new_v4().to_string();
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":change,"applicability":{"scope":"user"},"origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":true,"relations":[],"work_targets":[]}),
                body: String::new(),
            }],
        },
    };
    let frames = codec::read("current", &mut Cursor::new(unit.encode().unwrap())).unwrap();
    let state = &frames[0];
    let block = vec![b'x'; 65536];
    let repetitions = 1025u64;
    let bytes = block.len() as u64 * repetitions;
    let mut hash = Sha256::new();
    for _ in 0..repetitions {
        hash.update(&block)
    }
    let digest = format!("{:x}", hash.finalize());
    let parent = root.join("current/memory").join(&id[..2]);
    std::fs::create_dir_all(&parent).unwrap();
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(parent.join(format!("{id}.md"))).unwrap());
    writeln!(file, "<!-- agentlaw-file-v1 kind=current -->").unwrap();
    writeln!(
        file,
        "<!-- agentlaw-record-v1 type=state key={id} bytes={} sha256={} -->",
        state.payload.len(),
        codec::digest(&state.payload)
    )
    .unwrap();
    file.write_all(&state.payload).unwrap();
    file.write_all(b"\n<!-- /agentlaw-record-v1 -->\n").unwrap();
    writeln!(
        file,
        "<!-- agentlaw-record-v1 type=body key={change} bytes={bytes} sha256={digest} -->"
    )
    .unwrap();
    for _ in 0..repetitions {
        file.write_all(&block).unwrap()
    }
    file.write_all(b"\n<!-- /agentlaw-record-v1 -->\n<!-- /agentlaw-file-v1 records=2 -->\n")
        .unwrap();
    file.flush().unwrap();
    drop(file);
    let result = f
        .query(json!({"memory_ids":[id]}), &RequestControl::default())
        .unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    assert_eq!(result["content_read"], false);
    assert!(result["artifact"]["bytes"].as_u64().unwrap() > bytes);
    let length = std::fs::metadata(result["artifact"]["path"].as_str().unwrap())
        .unwrap()
        .len();
    assert_eq!(length, result["artifact"]["bytes"].as_u64().unwrap());
    let source = f.store.owned_published_reader();
    let rebuilt = agentlaw_flows::exact::ExactIndexWriter::rebuild_shadow_in(
        &f.dir.path().join("rebuilt-projection"),
        &source,
        &RequestControl::default(),
    )
    .unwrap();
    let staged = f.registry.stage(rebuilt).unwrap();
    f.registry.commit(staged).unwrap();
    let restored = f
        .query(
            json!({"recall_for":"unrelated ordinary work"}),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(restored["code"], "complete_content_in_file");
    assert!(restored["artifact"]["bytes"].as_u64().unwrap() > bytes);
    let file = std::fs::File::open(restored["artifact"]["path"].as_str().unwrap()).unwrap();
    let mut reader = std::io::BufReader::new(file);
    let prefix = String::from_utf8_lossy(reader.fill_buf().unwrap());
    assert!(
        prefix.contains("\"candidate_counts\""),
        "FR-06.5: contextual file lost candidate counts"
    );
    let content: Value = serde_json::from_reader(reader).unwrap();
    let item = &content["rules"][0]["items"][0];
    assert_eq!(item["memory_id"], id);
    let body = item["what_to_remember"].as_str().unwrap();
    assert_eq!(body.len() as u64, bytes);
    assert_eq!(format!("{:x}", Sha256::digest(body.as_bytes())), digest);
}

#[test]
fn streamed_exact_result_preserves_body_and_observed_version() {
    let f = Fixture::new(1);
    let body = "한글\r\n\"quote\"\\backslash\twithout final newline";
    let id = Fixture::id();
    f.publish(&id, body, false, json!([]), vec![]);
    let current = f.store.read_current(&id).unwrap();
    let reference = json!(current.references().unwrap()[0]);
    let envelope = f
        .query(json!({"memory_ids":[id]}), &RequestControl::default())
        .unwrap();
    assert_eq!(envelope["content_read"], false);
    let result: Value = serde_json::from_slice(
        &std::fs::read(envelope["artifact"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        result["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
    assert_eq!(
        result["memories"][0]["current_heads"][0]["memory_ref"],
        reference
    );
}

#[test]
fn request_reuses_body_and_title_pieces_after_installation_cache_is_cleared() {
    use crate::host::supervision::Supervisor;
    use crate::model::{run_model_component, ModelHandle};
    use crate::ports::{ComponentId, ComponentKind, HealthSink};
    use crate::semantic::{
        recent_vectors::RecentVectors, SemanticConfig, SemanticIndexer, SemanticSearch,
    };
    use crate::test_support::{
        model_assets, model_child_executable, model_transcript, FakeIndexWork,
    };
    use agentlaw_flows::semantic::{
        PiecePlan, PieceVectorReuse, QueryVector, SemanticCandidates, SemanticError, SemanticPort,
        SemanticQuery,
    };
    use agentlaw_worker::{
        supervisor::{IndexResult, IndexTask, IndexWorkPort, ModelRequestIds},
        RuntimeConfig, WorkerState,
    };
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const BODY: &str = "first preface\n# Title\n|second|third|";
    const SECTION: &str = "# Title\n|second|third|";

    #[derive(Default)]
    struct Observed {
        plan_inputs: Vec<Vec<String>>,
        first_record_calls: usize,
        embeds_at_clear: Option<Vec<Value>>,
        material_hits: Vec<String>,
        overlap_hits: Vec<String>,
    }
    struct Probe {
        adapter: SemanticSearch,
        install_cache: Arc<RecentVectors>,
        assets: PathBuf,
        observed: Mutex<Observed>,
    }
    impl SemanticPort for Probe {
        fn embed_prefix(
            &self,
            text: &str,
            control: &RequestControl,
        ) -> Result<agentlaw_flows::semantic::PrefixEmbedding, SemanticError> {
            self.adapter.embed_prefix(text, control)
        }
        fn plan_pieces(
            &self,
            records: &[&str],
            control: &RequestControl,
        ) -> Result<PiecePlan, SemanticError> {
            self.observed
                .lock()
                .unwrap()
                .plan_inputs
                .push(records.iter().map(|record| (*record).to_owned()).collect());
            self.adapter.plan_pieces(records, control)
        }
        fn embed_planned(
            &self,
            plan: &PiecePlan,
            record: usize,
            reuse: &PieceVectorReuse,
            control: &RequestControl,
        ) -> Result<Vec<QueryVector>, SemanticError> {
            {
                let mut observed = self.observed.lock().unwrap();
                if record == 0 {
                    observed.first_record_calls += 1;
                    if observed.first_record_calls == 2 {
                        observed.embeds_at_clear =
                            Some(model_transcript(&self.assets, "requests.jsonl"));
                        self.install_cache.clear();
                    }
                }
            }
            self.adapter.embed_planned(plan, record, reuse, control)
        }
        fn search_available(
            &self,
            query: SemanticQuery<'_>,
            control: &RequestControl,
        ) -> Result<SemanticCandidates, SemanticError> {
            let result = self.adapter.search_available(query, control)?;
            let mut observed = self.observed.lock().unwrap();
            let hits = result.hits.iter().map(|hit| hit.memory_id.clone());
            if query.scopes.len() > 1 {
                observed.material_hits.extend(hits);
            } else {
                observed.overlap_hits.extend(hits);
            }
            Ok(result)
        }
    }

    let f = Fixture::new(65_536);
    let cache = Arc::new(RecentVectors::default());
    let handle = ModelHandle::with_recent_vectors(cache.clone());
    let assets = f.dir.path().join("assets");
    let health = HealthSink::default();
    let supervisor = Supervisor::new(health.clone());
    let worker = handle.clone();
    handle.set_warm(true);
    let config = RuntimeConfig {
        model: Some(model_assets(&assets)),
        executable: model_child_executable(),
        state_dir: f.dir.path().join("worker"),
    };
    std::fs::create_dir(&config.state_dir).unwrap();
    supervisor
        .spawn(
            ComponentId {
                binding: None,
                kind: ComponentKind::Model,
            },
            move |stop| {
                run_model_component(
                    config.clone(),
                    Arc::new(FakeIndexWork::new()),
                    ModelRequestIds::new(),
                    health.clone(),
                    worker.clone(),
                    stop,
                )
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while handle.snapshot().state != WorkerState::Ready {
        assert!(
            Instant::now() < deadline,
            "fixture model did not become ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let user_memory = Fixture::id();
    let machine_memory = Fixture::id();
    for (id, applicability) in [
        (&user_memory, json!({"scope":"user"})),
        (
            &machine_memory,
            json!({"scope":"machine", "machine_id":f.identity.machine_id}),
        ),
    ] {
        f.store.publish(&Fixture::id(), vec![Mutation {
            unit:CurrentUnit { entity_id:id.clone(),entity_type:"memory".into(),state:UnitState::Live { heads:vec![Head {
                metadata:json!({
                    "change_id":Fixture::id(),"applicability":applicability,
                    "origin":{"machine_id":f.identity.machine_id,"user_id":"user"},"recorded_at_ms":0,
                    "is_rule":false,"relations":[],"work_targets":[]
                }),
                body:"stored match".into(),
            }] } },
            expected_versions:vec![], evidence:"scoped fixture".into(),
        }]).unwrap();
    }
    let source = f.store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let mut config = SemanticConfig::new(f.dir.path().join("indices"), "config".into());
    config.recent_vectors = cache.clone();
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding(&f.identity.binding_id, source.clone())
        .unwrap();
    indexer.model_ready("fixture-model");
    let mut vector = vec![0.0; 256];
    vector[0] = 1.0;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !indexer
        .progress(&f.identity.binding_id)
        .is_some_and(|progress| {
            progress.complete
                && progress
                    .ack
                    .is_some_and(|ack| ack.sequence == position.sequence as i64)
        })
    {
        if let Some(work) = indexer.next_work("fixture-model") {
            let result = match work.task {
                IndexTask::Split(text) => IndexResult::Pieces(vec![(0, text.len() as u32)]),
                IndexTask::Embed(_) => IndexResult::Vector(vector.clone()),
            };
            indexer.finish(work.id, Ok(result));
        }
        assert!(
            Instant::now() < deadline,
            "scoped fixture did not finish indexing"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let probe = Probe {
        adapter: SemanticSearch::new(indexer.clone(), handle),
        install_cache: cache,
        assets: assets.clone(),
        observed: Mutex::default(),
    };
    let control_reader = ControlReader::open(&f.identity.local).unwrap();
    let history_cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &control_reader,
        semantic: &probe,
        history_semantic: &probe.adapter,
        history_cache: &history_cache,
        request: &control,
    };
    let guard = source.read_snapshot().unwrap();
    let mut exact =
        ExactSnapshot::build(f.registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
    drop(guard);
    let request=serde_json::from_value(json!({
        "memories":[{"operation":"create","what_to_remember":BODY,"applies_to":["user"],"evidence":"new titled proposal"}]
    })).unwrap();
    let prepared = agentlaw_flows::prepare::prepare(&ctx, &mut exact, request);
    let observations = probe.observed.lock().unwrap();
    let splits = model_transcript(&assets, "splits.jsonl");
    let embeds = model_transcript(&assets, "requests.jsonl");
    assert!(indexer.shutdown(Duration::from_secs(5)));
    supervisor.shutdown();

    assert_eq!(
        observations.plan_inputs,
        [vec![BODY.to_owned(), SECTION.to_owned()]]
    );
    assert!(matches!(prepared.unwrap(), Prepared::Remember(_)));
    let counts = |transcript: &[Value]| {
        let mut counts = std::collections::BTreeMap::new();
        for entry in transcript {
            *counts
                .entry(entry.as_str().unwrap().to_owned())
                .or_insert(0) += 1;
        }
        counts
    };
    assert_eq!(
        counts(&splits),
        [(BODY.to_owned(), 1), (SECTION.to_owned(), 1)].into()
    );
    let expected: std::collections::BTreeMap<_, _> = [
        "first preface\n# Title\n|",
        "second|",
        "third|",
        "# Title\n|",
    ]
    .map(|piece| (piece.to_owned(), 1))
    .into();
    assert_eq!(counts(&embeds), expected);
    assert_eq!(
        observations
            .embeds_at_clear
            .as_ref()
            .map(|entries| counts(entries)),
        Some(expected)
    );
    assert!(observations.material_hits.contains(&user_memory));
    assert!(observations.material_hits.contains(&machine_memory));
    assert!(!observations.overlap_hits.is_empty());
    assert!(observations
        .overlap_hits
        .iter()
        .all(|id| id == &user_memory));
}

#[test]
fn fr_01_5_prefix_selection_counts_identities_by_kind_and_returns_all_heads() {
    // ID-06
    let f = Fixture::new(65536);
    let memory = "abcdef12-a000-4000-8000-000000000001";
    let procedure = "abcdef12-b000-4000-8000-000000000002";
    let origin = json!({"machine_id":f.identity.machine_id,"user_id":"user"});
    let memory_unit = CurrentUnit {
        entity_id: memory.into(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: ["Review the first deployment.", "Review the second deployment."]
                .map(|body| Head {
                    metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"user"},
                        "origin":origin,"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: body.into(),
                }).into(),
        },
    };
    let instructions = "Check the deployment logs in order.";
    let procedure_unit = CurrentUnit {
        entity_id: procedure.into(),
        entity_type: "learned_procedure".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":Fixture::id(),"applicability":{"scope":"user"},
                    "origin":origin,"recorded_at_ms":0,"name":"Deployment review","use_when":"deployment",
                    "evidence_memory_ids":[memory]}),
                body: instructions.into(),
            }],
        },
    };
    f.store
        .publish(
            &Fixture::id(),
            [memory_unit, procedure_unit]
                .into_iter()
                .map(|unit| Mutation {
                    unit,
                    expected_versions: vec![],
                    evidence: "observed concurrent publication".into(),
                })
                .collect(),
        )
        .unwrap();

    let query = |memory_id, procedure_id| {
        f.query(
            json!({"memory_ids":[memory_id],"procedure_ids":[procedure_id]}),
            &RequestControl::default(),
        )
        .unwrap()
    };
    let canonical = query(memory, procedure);
    let prefix = query("abcdef12", "abcdef12");
    assert_eq!(prefix["memories"], canonical["memories"], "FR-01.5 ID-06");
    assert_eq!(
        prefix["learned_procedures"],
        canonical["learned_procedures"]
    );
    let heads = prefix["memories"][0]["current_heads"].as_array().unwrap();
    assert_eq!(heads.len(), 2);
    for body in [
        "Review the first deployment.",
        "Review the second deployment.",
    ] {
        assert!(heads
            .iter()
            .any(|head| head["memory_ref"]["memory_id"] == memory
                && head["what_to_remember"] == body));
    }
    let selected = &prefix["learned_procedures"][0];
    assert_eq!(selected["procedure_ref"]["procedure_id"], procedure);
    assert_eq!(selected["instructions"], instructions);
}

#[test]
fn fr_01_5_write_rechecks_prefix_uniqueness_after_preparation() {
    // ID-11
    use agentlaw_flows::{apply, control::ControlWriter, store::LaneCtx};
    let f = Fixture::new(65536);
    let projection = start_read_projection(&f);
    let coordinator = ReadCoordinator::new(projection.clone(), DeltaBudget::default());
    let parent = "abcdef12-a000-4000-8000-000000000001";
    let concurrent = "abcdef12-b000-4000-8000-000000000002";
    f.publish(parent, "Original deployment note", false, json!([]), vec![]);
    let before = f.store.read_current(parent).unwrap();
    let version = &before.references().unwrap()[0].observed_version;
    let source = f.store.owned_published_reader();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &f.semantic,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let mut writer = ControlWriter::open(&f.identity.local).unwrap();
    let mut lane = LaneCtx {
        identity: &f.identity,
        source: &f.store,
        control: &mut writer,
        request: &control,
    };
    let original = json!({
        "operation":"evolve","parent_refs":[{"memory_id":parent,"observed_version":"previous-version"}],
        "what_to_remember":"Revised deployment note","evidence":"New deployment evidence"
    });
    let prepare = |input| {
        coordinator
            .run_prepare(&ctx, serde_json::from_value(input).unwrap())
            .unwrap()
    };
    let apply::Applied::Response(pending) =
        apply::apply(&mut lane, prepare(json!({"memories":[original]}))).unwrap()
    else {
        panic!("FR-01.5 ID-11: pending response required");
    };
    assert_eq!(pending["status"], "resolution_required", "{pending}");
    let reference = serde_json::from_value(pending["pending_batch_ref"].clone()).unwrap();
    let inspect = || {
        ControlReader::open(&f.identity.local)
            .unwrap()
            .pending_inspect(&f.identity.binding_id, &reference)
            .unwrap()
    };
    let pending_before = inspect();
    assert_eq!(pending_before["proposals"].as_array().unwrap().len(), 1);
    let mut replacement = original;
    replacement["parent_refs"] = json!([{"memory_id":"abcdef12","observed_version":version}]);
    replacement["proposal_id"] = pending["proposal_map"][0]["proposal_id"].clone();
    let submitted = serde_json::from_value(
        json!({"pending_action":"continue","pending_batch_ref":reference,"memories":[replacement]}),
    )
    .unwrap();
    let continuation_reader = ControlReader::open(&f.identity.local).unwrap();
    let continuation = ReadCtx {
        control: &continuation_reader,
        ..ctx
    };
    let plan = coordinator
        .run_prepare(&continuation, submitted)
        .unwrap_or_else(|error| panic!("FR-01.5 ID-11: {error:?}"));
    f.publish(
        concurrent,
        "Concurrent deployment note",
        false,
        json!([]),
        vec![],
    );
    let generation = f.store.generation().unwrap();
    let result = apply::apply(&mut lane, plan);
    assert_eq!(f.store.generation().unwrap(), generation);
    assert_eq!(
        f.store.read_current(parent).unwrap().references().unwrap()[0].observed_version,
        *version
    );
    assert_eq!(
        f.store.read_current(concurrent).unwrap().heads()[0].body,
        "Concurrent deployment note"
    );
    assert_eq!(inspect(), pending_before);
    let database = Connection::open(f.identity.local.join("control.sqlite")).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM prefix_executions", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert!(projection.shutdown(std::time::Duration::from_secs(10)));
    let error = result
        .err()
        .expect("FR-01.5 ID-11: stale prefix must be rejected");
    assert_eq!(error.code, "ambiguous", "{error:?}");
    assert!(error
        .details
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .any(
            |entry| entry["path"] == "/memories/0/parent_refs/0/memory_id"
                && entry["provided_value"] == "abcdef12"
        ));
}

#[cfg(any(unix, windows))]
struct CanonicalReadDenial<'a> {
    path: &'a std::path::Path,
    #[cfg(unix)]
    permissions: std::fs::Permissions,
    #[cfg(windows)]
    principal: String,
}

#[cfg(any(unix, windows))]
impl<'a> CanonicalReadDenial<'a> {
    fn apply(path: &'a std::path::Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::metadata(path).unwrap().permissions();
            let mode = if path.is_dir() { 0o300 } else { 0 };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
            Self { path, permissions }
        }
        #[cfg(windows)]
        {
            use std::process::Command;
            let user = Command::new("whoami")
                .args(["/user", "/fo", "csv", "/nh"])
                .output()
                .unwrap();
            assert!(user.status.success(), "{user:?}");
            let identity = String::from_utf8_lossy(&user.stdout);
            let sid = identity
                .trim()
                .rsplit(',')
                .next()
                .unwrap()
                .trim_matches('"');
            assert!(sid.starts_with("S-1-"));
            let principal = format!("*{sid}");
            let result = Command::new("icacls")
                .arg(path)
                .args(["/deny", &format!("{principal}:R")])
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
            Self { path, principal }
        }
    }
}

#[cfg(any(unix, windows))]
impl Drop for CanonicalReadDenial<'_> {
    fn drop(&mut self) {
        #[cfg(unix)]
        std::fs::set_permissions(self.path, self.permissions.clone()).unwrap();
        #[cfg(windows)]
        {
            let restored = std::process::Command::new("icacls")
                .arg(self.path)
                .args(["/remove:d", &self.principal])
                .output()
                .unwrap();
            assert!(restored.status.success(), "{restored:?}");
        }
    }
}

#[cfg(any(unix, windows))]
#[test]
fn fr_01_5_prefix_lookup_reports_unreadable_identity_inventory() {
    // ID-15
    let f = Fixture::new(65536);
    let id = "abcdef12-a000-4000-8000-000000000001";
    let body = "Deployment notes";
    f.publish(id, body, false, json!([]), vec![]);
    let directory = f.identity.root.join("current/memory").join(&id[..2]);
    let path = directory.join(format!("{id}.md"));
    let original = std::fs::read(&path).unwrap();
    let query = || f.query(json!({"memory_ids":[&id[..8]]}), &RequestControl::default());
    let failure = {
        let _denial = CanonicalReadDenial::apply(&directory);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::read_dir(&directory).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        query()
    };
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let error = failure.unwrap_err();
    assert_eq!(
        error.code, "source_operation_failed",
        "FR-01.5 ID-15: {error:?}"
    );
    assert!(!error.retryable);
    let action = error.next_action.unwrap().to_lowercase();
    assert!(action.contains("access") || action.contains("permission"));
    let restored =
        query().unwrap_or_else(|error| panic!("FR-01.5 ID-15: restored lookup: {error:?}"));
    assert_eq!(
        restored["memories"][0]["current_heads"][0]["memory_ref"]["memory_id"],
        id
    );
    assert_eq!(
        restored["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
}

#[cfg(any(unix, windows))]
#[test]
fn fr_01_6_prepare_and_procedure_search_report_unreadable_source_with_operation_guidance() {
    // ER-14
    use crate::protocol::InventoryFilter;
    let f = Fixture::new(65536);
    let projection = start_read_projection(&f);
    let coordinator = ReadCoordinator::new(projection.clone(), DeltaBudget::default());
    let id = Fixture::id();
    f.publish(&id, "Canonical deployment record", false, json!([]), vec![]);
    let path = f
        .identity
        .root
        .join("current/memory")
        .join(&id[..2])
        .join(format!("{id}.md"));
    let original = std::fs::read(&path).unwrap();
    let generation = f.store.generation().unwrap();
    let source = f.store.owned_published_reader();
    let reader = ControlReader::open(&f.identity.local).unwrap();
    let pending = || {
        reader
            .pending_notices(&f.identity.binding_id, None)
            .unwrap()
    };
    let prior_pending = pending();
    let cache = HistoryCacheGuards::new();
    let control = RequestControl::default();
    let ctx = ReadCtx {
        identity: &f.identity,
        source: &source,
        control: &reader,
        semantic: &f.semantic,
        history_semantic: &f.semantic,
        history_cache: &cache,
        request: &control,
    };
    let prepare = || {
        coordinator.run_prepare(&ctx, serde_json::from_value(json!({
        "memories":[{"operation":"create","what_to_remember":"Additional deploy instructions",
        "evidence":"Observed","applies_to":["user"],"related_memory_ids":[id]}]
    })).unwrap())
    };
    let search = || {
        coordinator.search_procedures(
            &ctx,
            "deployment",
            &InventoryFilter {
                scope: None,
                project: None,
                machine: None,
                table: false,
            },
            1,
        )
    };
    let (preparation, procedures) = {
        let _denial = CanonicalReadDenial::apply(&path);
        assert_eq!(
            std::fs::File::open(&path).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        (prepare(), search())
    };
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(f.store.generation().unwrap(), generation);
    assert_eq!(pending(), prior_pending);
    assert!(prepare().is_ok(), "FR-01.6 ER-14: restored preparation");
    assert!(search().is_ok(), "FR-01.6 ER-14: restored procedure search");
    assert!(projection.shutdown(std::time::Duration::from_secs(10)));
    for (error, terms) in [
        (preparation.err(), ["prepar", "remember_this"]),
        (procedures.err(), ["search", "procedure"]),
    ] {
        let error = error.expect("FR-01.6 ER-14: operation must fail under denial");
        assert_eq!(error.code, "source_operation_failed", "{error:?}");
        assert!(!error.retryable);
        let action = error.next_action.unwrap().to_lowercase();
        assert!(
            action.contains("permission") || action.contains("access"),
            "{action}"
        );
        assert!(terms.iter().any(|term| action.contains(term)), "{action}");
    }
}

#[test]
fn published_redirect_counts_as_a_distinct_prefix_candidate() {
    // ID-19
    let f = Fixture::new(65536);
    let first = "abcdef12-a000-4000-8000-000000000001";
    let second = "abcdef12-b000-4000-8000-000000000002";
    f.publish(
        first,
        "Review the deployment checklist.",
        false,
        json!([]),
        vec![],
    );
    f.publish(
        second,
        "Verify checksums against the manifest.",
        false,
        json!([]),
        vec![],
    );
    let reference = |id| {
        let unit = f.store.read_current(id).unwrap();
        json!({"memory_id":id,"observed_version":unit.references().unwrap()[0].observed_version})
    };
    let request = json!({"action":"remember_this","remember_this":{"memories":[{
        "operation":"consolidate","consolidation_refs":[reference(first),reference(second)],
        "what_to_remember":"Verify deployment and manifest together.","evidence":"Both records reviewed"
    }]}});
    let merged = f
        .call(agentlaw_flows::parse_request(&request.to_string()).unwrap())
        .unwrap();
    assert_eq!(merged["status"], "remembered", "{merged}");
    let resolved = f.store.resolve_current(second).unwrap();
    assert_eq!(
        resolved.current.entity_id,
        merged["results"][0]["memory_ref"]["memory_id"]
    );
    assert!(!resolved.redirect_path.is_empty());
    let query = |id| f.query(json!({"memory_ids":[id]}), &RequestControl::default());
    let error = query("abcdef12").unwrap_err();
    assert_eq!(error.code, "ambiguous", "FR-01.5 ID-19: {error:?}");
    let exact = query(second).unwrap();
    let prefix = query("abcdef12b").unwrap();
    for field in ["memories", "redirects"] {
        assert_eq!(prefix[field], exact[field], "FR-01.5 ID-19: {field}");
    }
    assert_eq!(
        prefix["memories"][0]["current_heads"][0]["what_to_remember"],
        "Verify deployment and manifest together."
    );
}
