use super::*;
use crate::{
    store::lane::SyncOperationRow,
    test_support::store_service::{deps, fixture, id},
};
use std::{sync::mpsc, thread, time::Instant};

fn wait_for(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < until, "condition timed out");
        thread::sleep(Duration::from_millis(20));
    }
}

fn prepare_remember(service: &StoreService, body: &str) -> Result<Prepared> {
    let reader = ControlReader::open(&service.identity.local)?;
    let control = RequestControl::default();
    let ctx = ReadCtx {
        identity: &service.identity,
        source: &service.source,
        control: &reader,
        semantic: service.semantic.as_ref(),
        history_semantic: service.semantic.as_ref(),
        history_cache: &service.history_cache,
        request: &control,
    };
    let request = serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed","applies_to":["user"]}]})).unwrap();
    service.reads.run_prepare(&ctx, request)
}

#[test]
fn busy_apply_allows_query_and_another_preparation_before_lane_entry() {
    let (dir, identity) = fixture();
    let local = identity.local.clone();
    let service = Arc::new(
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap(),
    );
    let first = prepare_remember(&service, "first distinct observation").unwrap();
    let lock = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let first_service = service.clone();
    let worker = thread::spawn(move || {
        started_tx.send(()).unwrap();
        done_tx
            .send(first_service.apply_prepared(first, &RequestControl::default()))
            .unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    let (query_tx, query_rx) = mpsc::channel();
    let other = service.clone();
    let query = thread::spawn(move || {
        query_tx
            .send(other.query(
                QueryRequest::Recall(RecallRequest::default()),
                &RequestControl::default(),
            ))
            .unwrap()
    });
    assert!(query_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .is_ok());
    let (invalid_tx, invalid_rx) = mpsc::channel();
    let other = service.clone();
    let invalid = thread::spawn(move || {
        let request = serde_json::from_value(json!({})).unwrap();
        invalid_tx
            .send(other.command(
                crate::protocol::CommandRequest::Remember(request),
                &RequestControl::default(),
            ))
            .unwrap();
    });
    assert!(
        matches!(invalid_rx.recv_timeout(Duration::from_secs(2)).unwrap(), Err(error) if error.code == "invalid_input")
    );
    let (prepare_tx, prepare_rx) = mpsc::channel();
    let other = service.clone();
    let preparing = thread::spawn(move || {
        prepare_tx
            .send(prepare_remember(&other, "second distinct observation"))
            .unwrap()
    });
    let _second = prepare_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    let cancellation = RequestControl::default();
    let queued_control = cancellation.clone();
    let queued_service = service.clone();
    let (queued_tx, queued_rx) = mpsc::channel();
    let queued = thread::spawn(move || {
        let request = serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"second distinct observation","evidence":"Observed","applies_to":["user"]}]})).unwrap();
        queued_tx
            .send(queued_service.command(
                crate::protocol::CommandRequest::Remember(request),
                &queued_control,
            ))
            .unwrap()
    });
    assert!(queued_rx.recv_timeout(Duration::from_millis(150)).is_err());
    cancellation.cancel.store(true, Ordering::Release);
    assert!(
        matches!(queued_rx.recv_timeout(Duration::from_secs(2)).unwrap(), Err(error) if error.code == "cancelled")
    );
    lock.execute_batch("COMMIT").unwrap();
    assert!(done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .is_ok());
    worker.join().unwrap();
    query.join().unwrap();
    preparing.join().unwrap();
    invalid.join().unwrap();
    queued.join().unwrap();
}

#[test]
fn command_remember_then_recall_returns_published_memory() {
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let request = serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"A unique remembered rule.","evidence":"Observed","applies_to":["user"],"is_rule":true}]})).unwrap();
    let result = service
        .command(
            crate::protocol::CommandRequest::Remember(request),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(result["status"], "remembered");
    let id = result["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let recall = serde_json::from_value(json!({"memory_ids":[id]})).unwrap();
    let result = service
        .query(QueryRequest::Recall(recall), &RequestControl::default())
        .unwrap();
    assert!(result["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |memory| memory["current_heads"][0]["what_to_remember"] == "A unique remembered rule."
        ));
}

#[test]
fn cancelled_command_does_not_publish_memory() {
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let before = service.source.read_snapshot().unwrap().position().clone();
    let control = RequestControl::default();
    control.cancel.store(true, Ordering::Release);
    let request = serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"Cancelled observation","evidence":"Observed","applies_to":["user"]}]})).unwrap();
    let result = service.command(crate::protocol::CommandRequest::Remember(request), &control);
    assert!(matches!(result, Err(error) if error.code == "cancelled"));
    assert_eq!(*service.source.read_snapshot().unwrap().position(), before);
}

