use super::*;

#[test]
fn thread_panic_must_fail_waiters_and_clear_ready() {
    let f = ReviewFixture::new(true, false, false, None);
    f.ready();
    let owner = f.owner.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelled = cancel.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let query = std::thread::spawn(move || {
        let outcome = owner.embed_query("pending", &cancelled);
        done_tx.send(outcome).unwrap();
    });
    review_until(|| {
        f.owner
            .shared
            .state
            .lock()
            .unwrap()
            .queries
            .flights
            .values()
            .any(|q| q.running)
    });
    f.index.panic_status.store(true, Ordering::SeqCst);
    f.owner.wake();
    review_until(|| f.owner.thread.as_ref().unwrap().is_finished());
    let snapshot = f.owner.snapshot();
    let completion = done_rx.recv_timeout(Duration::from_secs(5));
    cancel.store(true, Ordering::SeqCst);
    query.join().unwrap();
    assert!(f.close().is_err());
    assert!(
        completion.is_ok(),
        "supervisor thread ended but query remained pending; snapshot={snapshot:?}"
    );
    assert_ne!(snapshot.state, WorkerState::Ready);
}

#[test]
fn query_arriving_during_index_handoff_runs_first() {
    let (release, held) = mpsc::channel();
    let f = ReviewFixture::new(true, true, false, None);
    *f.index.release_next.lock().unwrap() = Some(held);
    let inc = f.ready();
    f.taken.recv_timeout(Duration::from_secs(5)).unwrap();
    let owner = f.owner.clone();
    let query = std::thread::spawn(move || owner.embed_query("priority", &AtomicBool::new(false)));
    review_until(|| {
        !f.owner
            .shared
            .state
            .lock()
            .unwrap()
            .queries
            .pending
            .is_empty()
    });
    release.send(()).unwrap();
    review_until(|| {
        f.owner
            .shared
            .state
            .lock()
            .unwrap()
            .queries
            .flights
            .get(&ModelRequestId(2))
            .is_some_and(|flight| flight.running)
    });
    let mut vector = vec![0.; crate::DIMENSIONS];
    vector[0] = 1.;
    f.events
        .send(Event::Output(
            inc.clone(),
            Output::Result {
                id: 2,
                attempt: 1,
                result: Ok(vector.clone()),
            },
        ))
        .unwrap();
    assert!(query.join().unwrap().is_ok());
    for _ in 0..2 {
        while f.statuses.try_recv().is_ok() {}
        f.owner.wake();
        f.statuses.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    f.events
        .send(Event::Output(
            inc,
            Output::Result {
                id: 1,
                attempt: 1,
                result: Ok(vector),
            },
        ))
        .unwrap();
    f.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    f.close().unwrap();
}

#[test]
fn poisoned_shared_lock_preserves_query_completion_and_stop() {
    let mut f = Recheck::new(true, false, false);
    let inc = f.ready("a");
    let query = f.running_query("survive poison");
    until(|| {
        f.shared
            .state
            .lock()
            .unwrap()
            .queries
            .flights
            .values()
            .any(|q| q.running)
    });
    let shared = f.shared.clone();
    assert!(std::thread::spawn(move || {
        let _guard = shared.state.lock().unwrap();
        panic!("injected poison without changing invariants");
    })
    .join()
    .is_err());
    f.events
        .send(Event::Output(
            inc,
            Output::Result {
                id: 1,
                attempt: 1,
                result: Ok(vector()),
            },
        ))
        .unwrap();
    let result = query.join().unwrap().unwrap();
    assert_eq!(result.model_digest, "a");
    assert_eq!(f.owner().snapshot().state, WorkerState::Ready);
    let shared = f.shared.clone();
    f.shutdown().unwrap();
    let state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(state.snapshot.state, WorkerState::Stopped);
    assert!(state.terminated.is_some());
    assert!(state.queries.flights.is_empty());
}
