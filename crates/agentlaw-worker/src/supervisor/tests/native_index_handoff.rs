use super::*;

#[test]
fn ram_supervisor_returns_running_index_work_on_child_exit() {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: "unused".into(),
        model: Some(crate::ModelAssets {
            onnx_model: "unused".into(),
            tokenizer_json: "unused".into(),
            runtime_library: "unused".into(),
        }),
    };
    let (events, incoming) = mpsc::channel();
    let shared = Arc::new(SupervisorShared {
        state: std::sync::Mutex::new(SupervisorState {
            snapshot: ModelSnapshot {
                state: WorkerState::Stopped,
                incarnation: None,
                model_digest: None,
                cause: None,
            },
            warm: false,
            stop: false,
            loading_demand: false,
            queries: QueryQueue::default(),
            terminated: None,
        }),
        signal: Arc::new(RamSignal {
            events: events.clone(),
        }),
        changed: std::sync::Condvar::new(),
        events,
        wake_pending: AtomicBool::new(false),
    });
    let (children_tx, children_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (taken_tx, taken_rx) = mpsc::channel();
    let (completed_tx, completed_rx) = mpsc::channel();
    let ids = ModelRequestIds::new();
    let id = ids.next().unwrap();
    let index = Arc::new(OneRamIndex {
        work: std::sync::Mutex::new(Some(IndexWork {
            id,
            text: Arc::from("index"),
        })),
        taken: taken_tx,
        completed: completed_tx,
    });
    let port = FakeRamProcess {
        children: children_tx,
        observed: observed_tx,
    };
    let worker_shared = shared.clone();
    let thread = std::thread::spawn(move || {
        model_supervision_loop(config, index, worker_shared, port, Instant::now, incoming)
    });
    let supervisor = ModelSupervisor {
        shared,
        thread: Some(thread),
        ids,
        stop_result: None,
    };
    supervisor.set_warm(true);
    let child = children_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (inc, tx) = observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    tx.send(Event::Output(
        inc.clone(),
        Output::Ready {
            model: "digest".into(),
        },
    ))
    .unwrap();
    assert_eq!(taken_rx.recv_timeout(Duration::from_secs(5)).unwrap(), id);
    child.terminate();
    assert_eq!(
        completed_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        (id, true)
    );
    let mut stale = vec![0.0; crate::DIMENSIONS];
    stale[0] = 1.0;
    tx.send(Event::Output(
        inc,
        Output::Result {
            id: id.0,
            attempt: 1,
            result: Ok(stale),
        },
    ))
    .unwrap();
    assert!(completed_rx
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    drop(supervisor);
}

#[test]
fn next_work_must_not_hold_supervisor_state_lock() {
    let f = ReviewFixture::new(true, false, true, None);
    f.ready();
    let was_unlocked = f.lock_result.recv_timeout(Duration::from_secs(5)).unwrap();
    f.close().unwrap();
    assert!(
        was_unlocked,
        "IndexWorkPort::next_work was called with SupervisorShared.state locked"
    );
}

#[test]
fn supervisor_panic_completes_owned_index_and_is_observable() {
    let f = ReviewFixture::new(true, true, false, None);
    f.ready();
    f.taken.recv_timeout(Duration::from_secs(5)).unwrap();
    f.index.panic_status.store(true, Ordering::SeqCst);
    f.owner.wake();
    assert!(f.owner.wait_terminated(Duration::from_secs(5)).is_some());
    f.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(f.owner.snapshot().state, WorkerState::Failed);
    f.owner.set_warm(true);
    assert!(matches!(
        f.owner.embed_query("after", &AtomicBool::new(false)),
        Err(QueryError::Unavailable(_))
    ));
    assert!(f.close().is_err());
}

#[test]
fn supervisor_panic_completes_held_index() {
    let (release, held) = mpsc::channel();
    let f = ReviewFixture::new(true, true, false, None);
    *f.index.release_next.lock().unwrap() = Some(held);
    f.ready();
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
    f.index.panic_status.store(true, Ordering::SeqCst);
    f.owner.wake();
    assert!(f.owner.wait_terminated(Duration::from_secs(5)).is_some());
    f.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        query.join().unwrap(),
        Err(QueryError::Unavailable(_))
    ));
    assert!(f.close().is_err());
}

