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

fn configured_store_model(
    dir: &std::path::Path,
    missing_assets: bool,
) -> (
    StoreDeps,
    crate::model::ModelHandle,
    crate::host::supervision::Supervisor,
) {
    use crate::{
        host::supervision::Supervisor,
        model::{run_model_component, ModelHandle},
        ports::{ComponentId, ComponentKind, HealthSink},
        projection::ProjectionConfig,
        semantic::{SemanticConfig, SemanticIndexer, SemanticSearch},
        test_support::{model_assets, model_child_executable},
    };
    let ids = agentlaw_worker::supervisor::ModelRequestIds::new();
    let health = HealthSink::default();
    let indexer = SemanticIndexer::start(
        SemanticConfig::new(dir.join("semantic"), "test".into()),
        ids.clone(),
        health.clone(),
    )
    .unwrap();
    let handle = ModelHandle::new();
    handle.set_warm(true);
    let waker = handle.clone();
    indexer.set_model_waker(Arc::new(move || waker.wake()));
    let mut assets = model_assets(&dir.join("assets"));
    if missing_assets {
        assets.onnx_model = dir.join("assets/missing.onnx");
    }
    std::fs::create_dir_all(dir.join("worker")).unwrap();
    let config = agentlaw_worker::RuntimeConfig {
        state_dir: dir.join("worker"),
        executable: model_child_executable(),
        model: Some(assets),
    };
    let supervisor = Supervisor::new(health.clone());
    let work = indexer.clone();
    let sink = health.clone();
    let worker_handle = handle.clone();
    supervisor
        .spawn(
            ComponentId {
                binding: None,
                kind: ComponentKind::Model,
            },
            move |stop| {
                run_model_component(
                    config.clone(),
                    Arc::new(work.clone()),
                    ids.clone(),
                    sink.clone(),
                    worker_handle.clone(),
                    stop,
                )
            },
        )
        .unwrap();
    let dependencies = StoreDeps {
        health,
        semantic: Arc::new(SemanticSearch::new(indexer.clone(), handle.clone())),
        indexer,
        projection: ProjectionConfig::new(),
        coordination: dir.join("coordination"),
        state: dir.join("state"),
    };
    (dependencies, handle, supervisor)
}

#[test]
fn failing_configured_model_keeps_store_operations_available_and_cancellation_unpublished() {
    use crate::ports::ComponentKind;
    let (dir, identity) = fixture();
    let (dependencies, handle, supervisor) = configured_store_model(dir.path(), true);
    let indexer = dependencies.indexer.clone();
    let health = dependencies.health.clone();
    let service = StoreService::open(identity, dependencies, &RequestControl::default()).unwrap();
    wait_for(|| {
        health.snapshot().iter().any(|(id, status)| {
            id.kind == ComponentKind::Model
                && status
                    .reason
                    .as_deref()
                    .is_some_and(|r| r.contains("fixture model assets missing"))
        })
    });
    let before = service.source.read_snapshot().unwrap().position().clone();
    let remembered = service.command(CommandRequest::Remember(serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"Lexical persistence survives model failure.","evidence":"Observed","applies_to":["user"]}]})).unwrap()), &RequestControl::default()).unwrap();
    assert_eq!(remembered["status"], "remembered");
    assert!(service.source.read_snapshot().unwrap().position().sequence > before.sequence);
    let memory_id = remembered["results"][0]["memory_ref"]["memory_id"].clone();
    assert_ne!(handle.snapshot().state, agentlaw_worker::WorkerState::Ready);
    let query_control = RequestControl::default();
    let recalled = thread::scope(|scope| {
        let (done_tx, done_rx) = mpsc::channel();
        let service = &service;
        let control = query_control.clone();
        let memory_id = memory_id.clone();
        scope.spawn(move || {
            let result = service.query(
                QueryRequest::Recall(serde_json::from_value(json!({"recall_for":"Lexical persistence survives","memory_ids":[memory_id]})).unwrap()),
                &control,
            );
            let _ = done_tx.send(result);
        });
        match done_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(result) => result.unwrap(),
            Err(error) => {
                query_control.cancel.store(true, Ordering::Release);
                panic!("recall did not complete while the configured model remained unavailable: {error}");
            }
        }
    });
    assert_ne!(handle.snapshot().state, agentlaw_worker::WorkerState::Ready);
    assert_eq!(recalled["semantic_complete"], false, "{recalled}");
    assert_eq!(
        recalled["memories"][0]["current_heads"][0]["what_to_remember"],
        "Lexical persistence survives model failure."
    );
    let lexical = service
        .query(
            QueryRequest::Recall(
                serde_json::from_value(json!({"recall_for":"Lexical persistence survives"}))
                    .unwrap(),
            ),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(lexical["semantic_complete"], false, "{lexical}");
    assert!(
        lexical["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["memory_id"] == memory_id)
            || lexical["memories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["memory_id"] == memory_id),
        "{lexical}"
    );
    assert!(
        recalled["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"]
                .as_str()
                .is_some_and(|c| c.starts_with("semantic_channel_"))),
        "{recalled}"
    );
    let before_cancel = service.source.read_snapshot().unwrap().position().clone();
    let control = RequestControl::default();
    control.cancel.store(true, Ordering::Release);
    let cancelled = service.command(CommandRequest::Remember(serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"This cancelled observation must never publish.","evidence":"Observed","applies_to":["user"]}]})).unwrap()), &control);
    assert!(matches!(cancelled, Err(error) if error.code == "cancelled"));
    assert_eq!(
        service.source.read_snapshot().unwrap().position(),
        &before_cancel
    );
    let path = dir.path().join("project");
    std::fs::create_dir(&path).unwrap();
    let connected = service.command(CommandRequest::Connect(serde_json::from_value(json!({"project_path":path,"intent":"create","project_name":"Failing model project","restore_context":true,"recall_for":"Restore project context"})).unwrap()), &RequestControl::default()).unwrap();
    let reader = ControlReader::open(&service.identity.local).unwrap();
    assert_eq!(
        reader
            .exact_association(&service.identity.binding_id, path.to_str().unwrap())
            .unwrap()
            .unwrap()
            .project_id,
        connected["project_connection"]["project_id"]
            .as_str()
            .unwrap()
    );
    service.close();
    supervisor.shutdown();
    assert!(indexer.shutdown(Duration::from_secs(10)));
    assert_eq!(
        connected["code"], "restore_preparation_incomplete",
        "{connected}"
    );
    assert!(
        connected["turn_instruction"]
            .as_str()
            .is_some_and(|s| s.contains("Repeat") || s.contains("resume")),
        "{connected}"
    );
}

