use super::*;
use crate::control::{ControlReader, ControlWriter};
use crate::exact::{DeltaBudget, ExactIndexWriter, ExactSnapshot, ProjectionGenerations};
use crate::history_cache::HistoryCacheGuards;
use crate::semantic::*;
use rusqlite::Connection;
use std::sync::Arc;

enum Ports {
    Ready,
    Incomplete(&'static str),
}
impl SemanticPort for Ports {
    fn embed_query(
        &self,
        _: &str,
        _: &RequestControl,
    ) -> std::result::Result<QueryVector, SemanticError> {
        if let Self::Incomplete(cause) = self {
            return Err(SemanticError::Unavailable((*cause).into()));
        }
        Ok(QueryVector {
            model: "test".into(),
            values: Arc::from([1.0]),
        })
    }
    fn search_available(
        &self,
        q: SemanticQuery<'_>,
        _: &RequestControl,
    ) -> std::result::Result<SemanticCandidates, SemanticError> {
        Ok(SemanticCandidates {
            hits: vec![],
            coverage: SemanticCoverage::Partial {
                available: q.source.clone(),
                requested: q.source.clone(),
            },
        })
    }
}
impl HistorySemanticPort for Ports {
    fn offer(&self, _: HistoryIngestHint) -> OfferResult {
        OfferResult::Accepted
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
        Ok(SemanticCandidates {
            hits: vec![],
            coverage: SemanticCoverage::Unavailable("test".into()),
        })
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    // The store owner keeps the journal writer open, so ordinary reads never create its WAL.
    _journal: agentlaw_storage::JournalWriter,
    store: agentlaw_storage::Store,
    writer: ControlWriter,
    identity: StoreIdentity,
    registry: ProjectionGenerations,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        let store = agentlaw_storage::Store::open(&root, local.join("canonical")).unwrap();
        let journal = store.hold_journal_writer().unwrap();
        let identity = StoreIdentity {
            root: std::fs::canonicalize(&root).unwrap(),
            local: local.clone(),
            user_id: uuid::Uuid::new_v4().to_string(),
            machine_id: uuid::Uuid::new_v4().to_string(),
            binding_id: uuid::Uuid::new_v4().to_string(),
            history_limit: 8192,
            response_limit: 65536,
            config_file: Some(std::path::PathBuf::from("test-config.json")),
        };
        let mut writer = ControlWriter::open(&local).unwrap();
        writer.bind_identity(&identity).unwrap();
        writer.finish_binding_initialization(&identity).unwrap();
        let source = store.owned_published_reader();
        let position = source.read_snapshot().unwrap().position().clone();
        let directory = local.join("projection");
        let shadow =
            ExactIndexWriter::rebuild_shadow_in(&directory, &source, &RequestControl::default())
                .unwrap();
        let registry = ProjectionGenerations::open_for_recovery(
            "bootstrap",
            &position.epoch,
            directory.join("exact-bootstrap.sqlite"),
            directory.join("lexical-bootstrap.sqlite"),
        )
        .unwrap();
        let staged = registry.stage(shadow).unwrap();
        registry.commit(staged).unwrap();
        Self {
            _dir: dir,
            _journal: journal,
            store,
            writer,
            identity,
            registry,
        }
    }
    fn run(&mut self, action: &str, fields: Value) -> Value {
        let plan = match action {
            "remember_this" => self.prepare(fields),
            "connect_project_memory" => self.connect(fields),
            _ => panic!("unexpected request"),
        }
        .unwrap();
        match self.apply(plan).unwrap() {
            crate::apply::Applied::Response(value) => value,
            crate::apply::Applied::Connected { response, .. } => crate::to_value(response).unwrap(),
        }
    }
    fn prepare(&self, request: Value) -> Result<Prepared> {
        self.prepare_with_semantic(request, &Ports::Ready)
    }
    fn prepare_with_semantic(
        &self,
        request: Value,
        semantic: &dyn SemanticPort,
    ) -> Result<Prepared> {
        let source = self.store.owned_published_reader();
        let reader = ControlReader::open(&self.identity.local)?;
        let cache = HistoryCacheGuards::new();
        let control = RequestControl::default();
        let ctx = ReadCtx {
            identity: &self.identity,
            source: &source,
            control: &reader,
            semantic,
            history_semantic: &Ports::Ready,
            history_cache: &cache,
            request: &control,
        };
        let guard = source.read_snapshot().unwrap();
        let mut exact = ExactSnapshot::build(self.registry.pin()?, &guard, DeltaBudget::default())?;
        drop(guard);
        super::prepare(&ctx, &mut exact, serde_json::from_value(request).unwrap())
    }
    fn connect(&self, request: Value) -> Result<Prepared> {
        let source = self.store.owned_published_reader();
        let reader = ControlReader::open(&self.identity.local)?;
        let cache = HistoryCacheGuards::new();
        let control = RequestControl::default();
        let ctx = ReadCtx {
            identity: &self.identity,
            source: &source,
            control: &reader,
            semantic: &Ports::Ready,
            history_semantic: &Ports::Ready,
            history_cache: &cache,
            request: &control,
        };
        super::prepare_connect(&ctx, serde_json::from_value(request).unwrap())
    }
    fn apply(&mut self, plan: Prepared) -> Result<crate::apply::Applied> {
        let request = RequestControl::default();
        crate::apply::apply(
            &mut LaneCtx {
                identity: &self.identity,
                source: &self.store,
                control: &mut self.writer,
                request: &request,
            },
            plan,
        )
    }
}
fn create(body: &str) -> Value {
    json!({"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed by test","applies_to":["user"]}]})
}
fn response(applied: crate::apply::Applied) -> Value {
    match applied {
        crate::apply::Applied::Response(value) => value,
        _ => panic!("expected response"),
    }
}

