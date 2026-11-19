use super::*;
use std::sync::Mutex;

struct TestPort {
    work: Mutex<Option<IndexWork>>,
    finished: Mutex<
        Option<(
            ModelRequestId,
            std::result::Result<IndexResult, EmbeddingError>,
        )>,
    >,
}

impl IndexWorkPort for TestPort {
    fn model_ready(&self, _: &str) {}

    fn next_work(&self, _: &str) -> Option<IndexWork> {
        self.work.lock().unwrap().take()
    }

    fn finish(
        &self,
        id: ModelRequestId,
        outcome: std::result::Result<IndexResult, EmbeddingError>,
    ) {
        *self.finished.lock().unwrap() = Some((id, outcome));
    }

    fn status(&self) -> IndexWorkStatus {
        IndexWorkStatus {
            runnable: false,
            next_retry_at: None,
            awaiting_model: false,
        }
    }
}

const HELPER_MARKER: &[u8] = b"model-helper-ready\n";

struct HelperProcess;

impl WorkerProcessPort for HelperProcess {
    fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
        use std::io::Read;
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                "supervisor::native_tests::index_handoff::model_handoff_helper",
            ])
            .env("AGENTLAW_HANDOFF_HELPER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        // Discard the test harness banner before handing the protocol pipe to the supervisor.
        let stdout = child.stdout.as_mut().unwrap();
        let mut matched = 0;
        while matched < HELPER_MARKER.len() {
            let mut byte = [0];
            stdout.read_exact(&mut byte)?;
            matched = if byte[0] == HELPER_MARKER[matched] {
                matched + 1
            } else {
                0
            };
        }
        Ok(child)
    }

    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<()> {
        NativeProcess.observe(child, inc, events, signal)
    }
    fn stop(&self, child: &mut Child) -> Result<()> {
        NativeProcess.stop(child)
    }
    fn control(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<u64>> {
        Ok(None)
    }
}

