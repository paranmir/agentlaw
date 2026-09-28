//! Native fault test: terminates ONLY a duplicated handle obtained directly
//! from this test's spawn. No PID lookup, taskkill, user daemon or global state.
use super::*;
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
