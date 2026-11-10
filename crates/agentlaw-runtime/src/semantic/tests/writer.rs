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
    config.chunk_bytes = 4;
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
    let work = wait_work(&first);
    first.finish(work.id, Ok(vec![1.0, 0.0]));
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
    assert_eq!(&*work_a.text, "aaa");
    indexer.finish(work_a.id, Err(EmbeddingError::Failed("failure".into())));
    assert_eq!(
        indexer.status().next_retry_at,
        Some(base + Duration::from_secs(1))
    );
    let work_b = indexer.next_work("model").unwrap();
    assert_eq!(&*work_b.text, "bbb");
    indexer.finish(work_b.id, Ok(vec![1.0, 0.0]));
    assert!(indexer.progress("a").unwrap().ack.is_none());
    for (now, delay) in [(1, 2), (3, 4), (7, 8), (15, 16), (31, 30), (61, 30)] {
        offset.store(now, Ordering::SeqCst);
        let work = indexer.next_work("model").unwrap();
        assert_eq!(&*work.text, "aaa");
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
