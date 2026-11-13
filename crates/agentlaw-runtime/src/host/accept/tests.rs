use super::*;
use agentlaw_contracts::{wire, DomainError, RequestControl};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::{
    io::Write,
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Condvar, Mutex,
    },
    time::Duration,
};

const SECRET: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn limits() -> HostLimits {
    HostLimits {
        admission_slots: 2,
        body_budget_bytes: 64 * 1024 * 1024,
    }
}

#[test]
fn default_resource_profile_has_specified_capacity() {
    let limits = HostLimits::default();
    assert_eq!(limits.admission_slots, 64);
    assert_eq!(limits.body_budget_bytes, 64 * 1024 * 1024);
}

struct BudgetDispatch {
    admissions: AtomicUsize,
    runs: AtomicUsize,
    started: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl Dispatcher for BudgetDispatch {
    type Admitted = ();
    fn admit(&self, _: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        self.admissions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn run(&self, _: (), _: &RequestControl) -> Result<Value, DomainError> {
        if self.runs.fetch_add(1, Ordering::SeqCst) == 0 {
            self.started.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        Ok(json!({"ok": true}))
    }
}

#[test]
fn body_budget_rejects_until_running_execution_releases_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let dispatch = Arc::new(BudgetDispatch {
        admissions: AtomicUsize::new(0),
        runs: AtomicUsize::new(0),
        started: started_tx,
        release: Mutex::new(release_rx),
    });
    let host = start(
        dir.path(),
        "build",
        SECRET,
        dispatch.clone(),
        Arc::new(ConfigRegistry::new()),
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        HostLimits {
            admission_slots: 2,
            body_budget_bytes: 1024 * 1024,
        },
    )
    .expect("start host");
    let request = wire::ValidatedRequest::parse_public(
        &json!({"action":"recall", "recall":{"recall_for":"x".repeat(700 * 1024)}}).to_string(),
    )
    .unwrap();
    let bytes = wire::encode_wire(request.request()).unwrap();
    assert!(bytes.len() < 1024 * 1024 && bytes.len() * 2 > 1024 * 1024);
    let connect = || {
        let mut stream = TcpStream::connect_timeout(&host.data, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        send(&mut stream, &hello());
        assert_eq!(receive::<HelloReply>(&mut stream), HelloReply::Accepted);
        stream
    };
    let mut first = connect();
    let first_id = RequestId::new();
    send(
        &mut first,
        &header(first_id, RequestKind::Query, bytes.len() as u32),
    );
    assert_eq!(
        receive::<DataFrame>(&mut first),
        DataFrame::Proceed { id: first_id }
    );
    first.write_all(&bytes).unwrap();
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut second = connect();
    let rejected_id = RequestId::new();
    send(
        &mut second,
        &header(rejected_id, RequestKind::Query, bytes.len() as u32),
    );
    assert_eq!(
        receive::<DataFrame>(&mut second),
        DataFrame::Rejected {
            id: rejected_id,
            reason: RejectReason::Busy
        }
    );
    assert_eq!(dispatch.admissions.load(Ordering::SeqCst), 1);
    assert_eq!(dispatch.runs.load(Ordering::SeqCst), 1);
    release_tx.send(()).unwrap();
    assert_eq!(
        receive::<DataFrame>(&mut first),
        DataFrame::Completed {
            id: first_id,
            result: json!({"ok":true})
        }
    );
    drop(second);
    let mut retry = connect();
    let retry_id = RequestId::new();
    send(
        &mut retry,
        &header(retry_id, RequestKind::Query, bytes.len() as u32),
    );
    assert_eq!(
        receive::<DataFrame>(&mut retry),
        DataFrame::Proceed { id: retry_id }
    );
    retry.write_all(&bytes).unwrap();
    assert_eq!(
        receive::<DataFrame>(&mut retry),
        DataFrame::Completed {
            id: retry_id,
            result: json!({"ok":true})
        }
    );
    assert_eq!(dispatch.admissions.load(Ordering::SeqCst), 2);
    assert_eq!(dispatch.runs.load(Ordering::SeqCst), 2);
}

#[derive(Default)]
struct Fake {
    admissions: AtomicUsize,
    runs: AtomicUsize,
    reject: Option<RejectReason>,
    targets: Mutex<Vec<CallTarget>>,
}
impl Dispatcher for Fake {
    type Admitted = ();
    fn admit(&self, target: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        self.admissions.fetch_add(1, Ordering::SeqCst);
        self.targets.lock().unwrap().push(*target);
        match &self.reject {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }
    fn run(&self, _: (), _: &RequestControl) -> Result<Value, DomainError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"ok":true}))
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    host: Host,
    fake: Arc<Fake>,
}
impl Fixture {
    fn new(fake: Fake) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(fake);
        let host = start(
            dir.path(),
            "build",
            SECRET,
            fake.clone(),
            Arc::new(ConfigRegistry::new()),
            HealthSink::default(),
            Lifecycle::system(Arc::new(|_| {})),
            limits(),
        )
        .expect("start host");
        Self {
            _dir: dir,
            host,
            fake,
        }
    }
    fn connect(&self, address: SocketAddr) -> TcpStream {
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
    }
    fn data(&self) -> TcpStream {
        let mut stream = self.connect(self.host.data);
        send(&mut stream, &hello());
        assert_eq!(receive::<HelloReply>(&mut stream), HelloReply::Accepted);
        stream
    }
}
fn hello() -> Hello {
    Hello {
        protocol: PROTOCOL_VERSION,
        installation_build: "build".into(),
        secret: SECRET.into(),
    }
}
fn send<T: Serialize>(stream: &mut TcpStream, value: &T) {
    write_frame(
        stream,
        &serde_json::to_vec(value).unwrap(),
        MAX_CONTROL_FRAME_BYTES,
    )
    .unwrap();
}
fn receive<T: DeserializeOwned>(stream: &mut TcpStream) -> T {
    serde_json::from_slice(&read_frame(stream, MAX_RESULT_FRAME_BYTES).unwrap()).unwrap()
}
fn body() -> Vec<u8> {
    let request = wire::ValidatedRequest::parse_public(&json!({"action":"connect_project_memory", "connect_project_memory":{"project_path":"C:/work/app", "intent":"discover"}}).to_string()).unwrap();
    wire::encode_wire(request.request()).unwrap()
}
fn header(id: RequestId, kind: RequestKind, len: u32) -> CallHeader {
    let target = if kind == RequestKind::Setup {
        CallTarget::Setup
    } else {
        CallTarget::Store(serde_json::from_value(json!("00".repeat(32))).unwrap())
    };
    CallHeader {
        id,
        target,
        kind,
        body_len: len,
    }
}

