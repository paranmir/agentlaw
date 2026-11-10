use super::*;
use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    local: PathBuf,
    state: PathBuf,
    remote: PathBuf,
    machine: String,
    policy: DelegationPolicy,
    id: String,
}
fn uid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn unit(id: &str, machine: &str, body: &str) -> CurrentUnit {
    CurrentUnit {
        entity_id: id.into(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":uid(),"applicability":{"scope":"user"},"origin":{"machine_id":machine,"user_id":"test","project_id":null},"recorded_at_ms":1,"is_rule":false,"relations":[],"work_targets":[]}),
                body: body.into(),
            }],
        },
    }
}
fn save(store: &Store, id: &str, machine: &str, body: &str) {
    let expected = store
        .read_current(id)
        .ok()
        .map(|u| {
            u.references()
                .unwrap()
                .into_iter()
                .map(|r| r.observed_version)
                .collect()
        })
        .unwrap_or_default();
    store
        .publish(
            &uid(),
            vec![Mutation {
                unit: unit(id, machine, body),
                expected_versions: expected,
                evidence: "isolated sync test".into(),
            }],
        )
        .unwrap();
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source with spaces");
        let local = temp.path().join("runtime");
        let state = temp.path().join("state");
        let remote = temp.path().join("remote.git");
        fs::create_dir_all(&remote).unwrap();
        fs::create_dir_all(&state).unwrap();
        git::run(&remote, &["init", "--bare", "--initial-branch=main"]).unwrap();
        let store = Store::open(&source, local.join("canonical")).unwrap();
        rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
        git::run(&source, &["init", "--initial-branch=main"]).unwrap();
        git::run(&source, &["config", "user.name", "Sync Test"]).unwrap();
        git::run(&source, &["config", "user.email", "sync@example.invalid"]).unwrap();
        git::run(
            &source,
            &["remote", "add", "origin", &remote.to_string_lossy()],
        )
        .unwrap();
        let machine = uid();
        let id = uid();
        save(&store, &id, &machine, "base");
        git::continuity_save(&store, &local.join("git"), "base").unwrap();
        git::run(&source, &["push", "origin", "HEAD:refs/heads/main"]).unwrap();
        let mut policy = propose_policy(&store, &local, "origin", "refs/heads/main").unwrap();
        policy.enabled = true;
        let file = temp.path().join("policy.json");
        git::save_local_json(&file, &policy).unwrap();
        configure_policy(&state, &store, &local, &file, true).unwrap();
        Self {
            _temp: temp,
            store,
            local,
            state,
            remote,
            machine,
            policy,
            id,
        }
    }
    fn request(&self, input: Value, control: RequestControl) -> Value {
        let request =
            agentlaw_contracts::parse_request(&json!({"action":"sync","sync":input}).to_string())
                .unwrap();
        let agentlaw_contracts::Request::Sync(request) = request else {
            panic!()
        };
        call(
            &self.store,
            &self.local,
            &self.state,
            "test",
            &self.machine,
            request,
            control,
        )
        .unwrap()
    }
    fn start(&self, control: RequestControl) -> Value {
        self.request(
            json!({"command":"start","policy_id":self.policy.policy_id,"request_id":uid()}),
            control,
        )
    }
    fn status(&self, op: &Value) -> Value {
        self.request(
            json!({"command":"status","operation_id":op["operation_id"]}),
            RequestControl::default(),
        )
    }
    fn remote_write(&self, body: &str) {
        let oid = git::text(&self.remote, &["rev-parse", "refs/heads/main"]).unwrap();
        let view = checkout(
            &self.store,
            &self.remote,
            &oid,
            self._temp.path(),
            &format!("remote-writer-{}", uid()),
        )
        .unwrap();
        git::run(&view.root, &["config", "user.name", "Other Test"]).unwrap();
        git::run(
            &view.root,
            &["config", "user.email", "other@example.invalid"],
        )
        .unwrap();
        let store = view.open(&self.store).unwrap();
        save(&store, &self.id, &self.machine, body);
        git::continuity_save(&store, &view.local.join("git"), "remote change").unwrap();
        git::run(
            &view.root,
            &[
                "push",
                &self.remote.to_string_lossy(),
                "HEAD:refs/heads/main",
            ],
        )
        .unwrap();
    }
    fn resolve(&self, packet_result: &Value, body: &str, control: RequestControl) -> Value {
        let inputs = packet_result["resolution_packet"]["inputs"]
            .as_object()
            .unwrap();
        let handles = inputs
            .iter()
            .filter(|(_, i)| i["entity_id"] == self.id)
            .map(|(h, _)| h.clone())
            .collect::<Vec<_>>();
        let metadata = inputs
            .iter()
            .find(|(_, i)| i["entity_id"] == self.id)
            .unwrap()
            .0;
        self.request(json!({"command":"resolve","operation_id":packet_result["operation_id"],"expected_revision":packet_result["revision"],"request_id":uid(),"solution":{"units":[{"target":self.id,"metadata_from":metadata,"derived_from":handles,"body":body,"evidence":"whole packet branch reconciliation"}],"redirects":[],"projects":[],"dependent_dispositions":[]}}),control)
    }
}
#[test]
fn fixed_cutoff_tail_history_and_completed_status() {
    let f = Fixture::new();
    save(&f.store, &f.id, &f.machine, "included at cutoff");
    let tail_id = uid();
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let local = f.local.clone();
    let machine = f.machine.clone();
    let tail = tail_id.clone();
    let original = f.id.clone();
    let coordination = f.store.coordination_root().to_path_buf();
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "preserving_post_cutoff_local_writes" {
            let store =
                Store::open_with_coordination(&source, local.join("canonical"), &coordination)
                    .unwrap();
            save(&store, &tail, &machine, "post-cutoff other memory");
        }
        if phase == "git_ref_index_handoff" {
            let store =
                Store::open_with_coordination(&source, local.join("canonical"), &coordination)
                    .unwrap();
            save(
                &store,
                &original,
                &machine,
                "same-memory write after local decision",
            );
        }
    });
    let result = f.start(control);
    assert_eq!(result["status"], "completed_for_cutoff", "{result:#}");
    assert_eq!(
        f.store.read_current(&tail_id).unwrap().heads()[0].body,
        "post-cutoff other memory"
    );
    assert_eq!(
        f.store.read_current(&f.id).unwrap().heads()[0].body,
        "same-memory write after local decision"
    );
    let remote = git::text(&f.remote, &["rev-parse", "refs/heads/main"]).unwrap();
    assert_eq!(remote, result["candidate_commit"].as_str().unwrap());
    assert!(git::text(
        &f.remote,
        &[
            "show",
            &format!("{remote}:current/memory/{}/{}.md", &f.id[..2], f.id)
        ]
    )
    .unwrap()
    .contains("included at cutoff"));
    assert!(
        !git::text(&f.remote, &["ls-tree", "-r", "--name-only", &remote])
            .unwrap()
            .contains(&tail_id)
    );
    assert!(f
        .store
        .history(&f.id)
        .unwrap()
        .iter()
        .any(|h| h.body == "same-memory write after local decision"));
    let generation = f.store.generation().unwrap();
    save(&f.store, &uid(), &f.machine, "later after completion");
    let status = f.status(&result);
    assert_eq!(status["status"], "completed_for_cutoff");
    assert_eq!(status["candidate_commit"], result["candidate_commit"]);
    assert!(f.store.generation().unwrap() > generation);
}
#[test]
fn same_identity_tail_resolution_keeps_candidate_and_scan() {
    let f = Fixture::new();
    f.remote_write("incoming concurrent");
    save(&f.store, &f.id, &f.machine, "outgoing");
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let local = f.local.clone();
    let machine = f.machine.clone();
    let id = f.id.clone();
    let coordination = f.store.coordination_root().to_path_buf();
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "preserving_post_cutoff_local_writes" {
            let store =
                Store::open_with_coordination(&source, local.join("canonical"), &coordination)
                    .unwrap();
            save(&store, &id, &machine, "local-only tail");
        }
    });
    let initial = f.start(RequestControl::default());
    assert_eq!(
        initial["resolution_packet"]["phase"], "outgoing",
        "{initial:#}"
    );
    let first = f.resolve(&initial, "reconciled outgoing and incoming", control);
    assert_eq!(first["status"], "needs_resolution", "{first:#}");
    assert_eq!(first["resolution_packet"]["phase"], "local_overlay");
    let packet = &first["resolution_packet"];
    let inputs = packet["inputs"].as_object().unwrap();
    let handles = inputs
        .iter()
        .filter(|(_, i)| i["entity_id"] == f.id)
        .map(|(h, _)| h.clone())
        .collect::<Vec<_>>();
    let metadata = inputs
        .iter()
        .find(|(_, i)| i["body"] == "local-only tail")
        .unwrap()
        .0;
    let result=f.request(json!({"command":"resolve","operation_id":first["operation_id"],"expected_revision":first["revision"],"request_id":uid(),"solution":{"units":[{"target":f.id,"metadata_from":metadata,"derived_from":handles,"body":"preserved local-only tail and outgoing","evidence":"reconcile full retained branches"}],"redirects":[],"projects":[],"dependent_dispositions":[]}}),RequestControl::default());
    assert_eq!(result["status"], "completed_for_cutoff", "{result:#}");
    assert_eq!(result["candidate_commit"], first["candidate_commit"]);
    let oid = result["candidate_commit"].as_str().unwrap();
    let remote_body = git::text(
        &f.remote,
        &[
            "show",
            &format!("{oid}:current/memory/{}/{}.md", &f.id[..2], f.id),
        ],
    )
    .unwrap();
    assert!(remote_body.contains("outgoing"));
    assert!(!remote_body.contains("local-only tail"));
    assert!(f.store.read_current(&f.id).unwrap().heads()[0]
        .body
        .contains("local-only tail"));
    let conn = db(&f.local).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT state FROM sync_operations WHERE operation_id=?1",
            [first["operation_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let op: Operation = serde_json::from_str(&raw).unwrap();
    let scan = op.scan.unwrap();
    assert!(scan.transfer_store.exists());
    assert_eq!(scan.commit_oid, oid);
}
#[test]
fn request_replay_precedes_revision_and_payload_reuse_is_rejected() {
    let f = Fixture::new();
    let request_id = uid();
    let input = json!({"command":"start","policy_id":f.policy.policy_id,"request_id":request_id});
    let first = f.request(input.clone(), RequestControl::default());
    let next = f.request(input, RequestControl::default());
    assert_eq!(first, next);
    let request = agentlaw_contracts::SyncRequest {
        command: SyncCommand::Resume,
        policy_id: None,
        operation_id: Some(first["operation_id"].as_str().unwrap().into()),
        expected_revision: Some(0),
        request_id: Some(request_id),
        solution: None,
    };
    let error = call(
        &f.store,
        &f.local,
        &f.state,
        "test",
        &f.machine,
        request,
        RequestControl::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "request_id_reused");
}
#[test]
fn policy_revocation_blocks_push_after_mandatory_handoff() {
    let f = Fixture::new();
    save(&f.store, &f.id, &f.machine, "needs sharing");
    let path = f
        .state
        .join("policies")
        .join(format!("sync-{}.json", f.policy.policy_id));
    let mut policy = f.policy.clone();
    policy.enabled = false;
    let fired = Arc::new(AtomicBool::new(false));
    let flag = fired.clone();
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "git_ref_index_handoff" && !flag.swap(true, Ordering::AcqRel) {
            git::save_local_json(&path, &policy).unwrap();
        }
    });
    let first = f.start(control);
    assert_eq!(
        first["diagnostic"]["code"], "sync_delegation_disabled",
        "{first:#}"
    );
    assert_eq!(first["canonical_applied"], true);
    assert_eq!(first["git_handoff_completed"], true);
    let remote = git::text(&f.remote, &["rev-parse", "refs/heads/main"]).unwrap();
    assert_ne!(remote, first["candidate_commit"].as_str().unwrap());
}
#[test]
fn foreign_index_lock_is_preserved_and_resume_does_not_republish() {
    let f = Fixture::new();
    save(&f.store, &f.id, &f.machine, "fixed");
    let root = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let lock = root.join(".git/index.lock");
    fs::write(&lock, b"foreign-owned").unwrap();
    let first = f.start(RequestControl::default());
    assert_eq!(first["diagnostic"]["code"], "git_index_locked", "{first:#}");
    assert_eq!(first["canonical_applied"], true);
    assert_eq!(fs::read(&lock).unwrap(), b"foreign-owned");
    let generation = f.store.generation().unwrap();
    // Test-created lock only; never another process's lock.
    fs::remove_file(lock).unwrap();
    let result=f.request(json!({"command":"resume","operation_id":first["operation_id"],"expected_revision":first["revision"],"request_id":uid()}),RequestControl::default());
    assert_eq!(result["status"], "completed_for_cutoff", "{result:#}");
    assert_eq!(f.store.generation().unwrap(), generation);
}
#[test]
fn rejected_solution_closes_gate_and_second_start_reuses_operation() {
    let f = Fixture::new();
    f.remote_write("remote branch");
    save(&f.store, &f.id, &f.machine, "local branch");
    let first = f.start(RequestControl::default());
    assert_eq!(first["status"], "needs_resolution");
    let second = f.start(RequestControl::default());
    assert_eq!(first["operation_id"], second["operation_id"]);
    let input = first["resolution_packet"]["inputs"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    let failed=f.request(json!({"command":"resolve","operation_id":first["operation_id"],"expected_revision":first["revision"],"request_id":uid(),"solution":{"units":[{"target":f.id,"metadata_from":input,"derived_from":[],"body":"invalid missing ancestry","evidence":"test rejection"}],"redirects":[],"projects":[],"dependent_dispositions":[]}}),RequestControl::default());
    assert_eq!(failed["status"], "solution_rejected", "{failed:#}");
    let resumed=f.request(json!({"command":"resume","operation_id":failed["operation_id"],"expected_revision":failed["revision"],"request_id":uid()}),RequestControl::default());
    assert!(resumed["candidate_commit"].is_null());
    assert_eq!(
        f.store.read_current(&f.id).unwrap().heads()[0].body,
        "local branch"
    );
    let corrected = f.resolve(&resumed, "correct whole lineage", RequestControl::default());
    assert_eq!(corrected["status"], "completed_for_cutoff", "{corrected:#}");
}
#[test]
fn hold_wait_keeps_writes_available_and_cancel_before_decision_keeps_source() {
    let f = Fixture::new();
    f.remote_write("incoming branch");
    save(&f.store, &f.id, &f.machine, "local branch");
    let first = f.start(RequestControl::default());
    let held=f.request(json!({"command":"hold","operation_id":first["operation_id"],"expected_revision":first["revision"],"request_id":uid()}),RequestControl::default());
    assert_eq!(held["status"], "held");
    let other = uid();
    save(
        &f.store,
        &other,
        &f.machine,
        "ordinary write during LLM wait",
    );
    let cancelled=f.request(json!({"command":"cancel","operation_id":held["operation_id"],"expected_revision":held["revision"],"request_id":uid()}),RequestControl::default());
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(
        f.store.read_current(&other).unwrap().heads()[0].body,
        "ordinary write during LLM wait"
    );
    assert_eq!(
        f.store.read_current(&f.id).unwrap().heads()[0].body,
        "local branch"
    );
}
#[test]
fn response_loss_resumes_delivery_without_republication() {
    let f = Fixture::new();
    let first = f.start(RequestControl::default());
    assert_eq!(first["status"], "completed_for_cutoff");
    let conn = db(&f.local).unwrap();
    let mut op: Operation = serde_json::from_str(
        &conn
            .query_row(
                "SELECT state FROM sync_operations WHERE operation_id=?1",
                [first["operation_id"].as_str().unwrap()],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    op.phase = "pushing".into();
    op.delivery = None;
    persist(&conn, &op).unwrap();
    let before = f.store.generation().unwrap();
    let resumed = f.status(&first);
    assert_eq!(resumed["status"], "completed_for_cutoff");
    assert_eq!(resumed["candidate_commit"], first["candidate_commit"]);
    assert_eq!(f.store.generation().unwrap(), before);
    let scan = op.scan.unwrap();
    assert_eq!(
        resumed["scan_receipt"]["completed_at_ms"],
        json!(scan.completed_at_ms)
    );
}
#[test]
fn completion_survives_stage_loss_and_later_source_changes() {
    let f = Fixture::new();
    let completed = f.start(RequestControl::default());
    assert_eq!(completed["status"], "completed_for_cutoff");
    let conn = db(&f.local).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT state FROM sync_operations WHERE operation_id=?1",
            [completed["operation_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let op: Operation = serde_json::from_str(&raw).unwrap();
    fs::rename(&op.owned, op.owned.with_extension("retained-test-backup")).unwrap();
    save(
        &f.store,
        &f.id,
        &f.machine,
        "later source with no old stage",
    );
    let status = f.status(&completed);
    assert_eq!(status["status"], "completed_for_cutoff", "{status:#}");
    assert_eq!(status["candidate_commit"], completed["candidate_commit"]);
    assert_eq!(
        f.store.read_current(&f.id).unwrap().heads()[0].body,
        "later source with no old stage"
    );
}
fn advance_empty_remote(remote: &Path) {
    git::run(remote, &["config", "user.name", "Remote Race Test"]).unwrap();
    git::run(remote, &["config", "user.email", "race@example.invalid"]).unwrap();
    let parent = git::text(remote, &["rev-parse", "refs/heads/main"]).unwrap();
    let tree = git::text(remote, &["rev-parse", &format!("{parent}^{{tree}}")]).unwrap();
    let commit = create_commit(remote, &tree, &[parent.clone()], "remote advancement").unwrap();
    git::run(remote, &["update-ref", "refs/heads/main", &commit, &parent]).unwrap();
}
#[test]
fn one_remote_advance_reseals_candidate_without_local_tail() {
    let f = Fixture::new();
    save(&f.store, &f.id, &f.machine, "outgoing cutoff change");
    let remote = f.remote.clone();
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let local = f.local.clone();
    let coordination = f.store.coordination_root().to_path_buf();
    let machine = f.machine.clone();
    let tail = uid();
    let later = tail.clone();
    let once = AtomicBool::new(false);
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "checking_remote_delivery" && !once.swap(true, Ordering::SeqCst) {
            advance_empty_remote(&remote);
            let store =
                Store::open_with_coordination(&source, local.join("canonical"), &coordination)
                    .unwrap();
            save(
                &store,
                &later,
                &machine,
                "tail after first outgoing candidate",
            );
        }
    });
    let completed = f.start(control);
    assert_eq!(completed["status"], "completed_for_cutoff", "{completed:#}");
    let oid = completed["candidate_commit"].as_str().unwrap();
    assert!(
        !git::text(&f.remote, &["ls-tree", "-r", "--name-only", oid])
            .unwrap()
            .contains(&tail)
    );
    assert_eq!(
        f.store.read_current(&tail).unwrap().heads()[0].body,
        "tail after first outgoing candidate"
    );
    let conn = db(&f.local).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT state FROM sync_operations WHERE operation_id=?1",
            [completed["operation_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let op: Operation = serde_json::from_str(&raw).unwrap();
    assert_eq!(op.remote_updates, 1);
    assert_ne!(
        read_json::<(String, String)>(&op.owned.join("candidate-0.json"))
            .unwrap()
            .0,
        oid
    );
    assert_eq!(op.scan.unwrap().commit_oid, oid);
}
#[test]
fn second_remote_advance_pauses_without_force_delivery() {
    let f = Fixture::new();
    save(&f.store, &f.id, &f.machine, "outgoing cutoff change");
    let remote = f.remote.clone();
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "checking_remote_delivery" {
            advance_empty_remote(&remote);
        }
    });
    let paused = f.start(control);
    assert_eq!(paused["status"], "paused_remote_advanced", "{paused:#}");
    assert_ne!(
        git::text(&f.remote, &["rev-parse", "refs/heads/main"]).unwrap(),
        paused["candidate_commit"].as_str().unwrap()
    );
    assert!(paused["delivery"].is_null());
}
#[test]
fn incoming_projection_keeps_raw_bytes_despite_attributes_and_autocrlf() {
    let f = Fixture::new();
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    git::run(&source, &["config", "core.autocrlf", "false"]).unwrap();
    fs::write(source.join(".gitattributes"), b"* text\n").unwrap();
    git::continuity_save(&f.store, &f.local.join("git"), "text attributes fixture").unwrap();
    git::run(&source, &["push", "origin", "HEAD:refs/heads/main"]).unwrap();
    let root = f._temp.path().join("raw-projection");
    fs::create_dir_all(&root).unwrap();
    git::run(&root, &["init"]).unwrap();
    git::run(&root, &["config", "core.autocrlf", "true"]).unwrap();
    let oid = git::text(&f.remote, &["rev-parse", "refs/heads/main"]).unwrap();
    let view = checkout(&f.store, &f.remote, &oid, f._temp.path(), "raw-projection").unwrap();
    let path = format!("current/memory/{}/{}.md", &f.id[..2], f.id);
    assert_eq!(
        fs::read(view.root.join(&path)).unwrap(),
        git::run(&f.remote, &["show", &format!("{oid}:{path}")]).unwrap()
    );
    view.open(&f.store).unwrap().audit_source().unwrap();
}
#[test]
fn missing_required_predecision_view_is_not_initialized_as_empty_memory() {
    let f = Fixture::new();
    f.remote_write("incoming branch");
    save(&f.store, &f.id, &f.machine, "local branch");
    let first = f.start(RequestControl::default());
    assert_eq!(first["status"], "needs_resolution");
    let conn = db(&f.local).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT state FROM sync_operations WHERE operation_id=?1",
            [first["operation_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let op: Operation = serde_json::from_str(&raw).unwrap();
    let required = op.outgoing.unwrap().root;
    fs::rename(&required, required.with_extension("retained-test-backup")).unwrap();
    let rejected = f.resolve(
        &first,
        "must not publish without inputs",
        RequestControl::default(),
    );
    assert_eq!(rejected["status"], "solution_rejected", "{rejected:#}");
    assert_eq!(rejected["diagnostic"]["code"], "sync_view_unavailable");
    assert!(!required.exists());
    assert!(!rejected["canonical_applied"].as_bool().unwrap());
    assert_eq!(
        f.store.read_current(&f.id).unwrap().heads()[0].body,
        "local branch"
    );
}
#[test]
fn unchanged_sync_reuses_existing_commit_without_empty_git_history() {
    let f = Fixture::new();
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let before = git::optional_head(&source).unwrap().unwrap();
    let completed = f.start(RequestControl::default());
    assert_eq!(completed["status"], "completed_for_cutoff", "{completed:#}");
    assert_eq!(completed["candidate_commit"].as_str().unwrap(), before);
    assert_eq!(git::optional_head(&source).unwrap().unwrap(), before);
    assert_eq!(
        git::text(&f.remote, &["rev-parse", "refs/heads/main"]).unwrap(),
        before
    );
}

#[test]
fn handle_sync_completes_with_legacy_response_shape() {
    use crate::{store::service::StoreService, test_support::store_service::deps};
    use agentlaw_flows::runtime::StoreIdentity;
    let f = Fixture::new();
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let identity = StoreIdentity {
        root: source,
        local: f.local.clone(),
        user_id: "test".into(),
        machine_id: f.machine.clone(),
        binding_id: uid(),
        history_limit: 8192,
        response_limit: 65536,
    };
    let mut dependencies = deps(f._temp.path());
    dependencies.coordination = f.store.coordination_root().to_owned();
    dependencies.state = f.state.clone();
    let service = StoreService::open(identity, dependencies, &RequestControl::default()).unwrap();
    let request_id = uid();
    let input = json!({"action":"sync","sync":{"command":"start","policy_id":f.policy.policy_id,"request_id":request_id}});
    let agentlaw_contracts::Request::Sync(request) =
        agentlaw_contracts::parse_request(&input.to_string()).unwrap()
    else {
        panic!()
    };
    let result = call_with_handle(
        &service.git_handle(),
        &f.state,
        "test",
        &f.machine,
        request,
        RequestControl::default(),
    )
    .unwrap();
    assert_eq!(result["status"], "completed_for_cutoff", "{result:#}");
    for field in [
        "operation_id",
        "revision",
        "status",
        "phase",
        "cutoff",
        "canonical_applied",
        "git_handoff_completed",
        "delivery",
        "policy_id",
        "diagnostic",
        "next_action",
    ] {
        assert!(result.get(field).is_some(), "missing {field}: {result:#}");
    }
    assert_eq!(result["canonical_applied"], true);
    assert_eq!(result["git_handoff_completed"], true);
    let legacy = Fixture::new().start(RequestControl::default());
    let legacy_keys = legacy.as_object().unwrap().keys().collect::<Vec<_>>();
    let handle_keys = result.as_object().unwrap().keys().collect::<Vec<_>>();
    assert_eq!(handle_keys, legacy_keys);
    let agentlaw_contracts::Request::Sync(replay) =
        agentlaw_contracts::parse_request(&input.to_string()).unwrap()
    else {
        panic!()
    };
    let replayed = call_with_handle(
        &service.git_handle(),
        &f.state,
        "test",
        &f.machine,
        replay,
        RequestControl::default(),
    )
    .unwrap();
    assert_eq!(replayed, result);
    let reused = SyncRequest {
        command: SyncCommand::Resume,
        policy_id: None,
        operation_id: Some(result["operation_id"].as_str().unwrap().into()),
        expected_revision: Some(0),
        request_id: Some(request_id),
        solution: None,
    };
    assert!(
        matches!(call_with_handle(&service.git_handle(), &f.state, "test", &f.machine, reused, RequestControl::default()), Err(e) if e.code == "request_id_reused")
    );
}

// Uses a real SSH transport child to hold upload-pack at a file barrier. Sync
// invokes the policy's resolved URL, so remote.origin.uploadpack is bypassed.
struct SlowGit {
    started: PathBuf,
    release: PathBuf,
}
impl Drop for SlowGit {
    fn drop(&mut self) {
        let _ = fs::write(&self.release, b"release");
    }
}
fn slow_git_remote(f: &mut Fixture) -> SlowGit {
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let script = f._temp.path().join("slow-upload-pack.sh");
    let started = f._temp.path().join("upload-started");
    let release = f._temp.path().join("upload-release");
    let shell_path = |p: &Path| {
        git::git_path(p)
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''")
    };
    let body = format!("#!/bin/sh\ncase \"$*\" in *git-receive-pack*) exec git-receive-pack '{}' ;; esac\nprintf started > '{}'\ni=0\nwhile [ ! -f '{}' ]; do\n  i=$((i+1)); [ $i -lt 300 ] || exit 124\n  sleep 0.1\ndone\nexec git-upload-pack '{}'\n", shell_path(&f.remote), shell_path(&started), shell_path(&release), shell_path(&f.remote));
    fs::write(&script, body).unwrap();
    let command = format!("sh '{}'", shell_path(&script));
    git::run(&source, &["config", "core.sshCommand", &command]).unwrap();
    git::run(&source, &["config", "ssh.variant", "ssh"]).unwrap();
    git::run(
        &source,
        &["remote", "set-url", "origin", "ssh://fixture/remote"],
    )
    .unwrap();
    let mut policy = propose_policy(&f.store, &f.local, "origin", "refs/heads/main").unwrap();
    policy.enabled = true;
    let file = f._temp.path().join("slow-policy.json");
    git::save_local_json(&file, &policy).unwrap();
    configure_policy(&f.state, &f.store, &f.local, &file, true).unwrap();
    f.policy = policy;
    SlowGit { started, release }
}
fn sync_service(f: &Fixture) -> crate::store::service::StoreService {
    use agentlaw_flows::runtime::StoreIdentity;
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let identity = StoreIdentity {
        root: source,
        local: f.local.clone(),
        user_id: "test".into(),
        machine_id: f.machine.clone(),
        binding_id: uid(),
        history_limit: 8192,
        response_limit: 65536,
    };
    let mut dependencies = crate::test_support::store_service::deps(f._temp.path());
    dependencies.coordination = f.store.coordination_root().to_owned();
    dependencies.state = f.state.clone();
    crate::store::service::StoreService::open(identity, dependencies, &RequestControl::default())
        .unwrap()
}
fn parsed_sync(input: Value) -> SyncRequest {
    let agentlaw_contracts::Request::Sync(request) =
        agentlaw_contracts::parse_request(&json!({"action":"sync","sync":input}).to_string())
            .unwrap()
    else {
        panic!()
    };
    request
}
fn wait_for_path(path: &Path, limit: std::time::Duration) -> bool {
    let until = std::time::Instant::now() + limit;
    while std::time::Instant::now() < until {
        if path.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    path.exists()
}

#[test]
fn delayed_git_fetch_leaves_query_and_lane_available() {
    use crate::protocol::QueryRequest;
    use agentlaw_flows::RecallRequest;
    use std::{sync::mpsc, thread, time::Duration};
    let mut f = Fixture::new();
    let barrier = slow_git_remote(&mut f);
    let service = Arc::new(sync_service(&f));
    let ready_until = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if service
            .query(
                QueryRequest::Recall(RecallRequest::default()),
                &RequestControl::default(),
            )
            .is_ok()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < ready_until,
            "recall did not become ready"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let request =
        parsed_sync(json!({"command":"start","policy_id":f.policy.policy_id,"request_id":uid()}));
    let handle = service.git_handle();
    let state = f.state.clone();
    let machine = f.machine.clone();
    let (sync_tx, sync_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        sync_tx
            .send(call_with_handle(
                &handle,
                &state,
                "test",
                &machine,
                request,
                RequestControl::default(),
            ))
            .ok();
    });
    let started = wait_for_path(&barrier.started, Duration::from_secs(15));
    let (query_tx, query_rx) = mpsc::channel();
    let query_service = service.clone();
    let query = thread::spawn(move || {
        query_tx
            .send(query_service.query(
                QueryRequest::Recall(RecallRequest::default()),
                &RequestControl::default(),
            ))
            .ok();
    });
    let (lane_tx, lane_rx) = mpsc::channel();
    let lane_service = service.clone();
    let lane = thread::spawn(move || {
        lane_tx
            .send(lane_service.recover(&RequestControl::default()))
            .ok();
    });
    let (remember_tx, remember_rx) = mpsc::channel();
    let remember_service = service.clone();
    let remember = thread::spawn(move || {
        let request = serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"Saved while Git waits","evidence":"Observed during a delayed fetch","applies_to":["user"]}]})).unwrap();
        remember_tx
            .send(remember_service.command(
                crate::protocol::CommandRequest::Remember(request),
                &RequestControl::default(),
            ))
            .ok();
    });
    let query_result = query_rx.recv_timeout(Duration::from_secs(5));
    let lane_result = lane_rx.recv_timeout(Duration::from_secs(5));
    let remember_result = remember_rx.recv_timeout(Duration::from_secs(5));
    fs::write(&barrier.release, b"release").unwrap();
    let sync_result = sync_rx.recv_timeout(Duration::from_secs(25));
    assert!(started, "Git upload-pack did not reach the barrier");
    assert!(
        matches!(query_result, Ok(Ok(_))),
        "query did not complete while Git waited"
    );
    assert!(
        matches!(lane_result, Ok(Ok(()))),
        "lane did not complete while Git waited"
    );
    assert!(
        matches!(&remember_result, Ok(Ok(value)) if value["status"] == "remembered"),
        "remember did not complete while Git waited: {remember_result:?}"
    );
    assert!(
        matches!(sync_result, Ok(Ok(_))),
        "sync did not finish after releasing Git"
    );
    query.join().unwrap();
    lane.join().unwrap();
    remember.join().unwrap();
    worker.join().unwrap();
}

