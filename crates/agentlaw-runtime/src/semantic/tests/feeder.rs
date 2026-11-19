use super::*;

#[test]
fn real_store_inventory_and_incremental_search() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("alpha", vec![])],
        )
        .unwrap();
    let old_version = receipt.references[0].observed_version.clone();
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
    assert!(indexer.status().awaiting_model);
    indexer.model_ready("model");
    assert_eq!(drive(&indexer, receipt.generation), 1);
    let progress = indexer.progress("binding").unwrap();
    let epoch = progress.ack.unwrap().epoch;
    let path = std::fs::read_dir(&config.indices_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("vector.sqlite");
    let identity = VectorFileIdentity {
        binding: "binding".into(),
        kind: VectorFileKind::Current,
        epoch,
        model_digest: "model".into(),
        config: "config".into(),
        generation: "initial".into(),
        dimension: 2,
    };
    let view = VectorFileReadView::open(&path, &identity).unwrap();
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    assert_eq!(
        view.search(&[1.0, 0.0], &scope, 10).unwrap().hits[0].change_id,
        old_version
    );
    drop(view);
    let updated = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("beta", vec![old_version.clone()])],
        )
        .unwrap();
    indexer.wake("binding");
    assert_eq!(drive(&indexer, updated.generation), 1);
    let view = VectorFileReadView::open(&path, &identity).unwrap();
    let hits = view.search(&[1.0, 0.0], &scope, 10).unwrap().hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].change_id, updated.references[0].observed_version);
    drop(view);
    let db =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    for table in ["vectors", "vector_buckets"] {
        let count: i64 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE change_id = ?1"),
                [&old_version],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "old {table} rows remain");
    }
    drop(db);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn model_waker_is_reentrant_and_reports_queue_and_retry() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("alpha", vec![])],
        )
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let waker_calls = calls.clone();
    let reentrant = indexer.clone();
    indexer.set_model_waker(Arc::new(move || {
        let _ = reentrant.status();
        let _ = reentrant.next_work("different-model");
        waker_calls.fetch_add(1, Ordering::SeqCst);
    }));
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    let before_queue = calls.load(Ordering::SeqCst);
    indexer.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !indexer.status().runnable {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let work = wait_work(&indexer);
    assert!(calls.load(Ordering::SeqCst) > before_queue);
    let before = calls.load(Ordering::SeqCst);
    indexer.finish(work.id, Err(EmbeddingError::Failed("test failure".into())));
    assert!(calls.load(Ordering::SeqCst) > before);
    assert!(indexer.status().next_retry_at.is_some());
    assert!(!indexer.status().runnable);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn stale_model_result_is_discarded_after_publication() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let old = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("old", vec![])],
        )
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let stale = wait_work(&indexer);
    let new = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(
                "new",
                vec![old.references[0].observed_version.clone()],
            )],
        )
        .unwrap();
    indexer.wake("binding");
    indexer.finish(stale.id, Ok(fixture_result(&stale, vec![1.0, 0.0])));
    let fresh = wait_work(&indexer);
    assert_eq!(indexed_text(&fresh), "new");
    indexer.finish(fresh.id, Ok(fixture_result(&fresh, vec![1.0, 0.0])));
    drive(&indexer, new.generation);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn invalidation_after_batch_extraction_before_commit_discards_batch() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Barrier;
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let base = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("base", vec![])],
        )
        .unwrap();
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
    drive(&indexer, base.generation);
    let stale = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(
                "stale",
                vec![base.references[0].observed_version.clone()],
            )],
        )
        .unwrap();
    indexer.wake("binding");
    let work = begin_embedding(&indexer, usize::MAX);
    let barrier = Arc::new(Barrier::new(2));
    let at_commit = barrier.clone();
    let once = Arc::new(AtomicBool::new(false));
    let once_hook = once.clone();
    *indexer.inner.pre_commit_hook.lock().unwrap() = Some(Arc::new(move || {
        if !once_hook.swap(true, Ordering::SeqCst) {
            at_commit.wait();
            at_commit.wait();
        }
    }));
    indexer.finish(work.id, Ok(fixture_result(&work, vec![1.0, 0.0])));
    barrier.wait();
    let latest = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(
                "latest",
                vec![stale.references[0].observed_version.clone()],
            )],
        )
        .unwrap();
    indexer.wake("binding");
    barrier.wait();
    let latest_work = wait_work(&indexer);
    assert_eq!(indexed_text(&latest_work), "latest");
    let path = std::fs::read_dir(&config.indices_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("vector.sqlite");
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    for table in ["vectors", "vector_buckets"] {
        let count: i64 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE change_id=?1"),
                [&stale.references[0].observed_version],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
    assert_eq!(
        indexer.progress("binding").unwrap().ack.unwrap().sequence,
        base.generation as i64
    );
    drop(db);
    indexer.finish(
        latest_work.id,
        Ok(fixture_result(&latest_work, vec![1.0, 0.0])),
    );
    drive(&indexer, latest.generation);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn post_check_race_is_cleaned_before_updated_ack() {
    race_after_final_check(false);
}

#[test]
fn post_check_race_redirect_removes_all_source_rows_before_ack() {
    race_after_final_check(true);
}

#[test]
fn bounded_result_slots_are_returned_after_commit() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("abcdefgh", vec![])],
        )
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    config.pending_results = 1;
    config.commit_pieces = 1;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let first = begin_embedding(&indexer, 2);
    assert!(indexer.next_work("model").is_none());
    indexer.finish(first.id, Ok(fixture_result(&first, vec![1.0, 0.0])));
    let second = wait_work(&indexer);
    indexer.finish(second.id, Ok(fixture_result(&second, vec![1.0, 0.0])));
    drive(&indexer, receipt.generation);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn failed_inflight_piece_respects_queue_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("abcdefgh", vec![])],
        )
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    config.queue_pieces = 1;
    config.pending_results = 1;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let work = begin_embedding(&indexer, 2);
    std::thread::sleep(Duration::from_millis(100));
    {
        let state = indexer.inner.state.lock().unwrap();
        let lane = &state.schedule.lanes[&LaneKey::current("binding")];
        assert_eq!(lane.queue.len(), 0);
        assert_eq!(lane.running.len(), 1);
    }
    indexer.finish(work.id, Err(EmbeddingError::Failed("retry".into())));
    {
        let state = indexer.inner.state.lock().unwrap();
        let lane = &state.schedule.lanes[&LaneKey::current("binding")];
        assert_eq!(lane.queue.len(), 1);
        assert!(lane.queue.len() + lane.running.len() <= 1);
    }
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn incremental_update_does_not_recompute_other_memories() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let first = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("first", vec![])],
        )
        .unwrap();
    let mut other = mutation("second", vec![]);
    other.unit.entity_id = "55555555-5555-4555-8555-555555555555".into();
    let second = store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![other])
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    assert_eq!(drive(&indexer, second.generation), 2);
    let updated = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(
                "changed",
                vec![first.references[0].observed_version.clone()],
            )],
        )
        .unwrap();
    indexer.wake("binding");
    assert_eq!(drive(&indexer, updated.generation), 1);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn concurrent_heads_are_both_kept() {
    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &to.join(entry.file_name()));
            } else {
                std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
            }
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("active");
    let active = Store::open(&root, temp.path().join("active-local")).unwrap();
    let base = active
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("common", vec![])],
        )
        .unwrap();
    let incoming_root = temp.path().join("incoming");
    copy_dir(&root, &incoming_root);
    let incoming =
        Store::attach_existing(&incoming_root, temp.path().join("incoming-local")).unwrap();
    for (store, body) in [(&active, "left"), (&incoming, "right")] {
        store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![mutation(
                    body,
                    vec![base.references[0].observed_version.clone()],
                )],
            )
            .unwrap();
    }
    let staged = active
        .prepare_import_union(
            &incoming,
            temp.path().join("review"),
            temp.path().join("review-local"),
        )
        .unwrap();
    let heads = staged.read_current(ID).unwrap().references().unwrap();
    assert_eq!(heads.len(), 2);
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let indexer = SemanticIndexer::start(
        config.clone(),
        ModelRequestIds::new(),
        HealthSink::default(),
    )
    .unwrap();
    indexer
        .open_binding("binding", staged.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let target = staged
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .sequence;
    assert_eq!(drive(&indexer, target), 2);
    let path = std::fs::read_dir(&config.indices_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("vector.sqlite");
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    for head in heads {
        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM vectors WHERE change_id = ?1",
                [head.observed_version],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }
    drop(db);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn acquired_stream_splits_utf8_deterministically() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let body = "한글 alpha 끝";
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(body, vec![])],
        )
        .unwrap();
    let reader = store.owned_published_reader();
    let acquired = reader.read_snapshot().unwrap().acquire_current(ID).unwrap();
    let (heads, pieces) = source::map_acquired(acquired.clone(), ID, false).unwrap();
    let pieces = pieces.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(heads.change_ids.len(), 1);
    assert_eq!(
        pieces.iter().map(|p| p.text.as_str()).collect::<String>(),
        body
    );
    assert!(pieces.iter().all(|p| fixture_ranges(&p.text, 5)
        .into_iter()
        .all(|(start, end)| end - start <= 5)));
    let (_, again) = source::map_acquired(acquired, ID, false).unwrap();
    assert_eq!(pieces, again.collect::<Result<Vec<_>, _>>().unwrap());
}