#[test]
fn shutdown_rejects_body_that_arrives_after_proceed() {
    let fixture = Fixture::new(Fake::default());
    let mut data = fixture.data();
    let bytes = body();
    let id = RequestId::new();
    send(
        &mut data,
        &header(id, RequestKind::Query, bytes.len() as u32),
    );
    assert_eq!(receive::<DataFrame>(&mut data), DataFrame::Proceed { id });
    let mut control = fixture.connect(fixture.host.control);
    send(&mut control, &hello());
    assert_eq!(receive::<HelloReply>(&mut control), HelloReply::Accepted);
    send(
        &mut control,
        &ControlRequest::Shutdown {
            reason: "test".into(),
        },
    );
    assert_eq!(
        receive::<ControlReply>(&mut control),
        ControlReply::ShutdownAccepted
    );
    data.write_all(&bytes).unwrap();
    assert_eq!(
        receive::<DataFrame>(&mut data),
        DataFrame::Rejected {
            id,
            reason: RejectReason::Stopping
        }
    );
    assert_eq!(fixture.fake.admissions.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.fake.runs.load(Ordering::SeqCst), 0);
}

#[test]
fn endpoint_file_contains_exact_documented_fields() {
    let fixture = Fixture::new(Fake::default());
    let bytes = std::fs::read(fixture._dir.path().join("service/endpoint.json")).unwrap();
    let endpoint: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        endpoint,
        json!({
            "protocol": PROTOCOL_VERSION,
            "installation_build": "build",
            "control": fixture.host.control.to_string(),
            "data": fixture.host.data.to_string(),
            "secret": SECRET,
            "pid": std::process::id(),
        })
    );
}

