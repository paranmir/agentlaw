use super::*;
use crate::installation::owner::{owner_alive, OwnerLock};
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    sync::Barrier,
};

const BUILD: &str = "startup-test";
const HELPER_ENV: &str = "AGENTLAW_STARTUP_FIXTURE";

fn limits() -> AttachLimits {
    AttachLimits {
        start_wait: Duration::from_secs(3),
        hello_timeout: Duration::from_millis(100),
    }
}

#[test]
fn fixture() {
    let Some(role) = std::env::var_os(HELPER_ENV) else {
        return;
    };
    let state = PathBuf::from(std::env::var_os("AGENTLAW_STARTUP_STATE").unwrap());
    if role == "model" {
        let mut stream =
            TcpStream::connect(std::env::var("AGENTLAW_STARTUP_ADDRESS").unwrap()).unwrap();
        let mut byte = [0];
        let _ = stream.read(&mut byte);
        return;
    }
    if role == "owner-with-child" {
        let _lock = OwnerLock::try_acquire(&state).unwrap().expect("owner lock");
        let _model = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "client::tests::startup::fixture", "--nocapture"])
            .env(HELPER_ENV, "model")
            .env("AGENTLAW_STARTUP_STATE", &state)
            .env(
                "AGENTLAW_STARTUP_ADDRESS",
                std::env::var("AGENTLAW_STARTUP_ADDRESS").unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(state.join("fixture-ready"), b"ready").unwrap();
        loop {
            thread::park();
        }
    }
    if role == "host" {
        let deps = crate::ports::HostDeps {
            state,
            executable: std::env::current_exe().unwrap(),
            assets: Arc::new(|| Ok(None)),
            installation_build: BUILD.into(),
            start_gate: Arc::new(|| Ok(())),
        };
        let code = match crate::host::run(deps) {
            Ok(crate::host::RunExit::Stopped) => 0,
            Ok(crate::host::RunExit::AlreadyOwned) => 3,
            Ok(crate::host::RunExit::StartBlocked(_)) => 4,
            Err(_) => 1,
        };
        std::process::exit(code);
    }
    if role == "delayed-owner" {
        let _lock = OwnerLock::try_acquire(&state).unwrap().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        crate::transport::write_endpoint(
            &state,
            &crate::transport::Endpoint {
                protocol: PROTOCOL_VERSION,
                installation_build: BUILD.into(),
                control: address,
                data: address,
                secret: "a".repeat(64),
                pid: std::process::id(),
            },
        )
        .unwrap();
        std::fs::write(state.join("fixture-ready"), b"ready").unwrap();
        let (mut first, _) = listener.accept().unwrap();
        let _: Hello = recv(&mut first);
        std::fs::write(state.join("hello-received"), b"ready").unwrap();
        wait_for(|| state.join("release-hello").exists());
        let _ = transport::send(&mut first, &HelloReply::Accepted, MAX_CONTROL_REPLY_BYTES);
        for connection in listener.incoming() {
            let mut stream = connection.unwrap();
            let _: Hello = recv(&mut stream);
            send(&mut stream, &HelloReply::Accepted);
            if let Ok(ControlRequest::Lease { .. }) =
                transport::receive(&mut stream, MAX_CONTROL_FRAME_BYTES)
            {
                send(&mut stream, &ControlReply::LeaseRenewed);
            }
        }
        return;
    }
    let _lock = if role == "silent-owner" {
        Some(
            OwnerLock::try_acquire(&state)
                .unwrap()
                .expect("fixture owner lock"),
        )
    } else {
        None
    };
    let requested =
        std::env::var("AGENTLAW_STARTUP_ADDRESS").unwrap_or_else(|_| "127.0.0.1:0".into());
    let listener = TcpListener::bind(requested).unwrap();
    if role == "silent-owner" {
        let address = listener.local_addr().unwrap();
        crate::transport::write_endpoint(
            &state,
            &crate::transport::Endpoint {
                protocol: PROTOCOL_VERSION,
                installation_build: BUILD.into(),
                control: address,
                data: address,
                secret: "a".repeat(64),
                pid: std::process::id(),
            },
        )
        .unwrap();
    }
    std::fs::write(state.join("fixture-ready"), b"ready").unwrap();
    let mut streams = Vec::new();
    for stream in listener.incoming() {
        streams.push(stream.unwrap());
    }
}

