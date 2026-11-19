use super::*;

fn fixture() -> (
    tempfile::TempDir,
    Store,
    SemanticConfig,
    SemanticIndexer,
    agentlaw_storage::published::SourcePosition,
) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("indexed", vec![])],
        )
        .unwrap();
    let target = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let indexer = SemanticIndexer::start(
        config.clone(),
        ModelRequestIds::new(),
        HealthSink::default(),
    )
    .unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    drive(&indexer, receipt.generation);
    (temp, store, config, indexer, target)
}

fn active(indexer: &SemanticIndexer) -> std::path::PathBuf {
    indexer.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")]
        .active
        .as_ref()
        .unwrap()
        .path
        .clone()
}

fn finish_repair(indexer: &SemanticIndexer) -> SemanticRepairProgress {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(work) = indexer.next_work("model") {
            indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
        }
        let progress = indexer.repair_progress("binding").unwrap();
        if progress.state != SemanticRepairState::Building {
            return progress;
        }
        assert!(Instant::now() < deadline, "repair timed out: {progress:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn search_once(
    indexer: &SemanticIndexer,
    target: &agentlaw_storage::published::SourcePosition,
) -> Result<agentlaw_flows::semantic::SemanticCandidates, SemanticError> {
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    let scopes = vec!["user".into()];
    search.search_available(
        SemanticQuery {
            binding: "binding",
            scopes: &scopes,
            text: "indexed",
            vector: Some(&vector),
            limit: 10,
            source: target,
        },
        &RequestControl::default(),
    )
}

#[test]
fn repair_keeps_old_view_until_reader_finishes_and_wait_target_waits() {
    let (_temp, store, config, indexer, target) = fixture();
    let old = active(&indexer);
    indexer.start_repair("binding").unwrap();
    let work = wait_work(&indexer);
    assert_eq!(search_once(&indexer, &target).unwrap().hits.len(), 1);
    let handle = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new())
        .preparation("binding");
    let expected = target.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        tx.send(handle.wait_with_snapshot(
            &expected,
            &RequestControl::default(),
            || test_snapshot(agentlaw_worker::WorkerState::Ready),
            Duration::from_millis(20),
        ))
        .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let reader_barrier = barrier.clone();
    *indexer.inner.readview_hook.lock().unwrap() = Some(Arc::new(move || {
        reader_barrier.wait();
        reader_barrier.wait();
    }));
    let reader_indexer = indexer.clone();
    let reader_target = target.clone();
    let reader = std::thread::spawn(move || search_once(&reader_indexer, &reader_target));
    barrier.wait();
    indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
    assert_eq!(finish_repair(&indexer).state, SemanticRepairState::Complete);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !indexer.inner.health.snapshot().iter().any(|(id, status)| {
        id.binding.as_deref() == Some("binding")
            && status
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("power-loss durability unverified"))
    }) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_ne!(active(&indexer), old);
    assert!(old.exists());
    assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
    waiter.join().unwrap();
    barrier.wait();
    assert_eq!(reader.join().unwrap().unwrap().hits.len(), 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while old.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!old.exists());
    *indexer.inner.readview_hook.lock().unwrap() = None;
    assert_eq!(search_once(&indexer, &target).unwrap().hits.len(), 1);
    let current = active(&indexer);
    assert!(indexer.shutdown(Duration::from_secs(5)));
    let reopened =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    reopened
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    reopened.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    while reopened.progress("binding").is_none_or(|p| !p.complete) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(active(&reopened), current);
    assert_eq!(search_once(&reopened, &target).unwrap().hits.len(), 1);
    assert!(reopened.shutdown(Duration::from_secs(5)));
}