#[test]
fn client_hello_uses_own_build_and_does_not_retry_build_mismatch() {
    use crate::client::Client;
    use std::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let host = start(
        dir.path(),
        "build-a",
        SECRET,
        fake.clone(),
        Arc::new(ConfigRegistry::new()),
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        limits(),
    )
    .expect("start host");
    let endpoint_path = dir.path().join("service/endpoint.json");
    let original = std::fs::read(&endpoint_path).unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut endpoint: Value = serde_json::from_slice(&original).unwrap();
    endpoint["control"] = json!(proxy.local_addr().unwrap().to_string());
    std::fs::write(&endpoint_path, serde_json::to_vec(&endpoint).unwrap()).unwrap();
    let listener = proxy.try_clone().unwrap();
    let (hello_tx, hello_rx) = mpsc::channel();
    let control_address = host.control;
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut frontend = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::park_timeout(Duration::from_millis(5))
                }
                Err(e) => panic!("proxy accept: {e}"),
            }
        };
        frontend.set_nonblocking(false).unwrap();
        frontend
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        frontend
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let hello: Hello = receive(&mut frontend);
        let mut upstream =
            TcpStream::connect_timeout(&control_address, Duration::from_secs(2)).unwrap();
        upstream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        upstream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        send(&mut upstream, &hello);
        let reply: HelloReply = receive(&mut upstream);
        send(&mut frontend, &reply);
        hello_tx.send((hello, reply)).unwrap();
    });
    let (result_tx, result_rx) = mpsc::channel();
    let state = dir.path().to_owned();
    std::thread::spawn(move || {
        let _ = result_tx.send(
            Client::connect(&state, "build-b")
                .err()
                .map(|e| format!("{e:?}")),
        );
    });
    let (hello, reply) = hello_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(hello.installation_build, "build-b");
    assert_eq!(reply, HelloReply::BuildMismatch);
    let error = result_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .expect("mismatched build must fail");
    assert!(error.to_lowercase().contains("build"));
    proxy.set_nonblocking(true).unwrap();
    assert!(matches!(proxy.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    assert_eq!(fake.admissions.load(Ordering::SeqCst), 0);
    std::fs::write(&endpoint_path, original).unwrap();
    let (result_tx, result_rx) = mpsc::channel();
    let state = dir.path().to_owned();
    std::thread::spawn(move || {
        let _ = result_tx.send(Client::connect(&state, "build-a").is_ok());
    });
    assert!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap());
}

