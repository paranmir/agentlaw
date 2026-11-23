use super::*;
use crate::{
    git::{
        git_ops,
        imports::{self, ImportStatus, ProgressRegistry},
    },
    host::stores::Stores,
    installation::{
        config::Config,
        registry::{ConfigRegistry, ConfigSnapshot, ConfigState},
    },
    protocol::BindingRef,
};
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Barrier},
    thread,
    time::Duration,
};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    snapshot: Arc<ConfigSnapshot>,
    registry: Arc<ConfigRegistry>,
    stores: Arc<Stores>,
    progress: Arc<ProgressRegistry>,
    git_local: PathBuf,
    reference: String,
    token: String,
    imported_id: String,
}

fn snapshot(root: &Path, local: &Path) -> Arc<ConfigSnapshot> {
    let config = Config {
        memory_store_path: root.to_owned(),
        runtime_path: Some(local.to_owned()),
        user_id: "user".into(),
        history_response_limit_bytes: 8192,
        response_limit_bytes: 65536,
    };
    let key = agentlaw_storage::binding_identity(root, local).unwrap();
    let reference = BindingRef::new(&key, &config.user_id);
    Arc::new(ConfigSnapshot {
        config,
        key,
        reference,
    })
}

fn publish_memory(store: &Store, text: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    store.publish(&uuid::Uuid::new_v4().to_string(), vec![Mutation { unit: CurrentUnit {
        entity_type: "memory".into(), entity_id: id.clone(), state: UnitState::Live { heads: vec![Head {
            metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}), body: text.into(),
        }] },
    }, expected_versions: vec![], evidence: "Observed fixture".into() }]).unwrap();
    id
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&local).unwrap();
    let coordination = dir.path().join("coordination");
    let store =
        Store::open_with_coordination(&root, local.join("canonical"), &coordination).unwrap();
    git(&root, &["init", "--initial-branch=main"]);
    git(&root, &["config", "user.name", "Fixture"]);
    git(&root, &["config", "user.email", "fixture@example.invalid"]);
    publish_memory(&store, "Original memory.");
    let git_local = local.join("git");
    let git_handle = crate::test_support::store_service::git_handle(&store);
    let commit = git_ops::continuity_save(&git_handle.handle, &git_local, "fixture")
        .unwrap()
        .commit_oid;
    let review = git_ops::prepare_import(
        &git_handle.handle,
        &git_local,
        &commit,
        &RequestControl::default(),
    )
    .unwrap();
    let stage =
        git_ops::get_import_stage(&git_handle.handle, &git_local, &review.import_ref).unwrap();
    let stage_store = Store::open_with_coordination(
        &stage.store_path,
        stage.runtime_path.join("canonical"),
        &coordination,
    )
    .unwrap();
    let imported_id = publish_memory(&stage_store, "Staged imported memory.");
    drop(stage_store);
    let token = git_ops::resolve_import(
        &git_handle.handle,
        &git_local,
        &review.import_ref,
        &RequestControl::default(),
    )
    .unwrap()
    .resolution_token;
    drop(git_handle);
    crate::installation::machine::load_or_create(dir.path()).unwrap();
    let registry = Arc::new(ConfigRegistry::new());
    let snapshot = snapshot(&root, &local);
    let stores = Arc::new(Stores::new(
        dir.path().to_owned(),
        crate::test_support::store_service::deps(dir.path()),
        registry.clone(),
    ));
    Fixture {
        _dir: dir,
        store,
        snapshot,
        registry,
        stores,
        progress: Arc::new(ProgressRegistry::default()),
        git_local,
        reference: review.import_ref,
        token,
        imported_id,
    }
}

fn publish_command(f: &Fixture) -> AdminCommand {
    AdminCommand::ImportPublish {
        import_ref: f.reference.clone(),
        resolution: f.token.clone(),
        user_confirmed: true,
    }
}

