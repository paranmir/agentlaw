use super::super::search::validate_vector;
use super::*;

#[test]
fn search_completed_index_and_rejects_incompatible_or_missing_vector() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("alpha", vec![])],
        )
        .unwrap();
    let source = store
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
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    let scopes = vec!["user".into()];
    let request = SemanticQuery {
        binding: "binding",
        scopes: &scopes,
        text: "alpha",
        vector: Some(&vector),
        limit: 10,
        source: &source,
    };
    let index_dir = {
        let state = indexer.inner.state.lock().unwrap();
        state.schedule.lanes[&LaneKey::current("binding")]
            .active
            .as_ref()
            .unwrap()
            .path
            .parent()
            .unwrap()
            .to_owned()
    };
    let image = || {
        let mut files: Vec<_> = std::fs::read_dir(&index_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| !path.to_string_lossy().ends_with("-shm"))
            .map(|path| {
                (
                    path.file_name().unwrap().to_owned(),
                    std::fs::read(&path).unwrap(),
                )
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    };
    let before = image();
    let found = search
        .search_available(request, &RequestControl::default())
        .unwrap();
    assert_eq!(found.coverage, SemanticCoverage::CompleteAt(source.clone()));
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].memory_id, ID);
    assert_eq!(image(), before);
    let future = agentlaw_storage::published::SourcePosition {
        epoch: source.epoch.clone(),
        sequence: source.sequence + 1,
    };
    let incomplete = search
        .search_available(
            SemanticQuery {
                source: &future,
                ..request
            },
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(
        incomplete.coverage,
        SemanticCoverage::Partial {
            available: source.clone(),
            requested: future
        }
    );
    assert!(matches!(
        search.search_available(
            SemanticQuery {
                binding: "missing",
                ..request
            },
            &RequestControl::default()
        ),
        Err(SemanticError::Unavailable(_))
    ));
    let wrong_epoch = agentlaw_storage::published::SourcePosition {
        epoch: "different".into(),
        sequence: source.sequence,
    };
    assert!(matches!(
        search.search_available(
            SemanticQuery {
                source: &wrong_epoch,
                ..request
            },
            &RequestControl::default()
        ),
        Err(SemanticError::Unavailable(_))
    ));
    let cancelled = RequestControl::default();
    cancelled.cancel.store(true, Ordering::Release);
    assert_eq!(
        search.search_available(request, &cancelled),
        Err(SemanticError::Cancelled)
    );
    let bad = QueryVector {
        model: "other".into(),
        values: Arc::from([1.0, 0.0]),
    };
    assert!(matches!(
        search.search_available(
            SemanticQuery {
                vector: Some(&bad),
                ..request
            },
            &RequestControl::default()
        ),
        Err(SemanticError::Unavailable(_))
    ));
    assert!(matches!(
        search.search_available(
            SemanticQuery {
                vector: None,
                ..request
            },
            &RequestControl::default()
        ),
        Err(SemanticError::Unavailable(_))
    ));
    assert!(matches!(
        search.embed_prefix("text", &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn initialized_empty_file_is_partial_and_has_no_hits() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("pending", vec![])],
        )
        .unwrap();
    let source = store
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
    let _work = wait_work(&indexer);
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    let scopes = vec!["user".into()];
    let query = SemanticQuery {
        binding: "binding",
        scopes: &scopes,
        text: "pending",
        vector: Some(&vector),
        limit: 10,
        source: &source,
    };
    let result = search
        .search_available(query, &RequestControl::default())
        .unwrap();
    assert!(result.hits.is_empty());
    assert_eq!(
        result.coverage,
        SemanticCoverage::Partial {
            available: agentlaw_storage::published::SourcePosition {
                epoch: source.epoch.clone(),
                sequence: 0
            },
            requested: source
        }
    );
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn acknowledged_target_without_complete_is_partial() {
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
    let original = {
        let state = indexer.inner.state.lock().unwrap();
        state.schedule.lanes[&LaneKey::current("binding")]
            .active
            .as_ref()
            .unwrap()
            .path
            .clone()
    };
    let copy = temp.path().join("incomplete.sqlite");
    let conn =
        rusqlite::Connection::open_with_flags(original, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    drop(conn);
    let conn = rusqlite::Connection::open(&copy).unwrap();
    conn.execute("UPDATE binding_vector_meta SET complete=0", [])
        .unwrap();
    drop(conn);
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
            .path = copy;
    }
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    let scopes = vec!["user".into()];
    let result = search
        .search_available(
            SemanticQuery {
                binding: "binding",
                scopes: &scopes,
                text: "indexed",
                vector: Some(&vector),
                limit: 10,
                source: &target,
            },
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(
        result.coverage,
        SemanticCoverage::Partial {
            available: target.clone(),
            requested: target
        }
    );
    assert_eq!(result.hits.len(), 1);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn validates_embedding_shape_and_values() {
    let vector = |model: &str, values: Vec<f32>| QueryVector {
        model: model.into(),
        values: Arc::from(values),
    };
    assert!(validate_vector(&vector("model", vec![1.0, 0.0]), 2).is_ok());
    assert!(validate_vector(&vector("", vec![1.0, 0.0]), 2).is_err());
    assert!(validate_vector(&vector("model", vec![1.0]), 2).is_err());
    assert!(validate_vector(&vector("model", vec![f32::NAN, 1.0]), 2).is_err());
    assert!(validate_vector(&vector("model", vec![0.0, 0.0]), 2).is_err());
}