type SharedChild = Arc<Mutex<Child>>;
struct TestLauncher {
    state: PathBuf,
    children: Mutex<Vec<SharedChild>>,
    already_owned: Arc<AtomicUsize>,
    launch_barrier: Option<(Arc<Barrier>, usize)>,
    launches: AtomicUsize,
}
struct TestCandidate {
    child: SharedChild,
    already_owned: Arc<AtomicUsize>,
}
impl TestLauncher {
    fn new(state: &Path) -> Self {
        Self {
            state: state.into(),
            children: Mutex::new(Vec::new()),
            already_owned: Arc::new(AtomicUsize::new(0)),
            launch_barrier: None,
            launches: AtomicUsize::new(0),
        }
    }
    fn spawn(&self, role: &str, address: Option<&str>) -> SharedChild {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "client::tests::startup::fixture", "--nocapture"])
            .env(HELPER_ENV, role)
            .env("AGENTLAW_STARTUP_STATE", &self.state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(address) = address {
            command.env("AGENTLAW_STARTUP_ADDRESS", address);
        }
        let child = Arc::new(Mutex::new(command.spawn().unwrap()));
        self.children.lock().unwrap().push(child.clone());
        child
    }
}
impl Drop for TestLauncher {
    fn drop(&mut self) {
        for child in self.children.lock().unwrap().iter() {
            let mut child = child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl RuntimeLauncher for TestLauncher {
    fn launch(&self) -> io::Result<Box<dyn RuntimeCandidate>> {
        if let Some((barrier, count)) = &self.launch_barrier {
            if self.launches.fetch_add(1, Ordering::SeqCst) < *count {
                barrier.wait();
            }
        }
        Ok(Box::new(TestCandidate {
            child: self.spawn("host", None),
            already_owned: self.already_owned.clone(),
        }))
    }
}
impl RuntimeCandidate for TestCandidate {
    fn try_exit(&mut self) -> io::Result<Option<CandidateExit>> {
        let Some(status) = self.child.lock().unwrap().try_wait()? else {
            return Ok(None);
        };
        Ok(Some(if status.code() == Some(3) {
            self.already_owned.fetch_add(1, Ordering::SeqCst);
            CandidateExit::AlreadyOwned
        } else {
            CandidateExit::Failed(format!("{status}"))
        }))
    }
}
fn wait_for(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "fixture deadline elapsed");
        thread::sleep(Duration::from_millis(5));
    }
}
fn start_host(launcher: &TestLauncher) -> SharedChild {
    let child = launcher.spawn("host", None);
    wait_for(|| {
        assert!(
            child.lock().unwrap().try_wait().unwrap().is_none(),
            "host exited before publishing endpoint"
        );
        launcher.state.join("service/endpoint.json").exists()
    });
    child
}
fn kill(child: &SharedChild) {
    let mut child = child.lock().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
}
fn attached(state: &Path, launcher: &TestLauncher) -> Client {
    Client::attach(state, BUILD, launcher, limits())
        .unwrap_or_else(|e| panic!("attach failed: {e:?}"))
}

#[test]
fn simultaneous_attach_uses_one_owner_and_one_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let count = 8;
    let mut launcher = TestLauncher::new(dir.path());
    launcher.launch_barrier = Some((Arc::new(Barrier::new(count)), count));
    let launcher = Arc::new(launcher);
    let barrier = Arc::new(Barrier::new(count));
    let threads: Vec<_> = (0..count)
        .map(|_| {
            let launcher = launcher.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                let client = attached(&launcher.state, &launcher);
                client.inner.endpoint.pid
            })
        })
        .collect();
    let pids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(pids.iter().all(|pid| *pid == pids[0]));
    assert!(OwnerLock::try_acquire(dir.path()).unwrap().is_none());
    let children = launcher.children.lock().unwrap().clone();
    wait_for(|| {
        children.iter().any(|child| {
            let mut candidate = TestCandidate {
                child: child.clone(),
                already_owned: launcher.already_owned.clone(),
            };
            matches!(candidate.try_exit(), Ok(Some(CandidateExit::AlreadyOwned)))
        })
    });
}

