use super::*;

#[test]
fn unconfirmed_child_never_allows_another_spawn() {
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
    let (children, received) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let port = RefuseObserveAndStop {
        children,
        stops: Some(stop_tx),
    };
    let clock = Arc::new(std::sync::Mutex::new(Instant::now()));
    let clock_worker = clock.clone();
    let worker_shared = shared.clone();
    let thread = std::thread::spawn(move || {
        model_supervision_loop(
            config,
            Arc::new(EmptyRamIndex),
            worker_shared,
            port,
            move || *clock_worker.lock().unwrap(),
            incoming,
        )
    });
    let mut supervisor = ModelSupervisor {
        shared,
        thread: Some(thread),
        ids: ModelRequestIds::new(),
        stop_result: None,
    };
    supervisor.set_warm(true);
    let _first = received.recv_timeout(Duration::from_secs(5)).unwrap();
    wait_for(|| supervisor.snapshot().state == WorkerState::Failed);
    stop_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    *clock.lock().unwrap() += Duration::from_secs(30);
    supervisor.wake();
    stop_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(supervisor.stop().is_err());
    assert!(supervisor.stop().is_err());
    assert!(received.try_recv().is_err());
}

#[test]
fn warm_arriving_during_exit_must_not_need_second_wake() {
    let (release_tx, release_rx) = mpsc::channel();
    let f = ReviewFixture::new(false, true, false, Some(release_rx));
    let inc = f.ready();
    f.taken.recv_timeout(Duration::from_secs(5)).unwrap();
    f.events
        .send(Event::Pipe(inc, "injected execution failure".into()))
        .unwrap();
    f.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    f.owner.set_warm(true);
    release_tx.send(()).unwrap();
    let recovered_without_new_call = f.observed.recv_timeout(Duration::from_secs(5)).is_ok();
    f.close().unwrap();
    assert!(
        recovered_without_new_call,
        "live demand was retained in RAM but its wake was consumed using the old warm snapshot"
    );
}

#[test]
fn demand_flap_must_not_bypass_failure_delay() {
    let now = Instant::now();
    let mut lifecycle = ModelLifecycle::default();
    lifecycle.started();
    lifecycle.exited(now, "load failed".into());
    assert_eq!(lifecycle.action(now, false, true), LifecycleAction::None);
    assert_eq!(
        lifecycle.action(now, true, true),
        LifecycleAction::None,
        "a new attach started a replacement before the one-second failure deadline"
    );
}

#[test]
fn failure_backoff_starts_when_slow_spawn_has_finished() {
    struct SlowFail {
        clock: Arc<Mutex<Instant>>,
        attempts: mpsc::Sender<Instant>,
    }
    impl WorkerProcessPort for SlowFail {
        fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
            let mut now = self.clock.lock().unwrap();
            self.attempts.send(*now).unwrap();
            *now += Duration::from_secs(5);
            Err("slow spawn failure".into())
        }
        fn observe(
            &self,
            _: &Child,
            _: String,
            _: mpsc::Sender<Event>,
            _: Arc<dyn Fn() + Send + Sync>,
        ) -> Result<()> {
            unreachable!()
        }
        fn stop(&self, _: &mut Child) -> Result<()> {
            unreachable!()
        }
        fn control(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<u64>> {
            unreachable!()
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (events, incoming) = mpsc::channel();
    let shared = Arc::new(SupervisorShared {
        state: Mutex::new(SupervisorState {
            snapshot: ModelSnapshot {
                state: WorkerState::Stopped,
                incarnation: None,
                model_digest: None,
                cause: None,
            },
            warm: true,
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
    struct Idle;
    impl IndexWorkPort for Idle {
        fn model_ready(&self, _: &str) {}
        fn next_work(&self, _: &str) -> Option<IndexWork> {
            None
        }
        fn finish(&self, _: ModelRequestId, _: std::result::Result<Vec<f32>, EmbeddingError>) {}
        fn status(&self) -> IndexWorkStatus {
            IndexWorkStatus {
                runnable: false,
                next_retry_at: None,
                awaiting_model: false,
            }
        }
    }
    let origin = Instant::now();
    let clock = Arc::new(Mutex::new(origin));
    let (tx, rx) = mpsc::channel();
    let port = SlowFail {
        clock: clock.clone(),
        attempts: tx,
    };
    let worker_clock = clock.clone();
    let worker_shared = shared.clone();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: "unused".into(),
        model: Some(crate::ModelAssets {
            onnx_model: "unused".into(),
            tokenizer_json: "unused".into(),
            runtime_library: "unused".into(),
        }),
    };
    let thread = std::thread::spawn(move || {
        model_supervision_loop(
            config,
            Arc::new(Idle),
            worker_shared,
            port,
            move || *worker_clock.lock().unwrap(),
            incoming,
        )
    });
    let mut owner = ModelSupervisor {
        shared,
        thread: Some(thread),
        ids: ModelRequestIds::new(),
        stop_result: None,
    };
    let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let premature = rx.recv_timeout(Duration::from_millis(150)).ok();
    let mut early = None;
    let mut retried = None;
    if premature.is_none() {
        *clock.lock().unwrap() += Duration::from_millis(999);
        owner.wake();
        early = rx.recv_timeout(Duration::from_millis(50)).ok();
        if early.is_none() {
            *clock.lock().unwrap() += Duration::from_millis(1);
            owner.wake();
            retried = Some(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        }
    }
    owner.stop().unwrap();
    assert!(
        premature.is_none(),
        "next spawn started at {:?}; previous failure completed at {:?}, no one-second delay",
        premature.map(|x| x.duration_since(origin)),
        first.duration_since(origin) + Duration::from_secs(5)
    );
    assert!(early.is_none(), "retried before the failure deadline");
    assert_eq!(retried, Some(first + Duration::from_secs(6)));
}

#[test]
fn warm_arriving_after_event_drain_survives_until_next_iteration() {
    let mut f = Recheck::new(false, true, false);
    let inc = f.ready("a");
    f.next_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let (release, gate) = mpsc::channel();
    *f.index.status_gate.lock().unwrap() = Some(gate);
    f.events
        .send(Event::Pipe(inc, "failure before status".into()))
        .unwrap();
    f.status_entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    f.owner().set_warm(true);
    *f.clock.lock().unwrap() += Duration::from_secs(2);
    release.send(()).unwrap();
    let replacement = f.observed.recv_timeout(Duration::from_secs(5));
    f.shutdown().unwrap();
    assert!(
        replacement.is_ok(),
        "warm event arriving after re-read was lost"
    );
}

#[test]
fn stop_arriving_after_event_drain_does_not_wait_for_heartbeat() {
    let mut f = Recheck::new(true, false, false);
    f.ready("a");
    let (release, gate) = mpsc::channel();
    *f.index.status_gate.lock().unwrap() = Some(gate);
    f.owner().wake();
    f.status_entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
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
}