#[test]
fn native_exit_during_next_work_returns_the_owned_item_once() {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn TerminateProcess(process: *mut std::ffi::c_void, code: u32) -> i32;
    }
    let mut f = Recheck::new(true, true, false);
    let (release, gate) = mpsc::channel();
    *f.index.next_gate.lock().unwrap() = Some(gate);
    f.ready("a");
    f.next_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let handle = f.handles.lock().unwrap().pop().unwrap();
    assert_ne!(unsafe { TerminateProcess(handle.as_raw_handle(), 27) }, 0);
    f.owner().set_warm(false);
    f.owner().set_warm(true);
    release.send(()).unwrap();
    let outcome = f.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    f.shutdown().unwrap();
    assert_eq!(outcome.0, ModelRequestId(1));
    assert!(outcome.1);
    assert_eq!(f.index.finish_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn stop_during_next_work_returns_the_owned_item_once() {
    let mut f = Recheck::new(true, true, false);
    let (release, gate) = mpsc::channel();
    *f.index.next_gate.lock().unwrap() = Some(gate);
    f.ready("a");
    f.next_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut owner = Arc::try_unwrap(f.owner.take().unwrap()).ok().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let stop = std::thread::spawn(move || {
        done_tx.send(owner.stop()).unwrap();
    });
    until(|| f.shared.state.lock().unwrap().stop);
    release.send(()).unwrap();
    assert!(done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .is_ok());
    stop.join().unwrap();
    let outcome = f.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(outcome.0, ModelRequestId(1));
    assert!(outcome.1);
    assert_eq!(f.index.finish_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn child_failure_during_next_work_returns_the_owned_item_once() {
    let mut f = Recheck::new(true, true, false);
    let (release, gate) = mpsc::channel();
    *f.index.next_gate.lock().unwrap() = Some(gate);
    let inc = f.ready("a");
    f.next_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    f.events
        .send(Event::Pipe(inc, "failure during handoff".into()))
        .unwrap();
    release.send(()).unwrap();
    let outcome = f.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(outcome.1);
    f.shutdown().unwrap();
    assert_eq!(f.index.finish_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn held_work_must_not_cross_replacement_after_stop_retry() {
    let mut f = Recheck::new(true, true, true);
    let (release, gate) = mpsc::channel();
    *f.index.next_gate.lock().unwrap() = Some(gate);
    let old = f.ready("a");
    f.next_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let query = f.running_query("priority");
    until(|| !f.shared.state.lock().unwrap().queries.pending.is_empty());
    release.send(()).unwrap();
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
    f.events
        .send(Event::Pipe(old, "control failure".into()))
        .unwrap();
    assert!(matches!(
        query.join().unwrap(),
        Err(QueryError::Unavailable(_))
    ));
    until(|| f.stop_calls.load(Ordering::SeqCst) == 1);
    *f.clock.lock().unwrap() += Duration::from_secs(2);
    f.owner().wake();
    let new = f.ready("b");
    let before_replacement = f.finished.try_recv().ok();
    let mut observed = before_replacement.clone();
    if observed.is_none() {
        for _ in 0..10 {
            f.events
                .send(Event::Output(
                    new.clone(),
                    Output::Result {
                        id: 1,
                        attempt: 1,
                        result: Ok(vector()),
                    },
                ))
                .unwrap();
            if let Ok(outcome) = f.finished.recv_timeout(Duration::from_millis(50)) {
                observed = Some(outcome);
                break;
            }
        }
    }
    f.shutdown().unwrap();
    assert!(before_replacement.as_ref().is_some_and(|x| x.1),
        "held job from model a was not returned on confirmed exit; completion after model b READY: {observed:?}");
}

#[test]
fn finish_panic_must_not_deliver_a_second_conflicting_completion() {
    let mut f = Recheck::new(true, true, false);
    let inc = f.ready("a");
    f.next_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    f.index.panic_finish_once.store(true, Ordering::SeqCst);
    for _ in 0..10 {
        f.events
            .send(Event::Output(
                inc.clone(),
                Output::Result {
                    id: 1,
                    attempt: 1,
                    result: Ok(vector()),
                },
            ))
            .unwrap();
        if f.owner()
            .wait_terminated(Duration::from_millis(100))
            .is_some()
        {
            break;
        }
    }
    let terminated = f.owner().wait_terminated(Duration::from_secs(5));
    let calls = f.index.finish_calls.load(Ordering::SeqCst);
    let completions: Vec<_> = f.finished.try_iter().collect();
    let result = f.shutdown();
    assert!(terminated.is_some());
    assert!(result.is_err());
    assert_eq!(
        calls, 1,
        "finish received conflicting duplicate completions: {completions:?}"
    );
}
