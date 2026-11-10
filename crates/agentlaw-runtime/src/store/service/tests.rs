use super::*;
use crate::{model::ModelHandle, semantic::SemanticConfig, store::lane::SyncOperationRow};
use agentlaw_worker::supervisor::ModelRequestIds;
use std::{sync::mpsc, thread, time::Instant};

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn fixture() -> (tempfile::TempDir, StoreIdentity) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&local).unwrap();
    drop(Store::open(&root, local.join("canonical")).unwrap());
    let identity = StoreIdentity {
        root,
        local,
        user_id: id(),
        machine_id: id(),
        binding_id: id(),
        history_limit: 8192,
        response_limit: 65536,
    };
    (dir, identity)
}
fn deps(dir: &std::path::Path) -> StoreDeps {
    let health = HealthSink::default();
    let indexer = SemanticIndexer::start(
        SemanticConfig::new(dir.join("semantic"), "test".into()),
        ModelRequestIds::new(),
        health.clone(),
    )
    .unwrap();
    let semantic = Arc::new(SemanticSearch::new(indexer.clone(), ModelHandle::new()));
    StoreDeps {
        health,
        semantic,
        indexer,
        projection: ProjectionConfig::new(),
        coordination: dir.join("coordination"),
    }
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < until, "condition timed out");
        thread::sleep(Duration::from_millis(20));
    }
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
    };
    reopened.close();
    drop(reopened);
    drop(locked);
    let reopened =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    wait_for(|| !old.exists());
    reopened.close();
}