#[test]
fn publication_interruption_reports_handoff_and_same_token_resumes_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = fixture();
    let before = f.store.generation().unwrap();
    let hit = Arc::new(AtomicBool::new(false));
    let seen = hit.clone();
    let hook: BoundaryHook = Arc::new(move |event, _| {
        if event == "canonical_published" {
            seen.store(true, Ordering::SeqCst);
            return Err(DomainError::new(
                "fixture_interrupted",
                "Stopped before handoff.",
            ));
        }
        Ok(())
    });
    let first = run_observed(
        &f.snapshot,
        publish_command(&f),
        &RequestControl::default(),
        &f.stores,
        &f.progress,
        hook,
    );
    assert!(first.is_err());
    assert!(
        hit.load(Ordering::SeqCst),
        "canonical publication boundary was not reached"
    );
    let after_first = f.store.generation().unwrap();
    assert_eq!(after_first, before + 1);
    let listed = imports::list(&f.git_local, &f.progress).unwrap();
    assert!(listed.iter().any(
        |entry| entry.import_ref == f.reference && entry.status == ImportStatus::HandoffPending
    ));
    let result = run_observed(
        &f.snapshot,
        publish_command(&f),
        &RequestControl::default(),
        &f.stores,
        &f.progress,
        Arc::new(|_, _| Ok(())),
    )
    .unwrap();
    assert_eq!(result["status"], "published_and_committed");
    assert_eq!(f.store.generation().unwrap(), after_first);
    assert_eq!(
        f.store.read_current(&f.imported_id).unwrap().heads()[0].body,
        "Staged imported memory."
    );
}

#[test]
fn repair_waits_for_import_lifetime_guard_through_handoff() {
    let f = fixture();
    let gate = Arc::new(Barrier::new(2));
    let worker_gate = gate.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let hook: BoundaryHook = Arc::new(move |event, _| {
        if event == "publish_guard_held" {
            entered_tx.send(()).unwrap();
            worker_gate.wait();
        }
        Ok(())
    });
    let (published_tx, published_rx) = mpsc::channel();
    let worker = {
        let snapshot = f.snapshot.clone();
        let stores = f.stores.clone();
        let progress = f.progress.clone();
        let command = publish_command(&f);
        thread::spawn(move || {
            published_tx
                .send(run_observed(
                    &snapshot,
                    command,
                    &RequestControl::default(),
                    &stores,
                    &progress,
                    hook,
                ))
                .unwrap();
        })
    };
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("publish lifetime guard");
    let (waiting_tx, waiting_rx) = mpsc::channel();
    f.stores.observe(move |_, event| {
        if event == "maintain_waiting" {
            let _ = waiting_tx.send(());
        }
    });
    let (maintained_tx, maintained_rx) = mpsc::channel();
    let maintenance = {
        let stores = f.stores.clone();
        let snapshot = f.snapshot.clone();
        thread::spawn(move || {
            let guard = stores.maintain(&snapshot, &RequestControl::default());
            maintained_tx.send(guard.is_ok()).unwrap();
        })
    };
    waiting_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("maintenance must wait");
    assert!(maintained_rx.try_recv().is_err());
    gate.wait();
    let result = published_rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "published_and_committed");
    assert!(maintained_rx.recv_timeout(Duration::from_secs(30)).unwrap());
    worker.join().unwrap();
    maintenance.join().unwrap();
}

#[test]
fn staged_call_remains_pinned_when_selection_changes() {
    let f = fixture();
    let root_b = f._dir.path().join("source-b");
    let local_b = f._dir.path().join("local-b");
    std::fs::create_dir_all(&local_b).unwrap();
    let store_b = Store::open_with_coordination(
        &root_b,
        local_b.join("canonical"),
        f._dir.path().join("coordination"),
    )
    .unwrap();
    let id_b = publish_memory(&store_b, "Binding B remains unchanged.");
    let generation_b = store_b.generation().unwrap();
    f.registry.publish(ConfigState::Ready(f.snapshot.clone()));
    let gate = Arc::new(Barrier::new(2));
    let worker_gate = gate.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let hook: BoundaryHook = Arc::new(move |event, _| {
        if event == "staged_call_guard_held" {
            entered_tx.send(()).unwrap();
            worker_gate.wait();
        }
        Ok(())
    });
    let (tx, rx) = mpsc::channel();
    let worker = {
        let snapshot = f.snapshot.clone();
        let stores = f.stores.clone();
        let progress = f.progress.clone();
        let reference = f.reference.clone();
        let id = f.imported_id.clone();
        thread::spawn(move || {
            let request = json!({"action":"recall","recall":{"memory_ids":[id]}});
            tx.send(run_observed(
                &snapshot,
                AdminCommand::ImportCall {
                    import_ref: reference,
                    request,
                },
                &RequestControl::default(),
                &stores,
                &progress,
                hook,
            ))
            .unwrap();
        })
    };
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("staged call guard");
    f.registry
        .publish(ConfigState::Ready(snapshot(&root_b, &local_b)));
    gate.wait();
    let result: Value = rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
    assert_eq!(result["workspace"], "isolated_import_review");
    assert!(result.to_string().contains("Staged imported memory."));
    assert_eq!(store_b.generation().unwrap(), generation_b);
    assert_eq!(
        store_b.read_current(&id_b).unwrap().heads()[0].body,
        "Binding B remains unchanged."
    );
    worker.join().unwrap();
}