#[test]
fn restore_after_model_crash_keeps_incomplete_guidance_with_mandatory_content_in_a_file() {
    let (dir, mut identity) = fixture();
    identity.response_limit = 4096;
    let (dependencies, handle, supervisor) = configured_store_model(dir.path(), false);
    let indexer = dependencies.indexer.clone();
    let service = StoreService::open(identity, dependencies, &RequestControl::default()).unwrap();
    let body = "A mandatory rule must be read in full before resuming project work.\n".repeat(600);
    let saved = service.command(
        CommandRequest::Remember(serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed mandatory context","applies_to":["user"],"is_rule":true}]})).unwrap()),
        &RequestControl::default(),
    ).unwrap();
    assert_eq!(saved["status"], "remembered");
    let memory_id = saved["results"][0]["memory_ref"]["memory_id"].clone();
    wait_for(|| {
        handle.snapshot().state == agentlaw_worker::WorkerState::Ready
            && indexer
                .progress(&service.identity.binding_id)
                .is_some_and(|p| p.complete)
    });
    let target = service.source.read_snapshot().unwrap().position().clone();
    let prepared = service.ensure_target(target, &RequestControl::default());
    assert!(matches!(prepared.exact, ExactPreparation::Complete { .. }));
    assert!(matches!(
        prepared.semantic,
        Ok(SemanticCoverage::CompleteAt(_))
    ));
    // Only the subsequent restore query crashes the child; source indexing can finish first.
    let crash_query = "restore query crashes the ready child";
    std::fs::write(dir.path().join("assets/crash-query"), crash_query).unwrap();
    let path = dir.path().join("file-restore-project");
    std::fs::create_dir(&path).unwrap();
    let result = service.command(
        CommandRequest::Connect(serde_json::from_value(json!({"project_path":path,"intent":"create","project_name":"File restore project","restore_context":true,"recall_for":crash_query})).unwrap()),
        &RequestControl::default(),
    ).unwrap();
    assert!(
        dir.path().join("assets/crashed").is_file(),
        "restore query did not reach the ready child"
    );
    assert_eq!(
        result["recall_result"]["code"], "complete_content_in_file",
        "{result}"
    );
    assert_eq!(result["recall_result"]["content_read"], false, "{result}");
    let artifact = result["recall_result"]["artifact"]["path"]
        .as_str()
        .unwrap();
    let delivered: Value = serde_json::from_slice(&std::fs::read(artifact).unwrap()).unwrap();
    assert_eq!(delivered["semantic_complete"], false, "{delivered}");
    assert!(delivered["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == "semantic_channel_incomplete"));
    let rule = delivered["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["memory_id"] == memory_id)
        .unwrap();
    assert_eq!(rule["current_heads"][0]["what_to_remember"], body);
    let reader = ControlReader::open(&service.identity.local).unwrap();
    let association = reader
        .exact_association(&service.identity.binding_id, path.to_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        result["project_connection"]["project_id"],
        association.project_id
    );
    assert!(
        !result["diagnostics"].as_array().is_some_and(|items| items
            .iter()
            .any(|d| d["code"] == "semantic_preparation_incomplete")),
        "the child must crash after successful preparation, not during it: {result}"
    );
    service.close();
    supervisor.shutdown();
    assert!(indexer.shutdown(Duration::from_secs(10)));
    assert_eq!(result["code"], "restore_preparation_incomplete", "{result}");
    assert!(
        result["turn_instruction"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{result}"
    );
    assert!(result["project_connection"].is_object());
    assert!(result["recall_result"].is_object());
}

fn retained_storage_image(root: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(
        base: &std::path::Path,
        path: &std::path::Path,
        files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        if !path.exists() {
            return;
        }
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(base, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(base).unwrap().to_owned(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, &root.join("source"), &mut files);
    visit(root, &root.join("local/canonical"), &mut files);
    for name in ["control.sqlite", "control.sqlite-wal"] {
        let path = root.join("local").join(name);
        if path.exists() {
            files.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                std::fs::read(path).unwrap(),
            );
        }
    }
    files
}

#[test]
fn corruption_at_open_preserves_retained_proposals_and_decided_recovery_material() {
    use agentlaw_storage::{CurrentUnit, FaultPoint, Head, Mutation, UnitState};
    for damaged_file in ["control.sqlite", "canonical/source-fence"] {
        let (dir, identity) = fixture();
        let dependencies = deps(dir.path());
        let indexer = dependencies.indexer.clone();
        let service =
            StoreService::open(identity, dependencies, &RequestControl::default()).unwrap();
        let first = service.command(CommandRequest::Remember(serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"An exact duplicate requires explicit review.","evidence":"Observed","applies_to":["user"]}]})).unwrap()), &RequestControl::default()).unwrap();
        assert_eq!(first["status"], "remembered");
        let pending = service.command(CommandRequest::Remember(serde_json::from_value(json!({"memories":[{"operation":"create","what_to_remember":"An exact duplicate requires explicit review.","evidence":"Observed","applies_to":["user"]}]})).unwrap()), &RequestControl::default()).unwrap();
        assert_eq!(pending["status"], "resolution_required");
        let reference = serde_json::from_value(pending["pending_batch_ref"].clone()).unwrap();
        let retained = ControlReader::open(&service.identity.local)
            .unwrap()
            .pending_inspect(&service.identity.binding_id, &reference)
            .unwrap();
        assert_eq!(retained["status"], "pending");
        let identity = StoreIdentity {
            root: service.identity.root.clone(),
            local: service.identity.local.clone(),
            user_id: service.identity.user_id.clone(),
            machine_id: service.identity.machine_id.clone(),
            binding_id: service.identity.binding_id.clone(),
            history_limit: service.identity.history_limit,
            response_limit: service.identity.response_limit,
            config_file: service.identity.config_file.clone(),
        };
        service.close();
        drop(service);
        assert!(indexer.shutdown(Duration::from_secs(10)));
        let store = Store::open_with_coordination(
            &identity.root,
            identity.local.join("canonical"),
            dir.path().join("coordination"),
        )
        .unwrap();
        let result = store.publish_with_fault(&id(), vec![Mutation { unit: CurrentUnit { entity_id: id(), entity_type:"memory".into(), state:UnitState::Live { heads:vec![Head { metadata:json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":identity.machine_id,"user_id":identity.user_id},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}), body:"Decided recovery data must remain available.".into() }] } }, expected_versions:vec![], evidence:"Interrupted publication".into() }], Some(FaultPoint::Decision));
        assert!(matches!(
            result,
            Err(agentlaw_storage::Error::Interrupted(FaultPoint::Decision))
        ));
        drop(store);
        let before_damage = retained_storage_image(dir.path());
        assert!(
            before_damage
                .values()
                .any(|bytes| String::from_utf8_lossy(bytes)
                    .contains("Decided recovery data must remain available.")),
            "recovery material must exist before corruption"
        );
        let file = identity.local.join(damaged_file);
        if damaged_file == "control.sqlite" {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new().write(true).open(file).unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(b"damaged control!").unwrap();
            file.sync_all().unwrap();
        } else {
            std::fs::write(file, b"damaged source fence").unwrap();
        }
        let baseline = retained_storage_image(dir.path());
        let reopened = StoreService::open(identity, deps(dir.path()), &RequestControl::default());
        let error = match reopened {
            Err(error) => error,
            Ok(service) => {
                service.close();
                panic!("corrupt binding opened successfully")
            }
        };
        assert_eq!(
            retained_storage_image(dir.path()),
            baseline,
            "opening {damaged_file} changed retained or recovery bytes"
        );
        assert!(
            error.to_string().to_lowercase().contains("corrupt")
                || error.to_string().contains("database")
                || error.to_string().contains("fence"),
            "{error:?}"
        );
    }
}