#[test]
fn publication_wake_after_activation_does_not_retire_active_generation() {
    let (_temp, store, _config, indexer, target) = fixture();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let hook = barrier.clone();
    *indexer.inner.repair_published_hook.lock().unwrap() = Some(Arc::new(move || {
        hook.wait();
        hook.wait();
    }));
    indexer.start_repair("binding").unwrap();
    let work = wait_work(&indexer);
    indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
    barrier.wait();
    let published = active(&indexer);
    assert!(published.exists());
    let mut next = mutation("another memory", vec![]);
    next.unit.entity_id = "22222222-2222-4222-8222-222222222222".into();
    let receipt = store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![next])
        .unwrap();
    indexer.wake("binding");
    barrier.wait();
    assert_eq!(finish_repair(&indexer).state, SemanticRepairState::Complete);
    drive(&indexer, receipt.generation);
    assert_eq!(active(&indexer), published);
    assert!(published.exists());
    assert!(indexer
        .inner
        .state
        .lock()
        .unwrap()
        .retiring_readers
        .iter()
        .all(|(_, r)| r.retired.lock().unwrap().as_ref() != Some(&published)));
    let current = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    assert!(!search_once(&indexer, &current).unwrap().hits.is_empty());
    assert!(target.sequence < current.sequence);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn last_reader_after_reclaim_scan_is_removed_before_binding_closes() {
    let (_temp, _store, _config, indexer, target) = fixture();
    let old = active(&indexer);
    let reader_barrier = Arc::new(std::sync::Barrier::new(2));
    let hook = reader_barrier.clone();
    *indexer.inner.readview_hook.lock().unwrap() = Some(Arc::new(move || {
        hook.wait();
        hook.wait();
    }));
    let reader_indexer = indexer.clone();
    let reader = std::thread::spawn(move || search_once(&reader_indexer, &target));
    reader_barrier.wait();
    indexer.start_repair("binding").unwrap();
    assert_eq!(finish_repair(&indexer).state, SemanticRepairState::Complete);
    assert!(old.exists());
    let reclaim_barrier = Arc::new(std::sync::Barrier::new(2));
    let hook = reclaim_barrier.clone();
    *indexer.inner.reclaim_busy_hook.lock().unwrap() = Some(Arc::new(move || {
        hook.wait();
        hook.wait();
    }));
    let closing_indexer = indexer.clone();
    let closer = std::thread::spawn(move || closing_indexer.close_binding("binding"));
    reclaim_barrier.wait();
    reader_barrier.wait();
    let _ = reader.join().unwrap();
    reclaim_barrier.wait();
    closer.join().unwrap();
    assert!(!old.exists());
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn last_reader_releases_after_exit_snapshot_before_wait() {
    let (_temp, _store, _config, indexer, target) = fixture();
    let old = active(&indexer);
    let old_readers = indexer.inner.state.lock().unwrap().schedule.lanes
        [&LaneKey::current("binding")]
        .readers
        .clone();
    let reader_barrier = Arc::new(std::sync::Barrier::new(2));
    let hook = reader_barrier.clone();
    *indexer.inner.readview_hook.lock().unwrap() = Some(Arc::new(move || {
        hook.wait();
        hook.wait();
    }));
    let control = RequestControl::default();
    let cancel = control.cancel.clone();
    let reader_indexer = indexer.clone();
    let reader = std::thread::spawn(move || {
        let search = SemanticSearch::new(reader_indexer, crate::model::ModelHandle::new());
        let vector = QueryVector {
            model: "model".into(),
            values: Arc::from([1.0, 0.0]),
        };
        let scopes = vec!["user".into()];
        search.search_available(
            SemanticQuery {
                binding: "binding",
                scopes: &scopes,
                text: "indexed",
                vector: Some(&vector),
                limit: 10,
                source: &target,
            },
            &control,
        )
    });
    reader_barrier.wait();
    indexer.start_repair("binding").unwrap();
    assert_eq!(finish_repair(&indexer).state, SemanticRepairState::Complete);
    assert!(old.exists());
    let decision_barrier = Arc::new(std::sync::Barrier::new(2));
    let hook = decision_barrier.clone();
    *indexer.inner.reclaim_decision_hook.lock().unwrap() = Some(Arc::new(move || {
        hook.wait();
        hook.wait();
    }));
    let closing_indexer = indexer.clone();
    let closer = std::thread::spawn(move || closing_indexer.close_binding("binding"));
    decision_barrier.wait();
    cancel.store(true, Ordering::Release);
    reader_barrier.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while *old_readers.count.lock().unwrap() != 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    decision_barrier.wait();
    assert!(matches!(
        reader.join().unwrap(),
        Err(SemanticError::Cancelled)
    ));
    closer.join().unwrap();
    assert!(!old.exists());
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn validation_failure_preserves_active_generation() {
    let (_temp, _store, _config, indexer, target) = fixture();
    let old = active(&indexer);
    *indexer.inner.repair_validation_hook.lock().unwrap() = Some(Arc::new(corrupt_vectors_page));
    indexer.start_repair("binding").unwrap();
    let status = finish_repair(&indexer);
    assert_eq!(status.state, SemanticRepairState::Failed);
    assert_eq!(active(&indexer), old);
    assert_eq!(
        super::super::generation::selected(old.parent().unwrap()).unwrap(),
        "initial"
    );
    assert_eq!(search_once(&indexer, &target).unwrap().hits.len(), 1);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn interrupted_repair_is_discarded_on_restart() {
    let (_temp, store, config, indexer, target) = fixture();
    let old = active(&indexer);
    indexer.start_repair("binding").unwrap();
    let _unfinished = wait_work(&indexer);
    let id = indexer.repair_progress("binding").unwrap().generation;
    let shadow = super::super::generation::file(old.parent().unwrap(), &id);
    assert!(shadow.exists());
    assert!(indexer.shutdown(Duration::from_secs(5)));
    let sidecars: Vec<_> = ["-wal", "-shm"]
        .into_iter()
        .map(|suffix| {
            let mut name = shadow.as_os_str().to_owned();
            name.push(suffix);
            std::path::PathBuf::from(name)
        })
        .collect();
    let reopened =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    reopened
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    reopened.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    while reopened.progress("binding").is_none_or(|p| !p.complete) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(active(&reopened), old);
    assert!(!shadow.exists());
    assert!(sidecars.iter().all(|path| !path.exists()));
    assert_eq!(search_once(&reopened, &target).unwrap().hits.len(), 1);
    assert!(reopened.shutdown(Duration::from_secs(5)));
}

#[cfg(windows)]
#[test]
fn pointer_replace_failure_preserves_old_view() {
    use std::os::windows::fs::OpenOptionsExt;
    let (_temp, _store, _config, indexer, target) = fixture();
    let old = active(&indexer);
    let pointer = old.parent().unwrap().join("active-generation");
    std::fs::write(&pointer, b"initial").unwrap();
    indexer.start_repair("binding").unwrap();
    let work = wait_work(&indexer);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&pointer)
        .unwrap();
    indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
    assert_eq!(finish_repair(&indexer).state, SemanticRepairState::Failed);
    assert_eq!(std::fs::read(&pointer).unwrap(), b"initial");
    assert!(std::fs::read_dir(old.parent().unwrap())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".generation-")));
    assert_eq!(active(&indexer), old);
    assert_eq!(search_once(&indexer, &target).unwrap().hits.len(), 1);
    drop(lock);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[cfg(windows)]
#[test]
fn locked_retired_file_is_retried_on_restart() {
    use std::os::windows::fs::OpenOptionsExt;
    let (_temp, store, config, indexer, _target) = fixture();
    let old = active(&indexer);
    indexer.start_repair("binding").unwrap();
    let work = wait_work(&indexer);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&old)
        .unwrap();
    indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
    assert_eq!(finish_repair(&indexer).state, SemanticRepairState::Complete);
    let deadline = Instant::now() + Duration::from_secs(5);
    while indexer
        .inner
        .state
        .lock()
        .unwrap()
        .retiring_readers
        .iter()
        .any(|(_, r)| r.retired.lock().unwrap().as_ref() == Some(&old))
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(old.exists());
    drop(lock);
    assert!(indexer.shutdown(Duration::from_secs(5)));
    let reopened =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    reopened
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    reopened.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    while reopened.progress("binding").is_none_or(|p| !p.complete) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!old.exists());
    assert!(reopened.shutdown(Duration::from_secs(5)));
}