#[test]
fn killed_owner_endpoint_is_replaced_by_new_owner() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = TestLauncher::new(dir.path());
    let child = start_host(&launcher);
    let old = crate::transport::read_endpoint(dir.path()).unwrap();
    kill(&child);
    assert!(dir.path().join("service/endpoint.json").exists());
    let client = attached(dir.path(), &launcher);
    assert_ne!(client.inner.endpoint.pid, old.pid);
    assert_eq!(
        crate::transport::read_endpoint(dir.path()).unwrap().pid,
        client.inner.endpoint.pid
    );
}

#[test]
fn stale_port_used_by_unrelated_process_does_not_prevent_new_owner() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = TestLauncher::new(dir.path());
    let child = start_host(&launcher);
    let old = crate::transport::read_endpoint(dir.path()).unwrap();
    kill(&child);
    let unrelated = launcher.spawn("silent-port", Some(&old.control.to_string()));
    wait_for(|| {
        assert!(unrelated.lock().unwrap().try_wait().unwrap().is_none());
        dir.path().join("fixture-ready").exists()
    });
    let client = attached(dir.path(), &launcher);
    assert_ne!(client.inner.endpoint.pid, old.pid);
    assert_ne!(client.inner.endpoint.control, old.control);
    assert!(unrelated.lock().unwrap().try_wait().unwrap().is_none());
}

#[test]
fn owner_lock_is_released_when_owner_dies_while_model_child_lives() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let launcher = TestLauncher::new(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let owner = launcher.spawn(
        "owner-with-child",
        Some(&listener.local_addr().unwrap().to_string()),
    );
    let mut model = None;
    wait_for(|| {
        assert!(
            owner.lock().unwrap().try_wait().unwrap().is_none(),
            "owner fixture exited"
        );
        match listener.accept() {
            Ok((stream, _)) => {
                model = Some(stream);
                true
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => false,
            Err(e) => panic!("model connection: {e}"),
        }
    });
    let mut model = model.unwrap();
    kill(&owner);
    model.set_nonblocking(true).unwrap();
    let mut byte = [0];
    assert!(matches!(model.read(&mut byte), Err(ref e) if e.kind() == io::ErrorKind::WouldBlock));
    let lock = OwnerLock::try_acquire(dir.path()).unwrap();
    assert!(lock.is_some());
    model.set_nonblocking(false).unwrap();
    model
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    model.write_all(&[1]).unwrap();
    assert_eq!(model.read(&mut byte).unwrap(), 0);
}

#[test]
fn shutdown_removes_endpoint_and_allows_next_attach() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = TestLauncher::new(dir.path());
    let client = attached(dir.path(), &launcher);
    let old_pid = client.inner.endpoint.pid;
    client.shutdown("test").unwrap();
    wait_for(|| matches!(owner_alive(dir.path()), Ok(false)));
    assert!(!dir.path().join("service/endpoint.json").exists());
    let children = launcher.children.lock().unwrap().clone();
    let owner = children
        .iter()
        .find(|child| child.lock().unwrap().id() == old_pid)
        .expect("owned host handle");
    wait_for(|| owner.lock().unwrap().try_wait().unwrap().is_some());
    assert_eq!(
        owner.lock().unwrap().try_wait().unwrap().unwrap().code(),
        Some(0)
    );
    let next = attached(dir.path(), &launcher);
    assert_ne!(next.inner.endpoint.pid, old_pid);
}