fn durable_files(
    root: &std::path::Path,
    files: &mut Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)>,
) {
    if !root.exists() {
        return;
    }
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            durable_files(&path, files);
        } else if !path.to_string_lossy().ends_with("-shm") {
            let metadata = std::fs::metadata(&path).unwrap();
            files.push((
                path.clone(),
                std::fs::read(&path).unwrap(),
                metadata.modified().unwrap(),
            ));
        }
    }
}
fn durable_state(
    f: &Fixture,
) -> (
    Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)>,
    Vec<i64>,
) {
    let mut files = Vec::new();
    durable_files(&f.identity.root, &mut files);
    durable_files(&f.identity.local.join("canonical"), &mut files);
    for name in ["control.sqlite", "control.sqlite-wal"] {
        let path = f.identity.local.join(name);
        if path.exists() {
            let metadata = std::fs::metadata(&path).unwrap();
            files.push((
                path.clone(),
                std::fs::read(path).unwrap(),
                metadata.modified().unwrap(),
            ));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let db = Connection::open_with_flags(
        f.identity.local.join("control.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let counts = [
        "pending",
        "authoring",
        "authoring_executions",
        "associations",
        "project_creations",
    ]
    .iter()
    .map(|table| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    })
    .collect();
    (files, counts)
}

#[test]
fn new_remember_prepares_without_writing_and_preserves_public_result() {
    let mut f = Fixture::new();
    let position = f
        .store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let before = durable_state(&f);
    let plan = f.prepare(create("A unique observation")).unwrap();
    assert!(matches!(plan, Prepared::Remember(_)));
    assert_eq!(before, durable_state(&f));
    assert_eq!(
        position,
        *f.store
            .owned_published_reader()
            .read_snapshot()
            .unwrap()
            .position()
    );
    let result = response(f.apply(plan).unwrap());
    assert_eq!(result["status"], "remembered");
    assert!(result["results"][0]["memory_ref"]["memory_id"].is_string());
}

#[test]
fn pending_continue_discard_and_replay_prepare_matching_plans() {
    let mut f = Fixture::new();
    f.run(
        "remember_this",
        create("Verified shell command convention."),
    );
    let pending = f.run(
        "remember_this",
        create("Verified shell command convention."),
    );
    assert_eq!(pending["status"], "resolution_required");
    let reference = pending["pending_batch_ref"].clone();
    let before = durable_state(&f);
    let next = f
        .prepare(json!({"pending_action":"continue","pending_batch_ref":reference}))
        .unwrap();
    assert!(matches!(next, Prepared::Remember(_)));
    let discarded = f.prepare(json!({"pending_action":"discard","pending_batch_ref":reference,"user_confirmed":true,"discard_reason":"no longer needed"})).unwrap();
    assert!(matches!(discarded, Prepared::Discard(_)));
    assert_eq!(before, durable_state(&f));
    let result = response(f.apply(discarded).unwrap());
    assert_eq!(result["status"], "discarded");
    assert_eq!(result["pending_batch_ref"], reference);
    let stale = f.apply(next);
    assert!(matches!(stale, Err(error) if error.code == "stale_pending_ref"));
}

#[test]
fn discard_without_confirmation_or_reason_preserves_invalid_input_contract() {
    let mut f = Fixture::new();
    f.run("remember_this", create("Confirmed discard example"));
    let pending = f.run("remember_this", create("Confirmed discard example"));
    let reference = pending["pending_batch_ref"].clone();
    let before = durable_state(&f);
    for request in [
        json!({"pending_action":"discard","pending_batch_ref":reference,"discard_reason":"obsolete"}),
        json!({"pending_action":"discard","pending_batch_ref":reference,"user_confirmed":true}),
    ] {
        assert!(matches!(f.prepare(request), Err(error) if error.code == "invalid_input"));
    }
    assert_eq!(before, durable_state(&f));
}

#[test]
fn published_pending_ref_replays_recorded_result() {
    let mut f = Fixture::new();
    f.run("remember_this", create("Replay duplicate"));
    let pending = f.run("remember_this", create("Replay duplicate"));
    let result = f.run("remember_this", json!({"pending_action":"continue","pending_batch_ref":pending["pending_batch_ref"],"resolution_ref":pending["resolution_ref"]}));
    let reference = pending["pending_batch_ref"].clone();
    let replay = f
        .prepare(json!({"pending_action":"continue","pending_batch_ref":reference}))
        .unwrap();
    assert!(matches!(replay, Prepared::Replay(ref value) if *value == result));
}

#[test]
fn procedure_prepare_and_submission_produce_plans_and_public_results() {
    let mut f = Fixture::new();
    let evidence = f.run(
        "remember_this",
        create("Use a fixture to verify migration."),
    )["results"][0]["memory_ref"]["memory_id"]
        .clone();
    let before = durable_state(&f);
    let plan = f
        .prepare(json!({"procedure":{"operation":"create","evidence_memory_ids":[evidence]}}))
        .unwrap();
    assert!(matches!(plan, Prepared::Authoring(_)));
    assert_eq!(before, durable_state(&f));
    let context = response(f.apply(plan).unwrap());
    assert_eq!(context["status"], "authoring_required");
    let before = durable_state(&f);
    let plan = f.prepare(json!({"authoring_ref":context["authoring_ref"],"procedure":{"operation":"create","evidence_memory_ids":[evidence],"applies_to":["user"],"name":"Verify a migration","use_when":"When changing a schema.","instructions":"Run fixture checks before applying changes.","evidence":"The recorded observation supports this procedure."}})).unwrap();
    assert!(matches!(plan, Prepared::Procedure(_)));
    assert_eq!(before, durable_state(&f));
    let result = response(f.apply(plan).unwrap());
    assert_eq!(result["status"], "remembered");
    assert!(result["procedure_ref"]["procedure_id"].is_string());
}

#[test]
fn connect_create_select_and_discover_prepare_matching_plans() {
    let mut f = Fixture::new();
    let before = durable_state(&f);
    let create = f
        .connect(
            json!({"intent":"create","project_path":"C:/work/example","project_name":"Example"}),
        )
        .unwrap();
    assert!(matches!(
        create,
        Prepared::Connect(ConnectPlan {
            kind: ConnectPlanKind::Create { .. },
            ..
        })
    ));
    assert_eq!(before, durable_state(&f));
    let id = match f.apply(create).unwrap() {
        crate::apply::Applied::Connected { response, .. } => {
            response.project_connection.unwrap().project_id
        }
        _ => panic!("expected connection"),
    };
    let before = durable_state(&f);
    let select = f
        .connect(json!({"intent":"connect","project_path":"C:/work/other","project_id":id}))
        .unwrap();
    assert!(matches!(
        select,
        Prepared::Connect(ConnectPlan {
            kind: ConnectPlanKind::Select { .. },
            ..
        })
    ));
    let discovered = f
        .connect(
            json!({"intent":"discover","project_path":"C:/work/example","restore_context":true,"recall_for":"Restore the connected project context"}),
        )
        .unwrap();
    assert!(matches!(
        discovered,
        Prepared::Connect(ConnectPlan {
            kind: ConnectPlanKind::Discovered(_),
            ..
        })
    ));
    assert_eq!(before, durable_state(&f));
}

#[test]
fn concurrent_pending_continue_invalidates_prepared_discard() {
    let mut f = Fixture::new();
    f.run("remember_this", create("Pending version example"));
    let pending = f.run("remember_this", create("Pending version example"));
    let reference = pending["pending_batch_ref"].clone();
    let stale = f.prepare(json!({"pending_action":"discard","pending_batch_ref":reference,"user_confirmed":true,"discard_reason":"obsolete"})).unwrap();
    let updated = f.run(
        "remember_this",
        json!({"pending_action":"continue","pending_batch_ref":reference}),
    );
    assert_ne!(
        updated["pending_batch_ref"]["observed_version"],
        reference["observed_version"]
    );
    let before = durable_state(&f);
    assert!(matches!(f.apply(stale), Err(error) if error.code == "stale_pending_ref"));
    assert_eq!(before, durable_state(&f));
}

#[test]
fn changed_association_reuses_concurrent_project_without_another_catalog_entry() {
    let mut f = Fixture::new();
    let request =
        json!({"intent":"create","project_path":"C:/work/example","project_name":"Example"});
    let stale = f.connect(request.clone()).unwrap();
    let concurrent = f.run("connect_project_memory", request);
    assert!(concurrent["project_connection"]["project_id"].is_string());
    let id = concurrent["project_connection"]["project_id"]
        .as_str()
        .unwrap();
    let source = f.store.owned_published_reader();
    let before = source.read_snapshot().unwrap().position().clone();
    let response = match f.apply(stale).unwrap() {
        crate::apply::Applied::Connected { response, .. } => response,
        _ => panic!("expected connection"),
    };
    assert_eq!(response.project_connection.unwrap().project_id, id);
    let snapshot = source.read_snapshot().unwrap();
    assert_eq!(*snapshot.position(), before);
    let catalog = snapshot.list_catalog().unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0]["project_id"], id);
}