struct FailedPiece {
    _temp: tempfile::TempDir,
    store: Store,
    indexer: SemanticIndexer,
    config: SemanticConfig,
    health: HealthSink,
    elapsed: Arc<std::sync::atomic::AtomicU64>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    version: String,
}

fn finish_window(indexer: &SemanticIndexer, work: &IndexWork) -> IndexWork {
    if let agentlaw_worker::supervisor::IndexTask::Split(text) = &work.task {
        indexer.finish(
            work.id,
            Ok(IndexResult::Pieces(vec![(0, text.len() as u32)])),
        );
        wait_work(indexer)
    } else {
        work.clone()
    }
}

impl FailedPiece {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
        let receipt = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![mutation("oversized piece", vec![])],
            )
            .unwrap();
        let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
        config.dimension = 2;
        let health = HealthSink::default();
        let base = Instant::now();
        let elapsed = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let time = elapsed.clone();
        let clock = Arc::new(move || base + Duration::from_secs(time.load(Ordering::SeqCst)));
        let indexer = SemanticIndexer::start_with_clock(
            config.clone(),
            ModelRequestIds::new(),
            health.clone(),
            clock.clone(),
        )
        .unwrap();
        indexer
            .open_binding("binding", store.owned_published_reader())
            .unwrap();
        indexer.model_ready("model");
        let work = finish_window(&indexer, &wait_work(&indexer));
        assert_eq!(indexed_text(&work), "oversized piece");
        indexer.finish(work.id, Err(EmbeddingError::InputTooLong));
        Self {
            _temp: temp,
            store,
            indexer,
            config,
            health,
            elapsed,
            clock,
            version: receipt.references[0].observed_version.clone(),
        }
    }
}

