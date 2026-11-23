use super::*;

mod cache;
mod core_lifecycle;
mod core_query;
mod demand;
mod index_handoff;
mod protocol;
mod recovery;

#[cfg(windows)]
fn fixture_shared(warm: bool, events: &mpsc::Sender<Event>) -> Arc<SupervisorShared> {
    Arc::new(SupervisorShared {
        state: std::sync::Mutex::new(SupervisorState {
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
        signal: Arc::new(RamSignal {
            events: events.clone(),
        }),
        changed: std::sync::Condvar::new(),
        events: events.clone(),
        wake_pending: AtomicBool::new(false),
    })
}

#[cfg(windows)]
use std::sync::atomic::AtomicUsize;
#[cfg(windows)]
use std::sync::Mutex;
#[cfg(windows)]
use std::{ffi::c_void, os::windows::io::AsRawHandle};

#[cfg(windows)]
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
#[cfg(windows)]
pub(super) struct OwnedHandle(usize);

#[cfg(windows)]
struct LifetimeProcess(std::path::PathBuf);
#[cfg(windows)]
impl WorkerProcessPort for LifetimeProcess {
    fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
        Ok(Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                "supervisor::native_tests::job_lifetime_helper",
            ])
            .env("AGENTLAW_JOB_ROLE", "child")
            .env("AGENTLAW_JOB_DIR", &self.0)
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

#[cfg(windows)]
#[test]
#[ignore = "helper process for parent lifetime test"]
fn job_lifetime_helper() {
    let dir = std::path::PathBuf::from(std::env::var_os("AGENTLAW_JOB_DIR").unwrap());
    match std::env::var("AGENTLAW_JOB_ROLE").unwrap().as_str() {
        "child" => {
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.join("model.lock"))
                .unwrap();
            lock.lock_exclusive().unwrap();
            std::fs::write(dir.join("child-ready"), std::process::id().to_string()).unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        "parent" => {
            let config = RuntimeConfig {
                state_dir: dir.clone(),
                executable: "unused".into(),
                model: None,
            };
            let (events, _) = mpsc::channel();
            let signal = Arc::new(RamSignal {
                events: events.clone(),
            });
            let _child = start_ram_child(
                &LifetimeProcess(dir.clone()),
                &config,
                &events,
                &signal,
                Instant::now(),
                None,
            )
            .unwrap_or_else(|e| panic!("{}", e.cause));
            until(|| dir.join("child-ready").exists());
            std::fs::write(dir.join("parent-ready"), "ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        _ => panic!("unexpected helper role"),
    }
}

// Invoked by native_windows::observes_child_exit_while_descendant_keeps_stdout_open.
// The grandchild inherits stdout and
// keeps the pipe open after its parent exits, then exits on its own.
#[cfg(windows)]
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

#[cfg(windows)]
impl OwnedHandle {
    pub(super) fn pin(child: &Child) -> Self {
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
    pub(super) fn terminate(&self) {
        unsafe {
            assert_ne!(TerminateProcess(self.0 as *mut c_void, 71), 0);
            assert_eq!(WaitForSingleObject(self.0 as *mut c_void, 10000), 0);
        }
    }
}
#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0 as *mut c_void);
        }
    }
}

#[cfg(windows)]
struct FakeRamProcess {
    children: mpsc::Sender<OwnedHandle>,
    observed: mpsc::Sender<(String, mpsc::Sender<Event>)>,
}

#[cfg(windows)]
struct RefuseObserveAndStop {
    children: mpsc::Sender<OwnedHandle>,
    stops: Option<mpsc::Sender<()>>,
}
#[cfg(windows)]
impl WorkerProcessPort for RefuseObserveAndStop {
    fn spawn(&self, config: &RuntimeConfig) -> Result<Child> {
        let (tx, _) = mpsc::channel();
        FakeRamProcess {
            children: self.children.clone(),
            observed: tx,
        }
        .spawn(config)
    }
    fn observe(
        &self,
        _: &Child,
        _: String,
        _: mpsc::Sender<Event>,
        _: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<()> {
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

#[cfg(windows)]
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
        signal: Arc<dyn Fn() + Send + Sync>,
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

#[cfg(windows)]
struct EmptyRamIndex;
#[cfg(windows)]
impl IndexWorkPort for EmptyRamIndex {
    fn model_ready(&self, _: &str) {}
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        panic!("idle index must not be polled")
    }
    fn finish(&self, _: ModelRequestId, _: std::result::Result<IndexResult, EmbeddingError>) {
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

#[cfg(windows)]
struct OneRamIndex {
    work: std::sync::Mutex<Option<IndexWork>>,
    taken: mpsc::Sender<ModelRequestId>,
    completed: mpsc::Sender<(ModelRequestId, bool)>,
}
#[cfg(windows)]
impl IndexWorkPort for OneRamIndex {
    fn model_ready(&self, _: &str) {}
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        let work = self.work.lock().unwrap().take();
        if let Some(work) = &work {
            self.taken.send(work.id).unwrap();
        }
        work
    }
    fn finish(&self, id: ModelRequestId, result: std::result::Result<IndexResult, EmbeddingError>) {
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

#[cfg(windows)]
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

#[cfg(windows)]
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
#[cfg(windows)]
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
    fn finish(&self, _: ModelRequestId, _: std::result::Result<IndexResult, EmbeddingError>) {
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
#[cfg(windows)]
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
#[cfg(windows)]
impl ReviewFixture {
    fn new(warm: bool, work: bool, probe: bool, release: Option<mpsc::Receiver<()>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (events, incoming) = mpsc::channel();
        let shared = fixture_shared(warm, &events);
        let ids = ModelRequestIds::new();
        let (taken_tx, taken) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let (lock_tx, lock_result) = mpsc::channel();
        let (status_tx, statuses) = mpsc::channel();
        let index = Arc::new(ReviewIndex {
            work: Mutex::new(work.then(|| IndexWork {
                id: ids.next().unwrap(),
                task: IndexTask::Embed(Arc::from("index")),
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
        let port = RecheckProcess {
            observed: observed_tx,
            stop_calls: Arc::new(AtomicUsize::new(0)),
            refuse_first_stop: false,
            handles: Arc::new(Mutex::new(Vec::new())),
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
#[cfg(windows)]
fn review_until(predicate: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < end, "review fixture did not advance");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(windows)]
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
#[cfg(windows)]
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
    fn finish(
        &self,
        id: ModelRequestId,
        outcome: std::result::Result<IndexResult, EmbeddingError>,
    ) {
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
#[cfg(windows)]
struct RecheckProcess {
    observed: mpsc::Sender<String>,
    stop_calls: Arc<AtomicUsize>,
    refuse_first_stop: bool,
    handles: Arc<Mutex<Vec<std::os::windows::io::OwnedHandle>>>,
}
#[cfg(windows)]
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
        signal: Arc<dyn Fn() + Send + Sync>,
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
#[cfg(windows)]
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
#[cfg(windows)]
impl Recheck {
    fn new(warm: bool, work: bool, refuse_first_stop: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (events, incoming) = mpsc::channel();
        let shared = fixture_shared(warm, &events);
        let ids = ModelRequestIds::new();
        let (next_tx, next_entered) = mpsc::channel();
        let (status_tx, status_entered) = mpsc::channel();
        let (finish_tx, finished) = mpsc::channel();
        let index = Arc::new(RecheckIndex {
            work: Mutex::new(work.then(|| IndexWork {
                id: ids.next().unwrap(),
                task: IndexTask::Embed(Arc::from("index-a")),
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
        std::thread::spawn(move || owner.embed_piece(&text, &AtomicBool::new(false)))
    }
    fn shutdown(&mut self) -> std::result::Result<(), String> {
        let mut owner = Arc::try_unwrap(self.owner.take().unwrap())
            .ok()
            .expect("remaining owner");
        owner.stop()
    }
}
#[cfg(windows)]
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
#[cfg(windows)]
fn vector() -> Vec<f32> {
    let mut v = vec![0.; crate::DIMENSIONS];
    v[0] = 1.;
    v
}

#[cfg(windows)]
fn missing_model_cause(directory: &std::path::Path) -> String {
    let assets = crate::ModelAssets {
        onnx_model: directory.join("missing.onnx"),
        tokenizer_json: directory.join("missing.json"),
        runtime_library: directory.join("missing.dll"),
    };
    crate::OnnxProvider::load(&assets)
        .err()
        .unwrap()
        .to_string()
}

#[cfg(windows)]
mod lifecycle;
#[cfg(windows)]
mod native_index_handoff;
#[cfg(windows)]
mod native_windows;
#[cfg(windows)]
mod query;