#[test]
fn unresponsive_owner_remains_alive_and_candidate_loses_lock() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = TestLauncher::new(dir.path());
    let owner = launcher.spawn("silent-owner", None);
    wait_for(|| {
        assert!(
            owner.lock().unwrap().try_wait().unwrap().is_none(),
            "owner fixture exited"
        );
        dir.path().join("fixture-ready").exists()
    });
    let result = Client::attach(
        dir.path(),
        BUILD,
        &launcher,
        AttachLimits {
            start_wait: Duration::from_millis(500),
            hello_timeout: Duration::from_millis(50),
        },
    );
    assert!(matches!(
        result,
        Err(ClientError::Runtime(RuntimeError::Unresponsive))
    ));
    assert!(owner.lock().unwrap().try_wait().unwrap().is_none());
    assert!(launcher.already_owned.load(Ordering::SeqCst) > 0);
}

#[test]
fn delayed_hello_is_retried_against_original_owner() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = Arc::new(TestLauncher::new(dir.path()));
    let owner = launcher.spawn("delayed-owner", None);
    wait_for(|| dir.path().join("fixture-ready").exists());
    let worker = launcher.clone();
    let attach =
        thread::spawn(move || Client::attach(&worker.state, BUILD, worker.as_ref(), limits()));
    wait_for(|| dir.path().join("hello-received").exists());
    let deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < deadline {
        thread::park_timeout(Duration::from_millis(5));
    }
    std::fs::write(dir.path().join("release-hello"), b"go").unwrap();
    let client = attach
        .join()
        .unwrap()
        .unwrap_or_else(|e| panic!("attach: {e:?}"));
    assert_eq!(client.inner.endpoint.pid, owner.lock().unwrap().id());
    assert!(owner.lock().unwrap().try_wait().unwrap().is_none());
    assert!(OwnerLock::try_acquire(dir.path()).unwrap().is_none());
    let children = launcher.children.lock().unwrap().clone();
    for child in children.iter().filter(|child| !Arc::ptr_eq(child, &owner)) {
        wait_for(|| child.lock().unwrap().try_wait().unwrap().is_some());
    }
}

#[test]
fn config_get_uses_runtime_snapshot_and_reports_invalid_keys() {
    use crate::test_support::installation::TestInstallation;
    let installation = TestInstallation::new();
    installation.select_store("a").unwrap();
    crate::installation::config::set_limit(installation.state(), "response_limit_bytes", "8192")
        .unwrap();
    let launcher = TestLauncher::new(installation.state());
    let client = attached(installation.state(), &launcher);
    assert_eq!(
        client.config_get("response_limit_bytes").unwrap()["value"],
        8192
    );
    crate::installation::config::set_limit(installation.state(), "response_limit_bytes", "16384")
        .unwrap();
    assert_eq!(
        client.config_get("response_limit_bytes").unwrap()["value"],
        8192
    );
    assert!(matches!(
        client.config_get("unknown"),
        Err(ClientError::Domain(_))
    ));
}

#[test]
fn config_get_handles_missing_and_corrupt_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = TestLauncher::new(dir.path());
    let client = attached(dir.path(), &launcher);
    assert_eq!(
        client.config_get("response_limit_bytes").unwrap()["value"],
        crate::installation::config::default_response_limit()
    );
    client.shutdown("test").unwrap();
    wait_for(|| matches!(owner_alive(dir.path()), Ok(false)));
    std::fs::write(dir.path().join("config.json"), b"{").unwrap();
    let client = attached(dir.path(), &launcher);
    assert!(matches!(
        client.config_get("response_limit_bytes"),
        Err(ClientError::Domain(_))
    ));
}

#[test]
fn setup_store_connect_switches_binding_without_worker_daemon() {
    use crate::test_support::installation::TestInstallation;
    let installation = TestInstallation::new();
    installation.select_store("a").unwrap();
    installation.select_store("b").unwrap();
    installation.select_store("a").unwrap();
    let launcher = TestLauncher::new(installation.state());
    let client = attached(installation.state(), &launcher);
    let before = client.resolve_binding().unwrap();
    let result = client
        .setup(
            SetupCommand::StoreConnect {
                path: installation.store_path("b"),
                create: false,
            },
            &RequestControl::default(),
        )
        .unwrap();
    assert!(result.is_object());
    let after = client.resolve_binding().unwrap();
    assert_ne!(before, after);
    let request = ValidatedRequest::parse_public(
        &json!({"action":"recall","recall":{"recall_for":"store selection marker"}}).to_string(),
    )
    .unwrap();
    assert!(client
        .call(
            CallTarget::Store(after),
            request,
            &RequestControl::default()
        )
        .is_ok());
    assert!(!installation.state().join("worker/endpoint.json").exists());
}