#[test]
fn oversized_piece_does_not_retry_and_reports_health() {
    let f = FailedPiece::new();
    f.elapsed.store(120, Ordering::SeqCst);
    let status = f.indexer.status();
    let retry = f.indexer.next_work("model");
    let health = f.health.snapshot();
    assert_eq!(
        (
            retry.is_some(),
            status.next_retry_at.is_some(),
            status.runnable,
            f.indexer.progress("binding").unwrap().ack.is_some(),
            health.iter().any(|(component, state)| {
                component.binding.as_deref() == Some("binding")
                    && state
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("model token limit"))
            })
        ),
        (false, false, false, false, true)
    );
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn oversized_piece_leaves_other_binding_runnable() {
    let f = FailedPiece::new();
    let other = Store::open(
        f._temp.path().join("other"),
        f._temp.path().join("other-local"),
    )
    .unwrap();
    let receipt = other
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("other binding", vec![])],
        )
        .unwrap();
    f.indexer
        .open_binding("other", other.owned_published_reader())
        .unwrap();
    let work = finish_window(&f.indexer, &wait_work(&f.indexer));
    assert_eq!(indexed_text(&work), "other binding");
    f.indexer
        .finish(work.id, Ok(fixture_result(&work, vec![1.0, 0.0])));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f
        .indexer
        .progress("other")
        .unwrap()
        .ack
        .is_some_and(|a| a.sequence == receipt.generation as i64)
    {
        assert!(Instant::now() < deadline, "other binding never committed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(f.indexer.progress("binding").unwrap().ack.is_none());
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn new_revision_unblocks_oversized_piece() {
    let f = FailedPiece::new();
    let receipt = f
        .store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("corrected piece", vec![f.version.clone()])],
        )
        .unwrap();
    f.indexer.wake("binding");
    let work = finish_window(&f.indexer, &wait_work(&f.indexer));
    assert_eq!(indexed_text(&work), "corrected piece");
    f.indexer
        .finish(work.id, Ok(fixture_result(&work, vec![1.0, 0.0])));
    assert_eq!(drive(&f.indexer, receipt.generation), 0);
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn restart_attempts_oversized_piece_once_then_blocks_again() {
    let f = FailedPiece::new();
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
    let indexer = SemanticIndexer::start_with_clock(
        f.config.clone(),
        ModelRequestIds::new(),
        HealthSink::default(),
        f.clock.clone(),
    )
    .unwrap();
    indexer
        .open_binding("binding", f.store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let work = finish_window(&indexer, &wait_work(&indexer));
    assert_eq!(indexed_text(&work), "oversized piece");
    indexer.finish(work.id, Err(EmbeddingError::InputTooLong));
    f.elapsed.store(120, Ordering::SeqCst);
    assert_eq!(
        (
            indexer.next_work("model").is_some(),
            indexer.status().next_retry_at,
            indexer.progress("binding").unwrap().ack
        ),
        (false, None, None)
    );
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[derive(Clone, Copy)]
enum Handoff {
    Normal,
    RepeatedBytes,
    OneResult,
    Evicted,
    Restarted,
    DifferentModel,
}

fn observe_handoff(case: Handoff) {
    use crate::host::supervision::Supervisor;
    use crate::model::{run_model_component, ModelHandle};
    use crate::ports::{ComponentId, ComponentKind};
    use crate::semantic::recent_vectors::RecentVectors;
    use crate::test_support::{
        model_assets, model_child_executable, model_transcript, FakeIndexWork,
    };
    use agentlaw_flows::semantic::PieceVectorReuse;
    use agentlaw_worker::{supervisor::IndexTask, RuntimeConfig, WorkerState};

    let temp = tempfile::tempdir().unwrap();
    let assets_root = temp.path().join("assets");
    let body = if matches!(case, Handoff::RepeatedBytes) {
        "aa|aa|cc|"
    } else {
        "aa|bb|cc|"
    };
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    if matches!(case, Handoff::Evicted) {
        config.recent_vectors = Arc::new(RecentVectors::with_capacity(1_200));
    }
    if matches!(case, Handoff::OneResult) {
        config.pending_results = 1;
        config.commit_pieces = 1;
    }
    let handle = ModelHandle::with_recent_vectors(config.recent_vectors.clone());
    handle.set_warm(true);
    let health = HealthSink::default();
    let host = Supervisor::new(health.clone());
    let worker = handle.clone();
    let worker_config = RuntimeConfig {
        state_dir: temp.path().join("worker"),
        executable: model_child_executable(),
        model: Some(model_assets(&assets_root)),
    };
    std::fs::create_dir(&worker_config.state_dir).unwrap();
    std::fs::write(assets_root.join("distinct-vectors"), []).unwrap();
    host.spawn(
        ComponentId {
            binding: None,
            kind: ComponentKind::Model,
        },
        move |stop| {
            run_model_component(
                worker_config.clone(),
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
            "fixture model never became ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let control = RequestControl::default();
    let result = handle
        .plan_pieces(&[body], &control)
        .and_then(|plan| handle.embed_planned(&plan, 0, &PieceVectorReuse::default(), &control));
    host.shutdown();
    let prepared = result.unwrap();
    assert_eq!(prepared.len(), 3);
    let observed = model_transcript(&assets_root, "vectors.jsonl");
    let observed_vectors: Vec<Vec<f32>> = (0..3)
        .map(|i| {
            let text = &body[i * 3..i * 3 + 3];
            let entry = observed.iter().find(|entry| entry["text"] == text).unwrap();
            serde_json::from_value(entry["vector"].clone()).unwrap()
        })
        .collect();
    for (vector, recorded) in prepared.iter().zip(&observed_vectors) {
        assert_eq!(vector.values.as_ref(), recorded.as_slice());
    }
    assert_ne!(observed_vectors[0], observed_vectors[2]);
    assert_eq!(
        model_transcript(&assets_root, "splits.jsonl"),
        [json!(body)]
    );
    assert_eq!(
        model_transcript(&assets_root, "requests.jsonl").len(),
        if matches!(case, Handoff::RepeatedBytes) {
            2
        } else {
            3
        }
    );
    assert!(!config.indices_root.exists());

    if matches!(case, Handoff::Restarted) {
        drop(handle);
        config.recent_vectors = Arc::default();
    }
    let model = if matches!(case, Handoff::DifferentModel) {
        "replacement-model"
    } else {
        "fixture-model"
    };
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let indexer = SemanticIndexer::start(
        config.clone(),
        ModelRequestIds::new(),
        HealthSink::default(),
    )
    .unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready(model);
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(body, vec![])],
        )
        .unwrap();
    indexer.wake("binding");
    let deadline = Instant::now() + Duration::from_secs(20);
    let split = loop {
        if let Some(work) = indexer.next_work(model) {
            break work;
        }
        assert!(Instant::now() < deadline, "waiting for input window");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(matches!(&split.task,IndexTask::Split(text) if &**text==body));

    let hold = matches!(case, Handoff::OneResult).then(|| {
        let (entered, wait_entered) = std::sync::mpsc::sync_channel(1);
        let (release, wait_release) = std::sync::mpsc::sync_channel(1);
        let pending = std::sync::Mutex::new(Some(wait_release));
        *indexer.inner.pre_commit_hook.lock().unwrap() = Some(Arc::new(move || {
            if let Some(rx) = pending.lock().unwrap().take() {
                entered.try_send(()).unwrap();
                rx.recv_timeout(Duration::from_secs(20)).unwrap();
            }
        }));
        (wait_entered, release)
    });
    indexer.finish(
        split.id,
        Ok(IndexResult::Pieces(vec![(0, 3), (3, 6), (6, 9)])),
    );
    if let Some((entered, release)) = hold {
        entered
            .recv_timeout(Duration::from_secs(20))
            .expect("cache hit never entered commit");
        let found = indexer.next_work(model);
        let ack = indexer.progress("binding").unwrap().ack;
        let (occupied, next_range, ranges) = {
            let state = indexer.inner.state.lock().unwrap();
            let lane = &state.schedule.lanes[&LaneKey::current("binding")];
            (
                lane.occupied(),
                lane.pending_window.as_ref().map(|window| window.next_range),
                lane.pending_window
                    .as_ref()
                    .map(|window| window.ranges.clone()),
            )
        };
        release.try_send(()).unwrap();
        assert_eq!(occupied, 1);
        assert_eq!(next_range, Some(1));
        assert_eq!(
            ranges.as_ref().and_then(|ranges| ranges.get(1)),
            Some(&(3, 6))
        );
        assert!(found.is_none());
        // An empty source may already be acknowledged; the held publication must not be.
        assert!(ack.is_none_or(|ack| ack.sequence < receipt.generation as i64));
    }

    let mut embedded = Vec::new();
    let mut fresh = vec![0.0; 256];
    fresh[1] = 1.0;
    while !indexer.progress("binding").is_some_and(|p| {
        p.complete
            && p.ack
                .is_some_and(|ack| ack.sequence == receipt.generation as i64)
    }) {
        if let Some(work) = indexer.next_work(model) {
            let IndexTask::Embed(text) = work.task else {
                panic!("unexpected extra Split");
            };
            embedded.push(text.to_string());
            indexer.finish(work.id, Ok(IndexResult::Vector(fresh.clone())));
        }
        assert!(
            Instant::now() < deadline,
            "waiting for handoff rows and ack"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let expected: &[&str] = match case {
        Handoff::Evicted => &["aa|", "bb|"],
        Handoff::Restarted | Handoff::DifferentModel => &["aa|", "bb|", "cc|"],
        _ => &[],
    };
    assert_eq!(embedded, expected);
    let directory = std::fs::read_dir(&config.indices_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let conn = rusqlite::Connection::open_with_flags(
        directory.path().join("vector.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let rows: Vec<(String, Vec<u8>)> = conn
        .prepare("SELECT section, vector FROM vectors ORDER BY section")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows.len(), 3);
    for (i, (section, bytes)) in rows.iter().enumerate() {
        assert_eq!(section, &format!("body:0:{i}"));
        let vector: Vec<_> = bytes
            .chunks_exact(4)
            .map(|part| f32::from_le_bytes(part.try_into().unwrap()))
            .collect();
        let expected = if embedded.iter().any(|text| text == &body[i * 3..i * 3 + 3]) {
            &fresh[..]
        } else {
            &observed_vectors[i][..]
        };
        assert_eq!(vector, expected);
    }
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn prepared_vectors_commit_without_new_inference() {
    observe_handoff(Handoff::Normal);
}
#[test]
fn equal_bytes_in_distinct_sections_reuse_one_vector() {
    observe_handoff(Handoff::RepeatedBytes);
}
#[test]
fn cached_results_respect_one_pending_result_slot() {
    observe_handoff(Handoff::OneResult);
}
#[test]
fn indexer_infers_only_evicted_prepared_pieces() {
    observe_handoff(Handoff::Evicted);
}
#[test]
fn runtime_restart_recomputes_pieces_after_preparation_without_index_files() {
    observe_handoff(Handoff::Restarted);
}
#[test]
fn model_digest_change_does_not_use_prepared_vectors() {
    observe_handoff(Handoff::DifferentModel);
}

#[test]
fn unrelated_publication_preserves_the_block_and_preparation_returns_unavailable() {
    let f = FailedPiece::new();
    let source = f.store.owned_published_reader();
    let blocked_target = source.read_snapshot().unwrap().position().clone();
    let mut unrelated = mutation("unrelated memory", vec![]);
    unrelated.unit.entity_id = "22222222-2222-4222-8222-222222222222".into();
    f.store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![unrelated])
        .unwrap();
    let target = source.read_snapshot().unwrap().position().clone();
    f.indexer.wake("binding");
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.indexer.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")].dirty {
        assert!(
            Instant::now() < deadline,
            "writer did not reenter after publication"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let handle = SemanticSearch::new(f.indexer.clone(), crate::model::ModelHandle::new())
        .preparation("binding");
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let results = [blocked_target, target].map(|target| {
            handle.wait_with_snapshot(
                &target,
                &RequestControl::default(),
                || test_snapshot(agentlaw_worker::WorkerState::Ready),
                Duration::from_millis(10),
            )
        });
        tx.send(results).unwrap();
    });
    let reason = EmbeddingError::InputTooLong.to_string();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        [
            Err(SemanticError::Unavailable(reason.clone())),
            Err(SemanticError::Unavailable(reason.clone()))
        ]
    );
    waiter.join().unwrap();
    assert!(f.indexer.next_work("model").is_none());
    assert!(f.indexer.status().next_retry_at.is_none());
    assert_eq!(
        f.indexer.progress("binding").unwrap().reason.as_deref(),
        Some(reason.as_str())
    );
    assert!(f
        .health
        .snapshot()
        .iter()
        .any(
            |(component, status)| component.binding.as_deref() == Some("binding")
                && status.state == ComponentState::Degraded
                && status.reason.as_deref() == Some(reason.as_str())
        ));
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
}
