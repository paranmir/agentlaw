use super::*;
use agentlaw_flows::semantic::{
    HistoryIngestHint, HistorySemanticPort, OfferResult, SemanticCandidates,
};

fn hint(position: &agentlaw_storage::published::SourcePosition) -> HistoryIngestHint {
    HistoryIngestHint {
        binding: "binding".into(),
        subject: ID.into(),
        through: position.clone(),
    }
}

fn history_search(
    search: &SemanticSearch,
    vector: &QueryVector,
) -> Result<SemanticCandidates, SemanticError> {
    HistorySemanticPort::search_available(
        search,
        "binding",
        ID,
        vector,
        &["history".into()],
        20,
        &RequestControl::default(),
    )
}

fn finish_history(
    indexer: &SemanticIndexer,
    search: &SemanticSearch,
    target: &agentlaw_storage::published::SourcePosition,
) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut texts = Vec::new();
    loop {
        if HistorySemanticPort::offer(search, hint(target)) == OfferResult::Accepted {
            return texts;
        }
        if let Some(work) = indexer.next_work("model") {
            texts.push(indexed_text(&work).to_owned());
            indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
        }
        assert!(Instant::now() < deadline, "history indexing timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn history_initial_and_incremental_reuses_completed_changes() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let first = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("first", vec![])],
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
    drive(&indexer, first.generation);
    let position = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&position)),
        OfferResult::Queued
    );
    let first_work = finish_history(&indexer, &search, &position);
    assert_eq!(first_work.len(), 1);
    assert!(first_work[0].contains("first\n"));
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    let result = history_search(&search, &vector).unwrap();
    assert_eq!(
        result.coverage,
        SemanticCoverage::CompleteAt(position.clone())
    );
    assert_eq!(result.hits.len(), 1);
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&position)),
        OfferResult::Accepted
    );
    let second = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(
                "second",
                vec![first.references[0].observed_version.clone()],
            )],
        )
        .unwrap();
    indexer.wake("binding");
    let target = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Queued
    );
    let partial = history_search(&search, &vector).unwrap();
    assert_eq!(
        partial.coverage,
        SemanticCoverage::Partial {
            available: position,
            requested: target.clone()
        }
    );
    assert_eq!(partial.hits.len(), 1);
    let work = finish_history(&indexer, &search, &target);
    assert_eq!(work.iter().filter(|text| text.contains('\n')).count(), 1);
    assert_eq!(
        history_search(&search, &vector).unwrap().coverage,
        SemanticCoverage::CompleteAt(target)
    );
    assert!(second.generation > first.generation);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn initial_schema_in_progress_is_not_opened_or_quarantined() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("initializing", vec![])],
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
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let hook_barrier = barrier.clone();
    *indexer.inner.history_init_hook.lock().unwrap() = Some(Arc::new(move || {
        hook_barrier.wait();
        hook_barrier.wait();
    }));
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Queued
    );
    barrier.wait();
    let query_search = search.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let query = std::thread::spawn(move || {
        let vector = QueryVector {
            model: "model".into(),
            values: Arc::from([1.0, 0.0]),
        };
        tx.send(history_search(&query_search, &vector)).unwrap();
    });
    let observed = rx.recv_timeout(Duration::from_secs(2));
    let remained_unquarantined = indexer.inner.state.lock().unwrap().quarantined.is_empty();
    barrier.wait();
    query.join().unwrap();
    assert!(matches!(observed, Ok(Err(SemanticError::Unavailable(_)))));
    assert!(remained_unquarantined);
    finish_history(&indexer, &search, &target);
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    assert_eq!(
        history_search(&search, &vector).unwrap().coverage,
        SemanticCoverage::CompleteAt(target)
    );
    assert!(indexer.inner.state.lock().unwrap().quarantined.is_empty());
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn history_offer_capacity_and_current_priority() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let first = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("first", vec![])],
        )
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    config.history_lanes = 1;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    let target = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Busy
    );
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Busy
    );
    indexer.model_ready("model");
    drive(&indexer, first.generation);
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Queued
    );
    let mut other = hint(&target);
    other.subject = "55555555-5555-4555-8555-555555555555".into();
    assert_eq!(
        HistorySemanticPort::offer(&search, other),
        OfferResult::Busy
    );
    let _second = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(
                "current",
                vec![first.references[0].observed_version.clone()],
            )],
        )
        .unwrap();
    indexer.wake("binding");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = indexer.inner.state.lock().unwrap();
        let current = state
            .schedule
            .lanes
            .get(&LaneKey::current("binding"))
            .is_some_and(|l| !l.queue.is_empty());
        let history = state
            .schedule
            .lanes
            .get(&LaneKey::history("binding", ID))
            .is_some_and(|l| !l.queue.is_empty());
        drop(state);
        if current && history {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let work = indexer.next_work("model").unwrap();
    assert_eq!(indexed_text(&work), "current");
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn history_restart_reuses_completed_changes_without_early_ack() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let first_mutation = mutation("first body", vec![]);
    let first_id = first_mutation.unit.heads()[0].metadata["change_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![first_mutation])
        .unwrap();
    let second_mutation = mutation(
        "second body",
        vec![first.references[0].observed_version.clone()],
    );
    let second_id = second_mutation.unit.heads()[0].metadata["change_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![second_mutation])
        .unwrap();
    let target = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    config.chunk_bytes = 4;
    config.pending_results = 1;
    config.commit_pieces = 1;
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
    drive(&indexer, second.generation);
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Queued
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let file = {
            let state = indexer.inner.state.lock().unwrap();
            state
                .schedule
                .lanes
                .get(&LaneKey::history("binding", ID))
                .and_then(|lane| lane.active.clone())
        };
        if let Some(file) = file {
            let view = VectorFileReadView::open(&file.path, &file.identity).unwrap();
            if view
                .is_change_completed(&format!("change:{first_id}"), &first_id)
                .unwrap()
            {
                assert!(view.progress().ack.is_none());
                assert!(!view
                    .is_change_completed(&format!("change:{second_id}"), &second_id)
                    .unwrap());
                break;
            }
        }
        if let Some(work) = indexer.next_work("model") {
            indexer.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(indexer.shutdown(Duration::from_secs(5)));
    drop(search);
    drop(indexer);
    let reopened =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    reopened
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    reopened.model_ready("model");
    drive(&reopened, second.generation);
    let search = SemanticSearch::new(reopened.clone(), crate::model::ModelHandle::new());
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Queued
    );
    let work = finish_history(&reopened, &search, &target);
    assert!(!work.iter().any(|text| text == "firs"));
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    let result = history_search(&search, &vector).unwrap();
    assert_eq!(result.coverage, SemanticCoverage::CompleteAt(target));
    assert!(result.hits.iter().any(|hit| hit.change_id == first_id));
    assert!(result.hits.iter().any(|hit| hit.change_id == second_id));
    assert!(reopened.shutdown(Duration::from_secs(5)));
}

#[test]
fn history_corruption_isolated_and_files_release_after_close() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("current", vec![])],
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
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Queued
    );
    finish_history(&indexer, &search, &target);
    let file = {
        let state = indexer.inner.state.lock().unwrap();
        state.history_completed.back().unwrap().active.path.clone()
    };
    let snapshot = || {
        let mut paths = vec![file.clone()];
        let mut wal = file.as_os_str().to_owned();
        wal.push("-wal");
        let wal = std::path::PathBuf::from(wal);
        if wal.exists() {
            paths.push(wal);
        }
        paths
            .into_iter()
            .map(|p| (p.clone(), std::fs::read(p).unwrap()))
            .collect::<Vec<_>>()
    };
    let before = snapshot();
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(snapshot(), before);
    let copy = temp.path().join("damaged-history.sqlite");
    let db =
        rusqlite::Connection::open_with_flags(&file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    drop(db);
    corrupt_vectors_page(&copy);
    indexer
        .inner
        .state
        .lock()
        .unwrap()
        .history_completed
        .back_mut()
        .unwrap()
        .active
        .path = copy.clone();
    let damaged_file = copy;
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    assert!(matches!(
        history_search(&search, &vector),
        Err(SemanticError::Unavailable(_))
    ));
    assert_eq!(
        HistorySemanticPort::offer(&search, hint(&target)),
        OfferResult::Busy
    );
    let scopes = vec!["user".into()];
    let current = SemanticQuery {
        binding: "binding",
        scopes: &scopes,
        text: "current",
        vector: Some(&vector),
        limit: 10,
        source: &target,
    };
    assert_eq!(
        SemanticPort::search_available(&search, current, &RequestControl::default())
            .unwrap()
            .hits
            .len(),
        1
    );
    indexer.close_binding("binding");
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        for path in [&file, &damaged_file] {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(path)
                .unwrap();
        }
    }
    assert!(indexer.shutdown(Duration::from_secs(5)));
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        for path in [&file, &damaged_file] {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(path)
                .unwrap();
        }
    }
}

#[cfg(windows)]
#[test]
fn active_history_writer_closes_on_binding_close_and_shutdown() {
    use std::os::windows::fs::OpenOptionsExt;
    for close_only in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
        let receipt = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![mutation("body", vec![])],
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
        let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
        assert_eq!(
            HistorySemanticPort::offer(&search, hint(&target)),
            OfferResult::Queued
        );
        let _work = wait_work(&indexer);
        let file = {
            let state = indexer.inner.state.lock().unwrap();
            state.schedule.lanes[&LaneKey::history("binding", ID)]
                .active
                .as_ref()
                .unwrap()
                .path
                .clone()
        };
        if close_only {
            indexer.close_binding("binding");
        } else {
            assert!(indexer.shutdown(Duration::from_secs(5)));
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(file)
            .unwrap();
        assert!(indexer.shutdown(Duration::from_secs(5)));
    }
}
