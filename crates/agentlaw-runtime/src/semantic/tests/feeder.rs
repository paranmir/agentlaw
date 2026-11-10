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
    indexer.finish(stale.id, Ok(vec![1.0, 0.0]));
    let fresh = wait_work(&indexer);
    assert_eq!(&*fresh.text, "new");
    indexer.finish(fresh.id, Ok(vec![1.0, 0.0]));
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
    let work = wait_work(&indexer);
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
    indexer.finish(work.id, Ok(vec![1.0, 0.0]));
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
    assert_eq!(&*latest_work.text, "latest");
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
    indexer.finish(latest_work.id, Ok(vec![1.0, 0.0]));
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
    config.chunk_bytes = 2;
    config.pending_results = 1;
    config.commit_pieces = 1;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let first = wait_work(&indexer);
    assert!(indexer.next_work("model").is_none());
    indexer.finish(first.id, Ok(vec![1.0, 0.0]));
    let second = wait_work(&indexer);
    indexer.finish(second.id, Ok(vec![1.0, 0.0]));
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
    config.chunk_bytes = 2;
    config.queue_pieces = 1;
    config.pending_results = 1;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let work = wait_work(&indexer);
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
    let (heads, pieces) = source::map_acquired(acquired.clone(), ID, false, 5).unwrap();
    let pieces = pieces.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(heads.change_ids.len(), 1);
    assert_eq!(
        pieces.iter().map(|p| p.text.as_str()).collect::<String>(),
        body
    );
    assert!(pieces.iter().all(|p| p.text.len() <= 5));
    let (_, again) = source::map_acquired(acquired, ID, false, 5).unwrap();
    assert_eq!(pieces, again.collect::<Result<Vec<_>, _>>().unwrap());
}
