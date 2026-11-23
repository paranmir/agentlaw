use super::*;

#[test]
fn preparation_wakes_immediately_on_ack_and_returns_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("pending", vec![])],
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
    let work = begin_embedding(&indexer, usize::MAX);
    let handle = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new())
        .preparation("binding");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let phases = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reported = phases.clone();
    let expected = target.clone();
    let thread = std::thread::spawn(move || {
        let mut announced = false;
        let control = RequestControl::new(
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            move |phase| reported.lock().unwrap().push(phase.to_owned()),
        );
        let result = handle.wait_with_snapshot(
            &target,
            &control,
            || {
                if !announced {
                    ready_tx.send(()).unwrap();
                    announced = true;
                }
                test_snapshot(agentlaw_worker::WorkerState::Ready)
            },
            Duration::from_secs(5),
        );
        done_tx.send(result).unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    indexer.finish(work.id, Ok(fixture_result(&work, vec![1.0, 0.0])));
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap(),
        SemanticCoverage::CompleteAt(expected)
    );
    thread.join().unwrap();
    assert!(phases
        .lock()
        .unwrap()
        .iter()
        .any(|phase| phase == "semantic_preparation_progress"));
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[test]
fn preparation_cancel_and_unavailable_or_degraded_are_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("pending", vec![])],
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
    let _work = wait_work(&indexer);
    let handle = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new())
        .preparation("binding");
    assert!(matches!(
        handle.wait_target(&target, &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    for model_state in [
        agentlaw_worker::WorkerState::Stopped,
        agentlaw_worker::WorkerState::Failed,
        agentlaw_worker::WorkerState::Stopping,
    ] {
        let result = handle.wait_with_snapshot(
            &target,
            &RequestControl::default(),
            || test_snapshot(model_state.clone()),
            Duration::from_secs(5),
        );
        assert!(matches!(result, Err(SemanticError::Unavailable(_))));
    }
    let control = RequestControl::default();
    let cancel = control.cancel.clone();
    let wait_handle = handle.clone();
    let wait_target = target.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let result = wait_handle.wait_with_snapshot(
            &wait_target,
            &control,
            || {
                let _ = started_tx.send(());
                test_snapshot(agentlaw_worker::WorkerState::Loading)
            },
            Duration::from_millis(100),
        );
        done_tx.send(result).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    cancel.store(true, Ordering::Release);
    assert_eq!(
        done_rx.recv_timeout(Duration::from_millis(500)).unwrap(),
        Err(SemanticError::Cancelled)
    );
    thread.join().unwrap();
    {
        let mut state = indexer.inner.state.lock().unwrap();
        let lane = state
            .schedule
            .lanes
            .get_mut(&LaneKey::current("binding"))
            .unwrap();
        lane.state = ComponentState::Degraded;
        lane.reason = Some("damaged index".into());
    }
    let result = handle.wait_with_snapshot(
        &target,
        &RequestControl::default(),
        || test_snapshot(agentlaw_worker::WorkerState::Ready),
        Duration::from_secs(5),
    );
    assert_eq!(
        result,
        Err(SemanticError::Unavailable("damaged index".into()))
    );
    assert!(indexer.shutdown(Duration::from_secs(5)));
}