#[test]
fn changed_memory_head_requires_review_without_publishing_stale_change() {
    let mut f = Fixture::new();
    let first = f.run("remember_this", create("Original head"));
    let reference = first["results"][0]["memory_ref"].clone();
    let stale = f.prepare(json!({"memories":[{"operation":"evolve","parent_refs":[reference],"what_to_remember":"Stale correction","evidence":"Correction"}]})).unwrap();
    let concurrent = f.run("remember_this", json!({"memories":[{"operation":"evolve","parent_refs":[reference],"what_to_remember":"Concurrent correction","evidence":"Correction"}]}));
    assert_eq!(concurrent["status"], "remembered");
    let generation = f.store.generation().unwrap();
    let result = response(f.apply(stale).unwrap());
    assert_eq!(result["status"], "resolution_required");
    assert_eq!(result["issues"][0]["code"], "source_changed");
    assert_eq!(f.store.generation().unwrap(), generation);
}

#[test]
fn changed_generation_returns_procedure_for_review_without_publication() {
    let mut f = Fixture::new();
    let evidence = f.run(
        "remember_this",
        create("Use a fixture to verify migration."),
    )["results"][0]["memory_ref"]["memory_id"]
        .clone();
    let authoring = f.run(
        "remember_this",
        json!({"procedure":{"operation":"create","evidence_memory_ids":[evidence]}}),
    );
    let stale = f.prepare(json!({"authoring_ref":authoring["authoring_ref"],"procedure":{"operation":"create","evidence_memory_ids":[evidence],"applies_to":["user"],"name":"Verify a migration","use_when":"When changing a schema.","instructions":"Run fixture checks before applying changes.","evidence":"The recorded observation supports this procedure."}})).unwrap();
    let concurrent = f.run("remember_this", create("Unrelated change to generation"));
    assert_eq!(concurrent["status"], "remembered");
    let generation = f.store.generation().unwrap();
    let result = response(f.apply(stale).unwrap());
    assert_eq!(result["status"], "authoring_required");
    assert_eq!(result["code"], "source_changed");
    assert_eq!(f.store.generation().unwrap(), generation);
}