#[test]
fn public_setup_connect_switches_store_without_worker_endpoint() {
    use crate::test_support::installation::TestInstallation;
    let installation = TestInstallation::new();
    installation.select_store("a").unwrap();
    installation.select_store("b").unwrap();
    installation.select_store("a").unwrap();
    let project = installation.state().parent().unwrap().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let launcher = TestLauncher::new(installation.state());
    let client = attached(installation.state(), &launcher);
    let original = client.resolve_binding().unwrap();
    let input = json!({"action":"connect_project_memory", "connect_project_memory": {
        "project_path": project, "memory_store_path": installation.store_path("b")
    }});
    let request = ValidatedRequest::parse_public(&input.to_string()).unwrap();
    let result = client
        .call(CallTarget::Setup, request, &RequestControl::default())
        .unwrap();
    assert!(result.is_object());
    let selected = client.resolve_binding().unwrap();
    assert_ne!(original, selected);
    let recall = ValidatedRequest::parse_public(
        &json!({"action":"recall", "recall":{"recall_for":"selection marker"}}).to_string(),
    )
    .unwrap();
    assert!(client
        .call(
            CallTarget::Store(selected),
            recall,
            &RequestControl::default()
        )
        .is_ok());
    assert!(!installation.state().join("worker/endpoint.json").exists());
}

#[cfg(windows)]
#[test]
fn shutdown_during_upload_does_not_reopen_store_and_releases_owner() {
    use crate::test_support::installation::TestInstallation;
    let installation = TestInstallation::new();
    installation.select_store("a").unwrap();
    let db = installation
        .config()
        .unwrap()
        .unwrap()
        .runtime_root(installation.state())
        .join("control.sqlite");
    let launcher = TestLauncher::new(installation.state());
    let host = start_host(&launcher);
    let client = attached(installation.state(), &launcher);
    let binding = client.resolve_binding().unwrap();
    let endpoint = crate::transport::read_endpoint(installation.state()).unwrap();
    let mut data = TcpStream::connect(endpoint.data).unwrap();
    data.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    send(
        &mut data,
        &Hello {
            protocol: PROTOCOL_VERSION,
            installation_build: BUILD.into(),
            secret: endpoint.secret,
        },
    );
    assert_eq!(recv::<HelloReply>(&mut data), HelloReply::Accepted);
    let request = ValidatedRequest::parse_public(
        &json!({"action":"recall","recall":{"recall_for":"shutdown marker"}}).to_string(),
    )
    .unwrap();
    let body = wire::encode_wire(request.request()).unwrap();
    let id = RequestId::new();
    send(
        &mut data,
        &CallHeader {
            id,
            target: CallTarget::Store(binding),
            kind: RequestKind::Query,
            body_len: body.len() as u32,
        },
    );
    assert_eq!(recv::<DataFrame>(&mut data), DataFrame::Proceed { id });
    client.shutdown("test").unwrap();
    data.write_all(&body).unwrap();
    assert_eq!(
        recv::<DataFrame>(&mut data),
        DataFrame::Rejected {
            id,
            reason: RejectReason::Stopping
        }
    );
    let moved = db.with_file_name("control-moved.sqlite");
    wait_for(|| match std::fs::rename(&db, &moved) {
        Ok(()) => true,
        Err(e) if matches!(e.raw_os_error(), Some(32 | 33)) => false,
        Err(e) => panic!("rename: {e}"),
    });
    std::fs::rename(&moved, &db).unwrap();
    wait_for(|| host.lock().unwrap().try_wait().unwrap().is_some());
    assert_eq!(
        host.lock().unwrap().try_wait().unwrap().unwrap().code(),
        Some(0)
    );
    assert!(!installation.state().join("service/endpoint.json").exists());
    assert!(OwnerLock::try_acquire(installation.state())
        .unwrap()
        .is_some());
}