#[test]
fn command_connect_create_returns_association_and_restore() {
    let (dir, identity) = fixture();
    let local = identity.local.clone();
    let binding = identity.binding_id.clone();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    let request = serde_json::from_value(json!({"intent":"create","project_path":path,"project_name":"Example","restore_context":true,"recall_for":"Restore the connected project context"})).unwrap();
    let result = service
        .command(
            crate::protocol::CommandRequest::Connect(request),
            &RequestControl::default(),
        )
        .unwrap();
    let id = result["project_connection"]["project_id"].as_str().unwrap();
    assert!(result["recall_result"].is_object());
    let reader = ControlReader::open(&local).unwrap();
    assert_eq!(
        reader
            .exact_association(&binding, &path)
            .unwrap()
            .unwrap()
            .project_id,
        id
    );
}

#[test]
fn command_snapshot_response_limit_applies_to_connected_restore() {
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let body = "mandatory rule text ".repeat(600);
    let request = serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed","applies_to":["user"],"is_rule":true}]})).unwrap();
    let saved = service
        .command(
            crate::protocol::CommandRequest::Remember(request),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(saved["status"], "remembered");
    let key = agentlaw_storage::binding_identity(&service.identity.root, &service.identity.local)
        .unwrap();
    let snapshot = ConfigSnapshot {
        reference: service.reference,
        key,
        config: crate::installation::config::Config {
            memory_store_path: service.identity.root.clone(),
            user_id: service.identity.user_id.clone(),
            history_response_limit_bytes: 512,
            runtime_path: Some(service.identity.local.clone()),
            response_limit_bytes: 4096,
        },
    };
    let request = serde_json::from_value(json!({"intent":"create","project_path":dir.path().to_string_lossy(),"project_name":"Limited restore","restore_context":true,"recall_for":"Restore the connected project context"})).unwrap();
    let result = service
        .command_with_snapshot(
            crate::protocol::CommandRequest::Connect(request),
            &snapshot,
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(result["recall_result"]["code"], "complete_content_in_file");
    assert!(result["recall_result"]["artifact"]["path"].is_string());
}

#[test]
fn command_restore_stays_pinned_when_path_is_reconnected() {
    let (dir, identity) = fixture();
    let service = Arc::new(
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap(),
    );
    let path_a = dir.path().join("a").to_string_lossy().into_owned();
    let path_b = dir.path().join("b").to_string_lossy().into_owned();
    let connect = |path: &str, name: &str| {
        let request = serde_json::from_value(
            json!({"intent":"create","project_path":path,"project_name":name}),
        )
        .unwrap();
        service
            .command(
                crate::protocol::CommandRequest::Connect(request),
                &RequestControl::default(),
            )
            .unwrap()["project_connection"]["project_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let a = connect(&path_a, "Project A");
    let b = connect(&path_b, "Project B");
    for (path, body) in [(&path_a, "Project A rule"), (&path_b, "Project B rule")] {
        let request = serde_json::from_value(json!({"project_path":path,"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed","applies_to":["project"],"is_rule":true}]})).unwrap();
        assert_eq!(
            service
                .command(
                    crate::protocol::CommandRequest::Remember(request),
                    &RequestControl::default()
                )
                .unwrap()["status"],
            "remembered"
        );
    }
    let (reached_tx, reached_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume = std::sync::Mutex::new(resume_rx);
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "waiting_exact" {
            reached_tx.send(()).unwrap();
            resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    });
    let restoring = service.clone();
    let restore_path = path_a.clone();
    let restore_id = a.clone();
    let worker = thread::spawn(move || {
        let request = serde_json::from_value(json!({"intent":"connect","project_path":restore_path,"project_id":restore_id,"restore_context":true,"recall_for":"Restore the connected project context"})).unwrap();
        restoring.command(crate::protocol::CommandRequest::Connect(request), &control)
    });
    reached_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let request =
        serde_json::from_value(json!({"intent":"connect","project_path":path_a,"project_id":b}))
            .unwrap();
    let changed = service
        .command(
            crate::protocol::CommandRequest::Connect(request),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(changed["project_connection"]["project_id"], b);
    resume_tx.send(()).unwrap();
    let result = worker.join().unwrap().unwrap();
    assert_eq!(result["project_connection"]["project_id"], a);
    let memories = result["recall_result"]["memories"].as_array().unwrap();
    assert!(memories
        .iter()
        .any(|m| m["current_heads"][0]["what_to_remember"] == "Project A rule"));
    assert!(!memories
        .iter()
        .any(|m| m["current_heads"][0]["what_to_remember"] == "Project B rule"));
    let reader = ControlReader::open(&service.identity.local).unwrap();
    assert_eq!(
        reader
            .exact_association(&service.identity.binding_id, &path_a)
            .unwrap()
            .unwrap()
            .project_id,
        b
    );
}

#[test]
fn blocked_mutation_does_not_block_query_and_queued_cancellation() {
    let (dir, identity) = fixture();
    let local = identity.local.clone();
    let service = Arc::new(
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap(),
    );
    let lock = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let sender = service.lane_sender();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = sender.call(
            LaneCommand::SyncPersist(SyncOperationRow {
                id: "first".into(),
                source: "one".into(),
                state_json: "{}".into(),
                active: false,
            }),
            &RequestControl::default(),
        );
        done_tx.send(result.is_ok()).unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    let (query_tx, query_rx) = mpsc::channel();
    let query_service = service.clone();
    let query = thread::spawn(move || {
        let result = query_service.query(
            QueryRequest::Recall(RecallRequest::default()),
            &RequestControl::default(),
        );
        query_tx.send(result).unwrap();
    });
    assert!(query_rx.recv_timeout(Duration::from_secs(2)).is_ok());
    let cancel = RequestControl::default();
    let queued_control = cancel.clone();
    let sender = service.lane_sender();
    let queued = thread::spawn(move || {
        sender.call(
            LaneCommand::SyncPersist(SyncOperationRow {
                id: "cancelled".into(),
                source: "two".into(),
                state_json: "{}".into(),
                active: false,
            }),
            &queued_control,
        )
    });
    thread::sleep(Duration::from_millis(100));
    cancel.cancel.store(true, Ordering::Release);
    assert!(matches!(queued.join().unwrap(),Err(error) if error.code=="cancelled"));
    lock.execute_batch("COMMIT").unwrap();
    assert!(done_rx.recv_timeout(Duration::from_secs(4)).unwrap());
    worker.join().unwrap();
    query.join().unwrap();
    let count: i64 = lock
        .query_row(
            "SELECT count(*) FROM sync_operations WHERE operation_id='cancelled'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn busy_pointer_write_preserves_generation_then_retries() {
    let (dir, identity) = fixture();
    let local = identity.local.clone();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    wait_for(|| {
        service.projection.status().state == crate::projection::ProjectionState::Running
            && service.projection.generations().pin().is_ok()
    });
    let before = ControlReader::open(&local)
        .unwrap()
        .active_projection()
        .unwrap()
        .unwrap();
    let exact_bytes = std::fs::read(&before.2).unwrap();
    let lexical_bytes = std::fs::read(&before.3).unwrap();
    let generations = service.projection.generations();
    let shadow = ExactIndexWriter::rebuild_shadow_in(
        &local.join("projection"),
        &service.source,
        &RequestControl::default(),
    )
    .unwrap();
    let staged = generations.stage(shadow).unwrap();
    let generation = ActiveGeneration {
        id: staged.generation_id().into(),
        epoch: staged.epoch().into(),
        exact_file: staged.exact_file().to_owned(),
        lexical_file: staged.lexical_file().to_owned(),
    };
    let pin = generations.pin().unwrap();
    let lock = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let pointer = PointerSink(service.lane_sender());
    let result = pointer.persist_active(&service.identity.binding_id, &generation);
    assert!(result.is_err());
    assert_eq!(
        ControlReader::open(&local)
            .unwrap()
            .active_projection()
            .unwrap()
            .unwrap(),
        before
    );
    assert!(generations.pin().is_ok());
    assert!(generations.take_retired().is_empty());
    assert!(before.2.is_file() && before.3.is_file());
    assert_eq!(std::fs::read(&before.2).unwrap(), exact_bytes);
    assert_eq!(std::fs::read(&before.3).unwrap(), lexical_bytes);
    lock.execute_batch("COMMIT").unwrap();
    pointer
        .persist_active(&service.identity.binding_id, &generation)
        .unwrap();
    generations.commit(staged).unwrap();
    assert_eq!(
        ControlReader::open(&local)
            .unwrap()
            .active_projection()
            .unwrap()
            .unwrap()
            .0,
        generation.id
    );
    drop(pin);
    assert!(generations
        .take_retired()
        .iter()
        .any(|(exact, _)| exact == &before.2));
}

#[test]
fn follow_up_failure_keeps_connection_response_and_diagnostics() {
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let project = id();
    let response = agentlaw_flows::ConnectResponse {
        code: None,
        project_connection: Some(agentlaw_flows::ProjectConnection {
            project_id: project.clone(),
            project_path: dir.path().to_string_lossy().into_owned(),
        }),
        candidates: vec![],
        recall_result: None,
    };
    let control = RequestControl::default();
    control.cancel.store(true, Ordering::Release);
    let result = service
        .finish_connect(
            response,
            Some(agentlaw_flows::apply::FollowUp {
                restore: true,
                recall_for: Some("context".into()),
            }),
            &control,
            &service.identity,
        )
        .unwrap();
    assert_eq!(result["project_connection"]["project_id"], project);
    assert!(result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["code"] == "exact_preparation_incomplete"));
    assert!(result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["code"] == "restore_recall_failed"));
}

#[test]
fn restore_request_pins_the_project_not_its_mutable_path() {
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let project_a = id();
    let project_b = id();
    let path = dir.path().to_string_lossy().into_owned();
    let db = rusqlite::Connection::open(service.identity.local.join("control.sqlite")).unwrap();
    db.execute(
        "INSERT INTO associations(binding,path,project) VALUES(?1,?2,?3)",
        rusqlite::params![service.identity.binding_id, path, project_a],
    )
    .unwrap();
    // A's connection result was already produced; the same path is now rebound to B.
    db.execute(
        "UPDATE associations SET project=?1 WHERE binding=?2 AND path=?3",
        rusqlite::params![project_b, service.identity.binding_id, path],
    )
    .unwrap();
    let result = restore_connected(Some(&project_a), Some("restore".into()), |request, id| {
        assert!(request.selected_project_id.is_none());
        assert!(request.project_path.is_none());
        let reader = ControlReader::open(&service.identity.local).unwrap();
        assert_eq!(
            reader
                .exact_association(&service.identity.binding_id, &path)
                .unwrap()
                .unwrap()
                .project_id,
            project_b
        );
        assert_ne!(id, project_b);
        Ok(json!({"project_id":id}))
    })
    .unwrap();
    assert_eq!(result["project_id"], project_a);
}

#[test]
fn interrupted_initialization_can_retry_before_marker_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    let identity = StoreIdentity {
        root: root.clone(),
        local: local.clone(),
        user_id: id(),
        machine_id: id(),
        binding_id: id(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    let invalid = dir.path().join("not-a-directory");
    std::fs::write(&invalid, b"file").unwrap();
    let mut invalid_deps = deps(dir.path());
    invalid_deps.coordination = invalid;
    let retry = StoreIdentity {
        root,
        local: local.clone(),
        user_id: identity.user_id.clone(),
        machine_id: identity.machine_id.clone(),
        binding_id: identity.binding_id.clone(),
        history_limit: identity.history_limit,
        response_limit: identity.response_limit,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    assert!(StoreService::open(identity, invalid_deps, &RequestControl::default()).is_err());
    assert!(!local.join("control-initialized.json").exists());
    assert!(!local.join("canonical/source-fence").exists());
    let service = StoreService::open(retry, deps(dir.path()), &RequestControl::default()).unwrap();
    assert!(local.join("canonical/source-fence").is_file());
    assert!(local.join("control-initialized.json").is_file());
    service.close();
}

#[test]
fn established_binding_with_missing_fence_is_not_reinitialized() {
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let retry = StoreIdentity {
        root: service.identity.root.clone(),
        local: service.identity.local.clone(),
        user_id: service.identity.user_id.clone(),
        machine_id: service.identity.machine_id.clone(),
        binding_id: service.identity.binding_id.clone(),
        history_limit: service.identity.history_limit,
        response_limit: service.identity.response_limit,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    let fence = retry.local.join("canonical/source-fence");
    let saved = retry.local.join("canonical/saved-fence");
    service.close();
    drop(service);
    std::fs::rename(&fence, &saved).unwrap();
    assert!(StoreService::open(retry, deps(dir.path()), &RequestControl::default()).is_err());
    assert!(!fence.exists());
    assert!(saved.is_file());
}

#[test]
fn startup_rediscovers_and_removes_retired_files() {
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let old = service
        .identity
        .local
        .join("projection")
        .join("exact-retired.sqlite");
    let lexical = old.with_file_name("lexical-retired.sqlite");
    std::fs::write(&old, b"old").unwrap();
    std::fs::write(&lexical, b"old").unwrap();
    let mut wal = old.as_os_str().to_owned();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    std::fs::write(&wal, b"old").unwrap();
    service.projection.wake();
    // Rediscovery occurs on startup, not merely on a wake.
    let identity = StoreIdentity {
        root: service.identity.root.clone(),
        local: service.identity.local.clone(),
        user_id: service.identity.user_id.clone(),
        machine_id: service.identity.machine_id.clone(),
        binding_id: service.identity.binding_id.clone(),
        history_limit: service.identity.history_limit,
        response_limit: service.identity.response_limit,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    service.close();
    drop(service);
    let reopened =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    wait_for(|| !old.exists() && !lexical.exists() && !wal.exists());
    reopened.close();
}

#[test]
fn corrupt_control_bytes_are_preserved_and_open_fails() {
    let (dir, identity) = fixture();
    let local = identity.local.clone();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let identity = StoreIdentity {
        root: service.identity.root.clone(),
        local: local.clone(),
        user_id: service.identity.user_id.clone(),
        machine_id: service.identity.machine_id.clone(),
        binding_id: service.identity.binding_id.clone(),
        history_limit: service.identity.history_limit,
        response_limit: service.identity.response_limit,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    service.close();
    drop(service);
    let file = local.join("control.sqlite");
    let damaged = b"damaged control bytes";
    std::fs::write(&file, damaged).unwrap();
    assert!(StoreService::open(identity, deps(dir.path()), &RequestControl::default()).is_err());
    assert_eq!(std::fs::read(file).unwrap(), damaged);
}

#[test]
fn corrupt_source_fence_is_preserved_and_open_fails() {
    let (dir, identity) = fixture();
    let local = identity.local.clone();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let identity = StoreIdentity {
        root: service.identity.root.clone(),
        local: local.clone(),
        user_id: service.identity.user_id.clone(),
        machine_id: service.identity.machine_id.clone(),
        binding_id: service.identity.binding_id.clone(),
        history_limit: service.identity.history_limit,
        response_limit: service.identity.response_limit,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    service.close();
    drop(service);
    let file = local.join("canonical/source-fence");
    let damaged = b"damaged source fence";
    std::fs::write(&file, damaged).unwrap();
    assert!(StoreService::open(identity, deps(dir.path()), &RequestControl::default()).is_err());
    assert_eq!(std::fs::read(file).unwrap(), damaged);
}

#[cfg(windows)]
#[test]
fn locked_retired_file_is_retried_on_next_start() {
    use std::os::windows::fs::OpenOptionsExt;
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let old = service
        .identity
        .local
        .join("projection")
        .join("exact-retired.sqlite");
    std::fs::write(&old, b"old").unwrap();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&old)
        .unwrap();
    let identity = StoreIdentity {
        root: service.identity.root.clone(),
        local: service.identity.local.clone(),
        user_id: service.identity.user_id.clone(),
        machine_id: service.identity.machine_id.clone(),
        binding_id: service.identity.binding_id.clone(),
        history_limit: service.identity.history_limit,
        response_limit: service.identity.response_limit,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    service.close();
    drop(service);
    let reopened =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    thread::sleep(Duration::from_millis(300));
    assert!(old.exists());
    let identity = StoreIdentity {
        root: reopened.identity.root.clone(),
        local: reopened.identity.local.clone(),
        user_id: reopened.identity.user_id.clone(),
        machine_id: reopened.identity.machine_id.clone(),
        binding_id: reopened.identity.binding_id.clone(),
        history_limit: reopened.identity.history_limit,
        response_limit: reopened.identity.response_limit,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    reopened.close();
    drop(reopened);
    drop(locked);
    let reopened =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    wait_for(|| !old.exists());
    reopened.close();
}

#[test]
fn sync_command_runs_git_work_through_the_store_handle() {
    let (dir, identity) = fixture();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let request = serde_json::from_value(
        json!({"command":"status","operation_id":"00000000-0000-4000-8000-000000000000"}),
    )
    .unwrap();
    let result = service.command(CommandRequest::Sync(request), &RequestControl::default());
    assert!(
        matches!(&result, Err(error) if error.code == "sync_operation_not_found"),
        "{result:?}"
    );
}