struct LongDispatch {
    runs: AtomicUsize,
}
impl Dispatcher for LongDispatch {
    type Admitted = ();
    fn admit(&self, _: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        Ok(())
    }
    fn run(&self, _: (), _: &RequestControl) -> Result<Value, DomainError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let (_keep_sender, wait) = mpsc::channel::<()>();
        assert!(matches!(
            wait.recv_timeout(Duration::from_secs(6)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        Ok(json!({"long":"completed"}))
    }
}

#[test]
fn long_silent_query_and_command_complete_without_retry() {
    use crate::client::Client;
    for command in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(LongDispatch {
            runs: AtomicUsize::new(0),
        });
        let _host = start(
            dir.path(),
            "build",
            SECRET,
            fake.clone(),
            Arc::new(ConfigRegistry::new()),
            HealthSink::default(),
            Lifecycle::system(Arc::new(|_| {})),
            limits(),
        )
        .expect("start host");
        let client = Client::connect(dir.path(), "build").expect("connect");
        let input = if command {
            json!({"action":"connect_project_memory", "connect_project_memory":{"project_path":"C:/work/app", "restore_context":true, "recall_for":"project context"}})
        } else {
            json!({"action":"recall", "recall":{"recall_for":"project context"}})
        };
        let request = wire::ValidatedRequest::parse_public(&input.to_string()).unwrap();
        let target = header(RequestId::new(), RequestKind::Query, 0).target;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(client.call(target, request, &RequestControl::default()));
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(20)).unwrap().unwrap(),
            json!({"long":"completed"})
        );
        assert_eq!(fake.runs.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn hello_rejects_secret_build_and_protocol_before_dispatch() {
    let f = Fixture::new(Fake::default());
    for (h, expected) in [
        (
            Hello {
                secret: "bb".repeat(32),
                ..hello()
            },
            HelloReply::NotAuthorized,
        ),
        (
            Hello {
                installation_build: "other".into(),
                ..hello()
            },
            HelloReply::BuildMismatch,
        ),
        (
            Hello {
                protocol: PROTOCOL_VERSION + 1,
                ..hello()
            },
            HelloReply::ProtocolMismatch,
        ),
    ] {
        for address in [f.host.control, f.host.data] {
            let mut stream = f.connect(address);
            send(&mut stream, &h);
            assert_eq!(receive::<HelloReply>(&mut stream), expected);
        }
    }
    assert_eq!(f.fake.admissions.load(Ordering::SeqCst), 0);
    assert_eq!(f.fake.runs.load(Ordering::SeqCst), 0);
}

#[test]
fn oversized_header_is_rejected_without_sending_body() {
    let f = Fixture::new(Fake::default());
    let mut stream = f.data();
    let id = RequestId::new();
    send(
        &mut stream,
        &header(id, RequestKind::Setup, MAX_WIRE_BODY_BYTES + 1),
    );
    assert_eq!(
        receive::<DataFrame>(&mut stream),
        DataFrame::Rejected {
            id,
            reason: RejectReason::BodyTooLarge
        }
    );
    assert_eq!(f.fake.admissions.load(Ordering::SeqCst), 0);
}

#[test]
fn invalid_body_is_rejected_before_admission() {
    let f = Fixture::new(Fake::default());
    let mut stream = f.data();
    let id = RequestId::new();
    send(&mut stream, &header(id, RequestKind::Setup, 1));
    assert_eq!(receive::<DataFrame>(&mut stream), DataFrame::Proceed { id });
    stream.write_all(b"{").unwrap();
    assert!(
        matches!(receive::<DataFrame>(&mut stream), DataFrame::Rejected { id: got, reason: RejectReason::InvalidBody(_) } if got == id)
    );
    assert_eq!(f.fake.admissions.load(Ordering::SeqCst), 0);
}

#[test]
fn mismatched_classification_is_rejected_before_admission() {
    let f = Fixture::new(Fake::default());
    let mut stream = f.data();
    let bytes = body();
    let id = RequestId::new();
    send(
        &mut stream,
        &header(id, RequestKind::Command, bytes.len() as u32),
    );
    assert_eq!(receive::<DataFrame>(&mut stream), DataFrame::Proceed { id });
    stream.write_all(&bytes).unwrap();
    assert_eq!(
        receive::<DataFrame>(&mut stream),
        DataFrame::Rejected {
            id,
            reason: RejectReason::KindMismatch
        }
    );
    assert_eq!(f.fake.admissions.load(Ordering::SeqCst), 0);
}

#[test]
fn admission_error_is_rejected_and_never_run() {
    for reason in [
        RejectReason::BindingChanged,
        RejectReason::ConnectionRequired,
        RejectReason::ConfigInvalid("bad".into()),
        RejectReason::StoreUnavailable("gone".into()),
    ] {
        let f = Fixture::new(Fake {
            reject: Some(reason.clone()),
            ..Fake::default()
        });
        let mut stream = f.data();
        let bytes = body();
        let id = RequestId::new();
        send(
            &mut stream,
            &header(id, RequestKind::Query, bytes.len() as u32),
        );
        assert_eq!(receive::<DataFrame>(&mut stream), DataFrame::Proceed { id });
        stream.write_all(&bytes).unwrap();
        assert_eq!(
            receive::<DataFrame>(&mut stream),
            DataFrame::Rejected { id, reason }
        );
        assert_eq!(f.fake.runs.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn eof_mid_body_does_not_prevent_following_request() {
    let f = Fixture::new(Fake::default());
    let bytes = body();
    for _ in 0..2 {
        let mut first = f.data();
        let id = RequestId::new();
        send(
            &mut first,
            &header(id, RequestKind::Query, bytes.len() as u32),
        );
        assert_eq!(receive::<DataFrame>(&mut first), DataFrame::Proceed { id });
        first.write_all(&bytes[..bytes.len() / 2]).unwrap();
        first.shutdown(Shutdown::Write).unwrap();
        let mut byte = [0];
        use std::io::Read;
        assert_eq!(first.read(&mut byte).unwrap(), 0);
    }
    let mut next = f.data();
    let id = RequestId::new();
    send(
        &mut next,
        &header(id, RequestKind::Query, bytes.len() as u32),
    );
    assert_eq!(receive::<DataFrame>(&mut next), DataFrame::Proceed { id });
    next.write_all(&bytes).unwrap();
    assert_eq!(
        receive::<DataFrame>(&mut next),
        DataFrame::Completed {
            id,
            result: json!({"ok":true})
        }
    );
    assert_eq!(f.fake.runs.load(Ordering::SeqCst), 1);
}

#[test]
fn control_lease_health_and_inactive_cancel_are_available() {
    let f = Fixture::new(Fake::default());
    let mut stream = f.connect(f.host.control);
    send(&mut stream, &hello());
    assert_eq!(receive::<HelloReply>(&mut stream), HelloReply::Accepted);
    send(
        &mut stream,
        &ControlRequest::Lease {
            client: "frontend".into(),
        },
    );
    assert_eq!(
        receive::<ControlReply>(&mut stream),
        ControlReply::LeaseRenewed
    );
    send(&mut stream, &ControlRequest::Health);
    assert!(matches!(
        receive::<ControlReply>(&mut stream),
        ControlReply::Health(_)
    ));
    send(&mut stream, &ControlRequest::Cancel(RequestId::new()));
    assert_eq!(
        receive::<ControlReply>(&mut stream),
        ControlReply::NotActive
    );
    send(&mut stream, &ControlRequest::ResolveBinding);
    assert_eq!(
        receive::<ControlReply>(&mut stream),
        ControlReply::BindingError(BindingError::ConnectionRequired)
    );
    send(
        &mut stream,
        &ControlRequest::Shutdown {
            reason: "test".into(),
        },
    );
    assert_eq!(
        receive::<ControlReply>(&mut stream),
        ControlReply::ShutdownAccepted
    );
}

struct Blocking {
    started: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    finished: mpsc::Sender<()>,
}
struct Completion(mpsc::Sender<()>);
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
impl Dispatcher for Blocking {
    type Admitted = ();
    fn admit(&self, _: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        Ok(())
    }
    fn run(&self, _: (), _: &RequestControl) -> Result<Value, DomainError> {
        let _cleanup = Completion(self.finished.clone());
        self.started.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(json!({"done": true}))
    }
}

#[test]
fn disconnect_after_dispatch_does_not_cancel_work_or_block_health() {
    let dir = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let host = start(
        dir.path(),
        "build",
        SECRET,
        Arc::new(Blocking {
            started: started_tx,
            release: Mutex::new(release_rx),
            finished: finished_tx,
        }),
        Arc::new(ConfigRegistry::new()),
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        limits(),
    )
    .expect("start host");
    let mut data = TcpStream::connect_timeout(&host.data, Duration::from_secs(2)).unwrap();
    data.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    data.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    send(&mut data, &hello());
    assert_eq!(receive::<HelloReply>(&mut data), HelloReply::Accepted);
    let bytes = body();
    let id = RequestId::new();
    send(
        &mut data,
        &header(id, RequestKind::Query, bytes.len() as u32),
    );
    assert_eq!(receive::<DataFrame>(&mut data), DataFrame::Proceed { id });
    data.write_all(&bytes).unwrap();
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    data.shutdown(Shutdown::Both).unwrap();
    let mut control = TcpStream::connect_timeout(&host.control, Duration::from_secs(2)).unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    control
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    send(&mut control, &hello());
    assert_eq!(receive::<HelloReply>(&mut control), HelloReply::Accepted);
    send(&mut control, &ControlRequest::Health);
    assert!(matches!(
        receive::<ControlReply>(&mut control),
        ControlReply::Health(_)
    ));
    release_tx.send(()).unwrap();
    finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
}

#[test]
fn lease_renewal_extends_warm_state_until_sixty_seconds_after_renewal() {
    use std::time::Instant;
    let now = Arc::new(Mutex::new(Instant::now()));
    let warm = Arc::new(Mutex::new(Vec::new()));
    let clock = now.clone();
    let observed = warm.clone();
    let lifecycle = Lifecycle::new(
        Arc::new(move || *clock.lock().unwrap()),
        Arc::new(move |value| observed.lock().unwrap().push(value)),
    );
    let dir = tempfile::tempdir().unwrap();
    let host = start(
        dir.path(),
        "build",
        SECRET,
        Arc::new(Fake::default()),
        Arc::new(ConfigRegistry::new()),
        HealthSink::default(),
        lifecycle.clone(),
        limits(),
    )
    .expect("start host");
    let lease = || {
        let mut control =
            TcpStream::connect_timeout(&host.control, Duration::from_secs(2)).unwrap();
        control
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        control
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        send(&mut control, &hello());
        assert_eq!(receive::<HelloReply>(&mut control), HelloReply::Accepted);
        send(
            &mut control,
            &ControlRequest::Lease {
                client: "frontend".into(),
            },
        );
        assert_eq!(
            receive::<ControlReply>(&mut control),
            ControlReply::LeaseRenewed
        );
    };
    lease();
    *now.lock().unwrap() += Duration::from_secs(40);
    lease();
    *now.lock().unwrap() += Duration::from_secs(59);
    lifecycle.poll();
    assert_eq!(*warm.lock().unwrap(), [true]);
    *now.lock().unwrap() += Duration::from_secs(1);
    lifecycle.poll();
    assert_eq!(*warm.lock().unwrap(), [true, false]);
}

fn snapshot(
    dir: &std::path::Path,
    name: &str,
) -> Arc<crate::installation::registry::ConfigSnapshot> {
    use crate::installation::{config::Config, registry::ConfigSnapshot};
    let path = dir.join(name);
    std::fs::create_dir_all(&path).unwrap();
    let key = agentlaw_storage::binding_identity(&path, &path).unwrap();
    let config = Config {
        memory_store_path: path,
        user_id: "user".into(),
        history_response_limit_bytes: 8192,
        runtime_path: None,
        response_limit_bytes: 65536,
    };
    let reference = BindingRef::new(&key, &config.user_id);
    Arc::new(ConfigSnapshot {
        config,
        key,
        reference,
    })
}

#[test]
fn client_call_remains_pinned_to_original_binding_after_selection_changes() {
    use crate::{client::Client, installation::registry::ConfigState};
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(ConfigRegistry::new());
    let a = snapshot(dir.path(), "a");
    let b = snapshot(dir.path(), "b");
    registry.publish(ConfigState::Ready(a.clone()));
    let fake = Arc::new(Fake::default());
    let _host = start(
        dir.path(),
        "build",
        SECRET,
        fake.clone(),
        registry.clone(),
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        limits(),
    )
    .expect("start host");
    let client = Client::connect(dir.path(), "build").expect("connect");
    let selected = client.resolve_binding().unwrap();
    assert_eq!(selected, a.reference);
    registry.publish(ConfigState::Ready(b));
    let request = wire::decode_wire(&body()).unwrap();
    assert_eq!(
        client
            .call(
                CallTarget::Store(selected),
                request,
                &RequestControl::default()
            )
            .unwrap(),
        json!({"ok": true})
    );
    assert_eq!(fake.runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        *fake.targets.lock().unwrap(),
        [CallTarget::Store(a.reference)]
    );
}

struct Cancellable {
    started: mpsc::Sender<()>,
    inspect: Mutex<mpsc::Receiver<()>>,
    result: mpsc::Sender<bool>,
}
impl Dispatcher for Cancellable {
    type Admitted = ();
    fn admit(&self, _: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        Ok(())
    }
    fn run(&self, _: (), control: &RequestControl) -> Result<Value, DomainError> {
        self.started.send(()).unwrap();
        self.inspect
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        self.result
            .send(control.cancel.load(Ordering::Acquire))
            .unwrap();
        Ok(json!({}))
    }
}

#[test]
fn cancel_active_request_sets_dispatcher_flag() {
    let dir = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (inspect_tx, inspect_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let host = start(
        dir.path(),
        "build",
        SECRET,
        Arc::new(Cancellable {
            started: started_tx,
            inspect: Mutex::new(inspect_rx),
            result: result_tx,
        }),
        Arc::new(ConfigRegistry::new()),
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        limits(),
    )
    .expect("start host");
    let mut data = TcpStream::connect_timeout(&host.data, Duration::from_secs(2)).unwrap();
    data.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    data.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    send(&mut data, &hello());
    assert_eq!(receive::<HelloReply>(&mut data), HelloReply::Accepted);
    let bytes = body();
    let id = RequestId::new();
    send(
        &mut data,
        &header(id, RequestKind::Query, bytes.len() as u32),
    );
    assert_eq!(receive::<DataFrame>(&mut data), DataFrame::Proceed { id });
    data.write_all(&bytes).unwrap();
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut control = TcpStream::connect_timeout(&host.control, Duration::from_secs(2)).unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    control
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    send(&mut control, &hello());
    assert_eq!(receive::<HelloReply>(&mut control), HelloReply::Accepted);
    send(&mut control, &ControlRequest::Cancel(id));
    assert_eq!(
        receive::<ControlReply>(&mut control),
        ControlReply::CancellationRequested
    );
    inspect_tx.send(()).unwrap();
    assert!(result_rx.recv_timeout(Duration::from_secs(2)).unwrap());
}

#[test]
fn saturated_data_handlers_reject_unread_with_small_frame_and_close() {
    let f = Fixture::new(Fake::default());
    let _held = [f.data(), f.data()];
    let mut stream = f.connect(f.host.data);
    let frame = read_frame(&mut stream, MAX_CONTROL_REPLY_BYTES).unwrap();
    assert!(frame.len() + 4 <= 64);
    assert_eq!(
        serde_json::from_slice::<DataFrame>(&frame).unwrap(),
        DataFrame::RejectedUnread {
            reason: RejectReason::Busy
        }
    );
    let mut byte = [0];
    use std::io::Read;
    assert_eq!(stream.read(&mut byte).unwrap(), 0);
    assert_eq!(f.fake.runs.load(Ordering::SeqCst), 0);
}

struct LargeResult;
impl Dispatcher for LargeResult {
    type Admitted = ();
    fn admit(&self, _: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        Ok(())
    }
    fn run(&self, _: (), _: &RequestControl) -> Result<Value, DomainError> {
        Ok(json!({"data": "x".repeat(100_000)}))
    }
}

#[test]
fn oversized_result_is_delivered_as_complete_file() {
    use crate::{client::Client, installation::registry::ConfigState};
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(ConfigRegistry::new());
    registry.publish(ConfigState::Ready(snapshot(dir.path(), "a")));
    let _host = start(
        dir.path(),
        "build",
        SECRET,
        Arc::new(LargeResult),
        registry,
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        limits(),
    )
    .expect("start host");
    let client = Client::connect(dir.path(), "build").unwrap();
    let target = CallTarget::Store(client.resolve_binding().unwrap());
    let result = client
        .call(
            target,
            wire::decode_wire(&body()).unwrap(),
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    let path = result["artifact"]["path"].as_str().unwrap();
    let contents: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(contents, json!({"data": "x".repeat(100_000)}));
}

struct RegistryDispatch {
    registry: Arc<ConfigRegistry>,
    runs: AtomicUsize,
}
impl Dispatcher for RegistryDispatch {
    type Admitted = ();
    fn admit(&self, target: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        match target {
            CallTarget::Store(reference) => {
                self.registry
                    .resolve(reference)
                    .map(|_| ())
                    .map_err(|error| match error {
                        BindingError::BindingChanged => RejectReason::BindingChanged,
                        BindingError::ConnectionRequired => RejectReason::ConnectionRequired,
                        BindingError::ConfigInvalid(reason) => RejectReason::ConfigInvalid(reason),
                    })
            }
            CallTarget::Setup => Ok(()),
        }
    }
    fn run(&self, _: (), _: &RequestControl) -> Result<Value, DomainError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }
}

#[test]
fn unknown_binding_after_restart_is_rejected_without_running_current_selection() {
    use crate::{
        client::{Client, ClientError},
        installation::registry::ConfigState,
    };
    let dir = tempfile::tempdir().unwrap();
    let a = snapshot(dir.path(), "a");
    let b = snapshot(dir.path(), "b");
    let registry = Arc::new(ConfigRegistry::new());
    registry.publish(ConfigState::Ready(b));
    let fake = Arc::new(RegistryDispatch {
        registry: registry.clone(),
        runs: AtomicUsize::new(0),
    });
    let _host = start(
        dir.path(),
        "build",
        SECRET,
        fake.clone(),
        registry,
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        limits(),
    )
    .expect("start host");
    let client = Client::connect(dir.path(), "build").unwrap();
    assert!(matches!(
        client.call(
            CallTarget::Store(a.reference),
            wire::decode_wire(&body()).unwrap(),
            &RequestControl::default()
        ),
        Err(ClientError::Rejected(RejectReason::BindingChanged))
    ));
    assert_eq!(fake.runs.load(Ordering::SeqCst), 0);
}

struct SaturatedDispatch {
    started: mpsc::Sender<()>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}
impl Dispatcher for SaturatedDispatch {
    type Admitted = ();
    fn admit(&self, _: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        Ok(())
    }
    fn run(&self, _: (), _: &RequestControl) -> Result<Value, DomainError> {
        self.started.send(()).unwrap();
        let (lock, changed) = &*self.gate;
        let (released, _) = changed
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |released| {
                !*released
            })
            .unwrap();
        assert!(*released, "test did not release dispatcher");
        Ok(json!({}))
    }
}

#[test]
fn control_requests_complete_while_all_data_slots_run() {
    let dir = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let host = start(
        dir.path(),
        "build",
        SECRET,
        Arc::new(SaturatedDispatch {
            started: started_tx,
            gate: gate.clone(),
        }),
        Arc::new(ConfigRegistry::new()),
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        limits(),
    )
    .expect("start host");
    let mut held = Vec::new();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let mut stream = TcpStream::connect_timeout(&host.data, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        send(&mut stream, &hello());
        assert_eq!(receive::<HelloReply>(&mut stream), HelloReply::Accepted);
        let bytes = body();
        let id = RequestId::new();
        send(
            &mut stream,
            &header(id, RequestKind::Query, bytes.len() as u32),
        );
        assert_eq!(receive::<DataFrame>(&mut stream), DataFrame::Proceed { id });
        stream.write_all(&bytes).unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        ids.push(id);
        held.push(stream);
    }
    for (request, expected) in [
        (
            ControlRequest::ResolveBinding,
            Some(ControlReply::BindingError(BindingError::ConnectionRequired)),
        ),
        (
            ControlRequest::Lease {
                client: "saturated".into(),
            },
            Some(ControlReply::LeaseRenewed),
        ),
        (
            ControlRequest::Cancel(ids[0]),
            Some(ControlReply::CancellationRequested),
        ),
        (ControlRequest::Health, None),
        (
            ControlRequest::Shutdown {
                reason: "test".into(),
            },
            Some(ControlReply::ShutdownAccepted),
        ),
    ] {
        let mut stream = TcpStream::connect_timeout(&host.control, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        send(&mut stream, &hello());
        assert_eq!(receive::<HelloReply>(&mut stream), HelloReply::Accepted);
        send(&mut stream, &request);
        let reply = receive::<ControlReply>(&mut stream);
        if let Some(expected) = expected {
            assert_eq!(reply, expected);
        } else {
            assert!(matches!(reply, ControlReply::Health(_)));
        }
    }
    let (lock, changed) = &*gate;
    *lock.lock().unwrap() = true;
    changed.notify_all();
}

struct SlowReaderDispatch {
    calls: AtomicUsize,
    cleaned: mpsc::Sender<()>,
}
impl Dispatcher for SlowReaderDispatch {
    type Admitted = ();
    fn admit(&self, _: &CallTarget, _: Classified) -> Result<(), RejectReason> {
        Ok(())
    }
    fn run(&self, _: (), control: &RequestControl) -> Result<Value, DomainError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let _cleanup = Completion(self.cleaned.clone());
            for i in 0..10_000 {
                control.phase(&format!("phase-{i}"));
            }
            Ok(json!({"data": "x".repeat(32 * 1024 * 1024)}))
        } else {
            Ok(json!({"other": true}))
        }
    }
}

#[test]
fn nonreading_client_does_not_block_cleanup_other_request_or_health() {
    use crate::installation::registry::ConfigState;
    let dir = tempfile::tempdir().unwrap();
    let mut selected = snapshot(dir.path(), "a");
    Arc::get_mut(&mut selected)
        .unwrap()
        .config
        .response_limit_bytes = 64 * 1024 * 1024;
    let registry = Arc::new(ConfigRegistry::new());
    registry.publish(ConfigState::Ready(selected.clone()));
    let (cleaned_tx, cleaned_rx) = mpsc::channel();
    let host = start(
        dir.path(),
        "build",
        SECRET,
        Arc::new(SlowReaderDispatch {
            calls: AtomicUsize::new(0),
            cleaned: cleaned_tx,
        }),
        registry,
        HealthSink::default(),
        Lifecycle::system(Arc::new(|_| {})),
        limits(),
    )
    .expect("start host");
    let connect = |address| {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        send(&mut stream, &hello());
        assert_eq!(receive::<HelloReply>(&mut stream), HelloReply::Accepted);
        stream
    };
    let bytes = body();
    let mut slow = connect(host.data);
    let first = RequestId::new();
    let h = CallHeader {
        id: first,
        target: CallTarget::Store(selected.reference),
        kind: RequestKind::Query,
        body_len: bytes.len() as u32,
    };
    send(&mut slow, &h);
    assert_eq!(
        receive::<DataFrame>(&mut slow),
        DataFrame::Proceed { id: first }
    );
    slow.write_all(&bytes).unwrap();
    cleaned_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut other = connect(host.data);
    let second = RequestId::new();
    send(&mut other, &CallHeader { id: second, ..h });
    assert_eq!(
        receive::<DataFrame>(&mut other),
        DataFrame::Proceed { id: second }
    );
    other.write_all(&bytes).unwrap();
    assert_eq!(
        receive::<DataFrame>(&mut other),
        DataFrame::Completed {
            id: second,
            result: json!({"other":true})
        }
    );
    let mut control = connect(host.control);
    send(&mut control, &ControlRequest::Health);
    assert!(matches!(
        receive::<ControlReply>(&mut control),
        ControlReply::Health(_)
    ));
}