#[test]
fn active_corruption_aborts_waiter_and_allows_new_repair() {
    let (temp, _store, _config, indexer, target) = fixture();
    let original = active(&indexer);
    let copy = temp.path().join("corrupt-active.sqlite");
    let db = rusqlite::Connection::open_with_flags(
        &original,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    drop(db);
    corrupt_vectors_page(&copy);
    indexer.start_repair("binding").unwrap();
    let _work = wait_work(&indexer);
    let handle = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new())
        .preparation("binding");
    let expected = target.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        tx.send(handle.wait_with_snapshot(
            &expected,
            &RequestControl::default(),
            || test_snapshot(agentlaw_worker::WorkerState::Ready),
            Duration::from_millis(10),
        ))
        .unwrap();
    });
    indexer
        .inner
        .state
        .lock()
        .unwrap()
        .schedule
        .lanes
        .get_mut(&LaneKey::current("binding"))
        .unwrap()
        .active
        .as_mut()
        .unwrap()
        .path = copy;
    assert!(matches!(
        search_once(&indexer, &target),
        Err(SemanticError::Unavailable(_))
    ));
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Err(SemanticError::Unavailable(_))
    ));
    waiter.join().unwrap();
    assert_eq!(
        indexer.repair_progress("binding").unwrap().state,
        SemanticRepairState::Failed
    );
    indexer.start_repair("binding").unwrap();
    assert_eq!(finish_repair(&indexer).state, SemanticRepairState::Complete);
    assert_eq!(search_once(&indexer, &target).unwrap().hits.len(), 1);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn corrupt_active_file_at_start_can_be_repaired_without_a_readable_view() {
    use std::io::Write;
    let (_temp, store, config, indexer, target) = fixture();
    let old = active(&indexer);
    assert!(indexer.shutdown(Duration::from_secs(5)));
    let mut file = std::fs::OpenOptions::new().write(true).open(&old).unwrap();
    file.write_all(b"broken sqlite header").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let reopened =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    reopened
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    reopened.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !reopened.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")]
        .quarantined
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        search_once(&reopened, &target),
        Err(SemanticError::Unavailable(_))
    ));
    reopened.start_repair("binding").unwrap();
    assert_eq!(
        finish_repair(&reopened).state,
        SemanticRepairState::Complete
    );
    assert_eq!(search_once(&reopened, &target).unwrap().hits.len(), 1);
    assert!(reopened.shutdown(Duration::from_secs(5)));
}

