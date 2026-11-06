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
