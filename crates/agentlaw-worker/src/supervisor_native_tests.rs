//! Native fault test: terminates ONLY a duplicated handle obtained directly
//! from this test's spawn. No PID lookup, taskkill, user daemon or global state.
use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;
use std::{ffi::c_void, os::windows::io::AsRawHandle};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn DuplicateHandle(
        source: *mut c_void,
        original: *mut c_void,
        target: *mut c_void,
        output: *mut *mut c_void,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    fn TerminateProcess(handle: *mut c_void, exit_code: u32) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}
struct OwnedHandle(usize);

// Invoked only by the parent test below. The grandchild inherits stdout and
// keeps the pipe open after its parent exits, then exits on its own.
#[test]
#[ignore = "helper process for exit observation test"]
fn exit_observation_helper() {
    match std::env::var("AGENTLAW_EXIT_TEST_ROLE").as_deref() {
        Ok("child") => {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "supervisor::native_tests::exit_observation_helper",
                ])
                .env("AGENTLAW_EXIT_TEST_ROLE", "grandchild")
                .spawn()
                .unwrap();
        }
        Ok("grandchild") => std::thread::sleep(Duration::from_secs(3)),
        _ => panic!("helper must be launched by its parent test"),
    }
}

#[test]
fn observes_child_exit_while_descendant_keeps_stdout_open() {
    use std::io::Read;

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "supervisor::native_tests::exit_observation_helper",
        ])
        .env("AGENTLAW_EXIT_TEST_ROLE", "child")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let (exit_tx, exit_rx) = mpsc::channel();
    observe_exit(&child, move || exit_tx.send(()).unwrap()).unwrap();
    let (eof_tx, eof_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        eof_tx.send(()).unwrap();
    });
    exit_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        eof_rx.try_recv().is_err(),
        "stdout reached EOF before child exit was observed"
    );
    child.wait().unwrap();
    eof_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    reader.join().unwrap();
}
impl OwnedHandle {
    fn pin(child: &Child) -> Self {
        let mut handle = std::ptr::null_mut();
        unsafe {
            let process = GetCurrentProcess();
            assert_ne!(
                DuplicateHandle(
                    process,
                    child.as_raw_handle(),
                    process,
                    &mut handle,
                    0,
                    0,
                    2
                ),
                0
            );
        }
        Self(handle as usize)
    }
    fn terminate(&self) {
        unsafe {
            assert_ne!(TerminateProcess(self.0 as *mut c_void, 71), 0);
            assert_eq!(WaitForSingleObject(self.0 as *mut c_void, 10000), 0);
        }
    }
}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0 as *mut c_void);
        }
    }
}

struct FakeRamProcess {
    children: mpsc::Sender<OwnedHandle>,
    observed: mpsc::Sender<(String, mpsc::Sender<Event>)>,
}

struct RefuseObserveAndStop {
    children: mpsc::Sender<OwnedHandle>,
    stops: Option<mpsc::Sender<()>>,
}
impl WorkerProcessPort for RefuseObserveAndStop {
    fn spawn(&self, config: &RuntimeConfig) -> Result<Child> {
        let (tx, _) = mpsc::channel();
        FakeRamProcess {
            children: self.children.clone(),
            observed: tx,
        }
        .spawn(config)
    }
    fn observe(&self, _: &Child, _: String, _: mpsc::Sender<Event>, _: Arc<Signal>) -> Result<()> {
        Err("observer failure".into())
    }
    fn stop(&self, _: &mut Child) -> Result<()> {
        if let Some(stops) = &self.stops {
            let _ = stops.send(());
        }
        Err("stop failure".into())
    }
    fn control(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<u64>> {
        Ok(None)
    }
}

#[test]
fn failed_start_with_unconfirmed_stop_keeps_owned_child() {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: "unused".into(),
        model: None,
    };
    let (children, received) = mpsc::channel();
    let (events, _) = mpsc::channel();
    let error = match start_ram_child(
        &RefuseObserveAndStop {
            children,
            stops: None,
        },
        &config,
        &events,
        &Arc::new(Signal::default()),
        Instant::now(),
        None,
    ) {
        Ok(_) => panic!("observer must fail"),
        Err(error) => error,
    };
    let _handle = received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        error.owned.is_some(),
        "the unconfirmed process handle must remain owned"
    );
}

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
        signal: Arc::new(Signal::default()),
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
impl WorkerProcessPort for FakeRamProcess {
    fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
        let child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        self.children.send(OwnedHandle::pin(&child))?;
        Ok(child)
    }
    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<Signal>,
    ) -> Result<()> {
        self.observed.send((inc.clone(), events.clone()))?;
        NativeProcess.observe(child, inc, events, signal)
    }
    fn stop(&self, child: &mut Child) -> Result<()> {
        NativeProcess.stop(child)
    }
    fn control(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<u64>> {
        Ok(None)
    }
}