#[cfg(windows)]
#[test]
fn shutdown_immediately_after_attach_finishes_and_releases_store() {
    use crate::test_support::installation::TestInstallation;
    let installation = TestInstallation::new();
    installation.select_store("a").unwrap();
    let db = installation
        .config()
        .unwrap()
        .unwrap()
        .runtime_root(installation.state())
        .join("control.sqlite");
    let launcher = TestLauncher::new(installation.state());
    let host = start_host(&launcher);
    let client = attached(installation.state(), &launcher);
    client.shutdown("test").unwrap();
    wait_for(|| host.lock().unwrap().try_wait().unwrap().is_some());
    assert_eq!(
        host.lock().unwrap().try_wait().unwrap().unwrap().code(),
        Some(0)
    );
    assert!(!installation.state().join("service/endpoint.json").exists());
    assert!(OwnerLock::try_acquire(installation.state())
        .unwrap()
        .is_some());
    let moved = db.with_file_name("control-moved.sqlite");
    std::fs::rename(&db, &moved).unwrap();
    std::fs::rename(&moved, &db).unwrap();
}

#[cfg(windows)]
#[test]
fn switched_store_closes_and_pinned_request_reopens_it() {
    use crate::test_support::installation::TestInstallation;

    let installation = TestInstallation::new();
    installation.select_store("a").unwrap();
    let a_db = installation
        .config()
        .unwrap()
        .unwrap()
        .runtime_root(installation.state())
        .join("control.sqlite");
    installation.select_store("b").unwrap();
    let b_db = installation
        .config()
        .unwrap()
        .unwrap()
        .runtime_root(installation.state())
        .join("control.sqlite");
    installation.select_store("a").unwrap();
    let project = installation.state().parent().unwrap().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let launcher = TestLauncher::new(installation.state());
    let _host = start_host(&launcher);
    let client = attached(installation.state(), &launcher);
    let request = |input: Value| ValidatedRequest::parse_public(&input.to_string()).unwrap();
    let control = RequestControl::default();
    let a_binding = client.resolve_binding().unwrap();
    let saved = client.call(CallTarget::Store(a_binding), request(json!({
        "action":"remember_this", "remember_this":{"memories":[{
            "operation":"create", "what_to_remember":"A retained marker for reopening the selected store",
            "evidence":"Store switch fixture", "applies_to":["user"]
        }]}
    })), &control).unwrap();
    let id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .expect("saved memory id")
        .to_owned();
    client
        .call(
            CallTarget::Setup,
            request(json!({
                "action":"connect_project_memory", "connect_project_memory":{
                    "project_path":project, "memory_store_path":installation.store_path("b")
                }
            })),
            &control,
        )
        .unwrap();
    let b_binding = client.resolve_binding().unwrap();
    assert_ne!(a_binding, b_binding);
    let recall = || request(json!({"action":"recall", "recall":{"memory_ids":[id]}}));
    let b_result = client
        .call(CallTarget::Store(b_binding), recall(), &control)
        .unwrap();
    assert_eq!(b_result["missing_ids"]["memory_ids"][0], id);

    let a_renamed = a_db.with_file_name("control-test-moved.sqlite");
    wait_for(|| match std::fs::rename(&a_db, &a_renamed) {
        Ok(()) => true,
        Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => false,
        Err(error) => panic!("renaming A control database: {error}"),
    });
    std::fs::rename(&a_renamed, &a_db).unwrap();
    let b_renamed = b_db.with_file_name("control-test-moved.sqlite");
    match std::fs::rename(&b_db, &b_renamed) {
        Err(error) => assert!(
            matches!(error.raw_os_error(), Some(32 | 33)),
            "unexpected B rename error: {error}"
        ),
        Ok(()) => {
            std::fs::rename(&b_renamed, &b_db).unwrap();
            panic!("selected store B control database was not held open");
        }
    }
    let restored = client
        .call(CallTarget::Store(a_binding), recall(), &control)
        .unwrap();
    assert_eq!(
        restored["memories"][0]["current_heads"][0]["what_to_remember"],
        "A retained marker for reopening the selected store"
    );
}
