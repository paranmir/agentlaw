use super::*;

#[test]
fn restart_reuses_committed_sections_before_ack() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("abcdefghijklmnopqrst", vec![])],
        )
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    config.queue_pieces = 1;
    config.pending_results = 1;
    config.commit_pieces = 1;
    let first = SemanticIndexer::start(
        config.clone(),
        ModelRequestIds::new(),
        HealthSink::default(),
    )
    .unwrap();
    first
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    first.model_ready("model");
    let work = begin_embedding(&first, 4);
    first.finish(work.id, Ok(fixture_result(&work, vec![1.0, 0.0])));
    let deadline = Instant::now() + Duration::from_secs(20);
    let path = loop {
        if let Ok(mut dirs) = std::fs::read_dir(&config.indices_root) {
            if let Some(Ok(dir)) = dirs.next() {
                let path = dir.path().join("vector.sqlite");
                if let Ok(db) = rusqlite::Connection::open_with_flags(
                    &path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                ) {
                    if db
                        .query_row("SELECT count(*) FROM vectors", [], |r| r.get::<_, i64>(0))
                        .unwrap_or(0)
                        > 0
                    {
                        break path;
                    }
                }
            }
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(path.exists());
    assert!(first.progress("binding").unwrap().ack.is_none());
    drop(first);
    let second =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    second
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    second.model_ready("model");
    let split = wait_work(&second);
    second.finish(
        split.id,
        Ok(IndexResult::Pieces(fixture_ranges(indexed_text(&split), 4))),
    );
    assert_eq!(drive(&second, receipt.generation), 4);
    assert!(second.shutdown(Duration::from_secs(5)));
}

#[test]
fn failure_backoff_does_not_block_another_binding() {
    let temp = tempfile::tempdir().unwrap();
    let a = Store::open(temp.path().join("a"), temp.path().join("alocal")).unwrap();
    let b = Store::open(temp.path().join("b"), temp.path().join("blocal")).unwrap();
    a.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation("aaa", vec![])],
    )
    .unwrap();
    b.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation("bbb", vec![])],
    )
    .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let base = Instant::now();
    let offset = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let clock_offset = offset.clone();
    let indexer = SemanticIndexer::start_with_clock(
        config,
        ModelRequestIds::new(),
        HealthSink::default(),
        Arc::new(move || base + Duration::from_secs(clock_offset.load(Ordering::SeqCst))),
    )
    .unwrap();
    indexer
        .open_binding("a", a.owned_published_reader())
        .unwrap();
    indexer
        .open_binding("b", b.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !["a", "b"]
        .into_iter()
        .all(|name| indexer.progress(name).is_some_and(|p| p.queued_pieces > 0))
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let work_a = indexer.next_work("model").unwrap();
    assert_eq!(indexed_text(&work_a), "aaa");
    indexer.finish(work_a.id, Err(EmbeddingError::Failed("failure".into())));
    assert_eq!(
        indexer.status().next_retry_at,
        Some(base + Duration::from_secs(1))
    );
    let work_b = indexer.next_work("model").unwrap();
    assert_eq!(indexed_text(&work_b), "bbb");
    indexer.finish(work_b.id, Ok(fixture_result(&work_b, vec![1.0, 0.0])));
    let work_b = wait_work(&indexer);
    indexer.finish(work_b.id, Ok(fixture_result(&work_b, vec![1.0, 0.0])));
    assert!(indexer.progress("a").unwrap().ack.is_none());
    for (now, delay) in [(1, 2), (3, 4), (7, 8), (15, 16), (31, 30), (61, 30)] {
        offset.store(now, Ordering::SeqCst);
        let work = indexer.next_work("model").unwrap();
        assert_eq!(indexed_text(&work), "aaa");
        indexer.finish(work.id, Err(EmbeddingError::Failed("failure".into())));
        assert_eq!(
            indexer.status().next_retry_at,
            Some(base + Duration::from_secs(now + delay))
        );
    }
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn writer_failure_retries_after_storage_recovers() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("recover", vec![])],
        )
        .unwrap();
    let root = temp.path().join("indices");
    std::fs::write(&root, b"obstruction").unwrap();
    let mut config = SemanticConfig::new(root.clone(), "config".into());
    config.dimension = 2;
    let base = Instant::now();
    let offset = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let clock_offset = offset.clone();
    let health = HealthSink::default();
    let indexer = SemanticIndexer::start_with_clock(
        config,
        ModelRequestIds::new(),
        health.clone(),
        Arc::new(move || base + Duration::from_secs(clock_offset.load(Ordering::SeqCst))),
    )
    .unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !health
        .snapshot()
        .iter()
        .any(|(_, status)| status.state == ComponentState::Degraded)
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(indexer.progress("binding").unwrap().ack.is_none());
    std::fs::remove_file(root).unwrap();
    offset.store(1, Ordering::SeqCst);
    indexer.model_ready("model");
    assert_eq!(drive(&indexer, receipt.generation), 1);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn idle_does_not_write_vector_or_wal() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("idle", vec![])],
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
    drive(&indexer, receipt.generation);
    let dir = std::fs::read_dir(&config.indices_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let capture = || -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for item in std::fs::read_dir(&dir).unwrap() {
            let path = item.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !name.ends_with("-shm") {
                files.push((name, std::fs::read(path).unwrap()));
            }
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    };
    let before = capture();
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..10 {
        assert!(indexer.next_work("model").is_none());
        let _ = indexer.status();
        let _ = indexer.progress("binding");
    }
    assert_eq!(capture(), before);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

struct WindowIndex {
    _temp: tempfile::TempDir,
    store: Store,
    config: SemanticConfig,
    indexer: SemanticIndexer,
    target: u64,
}

impl WindowIndex {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
        let receipt = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![mutation("firstsecond", vec![])],
            )
            .unwrap();
        let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
        config.dimension = 2;
        config.commit_pieces = 1;
        config.pending_results = 1;
        config.queue_pieces = 1;
        let indexer = Self::start(&config, &store);
        Self {
            _temp: temp,
            store,
            config,
            indexer,
            target: receipt.generation,
        }
    }

    fn start(config: &SemanticConfig, store: &Store) -> SemanticIndexer {
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
        indexer
    }

    fn split(indexer: &SemanticIndexer) -> IndexWork {
        let work = wait_work(indexer);
        assert!(
            matches!(&work.task, agentlaw_worker::supervisor::IndexTask::Split(text) if &**text == "firstsecond"),
            "{work:?}"
        );
        work
    }

    fn ranges(indexer: &SemanticIndexer, work: IndexWork) {
        indexer.finish(work.id, Ok(IndexResult::Pieces(vec![(0, 5), (5, 11)])));
    }

    fn embed(indexer: &SemanticIndexer, expected: &str) -> IndexWork {
        let work = wait_work(indexer);
        assert!(
            matches!(&work.task, agentlaw_worker::supervisor::IndexTask::Embed(text) if &**text == expected),
            "{work:?}"
        );
        work
    }

    fn finish_embed(indexer: &SemanticIndexer, work: IndexWork) {
        indexer.finish(work.id, Ok(fixture_result(&work, vec![1.0, 0.0])));
    }

    fn committed_sections(&self) -> Vec<String> {
        let dir = std::fs::read_dir(&self.config.indices_root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let conn = rusqlite::Connection::open_with_flags(
            dir.path().join("vector.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut statement = conn
            .prepare("SELECT section FROM vectors ORDER BY section")
            .unwrap();
        statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
}

#[test]
fn split_result_cannot_advance_ack_before_embedding_commits() {
    let f = WindowIndex::new();
    let split = WindowIndex::split(&f.indexer);
    let saved_ack = || {
        let active = f.indexer.inner.state.lock().unwrap().schedule.lanes
            [&LaneKey::current("binding")]
            .active
            .clone()
            .unwrap();
        VectorFileReadView::open(&active.path, &active.identity)
            .unwrap()
            .progress()
            .ack
            .clone()
    };
    let before = saved_ack();
    let (checked, checked_rx) = std::sync::mpsc::sync_channel(1);
    let (release, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = std::sync::Mutex::new(Some(release_rx));
    *f.indexer.inner.completion_check_hook.lock().unwrap() = Some(Arc::new(move |done| {
        if let Some(rx) = release_rx.lock().unwrap().take() {
            checked.try_send(done).unwrap();
            rx.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }));
    f.indexer
        .inner
        .pause_pending_ranges
        .store(true, Ordering::Release);
    WindowIndex::ranges(&f.indexer, split);
    let done = checked_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("writer did not check completion with unconsumed ranges");
    let next = f.indexer.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")]
        .pending_window
        .as_ref()
        .map(|window| window.next_range);
    let after_check = saved_ack();
    f.indexer
        .inner
        .pause_pending_ranges
        .store(false, Ordering::Release);
    release.try_send(()).unwrap();
    assert_eq!(next, Some(0));
    assert!(!done);
    assert_eq!(after_check, before);
    let first = WindowIndex::embed(&f.indexer, "first");
    WindowIndex::finish_embed(&f.indexer, first);
    let second = WindowIndex::embed(&f.indexer, "second");
    WindowIndex::finish_embed(&f.indexer, second);
    assert_eq!(drive(&f.indexer, f.target), 0);

    assert_eq!(f.committed_sections(), ["body:0:0", "body:0:1"]);
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn model_exit_before_split_response_reissues_same_window() {
    let f = WindowIndex::new();
    let first = WindowIndex::split(&f.indexer);
    f.indexer.finish(
        first.id,
        Err(EmbeddingError::Unavailable("model child exited".into())),
    );
    f.indexer.model_ready("model");
    let second = WindowIndex::split(&f.indexer);
    assert_ne!(first.id, second.id);
    assert!(f.indexer.progress("binding").unwrap().ack.is_none());
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn same_model_restart_after_split_resumes_remaining_embeddings() {
    let f = WindowIndex::new();
    WindowIndex::ranges(&f.indexer, WindowIndex::split(&f.indexer));
    WindowIndex::finish_embed(&f.indexer, WindowIndex::embed(&f.indexer, "first"));
    let running = WindowIndex::embed(&f.indexer, "second");
    f.indexer.finish(
        running.id,
        Err(EmbeddingError::Unavailable("model child exited".into())),
    );
    f.indexer.model_ready("model");
    let remaining = WindowIndex::embed(&f.indexer, "second");
    assert_ne!(running.id, remaining.id);
    WindowIndex::finish_embed(&f.indexer, remaining);
    assert_eq!(drive(&f.indexer, f.target), 0);
    assert_eq!(f.committed_sections(), ["body:0:0", "body:0:1"]);
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn runtime_restart_splits_again_and_skips_committed_piece() {
    let f = WindowIndex::new();
    WindowIndex::ranges(&f.indexer, WindowIndex::split(&f.indexer));
    WindowIndex::finish_embed(&f.indexer, WindowIndex::embed(&f.indexer, "first"));
    let _uncommitted = WindowIndex::embed(&f.indexer, "second");
    let deadline = Instant::now() + Duration::from_secs(20);
    while f.committed_sections().is_empty() {
        assert!(Instant::now() < deadline, "first piece never committed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(f.committed_sections(), ["body:0:0"]);
    assert!(f.indexer.progress("binding").unwrap().ack.is_none());
    assert!(f.indexer.shutdown(Duration::from_secs(5)));
    let reopened = WindowIndex::start(&f.config, &f.store);
    WindowIndex::ranges(&reopened, WindowIndex::split(&reopened));
    let remaining = WindowIndex::embed(&reopened, "second");
    WindowIndex::finish_embed(&reopened, remaining);
    assert_eq!(drive(&reopened, f.target), 0);
    assert_eq!(f.committed_sections(), ["body:0:0", "body:0:1"]);
    assert!(reopened.shutdown(Duration::from_secs(5)));
}