struct EmptyRamIndex;
impl IndexWorkPort for EmptyRamIndex {
    fn model_ready(&self, _: &str) {}
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        panic!("idle index must not be polled")
    }
    fn finish(&self, _: ModelRequestId, _: std::result::Result<Vec<f32>, EmbeddingError>) {
        panic!("unexpected index completion")
    }
    fn status(&self) -> IndexWorkStatus {
        IndexWorkStatus {
            runnable: false,
            next_retry_at: None,
            awaiting_model: false,
        }
    }
}

struct OneRamIndex {
    work: std::sync::Mutex<Option<IndexWork>>,
    taken: mpsc::Sender<ModelRequestId>,
    completed: mpsc::Sender<(ModelRequestId, bool)>,
}
impl IndexWorkPort for OneRamIndex {
    fn model_ready(&self, _: &str) {}
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        let work = self.work.lock().unwrap().take();
        if let Some(work) = &work {
            self.taken.send(work.id).unwrap();
        }
        work
    }
    fn finish(&self, id: ModelRequestId, result: std::result::Result<Vec<f32>, EmbeddingError>) {
        self.completed.send((id, result.is_err())).unwrap();
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
        signal: Arc::new(Signal::default()),
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

fn wait_for(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "supervisor did not reach expected state"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn ram_supervisor_recovers_owned_child_and_discards_old_incarnation_results() {
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
        signal: Arc::new(Signal::default()),
        changed: std::sync::Condvar::new(),
        events,
        wake_pending: AtomicBool::new(false),
    });
    let (children_tx, children_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::channel();
    let port = FakeRamProcess {
        children: children_tx,
        observed: observed_tx,
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
    let supervisor = Arc::new(ModelSupervisor {
        shared,
        thread: Some(thread),
        ids: ModelRequestIds::new(),
        stop_result: None,
    });
    supervisor.set_warm(true);
    let first = children_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (old, tx) = observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    tx.send(Event::Output(
        old.clone(),
        Output::Ready {
            model: "digest-a".into(),
        },
    ))
    .unwrap();
    wait_for(|| supervisor.snapshot().state == WorkerState::Ready);
    let first_waiter_supervisor = supervisor.clone();
    let second_waiter_supervisor = supervisor.clone();
    let cancelled_waiter_supervisor = supervisor.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_waiter = cancel.clone();
    let cancelled_waiter = std::thread::spawn(move || {
        cancelled_waiter_supervisor.embed_query("shared", &cancel_waiter)
    });
    let first_waiter = std::thread::spawn(move || {
        first_waiter_supervisor.embed_query("shared", &AtomicBool::new(false))
    });
    let second_waiter = std::thread::spawn(move || {
        second_waiter_supervisor.embed_query("shared", &AtomicBool::new(false))
    });
    wait_for(|| {
        supervisor
            .shared
            .state
            .lock()
            .unwrap()
            .queries
            .flights
            .get(&ModelRequestId(1))
            .is_some_and(|f| f.running && f.waiters == 3)
    });
    cancel.store(true, Ordering::Release);
    assert_eq!(cancelled_waiter.join().unwrap(), Err(QueryError::Cancelled));
    first.terminate();
    assert!(matches!(
        first_waiter.join().unwrap(),
        Err(QueryError::Unavailable(_))
    ));
    assert!(matches!(
        second_waiter.join().unwrap(),
        Err(QueryError::Unavailable(_))
    ));
    wait_for(|| supervisor.snapshot().state == WorkerState::Failed);
    *clock.lock().unwrap() += Duration::from_secs(1);
    let _second = children_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (new, tx) = observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(old, new);
    tx.send(Event::Output(
        new.clone(),
        Output::Ready {
            model: "digest-b".into(),
        },
    ))
    .unwrap();
    wait_for(|| supervisor.snapshot().model_digest.as_deref() == Some("digest-b"));
    let query_supervisor = supervisor.clone();
    let query =
        std::thread::spawn(move || query_supervisor.embed_query("hello", &AtomicBool::new(false)));
    wait_for(|| {
        supervisor
            .shared
            .state
            .lock()
            .unwrap()
            .queries
            .flights
            .get(&ModelRequestId(2))
            .is_some_and(|f| f.running)
    });
    let mut old_vector = vec![0.; crate::DIMENSIONS];
    old_vector[0] = 1.;
    let mut new_vector = vec![0.; crate::DIMENSIONS];
    new_vector[1] = 1.;
    tx.send(Event::Output(
        old,
        Output::Result {
            id: 2,
            attempt: 1,
            result: Ok(old_vector),
        },
    ))
    .unwrap();
    tx.send(Event::Output(
        new,
        Output::Result {
            id: 2,
            attempt: 1,
            result: Ok(new_vector.clone()),
        },
    ))
    .unwrap();
    let answer = query.join().unwrap().unwrap();
    assert_eq!(answer.model_digest, "digest-b");
    assert_eq!(answer.vector, new_vector);
    let query_supervisor = supervisor.clone();
    let invalid = std::thread::spawn(move || {
        query_supervisor.embed_query("bad input", &AtomicBool::new(false))
    });
    wait_for(|| {
        supervisor
            .shared
            .state
            .lock()
            .unwrap()
            .queries
            .flights
            .get(&ModelRequestId(3))
            .is_some_and(|f| f.running)
    });
    let current = supervisor.snapshot().incarnation.unwrap();
    tx.send(Event::Output(
        current.clone(),
        Output::Result {
            id: 3,
            attempt: 1,
            result: Err(EmbeddingError::Failed("bad input".into())),
        },
    ))
    .unwrap();
    assert!(matches!(
        invalid.join().unwrap(),
        Err(QueryError::Failed(_))
    ));
    assert_eq!(
        supervisor.snapshot().incarnation.as_deref(),
        Some(current.as_str())
    );
    drop(supervisor);
}
struct ObservedNative(mpsc::Sender<OwnedHandle>);
impl WorkerProcessPort for ObservedNative {
    fn spawn(&self, c: &RuntimeConfig) -> Result<Child> {
        let child = NativeProcess.spawn(c)?;
        self.0.send(OwnedHandle::pin(&child))?;
        Ok(child)
    }
    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<Signal>,
    ) -> Result<()> {
        NativeProcess.observe(child, inc, events, signal)
    }
    fn stop(&self, child: &mut Child) -> Result<()> {
        NativeProcess.stop(child)
    }
    fn control(&self, address: &str, inc: &str, secret: &str, stop: bool) -> Result<Option<u64>> {
        NativeProcess.control(address, inc, secret, stop)
    }
}
struct SupervisorGuard {
    stop: Arc<AtomicBool>,
    signal: Arc<Signal>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}
impl Drop for SupervisorGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.signal.notify();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn await_ready(path: &std::path::Path, old: Option<&str>) -> String {
    let until = Instant::now() + Duration::from_secs(90);
    loop {
        let snapshot = inspect_runtime(path).unwrap();
        let w = &snapshot["worker"];
        if w["state"] == "ready" && w["incarnation"].as_str() != old {
            return w["incarnation"].as_str().unwrap().into();
        }
        assert!(
            Instant::now() < until,
            "model did not reach READY: {snapshot}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires provisioned real ONNX assets and AGENTLAW_SMOKE_WORKER executable"]
fn native_worker_death_recovers_without_another_frontend_request() {
    let dir = tempfile::tempdir().unwrap();
    let asset =
        |name| std::path::PathBuf::from(std::env::var_os(name).expect("explicit test artifact"));
    let c = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: asset("AGENTLAW_SMOKE_WORKER"),
        model: Some(crate::ModelAssets {
            onnx_model: asset("AGENTLAW_SMOKE_MODEL"),
            tokenizer_json: asset("AGENTLAW_SMOKE_TOKENIZER"),
            runtime_library: asset("AGENTLAW_SMOKE_ORT"),
        }),
    };
    let b = Broker::open(dir.path().join("broker.sqlite"), 64).unwrap();
    initialize(&b).unwrap();
    diagnostic(&b, "model_load", "previous model startup failure").unwrap();
    // Existing client demand survives. Until the replacement reaches READY,
    // the test sends no attach/embed/recall or wake to provoke recovery.
    b.renew_lease("test-client", false, now(), 240000).unwrap();
    drop(b);
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::new(Signal::default());
    let (tx, rx) = mpsc::channel();
    let (s, n) = (stop.clone(), signal.clone());
    let thread = std::thread::spawn(move || run_with_port(c, s, n, ObservedNative(tx)));
    let mut guard = SupervisorGuard {
        stop,
        signal,
        thread: Some(thread),
    };
    let first = rx.recv_timeout(Duration::from_secs(15)).unwrap();
    let old = await_ready(dir.path(), None);
    first.terminate();
    let _replacement = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("no automatic model restart");
    let new = await_ready(dir.path(), Some(&old));
    assert_ne!(new, old);
    let status = inspect_runtime(dir.path()).unwrap();
    assert!(!status["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["name"] == "model_load"));
    assert!(status["diagnostic_history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["name"] == "model_load" && d["detail"] == "previous model startup failure"));
    assert_eq!(status["recovery"][0]["failure_streak"], 1);
    assert!(status["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["name"] == "worker_supervision"
            && d["detail"].as_str().unwrap().contains("IPC ended")));
    assert!(
        rx.try_recv().is_err(),
        "more than one replacement was spawned"
    );

    // READY alone does not establish that the replacement can serve work.
    // Submit a fresh job only AFTER autonomous recovery has been observed.
    let mut broker = Broker::open(dir.path().join("broker.sqlite"), 64).unwrap();
    let text = "After a model worker crashes, the replacement must actually embed new memories.";
    let key = crate::JobKey {
        model_digest: status["worker"]["model"].as_str().unwrap().into(),
        config_digest: "post-recovery-query".into(),
        memory_id: uuid::Uuid::new_v4().to_string(),
        change_id: uuid::Uuid::new_v4().to_string(),
        content_digest: format!("{:x}", Sha256::digest(text.as_bytes())),
        section: "query".into(),
    };
    let crate::Admission::Accepted(job_id) = broker
        .enqueue(&key, text, false, true, Some("test-client"))
        .unwrap()
    else {
        panic!("a fresh post-recovery job must not reuse a cached result");
    };
    guard.signal.notify();
    let deadline = Instant::now() + Duration::from_secs(30);
    let vector = loop {
        let (job_state, job_incarnation, error): (String, Option<String>, Option<String>) = broker
            .connection
            .query_row(
                "SELECT state,incarnation,error FROM jobs WHERE id=?1",
                [job_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(
            !matches!(job_state.as_str(), "failed" | "cancelled"),
            "post-recovery inference failed: {job_state}, {error:?}"
        );
        if job_state == "acknowledged" {
            assert_eq!(job_incarnation.as_deref(), Some(new.as_str()));
            break broker
                .job_result(job_id)
                .unwrap()
                .expect("completed vector");
        }
        assert!(
            Instant::now() < deadline,
            "post-recovery inference timed out: {job_state}, {error:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(vector.len(), crate::DIMENSIONS);
    assert!(vector.iter().all(|value| value.is_finite()));
    let norm: f64 = vector
        .iter()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>()
        .sqrt();
    assert!((norm - 1.0).abs() < 1e-4, "invalid vector norm: {norm}");
    assert_eq!(
        inspect_runtime(dir.path()).unwrap()["worker"]["incarnation"],
        new
    );
    assert!(
        rx.try_recv().is_err(),
        "serving the job spawned another worker"
    );
    guard.stop.store(true, Ordering::Relaxed);
    guard.signal.notify();
    guard.thread.take().unwrap().join().unwrap().unwrap();
}

struct ReviewIndex {
    work: Mutex<Option<IndexWork>>,
    taken: mpsc::Sender<()>,
    finished: mpsc::Sender<()>,
    release_finish: Mutex<Option<mpsc::Receiver<()>>>,
    release_next: Mutex<Option<mpsc::Receiver<()>>>,
    panic_status: AtomicBool,
    probe_lock: AtomicBool,
    lock_result: mpsc::Sender<bool>,
    shared: Mutex<Option<Arc<SupervisorShared>>>,
    statuses: mpsc::Sender<()>,
}
impl IndexWorkPort for ReviewIndex {
    fn model_ready(&self, _: &str) {}
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        if self.probe_lock.swap(false, Ordering::SeqCst) {
            let shared = self.shared.lock().unwrap().clone().unwrap();
            self.lock_result
                .send(shared.state.try_lock().is_ok())
                .unwrap();
        }
        let work = self.work.lock().unwrap().take();
        let _ = self.taken.send(());
        if let Some(rx) = self.release_next.lock().unwrap().take() {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        work
    }
    fn finish(&self, _: ModelRequestId, _: std::result::Result<Vec<f32>, EmbeddingError>) {
        let _ = self.finished.send(());
        if let Some(rx) = self.release_finish.lock().unwrap().take() {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
    fn status(&self) -> IndexWorkStatus {
        let _ = self.statuses.send(());
        assert!(
            !self.panic_status.load(Ordering::SeqCst),
            "injected index status panic"
        );
        IndexWorkStatus {
            runnable: self.work.lock().unwrap().is_some() || self.probe_lock.load(Ordering::SeqCst),
            next_retry_at: None,
            awaiting_model: false,
        }
    }
}
struct ReviewProcess {
    observed: mpsc::Sender<String>,
}
impl WorkerProcessPort for ReviewProcess {
    fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
        use std::os::windows::process::CommandExt;
        Ok(Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?)
    }
    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<Signal>,
    ) -> Result<()> {
        self.observed.send(inc.clone())?;
        NativeProcess.observe(child, inc, events, signal)
    }
    fn stop(&self, child: &mut Child) -> Result<()> {
        NativeProcess.stop(child)
    }
    fn control(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<u64>> {
        Ok(None)
    }
}
struct ReviewFixture {
    owner: Arc<ModelSupervisor>,
    index: Arc<ReviewIndex>,
    events: mpsc::Sender<Event>,
    observed: mpsc::Receiver<String>,
    taken: mpsc::Receiver<()>,
    finished: mpsc::Receiver<()>,
    lock_result: mpsc::Receiver<bool>,
    statuses: mpsc::Receiver<()>,
    _dir: tempfile::TempDir,
}
impl ReviewFixture {
    fn new(warm: bool, work: bool, probe: bool, release: Option<mpsc::Receiver<()>>) -> Self {
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
                warm,
                stop: false,
                loading_demand: false,
                queries: QueryQueue::default(),
                terminated: None,
            }),
            signal: Arc::new(Signal::default()),
            changed: std::sync::Condvar::new(),
            events: events.clone(),
            wake_pending: AtomicBool::new(false),
        });
        let ids = ModelRequestIds::new();
        let (taken_tx, taken) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let (lock_tx, lock_result) = mpsc::channel();
        let (status_tx, statuses) = mpsc::channel();
        let index = Arc::new(ReviewIndex {
            work: Mutex::new(work.then(|| IndexWork {
                id: ids.next().unwrap(),
                text: Arc::from("index"),
            })),
            taken: taken_tx,
            finished: finished_tx,
            release_finish: Mutex::new(release),
            release_next: Mutex::new(None),
            panic_status: AtomicBool::new(false),
            probe_lock: AtomicBool::new(probe),
            lock_result: lock_tx,
            shared: Mutex::new(Some(shared.clone())),
            statuses: status_tx,
        });
        let (observed_tx, observed) = mpsc::channel();
        let port = ReviewProcess {
            observed: observed_tx,
        };
        let worker_shared = shared.clone();
        let worker_index = index.clone();
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
                worker_index,
                worker_shared,
                port,
                Instant::now,
                incoming,
            )
        });
        Self {
            owner: Arc::new(ModelSupervisor {
                shared,
                thread: Some(thread),
                ids,
                stop_result: None,
            }),
            index,
            events,
            observed,
            taken,
            finished,
            lock_result,
            statuses,
            _dir: dir,
        }
    }
    fn ready(&self) -> String {
        let inc = self.observed.recv_timeout(Duration::from_secs(5)).unwrap();
        self.events
            .send(Event::Output(
                inc.clone(),
                Output::Ready {
                    model: "review-digest".into(),
                },
            ))
            .unwrap();
        review_until(|| self.owner.snapshot().state == WorkerState::Ready);
        inc
    }
    fn close(self) -> std::result::Result<(), String> {
        let mut owner = Arc::try_unwrap(self.owner).ok().expect("remaining owner");
        owner.stop()
    }
}
fn review_until(predicate: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < end, "review fixture did not advance");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn review_next_work_must_not_hold_supervisor_state_lock() {
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
fn review_thread_panic_must_fail_waiters_and_clear_ready() {
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
fn review_warm_arriving_during_exit_must_not_need_second_wake() {
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
fn review_demand_flap_must_not_bypass_failure_delay() {
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
fn review_query_arriving_during_index_handoff_runs_first() {
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
fn review_supervisor_panic_completes_owned_index_and_is_observable() {
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
fn review_supervisor_panic_completes_held_index() {
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

struct RecheckIndex {
    work: Mutex<Option<IndexWork>>,
    next_gate: Mutex<Option<mpsc::Receiver<()>>>,
    finish_gate: Mutex<Option<mpsc::Receiver<()>>>,
    status_gate: Mutex<Option<mpsc::Receiver<()>>>,
    next_entered: mpsc::Sender<()>,
    status_entered: mpsc::Sender<()>,
    finished: mpsc::Sender<(ModelRequestId, bool, String)>,
    digest: Mutex<String>,
    panic_finish_once: AtomicBool,
    finish_calls: AtomicUsize,
}
impl IndexWorkPort for RecheckIndex {
    fn model_ready(&self, digest: &str) {
        *self.digest.lock().unwrap() = digest.into();
    }
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        let work = self.work.lock().unwrap().take();
        self.next_entered.send(()).unwrap();
        if let Some(rx) = self.next_gate.lock().unwrap().take() {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        work
    }
    fn finish(&self, id: ModelRequestId, outcome: std::result::Result<Vec<f32>, EmbeddingError>) {
        self.finish_calls.fetch_add(1, Ordering::SeqCst);
        self.finished
            .send((id, outcome.is_err(), self.digest.lock().unwrap().clone()))
            .unwrap();
        if let Some(rx) = self.finish_gate.lock().unwrap().take() {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(
            !self.panic_finish_once.swap(false, Ordering::SeqCst),
            "finish panics after accepting completion"
        );
    }
    fn status(&self) -> IndexWorkStatus {
        if let Some(rx) = self.status_gate.lock().unwrap().take() {
            self.status_entered.send(()).unwrap();
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        IndexWorkStatus {
            runnable: self.work.lock().unwrap().is_some(),
            next_retry_at: None,
            awaiting_model: false,
        }
    }
}
struct RecheckProcess {
    observed: mpsc::Sender<String>,
    stop_calls: Arc<AtomicUsize>,
    refuse_first_stop: bool,
    handles: Arc<Mutex<Vec<std::os::windows::io::OwnedHandle>>>,
}
impl WorkerProcessPort for RecheckProcess {
    fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
        use std::os::windows::process::CommandExt;
        Ok(Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?)
    }
    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<Signal>,
    ) -> Result<()> {
        use std::os::windows::io::AsHandle;
        self.handles
            .lock()
            .unwrap()
            .push(child.as_handle().try_clone_to_owned()?);
        self.observed.send(inc.clone())?;
        NativeProcess.observe(child, inc, events, signal)
    }
    fn stop(&self, child: &mut Child) -> Result<()> {
        let n = self.stop_calls.fetch_add(1, Ordering::SeqCst);
        if self.refuse_first_stop && n == 0 {
            return Err("injected unconfirmed termination".into());
        }
        NativeProcess.stop(child)
    }
    fn control(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<u64>> {
        Ok(None)
    }
}
struct Recheck {
    owner: Option<Arc<ModelSupervisor>>,
    shared: Arc<SupervisorShared>,
    index: Arc<RecheckIndex>,
    events: mpsc::Sender<Event>,
    observed: mpsc::Receiver<String>,
    next_entered: mpsc::Receiver<()>,
    status_entered: mpsc::Receiver<()>,
    finished: mpsc::Receiver<(ModelRequestId, bool, String)>,
    stop_calls: Arc<AtomicUsize>,
    clock: Arc<Mutex<Instant>>,
    handles: Arc<Mutex<Vec<std::os::windows::io::OwnedHandle>>>,
    _dir: tempfile::TempDir,
}
impl Recheck {
    fn new(warm: bool, work: bool, refuse_first_stop: bool) -> Self {
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
                warm,
                stop: false,
                loading_demand: false,
                queries: QueryQueue::default(),
                terminated: None,
            }),
            signal: Arc::new(Signal::default()),
            changed: std::sync::Condvar::new(),
            events: events.clone(),
            wake_pending: AtomicBool::new(false),
        });
        let ids = ModelRequestIds::new();
        let (next_tx, next_entered) = mpsc::channel();
        let (status_tx, status_entered) = mpsc::channel();
        let (finish_tx, finished) = mpsc::channel();
        let index = Arc::new(RecheckIndex {
            work: Mutex::new(work.then(|| IndexWork {
                id: ids.next().unwrap(),
                text: Arc::from("index-a"),
            })),
            next_gate: Mutex::new(None),
            finish_gate: Mutex::new(None),
            status_gate: Mutex::new(None),
            next_entered: next_tx,
            status_entered: status_tx,
            finished: finish_tx,
            digest: Mutex::new(String::new()),
            panic_finish_once: AtomicBool::new(false),
            finish_calls: AtomicUsize::new(0),
        });
        let (observed_tx, observed) = mpsc::channel();
        let stop_calls = Arc::new(AtomicUsize::new(0));
        let handles = Arc::new(Mutex::new(Vec::new()));
        let port = RecheckProcess {
            observed: observed_tx,
            stop_calls: stop_calls.clone(),
            refuse_first_stop,
            handles: handles.clone(),
        };
        let worker_shared = shared.clone();
        let worker_index = index.clone();
        let clock = Arc::new(Mutex::new(Instant::now()));
        let worker_clock = clock.clone();
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
                worker_index,
                worker_shared,
                port,
                move || *worker_clock.lock().unwrap(),
                incoming,
            )
        });
        Self {
            owner: Some(Arc::new(ModelSupervisor {
                shared: shared.clone(),
                thread: Some(thread),
                ids,
                stop_result: None,
            })),
            shared,
            index,
            events,
            observed,
            next_entered,
            status_entered,
            finished,
            stop_calls,
            clock,
            handles,
            _dir: dir,
        }
    }
    fn owner(&self) -> &Arc<ModelSupervisor> {
        self.owner.as_ref().unwrap()
    }
    fn ready(&self, digest: &str) -> String {
        let inc = self.observed.recv_timeout(Duration::from_secs(5)).unwrap();
        self.events
            .send(Event::Output(
                inc.clone(),
                Output::Ready {
                    model: digest.into(),
                },
            ))
            .unwrap();
        until(|| self.owner().snapshot().model_digest.as_deref() == Some(digest));
        inc
    }
    fn running_query(
        &self,
        text: &str,
    ) -> std::thread::JoinHandle<std::result::Result<QueryVector, QueryError>> {
        let owner = self.owner().clone();
        let text = text.to_owned();
        std::thread::spawn(move || owner.embed_query(&text, &AtomicBool::new(false)))
    }
    fn shutdown(&mut self) -> std::result::Result<(), String> {
        let mut owner = Arc::try_unwrap(self.owner.take().unwrap())
            .ok()
            .expect("remaining owner");
        owner.stop()
    }
}
fn until(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "recheck boundary was not reached"
        );
        std::thread::yield_now();
    }
}
fn vector() -> Vec<f32> {
    let mut v = vec![0.; crate::DIMENSIONS];
    v[0] = 1.;
    v
}

#[test]
fn recheck_poisoned_shared_lock_preserves_query_completion_and_stop() {
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

#[test]
fn recheck_native_exit_during_next_work_returns_the_owned_item_once() {
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
fn recheck_failure_backoff_starts_when_slow_spawn_has_finished() {
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
            _: Arc<Signal>,
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
        signal: Arc::new(Signal::default()),
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
fn recheck_stop_during_next_work_returns_the_owned_item_once() {
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
fn recheck_child_failure_during_next_work_returns_the_owned_item_once() {
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
fn recheck_held_work_must_not_cross_replacement_after_stop_retry() {
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
fn recheck_warm_arriving_after_event_drain_survives_until_next_iteration() {
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
fn recheck_stop_arriving_after_event_drain_does_not_wait_for_heartbeat() {
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

#[test]
fn recheck_finish_panic_must_not_deliver_a_second_conflicting_completion() {
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

#[test]
fn recheck_index_result_rejects_changed_digest_in_same_incarnation() {
    let mut f = Recheck::new(true, true, false);
    let inc = f.ready("a");
    f.next_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    f.events
        .send(Event::Output(
            inc.clone(),
            Output::Ready { model: "b".into() },
        ))
        .unwrap();
    until(|| f.owner().snapshot().model_digest.as_deref() == Some("b"));
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
    let outcome = f.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    f.shutdown().unwrap();
    assert!(
        outcome.1,
        "an index result for a different model digest was accepted"
    );
    assert_eq!(f.index.finish_calls.load(Ordering::SeqCst), 1);
}