fn packet_files(local: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let root = local.join("git").join("syncs");
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            if let Ok(children) = fs::read_dir(entry.path()) {
                for child in children.flatten() {
                    let name = child.file_name().to_string_lossy().into_owned();
                    if name.starts_with("packet-") && name.ends_with(".json") {
                        files.push(child.path());
                    }
                }
            }
        }
    }
    files
}

#[test]
fn packet_file_is_created_only_after_lane_persistence_succeeds() {
    use std::{sync::mpsc, thread, time::Duration};
    let mut failed = Fixture::new();
    failed.remote_write("incoming conflicting edit");
    save(
        &failed.store,
        &failed.id,
        &failed.machine,
        "local conflicting edit",
    );
    let barrier = slow_git_remote(&mut failed);
    let service = sync_service(&failed);
    let request = parsed_sync(
        json!({"command":"start","policy_id":failed.policy.policy_id,"request_id":uid()}),
    );
    let handle = service.git_handle();
    let state = failed.state.clone();
    let machine = failed.machine.clone();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        tx.send(call_with_handle(
            &handle,
            &state,
            "test",
            &machine,
            request,
            RequestControl::default(),
        ))
        .ok();
    });
    let started = wait_for_path(&barrier.started, Duration::from_secs(15));
    let before = packet_files(&failed.local);
    service.close();
    fs::write(&barrier.release, b"release").unwrap();
    let result = rx.recv_timeout(Duration::from_secs(25));
    assert!(started, "Git upload-pack did not reach the barrier");
    assert!(result.is_ok(), "sync failed to return after lane closure");
    match result.unwrap() {
        Err(_) => {}
        Ok(value) => assert_ne!(
            value["status"], "needs_resolution",
            "packet cannot be reported as persisted: {value:#}"
        ),
    }
    assert_eq!(packet_files(&failed.local), before);
    worker.join().unwrap();

    let success = Fixture::new();
    success.remote_write("incoming conflicting edit");
    save(
        &success.store,
        &success.id,
        &success.machine,
        "local conflicting edit",
    );
    let service = sync_service(&success);
    let request = parsed_sync(
        json!({"command":"start","policy_id":success.policy.policy_id,"request_id":uid()}),
    );
    let response = call_with_handle(
        &service.git_handle(),
        &success.state,
        "test",
        &success.machine,
        request,
        RequestControl::default(),
    )
    .unwrap();
    assert_eq!(response["status"], "needs_resolution", "{response:#}");
    let revision = response["revision"].as_u64().unwrap();
    let operation = response["operation_id"].as_str().unwrap();
    let expected = success
        .local
        .join("git")
        .join("syncs")
        .join(operation)
        .join(format!("packet-{revision}.json"));
    assert!(
        expected.is_file(),
        "missing persisted packet {}",
        expected.display()
    );
    let packet: Value = serde_json::from_slice(&fs::read(expected).unwrap()).unwrap();
    assert_eq!(packet, response["resolution_packet"]);
}