#[test]
fn repair_retries_a_real_busy_writer_and_reloads_progress() {
    let (_temp, _store, _config, indexer, target) = fixture();
    let old = active(&indexer);
    indexer.start_repair("binding").unwrap();
    let work = wait_work(&indexer);
    let id = indexer.repair_progress("binding").unwrap().generation;
    let shadow = super::super::generation::file(old.parent().unwrap(), &id);
    let db = rusqlite::Connection::open(&shadow).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let progress = indexer.repair_progress("binding").unwrap();
        if progress.reason.is_some() {
            assert_eq!(progress.state, SemanticRepairState::Building);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(search_once(&indexer, &target).unwrap().hits.len(), 1);
    db.execute_batch("ROLLBACK").unwrap();
    drop(db);
    assert_eq!(finish_repair(&indexer).state, SemanticRepairState::Complete);
    assert_eq!(search_once(&indexer, &target).unwrap().hits.len(), 1);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn missing_pointed_generation_rebuilds_without_exposing_empty_view() {
    let (_temp, store, config, indexer, target) = fixture();
    let old = active(&indexer);
    assert!(indexer.shutdown(Duration::from_secs(5)));
    std::fs::write(
        old.parent().unwrap().join("active-generation"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    let reopened =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    reopened
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    reopened.model_ready("model");
    let work = wait_work(&reopened);
    assert!(matches!(
        search_once(&reopened, &target),
        Err(SemanticError::Unavailable(_))
    ));
    reopened.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
    assert_eq!(
        finish_repair(&reopened).state,
        SemanticRepairState::Complete
    );
    assert_eq!(search_once(&reopened, &target).unwrap().hits.len(), 1);
    assert!(active(&reopened).exists());
    assert!(reopened.shutdown(Duration::from_secs(5)));
}

#[test]
fn cleanup_failure_is_reported_without_blocking_search() {
    let (_temp, store, config, indexer, target) = fixture();
    let old = active(&indexer);
    assert!(indexer.shutdown(Duration::from_secs(5)));
    let blocked = old
        .parent()
        .unwrap()
        .join(format!(".generation-{}.tmp", uuid::Uuid::new_v4()));
    std::fs::create_dir(&blocked).unwrap();
    let reopened =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    reopened
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    reopened.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let progress = reopened.progress("binding").unwrap();
        if progress.state == ComponentState::Degraded
            && progress
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains(&blocked.to_string_lossy().to_string()))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cleanup failure was not reported: {progress:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(search_once(&reopened, &target).unwrap().hits.len(), 1);
    assert!(
        reopened.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")]
            .cleaned_dir
            .is_none()
    );
    assert!(reopened.shutdown(Duration::from_secs(5)));
}

#[test]
fn disappeared_pointer_and_initial_file_rebuilds_from_source() {
    let (_temp, store, config, indexer, target) = fixture();
    let old = active(&indexer);
    assert!(indexer.shutdown(Duration::from_secs(5)));
    let orphan =
        super::super::generation::file(old.parent().unwrap(), &uuid::Uuid::new_v4().to_string());
    std::fs::rename(&old, &orphan).unwrap();
    let reopened =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    reopened
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    reopened.model_ready("model");
    let work = wait_work(&reopened);
    assert!(matches!(
        search_once(&reopened, &target),
        Err(SemanticError::Unavailable(_))
    ));
    reopened.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
    assert_eq!(
        finish_repair(&reopened).state,
        SemanticRepairState::Complete
    );
    assert!(!orphan.exists());
    assert_eq!(search_once(&reopened, &target).unwrap().hits.len(), 1);
    assert!(reopened.shutdown(Duration::from_secs(5)));
}

#[test]
fn transition_does_not_hold_state_lock_and_handles_close_or_model_change() {
    for close in [false, true] {
        let (_temp, _store, _config, indexer, _target) = fixture();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let hook = barrier.clone();
        *indexer.inner.repair_transition_hook.lock().unwrap() = Some(Arc::new(move || {
            hook.wait();
            hook.wait();
        }));
        indexer.start_repair("binding").unwrap();
        let work = wait_work(&indexer);
        indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
        barrier.wait();
        assert!(indexer.progress("binding").is_some());
        if close {
            let other = indexer.clone();
            let thread = std::thread::spawn(move || other.close_binding("binding"));
            let deadline = Instant::now() + Duration::from_secs(5);
            while indexer.progress("binding").is_some() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            barrier.wait();
            thread.join().unwrap();
        } else {
            indexer.model_ready("new-model");
            barrier.wait();
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(work) = indexer.next_work("new-model") {
                    indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
                }
                if indexer
                    .progress("binding")
                    .is_some_and(|p| p.model_digest.as_deref() == Some("new-model") && p.complete)
                {
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                indexer.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")]
                    .active
                    .as_ref()
                    .unwrap()
                    .identity
                    .model_digest,
                "new-model"
            );
        }
        assert!(indexer.shutdown(Duration::from_secs(5)));
    }
}

#[test]
fn child_repair_crash_fixture() {
    let Some(root) = std::env::var_os("AGENTLAW_SEMANTIC_CRASH_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let store = Store::open(root.join("source"), root.join("local")).unwrap();
    let target = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let mut config = SemanticConfig::new(root.join("indices"), "config".into());
    config.dimension = 2;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    drive(&indexer, target.sequence);
    indexer.start_repair("binding").unwrap();
    let _ = finish_repair(&indexer);
    panic!("crash point was not reached");
}

#[test]
fn killed_process_at_pointer_and_ram_boundaries_recovers_a_valid_generation() {
    for point in [
        "before_flush",
        "after_flush",
        "before_rename",
        "after_rename",
        "before_ram",
        "after_ram",
    ] {
        let (temp, store, config, indexer, target) = fixture();
        let old = active(&indexer);
        assert!(indexer.shutdown(Duration::from_secs(5)));
        let marker = temp.path().join("crash-marker");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "semantic::tests::repair::child_repair_crash_fixture",
                "--nocapture",
            ])
            .env("AGENTLAW_SEMANTIC_CRASH_ROOT", temp.path())
            .env("AGENTLAW_SEMANTIC_CRASH_MARKER", &marker)
            .env("AGENTLAW_SEMANTIC_CRASH_POINT", point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("child failed at {point}: {status}");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("child timed out at {point}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        let reopened =
            SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
        reopened
            .open_binding("binding", store.owned_published_reader())
            .unwrap();
        reopened.model_ready("model");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(work) = reopened.next_work("model") {
                reopened.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
            }
            if reopened.progress("binding").is_some_and(|p| p.complete) {
                break;
            }
            assert!(Instant::now() < deadline, "restart timed out after {point}");
            std::thread::sleep(Duration::from_millis(5));
        }
        let selected = active(&reopened);
        assert!(selected.exists(), "dangling pointer after {point}");
        assert_eq!(search_once(&reopened, &target).unwrap().hits.len(), 1);
        assert!(std::fs::read_dir(old.parent().unwrap()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".generation-")));
        let active_name = selected.file_name().unwrap().to_string_lossy().into_owned();
        for entry in std::fs::read_dir(old.parent().unwrap()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if (name.starts_with("vector-") || name.starts_with("vector.sqlite"))
                && (name.ends_with(".sqlite")
                    || name.ends_with(".sqlite-wal")
                    || name.ends_with(".sqlite-shm"))
            {
                assert!(
                    name.starts_with(&active_name),
                    "orphan after {point}: {name}"
                );
            }
        }
        assert!(reopened.shutdown(Duration::from_secs(5)));
    }
}

#[cfg(unix)]
#[test]
fn child_namespace_sync_failure_fixture() {
    let Some(root) = std::env::var_os("AGENTLAW_SEMANTIC_SYNC_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let outcome = super::super::generation::publish(&root.join("generation"), "initial");
    let result = if matches!(
        outcome,
        Ok(super::super::generation::Publication::ReplacedButUnsynced(
            _
        ))
    ) {
        "unsynced"
    } else {
        "unexpected"
    };
    std::fs::write(root.join("result"), result).unwrap();
}

#[cfg(unix)]
#[test]
fn namespace_sync_io_failure_is_distinct_from_replace_failure() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("generation");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("vector.sqlite"), b"old").unwrap();
    let marker = temp.path().join("marker");
    let resume = temp.path().join("resume");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "semantic::tests::repair::child_namespace_sync_failure_fixture",
        ])
        .env("AGENTLAW_SEMANTIC_SYNC_ROOT", temp.path())
        .env("AGENTLAW_SEMANTIC_CRASH_POINT", "after_rename")
        .env("AGENTLAW_SEMANTIC_CRASH_MARKER", &marker)
        .env("AGENTLAW_SEMANTIC_RESUME_MARKER", &resume)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !marker.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let moved = temp.path().join("moved");
    std::fs::rename(&dir, &moved).unwrap();
    std::fs::write(&resume, b"continue").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("result")).unwrap(),
        "unsynced"
    );
    assert_eq!(std::fs::read(moved.join("vector.sqlite")).unwrap(), b"old");
    assert_eq!(
        std::fs::read_to_string(moved.join("active-generation")).unwrap(),
        "initial"
    );
}