struct ImportDispatcher {
    snapshot: Arc<ConfigSnapshot>,
    stores: Arc<Stores>,
    progress: Arc<ProgressRegistry>,
    hook: BoundaryHook,
}
impl crate::host::dispatch::Dispatcher for ImportDispatcher {
    type Admitted = AdminCommand;
    fn admit(
        &self,
        _: &crate::protocol::CallTarget,
        request: crate::protocol::Classified,
    ) -> std::result::Result<AdminCommand, crate::protocol::RejectReason> {
        match request {
            crate::protocol::Classified::Admin(command) => Ok(command),
            _ => Err(crate::protocol::RejectReason::KindMismatch),
        }
    }
    fn run(&self, command: AdminCommand, control: &RequestControl) -> Result<Value> {
        run_observed(
            &self.snapshot,
            command,
            control,
            &self.stores,
            &self.progress,
            self.hook.clone(),
        )
    }
}

#[test]
fn accepted_prepare_survives_disconnected_client_and_appears_in_list() {
    use crate::{
        host::{
            accept::{start, HostLimits},
            lifecycle::Lifecycle,
        },
        ports::HealthSink,
        protocol::*,
        transport,
    };
    use std::io::Write;
    let f = fixture();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&f.snapshot.config.memory_store_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let commit = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    let gate = Arc::new(Barrier::new(2));
    let worker_gate = gate.clone();
    let (tx, rx) = mpsc::channel();
    let hook: BoundaryHook = Arc::new(move |event, reference| {
        if event == "prepare_started" {
            tx.send((event.to_owned(), reference.to_owned())).unwrap();
            worker_gate.wait();
        }
        if event == "prepare_completed" {
            tx.send((event.to_owned(), reference.to_owned())).unwrap();
        }
        Ok(())
    });
    let dispatcher = Arc::new(ImportDispatcher {
        snapshot: f.snapshot.clone(),
        stores: f.stores.clone(),
        progress: f.progress.clone(),
        hook,
    });
    let secret = "a".repeat(64);
    let host = start(
        f._dir.path(),
        "fixture-build",
        &secret,
        dispatcher,
        f.registry.clone(),
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        HostLimits::default(),
    )
    .unwrap();
    let mut stream = std::net::TcpStream::connect(host.data).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    transport::send(
        &mut stream,
        &Hello {
            protocol: PROTOCOL_VERSION,
            installation_build: "fixture-build".into(),
            secret,
        },
        MAX_CONTROL_FRAME_BYTES,
    )
    .unwrap();
    assert_eq!(
        transport::receive::<HelloReply>(&mut stream, MAX_CONTROL_REPLY_BYTES).unwrap(),
        HelloReply::Accepted
    );
    let body =
        serde_json::to_vec(&json!({"admin_command":{"import_prepare":{"commit":commit}}})).unwrap();
    let id = RequestId::new();
    transport::send(
        &mut stream,
        &CallHeader {
            id,
            target: CallTarget::Store(f.snapshot.reference),
            kind: RequestKind::Admin,
            body_len: body.len() as u32,
        },
        MAX_CONTROL_FRAME_BYTES,
    )
    .unwrap();
    assert_eq!(
        transport::receive::<DataFrame>(&mut stream, MAX_CONTROL_REPLY_BYTES).unwrap(),
        DataFrame::Proceed { id }
    );
    stream.write_all(&body).unwrap();
    let (event, reference) = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("prepare entered");
    assert_eq!(event, "prepare_started");
    drop(stream);
    let listed = imports::list(&f.git_local, &f.progress).unwrap();
    assert!(listed
        .iter()
        .any(|entry| entry.import_ref == reference && entry.status == ImportStatus::InProgress));
    gate.wait();
    let (event, completed_ref) = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("prepare completed after disconnect");
    assert_eq!(event, "prepare_completed");
    assert_eq!(completed_ref, reference);
    let listed = imports::list(&f.git_local, &f.progress).unwrap();
    assert!(listed
        .iter()
        .any(|entry| entry.import_ref == reference && entry.status == ImportStatus::ReviewWaiting));
}
