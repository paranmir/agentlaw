use super::*;

#[derive(Default)]
struct FakeRamIndex {
    calls: std::sync::Mutex<usize>,
    work: std::sync::Mutex<Option<IndexWork>>,
    finished: std::sync::Mutex<Vec<ModelRequestId>>,
}
impl IndexWorkPort for FakeRamIndex {
    fn model_ready(&self, _: &str) {}
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        *self.calls.lock().unwrap() += 1;
        self.work.lock().unwrap().take()
    }
    fn finish(&self, id: ModelRequestId, _: std::result::Result<IndexResult, EmbeddingError>) {
        self.finished.lock().unwrap().push(id);
    }
    fn status(&self) -> IndexWorkStatus {
        IndexWorkStatus {
            runnable: self.work.lock().unwrap().is_some(),
            next_retry_at: None,
            awaiting_model: false,
        }
    }
}

#[test]
fn idle_supervisor_does_not_create_files() {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: "missing-executable".into(),
        model: None,
    };
    let mut supervisor = ModelSupervisor::start(
        config,
        Arc::new(FakeRamIndex::default()),
        ModelRequestIds::new(),
    );
    assert_eq!(supervisor.wait_terminated(Duration::ZERO), None);
    assert!(matches!(
        supervisor.embed_query("query", &AtomicBool::new(false)),
        Err(QueryError::Unavailable(_))
    ));
    supervisor.set_warm(true);
    supervisor.wake();
    supervisor.stop().unwrap();
    assert_eq!(
        supervisor.wait_terminated(Duration::ZERO).as_deref(),
        Some("model supervisor stopped")
    );
    assert_eq!(supervisor.snapshot().state, WorkerState::Stopped);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn idle_supervisor_does_not_create_files_or_start_unconfigured_model() {
    struct Process(Arc<std::sync::atomic::AtomicUsize>);
    impl WorkerProcessPort for Process {
        fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err("unexpected spawn".into())
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
    struct Index(mpsc::Sender<()>);
    impl IndexWorkPort for Index {
        fn model_ready(&self, _: &str) {
            unreachable!()
        }
        fn next_work(&self, _: &str) -> Option<IndexWork> {
            unreachable!()
        }
        fn finish(&self, _: ModelRequestId, _: std::result::Result<IndexResult, EmbeddingError>) {
            unreachable!()
        }
        fn status(&self) -> IndexWorkStatus {
            let _ = self.0.send(());
            IndexWorkStatus {
                runnable: true,
                next_retry_at: None,
                awaiting_model: false,
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("worker");
    assert!(!state_dir.exists());
    let config = RuntimeConfig {
        state_dir: state_dir.clone(),
        executable: "missing".into(),
        model: None,
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
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (status_tx, status_rx) = mpsc::channel();
    let worker_shared = shared.clone();
    let process = Process(count.clone());
    let thread = std::thread::spawn(move || {
        model_supervision_loop(
            config,
            Arc::new(Index(status_tx)),
            worker_shared,
            process,
            Instant::now,
            incoming,
        )
    });
    let mut supervisor = ModelSupervisor {
        shared,
        thread: Some(thread),
        ids: ModelRequestIds::new(),
        stop_result: None,
    };
    status_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    supervisor.wake();
    status_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        !state_dir.exists(),
        "model demand must not create worker state when no model is configured"
    );
    supervisor.stop().unwrap();
    assert!(!state_dir.exists());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