#[test]
fn changed_inputs_during_index_preparation_are_preserved_and_resume() {
    use std::{sync::mpsc, thread, time::Duration};
    let f = Fixture::new();
    f.remote_write("remote update requiring local publication");
    let source = f
        .store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .unwrap();
    let arm = f._temp.path().join("arm-index-delay");
    let started = f._temp.path().join("index-delay-started");
    let release = f._temp.path().join("index-delay-release");
    let script = f._temp.path().join("slow-fsmonitor.sh");
    let shell_path = |p: &Path| {
        git::git_path(p)
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''")
    };
    let body = format!("#!/bin/sh\nif [ -f '{}' ] && [ ! -f '{}' ]; then\n  printf started > '{}'\n  i=0\n  while [ ! -f '{}' ]; do\n    i=$((i+1)); [ $i -lt 300 ] || exit 124\n    sleep 0.1\n  done\nfi\nprintf 'token\\000'\n", shell_path(&arm), shell_path(&started), shell_path(&started), shell_path(&release));
    fs::write(&script, body).unwrap();
    let command = format!("sh '{}'", shell_path(&script));
    git::run(&source, &["config", "core.fsmonitor", &command]).unwrap();
    git::run(&source, &["update-index", "--fsmonitor"]).unwrap();
    let service = sync_service(&f);
    let request =
        parsed_sync(json!({"command":"start","policy_id":f.policy.policy_id,"request_id":uid()}));
    let handle = service.git_handle();
    let state = f.state.clone();
    let machine = f.machine.clone();
    let armed = arm.clone();
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "preserving_post_cutoff_local_writes" {
            fs::write(&armed, b"armed").unwrap();
        }
    });
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        tx.send(call_with_handle(
            &handle, &state, "test", &machine, request, control,
        ))
        .ok();
    });
    let reached = wait_for_path(&started, Duration::from_secs(20));
    if reached {
        save(
            &f.store,
            &f.id,
            &f.machine,
            "real local write after overlay guard",
        );
    }
    fs::write(&release, b"release").unwrap();
    let first = rx.recv_timeout(Duration::from_secs(30));
    assert!(reached, "Git index preparation did not reach the barrier");
    let first = first
        .expect("sync did not finish after releasing index child")
        .unwrap();
    assert_eq!(first["phase"], "local_inputs_changed", "{first:#}");
    assert_eq!(
        f.store.read_current(&f.id).unwrap().heads()[0].body,
        "real local write after overlay guard"
    );
    worker.join().unwrap();
    let resume = parsed_sync(
        json!({"command":"resume","operation_id":first["operation_id"],"expected_revision":first["revision"],"request_id":uid()}),
    );
    let result = call_with_handle(
        &service.git_handle(),
        &f.state,
        "test",
        &f.machine,
        resume,
        RequestControl::default(),
    )
    .unwrap();
    assert_ne!(result["phase"], "local_inputs_changed", "{result:#}");
    assert_eq!(
        f.store.read_current(&f.id).unwrap().heads()[0].body,
        "real local write after overlay guard"
    );
}
