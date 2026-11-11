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
            assets: None,
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
