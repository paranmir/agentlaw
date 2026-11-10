use super::*;

#[test]
fn search_corruption_is_reported_only_for_affected_binding() {
    let temp = tempfile::tempdir().unwrap();
    let a = Store::open(temp.path().join("a"), temp.path().join("alocal")).unwrap();
    let b = Store::open(temp.path().join("b"), temp.path().join("blocal")).unwrap();
    let ra = a
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("alpha", vec![])],
        )
        .unwrap();
    let rb = b
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("beta", vec![])],
        )
        .unwrap();
    let pa = a
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let pb = b
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let health = HealthSink::default();
    let indexer = SemanticIndexer::start(config, ModelRequestIds::new(), health.clone()).unwrap();
    indexer
        .open_binding("a", a.owned_published_reader())
        .unwrap();
    indexer
        .open_binding("b", b.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(work) = indexer.next_work("model") {
            indexer.finish(work.id, Ok(vec![1.0, 0.0]));
        }
        if [("a", ra.generation), ("b", rb.generation)]
            .iter()
            .all(|(name, target)| {
                indexer.progress(name).is_some_and(|p| {
                    p.complete && p.ack.is_some_and(|p| p.sequence >= *target as i64)
                })
            })
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let original = {
        let state = indexer.inner.state.lock().unwrap();
        state.schedule.lanes[&LaneKey::current("a")]
            .active
            .as_ref()
            .unwrap()
            .path
            .clone()
    };
    let copy = temp.path().join("damaged.sqlite");
    let conn = rusqlite::Connection::open_with_flags(
        &original,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    conn.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    drop(conn);
    corrupt_vectors_page(&copy);
    let _updated = a
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(
                "changed alpha",
                vec![ra.references[0].observed_version.clone()],
            )],
        )
        .unwrap();
    indexer.wake("a");
    let deadline = Instant::now() + Duration::from_secs(20);
    let pending = loop {
        if let Some(work) = indexer.next_work("model") {
            break work;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    {
        let mut state = indexer.inner.state.lock().unwrap();
        state
            .schedule
            .lanes
            .get_mut(&LaneKey::current("a"))
            .unwrap()
            .active
            .as_mut()
            .unwrap()
            .path = copy.clone();
    }
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let hook_barrier = barrier.clone();
    *indexer.inner.post_check_hook.lock().unwrap() = Some(Arc::new(move || {
        hook_barrier.wait();
        hook_barrier.wait();
    }));
    indexer.finish(pending.id, Ok(vec![1.0, 0.0]));
    barrier.wait();
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    let scopes = vec!["user".into()];
    let query = SemanticQuery {
        binding: "a",
        scopes: &scopes,
        text: "x",
        vector: Some(&vector),
        limit: 10,
        source: &pa,
    };
    assert!(matches!(
        search.search_available(query, &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    barrier.wait();
    let other_scope = vec!["machine:other".into()];
    assert!(
        matches!(search.search_available(SemanticQuery { scopes: &other_scope, ..query }, &RequestControl::default()), Err(SemanticError::Unavailable(reason)) if reason.contains("index corrupt"))
    );
    let image = || {
        let mut paths = vec![original.clone(), copy.clone()];
        let mut wal = original.as_os_str().to_owned();
        wal.push("-wal");
        let wal = std::path::PathBuf::from(wal);
        if wal.exists() {
            paths.push(wal);
        }
        paths
            .into_iter()
            .map(|path| {
                let metadata = std::fs::metadata(&path).unwrap();
                (
                    path.clone(),
                    std::fs::read(path).unwrap(),
                    metadata.modified().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let before = image();
    indexer.wake("a");
    indexer.model_ready("model");
    assert!(indexer.next_work("model").is_none());
    assert!(!indexer.status().runnable);
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(image(), before);
    assert_eq!(
        indexer.progress("a").unwrap().state,
        ComponentState::Degraded
    );
    assert!(health
        .snapshot()
        .iter()
        .any(|(id, status)| id.binding.as_deref() == Some("a")
            && id.kind == crate::ports::ComponentKind::Semantic
            && status.state == ComponentState::Degraded));
    assert!(matches!(
        search.preparation("a").wait_with_snapshot(
            &pa,
            &RequestControl::default(),
            || test_snapshot(agentlaw_worker::WorkerState::Ready),
            Duration::from_secs(5)
        ),
        Err(SemanticError::Unavailable(_))
    ));
    let result = search
        .search_available(
            SemanticQuery {
                binding: "b",
                source: &pb,
                ..query
            },
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert!(indexer.shutdown(Duration::from_secs(5)));
    assert_eq!(image(), before);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        for path in [&original, &copy] {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(path)
                .unwrap();
        }
    }
}

#[test]
fn stored_invalid_vectors_and_profiles_quarantine_but_bad_queries_do_not() {
    for corrupt in ["zero", "nan", "profile", "text"] {
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
        let indexer =
            SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
        indexer
            .open_binding("binding", store.owned_published_reader())
            .unwrap();
        indexer.model_ready("model");
        drive(&indexer, receipt.generation);
        let active = {
            let state = indexer.inner.state.lock().unwrap();
            state.schedule.lanes[&LaneKey::current("binding")]
                .active
                .as_ref()
                .unwrap()
                .path
                .clone()
        };
        let copy = temp.path().join("corrupt.sqlite");
        if corrupt == "profile" {
            incompatible_profile_fixture(&active, &copy);
        } else {
            let conn = rusqlite::Connection::open_with_flags(
                active,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            conn.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
                .unwrap();
        }
        match corrupt {
            "zero" => corrupt_vector_bytes(&copy, [0.0, 0.0]),
            "nan" => corrupt_vector_bytes(&copy, [f32::NAN, 0.0]),
            "profile" => {}
            _ => corrupt_vectors_page(&copy),
        }
        {
            let mut state = indexer.inner.state.lock().unwrap();
            state
                .schedule
                .lanes
                .get_mut(&LaneKey::current("binding"))
                .unwrap()
                .active
                .as_mut()
                .unwrap()
                .path = copy.clone();
        }
        let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
        let scopes = vec!["user".into()];
        let vector = QueryVector {
            model: "model".into(),
            values: Arc::from([1.0, 0.0]),
        };
        let query = SemanticQuery {
            binding: "binding",
            scopes: &scopes,
            text: "x",
            vector: Some(&vector),
            limit: 10,
            source: &target,
        };
        for values in [vec![0., 0.], vec![f32::NAN, 1.], vec![1.]] {
            let invalid = QueryVector {
                model: "model".into(),
                values: Arc::from(values),
            };
            assert!(matches!(
                search.search_available(
                    SemanticQuery {
                        vector: Some(&invalid),
                        ..query
                    },
                    &RequestControl::default()
                ),
                Err(SemanticError::Failed(_))
            ));
            assert!(
                !indexer.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")]
                    .quarantined
            );
        }
        assert!(matches!(
            search.search_available(query, &RequestControl::default()),
            Err(SemanticError::Unavailable(_))
        ));
        assert!(
            indexer.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")]
                .quarantined
        );
        #[cfg(windows)]
        let _exclusive = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(&copy)
                .unwrap()
        };
        let absent_scope = vec!["machine:other".into()];
        assert!(
            matches!(search.search_available(SemanticQuery { scopes: &absent_scope, ..query }, &RequestControl::default()), Err(SemanticError::Unavailable(reason)) if reason.contains("index corrupt"))
        );
        assert!(indexer.shutdown(Duration::from_secs(5)));
    }
}

#[test]
fn mismatched_vector_file_is_reported_without_rewriting_identity() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("original", vec![])],
        )
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
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
    drive(&first, receipt.generation);
    let mut other_identity = first.inner.state.lock().unwrap().schedule.lanes
        [&LaneKey::current("binding")]
        .active
        .as_ref()
        .unwrap()
        .identity
        .clone();
    assert!(first.shutdown(Duration::from_secs(5)));
    let path = std::fs::read_dir(&config.indices_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("vector.sqlite");
    other_identity.binding = "other".into();
    let other_path = temp.path().join("other.sqlite");
    let mut other =
        agentlaw_search::vector_file::VectorFileWriter::open(&other_path, &other_identity).unwrap();
    other.checkpoint().unwrap();
    drop(other);
    std::fs::copy(&other_path, &path).unwrap();
    let health = HealthSink::default();
    let second = SemanticIndexer::start(config, ModelRequestIds::new(), health.clone()).unwrap();
    second
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    second.model_ready("model");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !health.snapshot().iter().any(|(id, status)| {
        id.binding.as_deref() == Some("binding") && status.state == ComponentState::Degraded
    }) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(second.next_work("model").is_none());
    assert!(
        second.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")].quarantined
    );
    let before = (
        std::fs::read(&path).unwrap(),
        std::fs::metadata(&path).unwrap().modified().unwrap(),
    );
    second.wake("binding");
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        (
            std::fs::read(&path).unwrap(),
            std::fs::metadata(&path).unwrap().modified().unwrap()
        ),
        before
    );
    assert_eq!(
        second.progress("binding").unwrap().state,
        ComponentState::Degraded
    );
    second.close_binding("binding");
    second
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if second.inner.state.lock().unwrap().schedule.lanes[&LaneKey::current("binding")]
            .quarantined
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        (
            std::fs::read(&path).unwrap(),
            std::fs::metadata(&path).unwrap().modified().unwrap()
        ),
        before
    );
    assert!(second.shutdown(Duration::from_secs(5)));
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        db.query_row("SELECT binding FROM binding_vector_meta", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "other"
    );
}