#[test]
#[ignore = "helper process for model handoff test"]
fn model_handoff_helper() {
    use std::io::Write;
    if std::env::var("AGENTLAW_HANDOFF_HELPER").as_deref() != Ok("1") {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(HELPER_MARKER).unwrap();
    stdout.flush().unwrap();
    let mut stdin = std::io::stdin().lock();
    let launch: Launch = model_child::read(&mut stdin).unwrap();
    let assets = launch.assets.unwrap();
    assert!(assets.onnx_model.is_file());
    assert!(assets.tokenizer_json.is_file());
    assert!(assets.runtime_library.is_file());
    let deadline = Instant::now() + Duration::from_secs(15);
    while !launch.state.join("release-ready").exists() {
        assert!(Instant::now() < deadline, "ready gate timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    model_child::write(
        &mut stdout,
        &Output::Ready {
            model: "helper-model".into(),
        },
    )
    .unwrap();
    loop {
        match model_child::read::<Input>(&mut stdin).unwrap() {
            Input::Embed { id, attempt, .. } => {
                let mut vector = vec![0.0; crate::DIMENSIONS];
                vector[0] = 1.0;
                model_child::write(
                    &mut stdout,
                    &Output::Result {
                        truncated: false,
                        id,
                        attempt,
                        result: Ok(vector),
                    },
                )
                .unwrap();
            }
            Input::Split { .. } => unreachable!("legacy fixture received Split"),
            Input::Stop => std::process::exit(0),
        }
    }
}

#[test]
fn query_load_handoff_follows_real_child_loading_and_ready() {
    let dir = tempfile::tempdir().unwrap();
    let asset = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, b"test asset").unwrap();
        path
    };
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: std::env::current_exe().unwrap(),
        model: Some(crate::ModelAssets {
            onnx_model: asset("model.onnx"),
            tokenizer_json: asset("tokenizer.json"),
            runtime_library: asset("runtime"),
        }),
    };
    let index = Arc::new(TestPort {
        work: Mutex::new(None),
        finished: Mutex::new(None),
    });
    let (events, incoming) = mpsc::channel();
    let shared = Arc::new(SupervisorShared {
        state: Mutex::new(SupervisorState {
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
    let worker_shared = shared.clone();
    let thread = std::thread::spawn(move || {
        model_supervision_loop(
            config,
            index,
            worker_shared,
            HelperProcess,
            Instant::now,
            incoming,
        )
    });
    let owner = Arc::new(ModelSupervisor {
        shared,
        thread: Some(thread),
        ids: ModelRequestIds::new(),
        stop_result: None,
    });
    owner.set_warm(true);
    let wait = |predicate: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "supervisor did not reach expected state: {:?}",
                owner.snapshot().state
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait(&|| owner.snapshot().state == WorkerState::Loading);
    assert!(owner.snapshot().incarnation.is_some());
    assert!(!owner.has_query_demand());
    owner.request_load();
    assert!(owner.has_query_demand());
    std::fs::write(dir.path().join("release-ready"), b"ready").unwrap();
    wait(&|| owner.snapshot().state == WorkerState::Ready);
    assert_eq!(
        owner.snapshot().model_digest.as_deref(),
        Some("helper-model")
    );
    assert!(!owner.has_query_demand());
    owner.request_load();
    assert!(!owner.has_query_demand());
    let (tx, rx) = mpsc::channel();
    let query_owner = owner.clone();
    let query = std::thread::spawn(move || {
        tx.send(query_owner.embed_piece("real query", &AtomicBool::new(false)))
            .unwrap()
    });
    let result = rx.recv_timeout(Duration::from_secs(10));
    if result.is_err() {
        owner.request_stop();
    }
    query.join().unwrap();
    let result = result.unwrap().unwrap();
    assert_eq!(result.model_digest, "helper-model");
    assert_eq!(result.vector.len(), crate::DIMENSIONS);
    assert_eq!(result.vector[0], 1.0);
    assert!(!owner.has_query_demand());
    Arc::try_unwrap(owner).ok().unwrap().stop().unwrap();
}

#[test]
fn index_work_port_crosses_threads_and_records_outcome() {
    let concrete = Arc::new(TestPort {
        work: Mutex::new(Some(IndexWork {
            id: ModelRequestId(42),
            task: IndexTask::Embed(Arc::from("queued text")),
        })),
        finished: Mutex::new(None),
    });
    let port: Arc<dyn IndexWorkPort> = concrete.clone();
    std::thread::spawn(move || {
        let work = port.next_work("model").unwrap();
        assert!(matches!(&work.task, IndexTask::Embed(text) if &**text == "queued text"));
        port.finish(work.id, Ok(IndexResult::Vector(vec![1.0, 2.0])));
    })
    .join()
    .unwrap();
    let finished = concrete.finished.lock().unwrap().take().unwrap();
    assert_eq!(finished.0, ModelRequestId(42));
    assert_eq!(finished.1.unwrap(), IndexResult::Vector(vec![1.0, 2.0]));
}

#[cfg(windows)]
#[test]
fn first_model_demand_creates_missing_state_directory_and_reaches_ready() {
    struct Idle {
        checked: mpsc::Sender<()>,
        ready: mpsc::Sender<String>,
    }
    impl IndexWorkPort for Idle {
        fn model_ready(&self, model: &str) {
            self.ready.send(model.into()).unwrap();
        }
        fn next_work(&self, _: &str) -> Option<IndexWork> {
            None
        }
        fn finish(&self, _: ModelRequestId, _: std::result::Result<IndexResult, EmbeddingError>) {
            unreachable!("this fixture has no indexing work")
        }
        fn status(&self) -> IndexWorkStatus {
            self.checked.send(()).unwrap();
            IndexWorkStatus {
                runnable: false,
                next_retry_at: None,
                awaiting_model: false,
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("worker");
    let asset = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, b"test asset").unwrap();
        path
    };
    let config = RuntimeConfig {
        state_dir: state_dir.clone(),
        executable: std::env::current_exe().unwrap(),
        model: Some(crate::ModelAssets {
            onnx_model: asset("model.onnx"),
            tokenizer_json: asset("tokenizer.json"),
            runtime_library: asset("runtime"),
        }),
    };
    assert!(!state_dir.exists());
    let (events, incoming) = mpsc::channel();
    let shared = fixture_shared(false, &events);
    let worker_shared = shared.clone();
    let (checked_tx, checked_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        model_supervision_loop(
            config,
            Arc::new(Idle {
                checked: checked_tx,
                ready: ready_tx,
            }),
            worker_shared,
            HelperProcess,
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
    checked_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    supervisor.wake();
    // A second loop boundary acknowledges the preceding idle lifecycle decision.
    checked_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(
        !state_dir.exists(),
        "configured supervisor created worker state before model demand"
    );

    supervisor.request_load();
    let deadline = Instant::now() + Duration::from_secs(30);
    let started = loop {
        let snapshot = supervisor.snapshot();
        if matches!(
            snapshot.state,
            WorkerState::Loading | WorkerState::Ready | WorkerState::Failed
        ) {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "first model demand did not produce a child-start outcome: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    if started.state == WorkerState::Failed {
        supervisor.stop().unwrap();
        panic!("first model demand must create the missing worker directory and start its child; observed {started:?}");
    }
    assert!(started.incarnation.is_some(), "model child did not start");
    assert!(
        state_dir.is_dir(),
        "first demand did not create the worker directory"
    );
    assert!(
        state_dir.join("model.lock").is_file(),
        "first demand did not create model.lock"
    );
    std::fs::write(state_dir.join("release-ready"), b"ready").unwrap();
    assert_eq!(
        ready_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "helper-model"
    );
    assert_eq!(supervisor.snapshot().state, WorkerState::Ready);
    assert_eq!(
        supervisor.snapshot().model_digest.as_deref(),
        Some("helper-model")
    );
    supervisor.stop().unwrap();
}