#[test]
fn incomplete_embeddings_admit_only_identical_overlap_candidates() {
    for cause in ["model loading", "model assets missing"] {
        let mut f = Fixture::new();
        let exact = f.run(
            "remember_this",
            create("Use the shell to inspect the workspace."),
        );
        let similar = f.run(
            "remember_this",
            create("Use the shell to inspect another workspace."),
        );
        assert_eq!(similar["status"], "remembered");
        let plan = f
            .prepare_with_semantic(
                create("Use the shell to inspect the workspace."),
                &Ports::Incomplete(cause),
            )
            .unwrap();
        let result = response(f.apply(plan).unwrap());
        assert_eq!(result["status"], "resolution_required");
        let overlap = result["issues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["code"] == "possible_overlap")
            .unwrap();
        let candidates = overlap["current_memories"].as_array().unwrap();
        assert_eq!(candidates.len(), 1, "{result}");
        assert_eq!(
            candidates[0]["memory_id"],
            exact["results"][0]["memory_ref"]["memory_id"]
        );
        assert!(
            result["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("semantic_channel_"))),
            "{result}"
        );
    }
}

#[test]
fn newly_published_dependent_requires_review_before_a_prepared_change_can_publish() {
    let mut f = Fixture::new();
    let first = f.run(
        "remember_this",
        create("Persist data before acknowledging a request."),
    );
    let reference = first["results"][0]["memory_ref"].clone();
    let prepared = f.prepare(json!({"memories":[{"operation":"evolve","parent_refs":[reference],"what_to_remember":"Acknowledge after persisting data and its journal.","evidence":"Revised protocol"}]})).unwrap();
    let dependent = f.run("remember_this", json!({"memories":[{"operation":"create","applies_to":["user"],"what_to_remember":"The client can retry after the durable acknowledgment.","evidence":"Protocol dependency","required_memory_ids":[reference["memory_id"]]}]}));
    assert_eq!(dependent["status"], "remembered");
    let generation = f.store.generation().unwrap();
    let result = response(f.apply(prepared).unwrap());
    assert_eq!(result["status"], "resolution_required", "{result}");
    assert_eq!(f.store.generation().unwrap(), generation);
    let refreshed = f.run(
        "remember_this",
        json!({"pending_action":"continue","pending_batch_ref":result["pending_batch_ref"]}),
    );
    assert_eq!(refreshed["status"], "resolution_required", "{refreshed}");
    assert!(
        refreshed["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "required_dependent_review"),
        "{refreshed}"
    );
    assert_eq!(f.store.generation().unwrap(), generation);
}
