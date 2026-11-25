use super::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::{
    io::Read,
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(3);

mod startup;

#[test]
fn attach_returns_candidate_maintenance_error() {
    struct BlockedLauncher;
    struct BlockedCandidate;
    impl RuntimeLauncher for BlockedLauncher {
        fn launch(&self) -> io::Result<Box<dyn RuntimeCandidate>> {
            Ok(Box::new(BlockedCandidate))
        }
    }
    impl RuntimeCandidate for BlockedCandidate {
        fn try_exit(&mut self) -> io::Result<Option<CandidateExit>> {
            Ok(Some(CandidateExit::StartBlocked(DomainError::new(
                "update_maintenance",
                "updating",
            ))))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let result = Client::attach(
        dir.path(),
        "test-build",
        &BlockedLauncher,
        AttachLimits {
            start_wait: Duration::from_millis(300),
            hello_timeout: Duration::from_millis(100),
        },
    );
    assert!(
        matches!(result, Err(ClientError::Domain(ref error)) if error.code == "update_maintenance")
    );
    assert!(!dir.path().join("service/endpoint.json").exists());
}
fn send<T: Serialize>(stream: &mut TcpStream, value: &T) {
    write_frame(
        stream,
        &serde_json::to_vec(value).unwrap(),
        MAX_CONTROL_FRAME_BYTES,
    )
    .unwrap();
}
fn recv<T: DeserializeOwned>(stream: &mut TcpStream) -> T {
    serde_json::from_slice(&read_frame(stream, MAX_RESULT_FRAME_BYTES).unwrap()).unwrap()
}
fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let until = Instant::now() + WAIT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(WAIT)).unwrap();
                stream.set_write_timeout(Some(WAIT)).unwrap();
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                thread::park_timeout(Duration::from_millis(5))
            }
            Err(e) => panic!("accept: {e}"),
        }
    }
}
fn hello(stream: &mut TcpStream) {
    let _: Hello = recv(stream);
    send(stream, &HelloReply::Accepted);
}
fn read_body(stream: &mut TcpStream, header: CallHeader) -> Vec<u8> {
    let mut body = vec![0; header.body_len as usize];
    stream.read_exact(&mut body).unwrap();
    body
}
fn target() -> CallTarget {
    CallTarget::Store(serde_json::from_value(json!("11".repeat(32))).unwrap())
}
fn request(kind: RequestKind) -> ValidatedRequest {
    let input = match kind {
        RequestKind::Query => {
            json!({"action":"connect_project_memory", "connect_project_memory":{"project_path":"C:/work/app", "intent":"discover"}})
        }
        RequestKind::Command => {
            json!({"action":"connect_project_memory", "connect_project_memory":{"project_path":"C:/work/app", "restore_context":true, "recall_for":"project context"}})
        }
        RequestKind::Setup => {
            json!({"action":"connect_project_memory", "connect_project_memory":{"project_path":"C:/work/app", "memory_store_path":"C:/work/store"}})
        }
        RequestKind::Admin => panic!("Admin is not a public request"),
    };
    ValidatedRequest::parse_public(&input.to_string()).unwrap()
}

struct Server {
    dir: tempfile::TempDir,
    data: TcpListener,
    stop: Arc<AtomicBool>,
    cancellations: Arc<AtomicUsize>,
    cancel_ids: Arc<(Mutex<Vec<RequestId>>, Condvar)>,
    leases: Arc<(Mutex<Vec<String>>, Condvar)>,
    hellos: Arc<AtomicUsize>,
}
impl Server {
    fn new() -> Self {
        Self::new_with_reply(HelloReply::Accepted)
    }
    fn new_with_reply(reply: HelloReply) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let control = TcpListener::bind("127.0.0.1:0").unwrap();
        let data = TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::create_dir_all(dir.path().join("service")).unwrap();
        std::fs::write(dir.path().join("service/endpoint.json"), serde_json::to_vec(&json!({"protocol":PROTOCOL_VERSION,"installation_build":"test-build","control":control.local_addr().unwrap().to_string(),"data":data.local_addr().unwrap().to_string(),"secret":"aa".repeat(32),"pid":std::process::id()})).unwrap()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let cancel_ids = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let leases = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let hellos = Arc::new(AtomicUsize::new(0));
        let ended = stop.clone();
        let observed = cancellations.clone();
        let ids = cancel_ids.clone();
        let lease_events = leases.clone();
        let hello_count = hellos.clone();
        thread::spawn(move || {
            control.set_nonblocking(true).unwrap();
            while !ended.load(Ordering::Acquire) {
                match control.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let observed = observed.clone();
                        let ids = ids.clone();
                        let lease_events = lease_events.clone();
                        let hello_count = hello_count.clone();
                        thread::spawn(move || {
                            stream.set_read_timeout(Some(WAIT)).unwrap();
                            stream.set_write_timeout(Some(WAIT)).unwrap();
                            let _: Hello = recv(&mut stream);
                            hello_count.fetch_add(1, Ordering::SeqCst);
                            send(&mut stream, &reply);
                            if reply != HelloReply::Accepted {
                                return;
                            }
                            while let Ok(bytes) = read_frame(&mut stream, MAX_CONTROL_FRAME_BYTES) {
                                let request: ControlRequest =
                                    serde_json::from_slice(&bytes).unwrap();
                                let reply = match request {
                                    ControlRequest::Cancel(id) => {
                                        observed.fetch_add(1, Ordering::SeqCst);
                                        ids.0.lock().unwrap().push(id);
                                        ids.1.notify_all();
                                        ControlReply::CancellationRequested
                                    }
                                    ControlRequest::Lease { client } => {
                                        lease_events.0.lock().unwrap().push(client);
                                        lease_events.1.notify_all();
                                        ControlReply::LeaseRenewed
                                    }
                                    _ => ControlReply::NotActive,
                                };
                                send(&mut stream, &reply);
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::park_timeout(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            dir,
            data,
            stop,
            cancellations,
            cancel_ids,
            leases,
            hellos,
        }
    }
    fn connect(&self) -> Client {
        Client::connect(self.dir.path(), "test-build").expect("connect client")
    }
}

#[test]
fn attach_renews_a_uuid_lease_without_waiting_for_periodic_renewal() {
    let server = Server::new();
    let _client = server.connect();
    let (lock, changed) = &*server.leases;
    let (leases, _) = changed
        .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(1), |leases| {
            leases.is_empty()
        })
        .unwrap();
    assert!(
        !leases.is_empty(),
        "attach did not renew a lease within one second"
    );
    let id = uuid::Uuid::parse_str(&leases[0]).expect("client lease identity is a UUID");
    assert_eq!(id.get_version_num(), 4);
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
fn call_async(
    client: Client,
    target: CallTarget,
    kind: RequestKind,
    control: RequestControl,
) -> mpsc::Receiver<Result<Value, ClientError>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(client.call(target, request(kind), &control));
    });
    rx
}

#[test]
fn failed_frame_preserves_domain_error_fields() {
    let server = Server::new();
    let client = server.connect();
    let listener = server.data.try_clone().unwrap();
    let error = DomainError {
        code: "capacity".into(),
        next_action: Some("Wait for indexing to catch up, then repeat the original recall.".into()),
        message: "\u{b2e4}\u{c2dc} \u{c2dc}\u{b3c4}\u{d558}\u{c138}\u{c694}".into(),
        retryable: true,
        details: None,
    };
    let sent = error.clone();
    thread::spawn(move || {
        let mut stream = accept(&listener);
        hello(&mut stream);
        let header: CallHeader = recv(&mut stream);
        send(&mut stream, &DataFrame::Proceed { id: header.id });
        read_body(&mut stream, header);
        send(
            &mut stream,
            &DataFrame::Failed {
                id: header.id,
                error: sent,
            },
        );
    });
    let outcome = call_async(
        client,
        target(),
        RequestKind::Query,
        RequestControl::default(),
    );
    match outcome.recv_timeout(WAIT).unwrap() {
        Err(ClientError::Domain(actual)) => {
            assert_eq!(actual.code, error.code);
            assert_eq!(actual.message, error.message);
            assert_eq!(actual.retryable, error.retryable);
            assert_eq!(actual.next_action, error.next_action);
        }
        other => panic!("expected Domain error, got {other:?}"),
    }
}

#[test]
fn completed_frame_preserves_nested_unicode_and_large_result() {
    let server = Server::new();
    let client = server.connect();
    let listener = server.data.try_clone().unwrap();
    let expected = json!({"outer": {"\u{d56d}\u{baa9}": ["\u{c548}\u{b155} \u{1f30f}", {"payload": "\u{ac00}\u{b098}\u{b2e4} abc".repeat(20_000)}]}, "ok": true});
    let sent = expected.clone();
    thread::spawn(move || {
        let mut stream = accept(&listener);
        hello(&mut stream);
        let header: CallHeader = recv(&mut stream);
        send(&mut stream, &DataFrame::Proceed { id: header.id });
        read_body(&mut stream, header);
        let frame = serde_json::to_vec(&DataFrame::Completed {
            id: header.id,
            result: sent,
        })
        .unwrap();
        write_frame(&mut stream, &frame, MAX_RESULT_FRAME_BYTES).unwrap();
    });
    let outcome = call_async(
        client,
        target(),
        RequestKind::Query,
        RequestControl::default(),
    );
    assert_eq!(outcome.recv_timeout(WAIT).unwrap().unwrap(), expected);
}

#[test]
fn progress_frames_reach_callback_in_order_before_terminal() {
    let server = Server::new();
    let client = server.connect();
    let listener = server.data.try_clone().unwrap();
    thread::spawn(move || {
        let mut stream = accept(&listener);
        hello(&mut stream);
        let header: CallHeader = recv(&mut stream);
        send(&mut stream, &DataFrame::Proceed { id: header.id });
        read_body(&mut stream, header);
        for phase in ["receiving", "\u{ac80}\u{c0c9}", "finishing"] {
            send(
                &mut stream,
                &DataFrame::Progress {
                    id: header.id,
                    phase: phase.into(),
                },
            );
        }
        send(
            &mut stream,
            &DataFrame::Completed {
                id: header.id,
                result: json!({"done":true}),
            },
        );
    });
    let phases = Arc::new(Mutex::new(Vec::new()));
    let observed = phases.clone();
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        observed.lock().unwrap().push(phase.to_owned())
    });
    let outcome = call_async(client, target(), RequestKind::Query, control);
    assert_eq!(
        outcome.recv_timeout(WAIT).unwrap().unwrap(),
        json!({"done":true})
    );
    assert_eq!(
        *phases.lock().unwrap(),
        ["receiving", "\u{ac80}\u{c0c9}", "finishing"]
    );
}

#[test]
fn before_body_failure_reconnects_once_and_succeeds() {
    let server = Server::new();
    let client = server.connect();
    let listener = server.data.try_clone().unwrap();
    let (seen_tx, seen_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut first = accept(&listener);
        hello(&mut first);
        let _: CallHeader = recv(&mut first);
        first.shutdown(Shutdown::Both).unwrap();
        let mut second = accept(&listener);
        hello(&mut second);
        let h: CallHeader = recv(&mut second);
        send(&mut second, &DataFrame::Proceed { id: h.id });
        read_body(&mut second, h);
        seen_tx.send(h.target).unwrap();
        send(
            &mut second,
            &DataFrame::Completed {
                id: h.id,
                result: json!({"ok":true}),
            },
        );
    });
    let outcome = call_async(
        client,
        target(),
        RequestKind::Query,
        RequestControl::default(),
    );
    assert_eq!(
        outcome.recv_timeout(WAIT).unwrap().unwrap(),
        json!({"ok":true})
    );
    assert_eq!(seen_rx.recv_timeout(WAIT).unwrap(), target());
}

#[test]
fn query_lost_after_body_is_requeried_once_with_same_binding() {
    let server = Server::new();
    let client = server.connect();
    let listener = server.data.try_clone().unwrap();
    let (seen_tx, seen_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut targets = Vec::new();
        for index in 0..2 {
            let mut stream = accept(&listener);
            hello(&mut stream);
            let h: CallHeader = recv(&mut stream);
            targets.push(h.target);
            send(&mut stream, &DataFrame::Proceed { id: h.id });
            read_body(&mut stream, h);
            if index == 0 {
                stream.shutdown(Shutdown::Both).unwrap();
            } else {
                send(
                    &mut stream,
                    &DataFrame::Completed {
                        id: h.id,
                        result: json!({"ok":true}),
                    },
                );
            }
        }
        seen_tx.send(targets).unwrap();
    });
    let outcome = call_async(
        client,
        target(),
        RequestKind::Query,
        RequestControl::default(),
    );
    assert_eq!(
        outcome.recv_timeout(WAIT).unwrap().unwrap(),
        json!({"ok":true})
    );
    assert_eq!(seen_rx.recv_timeout(WAIT).unwrap(), [target(), target()]);
}

#[test]
fn mutation_lost_after_body_reports_unknown_without_resend() {
    for kind in [RequestKind::Command, RequestKind::Setup] {
        let server = Server::new();
        let client = server.connect();
        let listener = server.data.try_clone().unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut stream = accept(&listener);
            hello(&mut stream);
            let h: CallHeader = recv(&mut stream);
            send(&mut stream, &DataFrame::Proceed { id: h.id });
            read_body(&mut stream, h);
            stream.shutdown(Shutdown::Both).unwrap();
            seen_tx.send(()).unwrap();
        });
        let target = if kind == RequestKind::Setup {
            CallTarget::Setup
        } else {
            target()
        };
        let outcome = call_async(client, target, kind, RequestControl::default());
        seen_rx.recv_timeout(WAIT).unwrap();
        assert!(matches!(
            outcome.recv_timeout(WAIT).unwrap(),
            Err(ClientError::MutationOutcomeUnknown)
        ));
        server.data.set_nonblocking(true).unwrap();
        assert!(
            matches!(server.data.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
}

#[test]
fn explicit_busy_and_stopping_retry_once_without_changing_target() {
    for reason in [RejectReason::Busy, RejectReason::Stopping] {
        let server = Server::new();
        let client = server.connect();
        let listener = server.data.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut targets = Vec::new();
            for attempt in 0..2 {
                let mut stream = accept(&listener);
                hello(&mut stream);
                let h: CallHeader = recv(&mut stream);
                targets.push(h.target);
                if attempt == 0 {
                    send(
                        &mut stream,
                        &DataFrame::Rejected {
                            id: h.id,
                            reason: reason.clone(),
                        },
                    );
                } else {
                    send(&mut stream, &DataFrame::Proceed { id: h.id });
                    read_body(&mut stream, h);
                    send(
                        &mut stream,
                        &DataFrame::Completed {
                            id: h.id,
                            result: json!({"ok":true}),
                        },
                    );
                }
            }
            tx.send(targets).unwrap();
        });
        let result = call_async(
            client,
            target(),
            RequestKind::Query,
            RequestControl::default(),
        );
        assert_eq!(
            result.recv_timeout(WAIT).unwrap().unwrap(),
            json!({"ok":true})
        );
        assert_eq!(rx.recv_timeout(WAIT).unwrap(), [target(), target()]);
    }
}

#[test]
fn second_lost_query_attempt_is_not_retried_again() {
    let server = Server::new();
    let client = server.connect();
    let listener = server.data.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept(&listener);
            hello(&mut stream);
            let h: CallHeader = recv(&mut stream);
            send(&mut stream, &DataFrame::Proceed { id: h.id });
            read_body(&mut stream, h);
            stream.shutdown(Shutdown::Both).unwrap();
        }
        tx.send(()).unwrap();
    });
    let result = call_async(
        client,
        target(),
        RequestKind::Query,
        RequestControl::default(),
    );
    rx.recv_timeout(WAIT).unwrap();
    assert!(result.recv_timeout(WAIT).unwrap().is_err());
    server.data.set_nonblocking(true).unwrap();
    assert!(matches!(server.data.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}

#[test]
fn permanent_rejection_returns_without_reroute() {
    let error = agentlaw_contracts::DomainError::new("invalid", "invalid request");
    for reason in [
        RejectReason::BindingChanged,
        RejectReason::InvalidBody(error),
        RejectReason::KindMismatch,
        RejectReason::BodyTooLarge,
    ] {
        let server = Server::new();
        let client = server.connect();
        let listener = server.data.try_clone().unwrap();
        let expected = reason.clone();
        thread::spawn(move || {
            let mut stream = accept(&listener);
            hello(&mut stream);
            let h: CallHeader = recv(&mut stream);
            send(&mut stream, &DataFrame::Rejected { id: h.id, reason });
        });
        let result = call_async(
            client,
            target(),
            RequestKind::Query,
            RequestControl::default(),
        );
        assert!(
            matches!(result.recv_timeout(WAIT).unwrap(), Err(ClientError::Rejected(actual)) if actual == expected)
        );
        server.data.set_nonblocking(true).unwrap();
        assert!(
            matches!(server.data.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
}

#[test]
fn cancellation_before_proceed_closes_connection_without_body_or_control_cancel() {
    let server = Server::new();
    let client = server.connect();
    let listener = server.data.try_clone().unwrap();
    let (header_tx, header_rx) = mpsc::channel();
    let (closed_tx, closed_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = accept(&listener);
        hello(&mut stream);
        let _: CallHeader = recv(&mut stream);
        header_tx.send(()).unwrap();
        let mut buf = [0];
        closed_tx.send(stream.read(&mut buf)).unwrap();
    });
    let control = RequestControl::default();
    let flag = control.cancel.clone();
    let result = call_async(client, target(), RequestKind::Query, control);
    header_rx.recv_timeout(WAIT).unwrap();
    flag.store(true, Ordering::Release);
    assert_eq!(closed_rx.recv_timeout(WAIT).unwrap().unwrap(), 0);
    assert!(result.recv_timeout(WAIT).unwrap().is_err());
    assert_eq!(server.cancellations.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_after_proceed_sends_the_active_request_id() {
    let server = Server::new();
    let client = server.connect();
    let listener = server.data.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stream = accept(&listener);
        hello(&mut stream);
        let h: CallHeader = recv(&mut stream);
        send(&mut stream, &DataFrame::Proceed { id: h.id });
        read_body(&mut stream, h);
        tx.send(h.id).unwrap();
        finish_rx.recv_timeout(WAIT).unwrap();
        send(
            &mut stream,
            &DataFrame::Completed {
                id: h.id,
                result: json!({}),
            },
        );
    });
    let control = RequestControl::default();
    let flag = control.cancel.clone();
    let result = call_async(client, target(), RequestKind::Query, control);
    let id = rx.recv_timeout(WAIT).unwrap();
    flag.store(true, Ordering::Release);
    let (lock, changed) = &*server.cancel_ids;
    let (ids, _) = changed
        .wait_timeout_while(lock.lock().unwrap(), WAIT, |ids| ids.is_empty())
        .unwrap();
    assert_eq!(&*ids, &[id]);
    drop(ids);
    finish_tx.send(()).unwrap();
    let _ = result.recv_timeout(WAIT).unwrap();
}

#[test]
fn authentication_build_and_protocol_failures_do_not_retry() {
    for reply in [
        HelloReply::NotAuthorized,
        HelloReply::BuildMismatch,
        HelloReply::ProtocolMismatch,
    ] {
        let server = Server::new_with_reply(reply);
        let state = server.dir.path().to_owned();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(Client::connect(&state, "test-build").is_err());
        });
        assert!(rx.recv_timeout(WAIT).unwrap());
        assert_eq!(server.hellos.load(Ordering::SeqCst), 1);
        server.data.set_nonblocking(true).unwrap();
        assert!(
            matches!(server.data.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
}

struct AttachClock {
    origin: Instant,
    current: Mutex<Instant>,
    during_wait: Mutex<Option<Box<dyn Fn(Instant, Instant) + Send>>>,
}
impl AttachClock {
    fn new() -> Arc<Self> {
        let origin = Instant::now();
        Arc::new(Self {
            origin,
            current: Mutex::new(origin),
            during_wait: Mutex::new(None),
        })
    }
    fn now(&self) -> Instant {
        *self.current.lock().unwrap()
    }
    fn wait(&self, duration: Duration) {
        let before = self.now();
        let after = before + duration;
        if let Some(hook) = self.during_wait.lock().unwrap().as_ref() {
            hook(before, after);
        }
        *self.current.lock().unwrap() = after;
    }
}
#[derive(Default)]
struct AttachLaunchLog {
    launched: Mutex<Vec<Instant>>,
    already_owned: Mutex<Vec<Instant>>,
}
struct RecordingAttachLauncher {
    clock: Arc<AttachClock>,
    log: Arc<AttachLaunchLog>,
    stop_on_launch: Option<usize>,
    publish_on_launch: Option<(usize, PathBuf, Endpoint)>,
}
#[derive(Clone, Copy)]
enum RecordedExit {
    AlreadyOwned,
    Blocked,
    Running,
}
struct RecordedAttachCandidate {
    clock: Arc<AttachClock>,
    log: Arc<AttachLaunchLog>,
    exit_at: Instant,
    exit: RecordedExit,
}
impl RuntimeLauncher for RecordingAttachLauncher {
    fn launch(&self) -> io::Result<Box<dyn RuntimeCandidate>> {
        let now = self.clock.now();
        let number = {
            let mut launches = self.log.launched.lock().unwrap();
            launches.push(now);
            launches.len()
        };
        let exit = if let Some((expected, state, endpoint)) = &self.publish_on_launch {
            if number == *expected {
                transport::write_endpoint(state, endpoint)?;
                RecordedExit::Running
            } else {
                RecordedExit::AlreadyOwned
            }
        } else if self.stop_on_launch == Some(number) {
            RecordedExit::Blocked
        } else {
            RecordedExit::AlreadyOwned
        };
        Ok(Box::new(RecordedAttachCandidate {
            clock: self.clock.clone(),
            log: self.log.clone(),
            // A candidate remains alive before its exit is observed, so launch time
            // cannot accidentally serve as the backoff origin.
            exit_at: now + Duration::from_millis(35),
            exit,
        }))
    }
}
impl RuntimeCandidate for RecordedAttachCandidate {
    fn try_exit(&mut self) -> io::Result<Option<CandidateExit>> {
        let now = self.clock.now();
        if now < self.exit_at {
            return Ok(None);
        }
        match self.exit {
            RecordedExit::AlreadyOwned => {
                self.log.already_owned.lock().unwrap().push(now);
                Ok(Some(CandidateExit::AlreadyOwned))
            }
            RecordedExit::Blocked => Ok(Some(CandidateExit::StartBlocked(DomainError::new(
                "fixture_observation_complete",
                "end of recorded launches",
            )))),
            RecordedExit::Running => Ok(None),
        }
    }
}
fn virtual_attach(state: &Path, launcher: &RecordingAttachLauncher) -> Result<Client, ClientError> {
    attach_with_clock(
        state,
        "test-build",
        launcher,
        AttachLimits::default(),
        &|| launcher.clock.now(),
        &|duration| launcher.clock.wait(duration),
    )
}

#[test]
fn already_owned_relaunch_uses_observed_exit_times_and_capped_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let clock = AttachClock::new();
    let log = Arc::new(AttachLaunchLog::default());
    let launcher = RecordingAttachLauncher {
        clock,
        log: log.clone(),
        stop_on_launch: Some(7),
        publish_on_launch: None,
    };
    let result = virtual_attach(dir.path(), &launcher);
    assert!(
        matches!(result, Err(ClientError::Domain(ref e)) if e.code == "fixture_observation_complete")
    );
    let launches = log.launched.lock().unwrap();
    let exits = log.already_owned.lock().unwrap();
    assert_eq!(
        launches.len(),
        7,
        "there must be no extra candidate between retry deadlines"
    );
    assert_eq!(exits.len(), 6);
    let intervals: Vec<_> = launches[1..]
        .iter()
        .zip(exits.iter())
        .map(|(launch, observed)| launch.duration_since(*observed))
        .collect();
    assert_eq!(
        intervals,
        [100, 200, 400, 800, 1000, 1000]
            .map(Duration::from_millis)
            .to_vec()
    );
}

#[cfg(windows)]
#[test]
fn changed_endpoint_is_authenticated_during_the_already_owned_retry_delay() {
    let server = Server::new();
    let endpoint = transport::read_endpoint(server.dir.path()).unwrap();
    let expected_address = endpoint.control;
    let dir = tempfile::tempdir().unwrap();
    let stale = BoundOnlyPort::bind(0);
    assert_ne!(stale.address, expected_address);
    transport::write_endpoint(
        dir.path(),
        &Endpoint {
            control: stale.address,
            data: stale.address,
            ..endpoint.clone()
        },
    )
    .unwrap();
    let clock = AttachClock::new();
    let log = Arc::new(AttachLaunchLog::default());
    let published = Arc::new(Mutex::new(None));
    let publisher_log = log.clone();
    let publication = published.clone();
    let state = dir.path().to_path_buf();
    *clock.during_wait.lock().unwrap() = Some(Box::new(move |_, after| {
        let mut published = publication.lock().unwrap();
        if published.is_some() {
            return;
        }
        if let Some(observed) = publisher_log.already_owned.lock().unwrap().first().copied() {
            let at = observed + Duration::from_millis(50);
            if after >= at {
                transport::write_endpoint(&state, &endpoint).unwrap();
                *published = Some(at);
            }
        }
    }));
    let launcher = RecordingAttachLauncher {
        clock: clock.clone(),
        log: log.clone(),
        stop_on_launch: None,
        publish_on_launch: None,
    };
    let client = virtual_attach(dir.path(), &launcher)
        .unwrap_or_else(|e| panic!("changed endpoint was not authenticated: {e:?}"));
    let published_at = published
        .lock()
        .unwrap()
        .expect("new owner did not replace the failed address");
    let observed = log.already_owned.lock().unwrap()[0];
    let first_failed_at = log.launched.lock().unwrap()[0];
    assert_eq!(published_at, observed + Duration::from_millis(50));
    assert!(
        clock.now() < first_failed_at + Duration::from_secs(1),
        "new address authentication waited for the unchanged failed-address recheck deadline"
    );
    assert!(
        clock.now() < observed + Duration::from_millis(100),
        "endpoint authentication waited for the candidate retry deadline"
    );
    assert_eq!(log.launched.lock().unwrap().len(), 1);
    assert_eq!(client.inner.endpoint.control, expected_address);
}

#[cfg(windows)]
#[test]
fn unreachable_endpoint_is_replaced_by_the_next_candidate_after_the_delay() {
    let server = Server::new();
    let endpoint = transport::read_endpoint(server.dir.path()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let stale = BoundOnlyPort::bind(0);
    transport::write_endpoint(
        dir.path(),
        &Endpoint {
            control: stale.address,
            data: stale.address,
            ..endpoint.clone()
        },
    )
    .unwrap();
    let clock = AttachClock::new();
    let log = Arc::new(AttachLaunchLog::default());
    let launcher = RecordingAttachLauncher {
        clock: clock.clone(),
        log: log.clone(),
        stop_on_launch: None,
        publish_on_launch: Some((2, dir.path().to_path_buf(), endpoint.clone())),
    };
    let client = virtual_attach(dir.path(), &launcher)
        .unwrap_or_else(|e| panic!("new candidate did not recover stale endpoint: {e:?}"));
    assert_eq!(client.inner.endpoint.control, endpoint.control);
    let launches = log.launched.lock().unwrap();
    let observed = log.already_owned.lock().unwrap();
    assert_eq!(launches.len(), 2);
    assert_eq!(observed.len(), 1);
    assert_eq!(
        launches[1].duration_since(observed[0]),
        Duration::from_millis(100)
    );
}

struct SilentAttachServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    attempts: Arc<Mutex<Vec<Instant>>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl SilentAttachServer {
    fn new(clock: Arc<AttachClock>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let ended = stop.clone();
        let observed = attempts.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            let mut held = Vec::new();
            ready_tx.send(()).unwrap();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if ended.load(Ordering::Acquire) {
                    break;
                }
                stream.set_read_timeout(Some(WAIT)).unwrap();
                let _: Hello = recv(&mut stream);
                observed.lock().unwrap().push(clock.now());
                held.push(stream); // No Hello response: the caller reaches its response deadline.
            }
        });
        ready_rx.recv_timeout(WAIT).unwrap();
        Self {
            address,
            stop,
            attempts,
            thread: Some(thread),
        }
    }
}
impl Drop for SilentAttachServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

#[test]
fn unchanged_failed_endpoint_is_rechecked_once_per_second_without_extending_start_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let clock = AttachClock::new();
    let server = SilentAttachServer::new(clock.clone());
    transport::write_endpoint(
        dir.path(),
        &Endpoint {
            protocol: PROTOCOL_VERSION,
            installation_build: "test-build".into(),
            control: server.address,
            data: server.address,
            secret: "a".repeat(64),
            pid: std::process::id(),
        },
    )
    .unwrap();
    let log = Arc::new(AttachLaunchLog::default());
    let launcher = RecordingAttachLauncher {
        clock: clock.clone(),
        log: log.clone(),
        stop_on_launch: None,
        publish_on_launch: None,
    };
    let result = attach_with_clock(
        dir.path(),
        "test-build",
        &launcher,
        AttachLimits {
            start_wait: Duration::from_secs(30),
            hello_timeout: Duration::from_millis(100),
        },
        &|| clock.now(),
        &|duration| clock.wait(duration),
    );
    assert!(matches!(
        result,
        Err(ClientError::Runtime(RuntimeError::Unresponsive))
    ));
    assert_eq!(
        clock.now().duration_since(clock.origin),
        Duration::from_secs(30),
        "candidate retries must not extend the original startup deadline"
    );
    let attempts = server.attempts.lock().unwrap();
    let expected: Vec<_> = (0..30)
        .map(|second| clock.origin + Duration::from_secs(second))
        .collect();
    assert_eq!(
        *attempts, expected,
        "unchanged failed address must be rechecked at one-second intervals"
    );
    assert!(log
        .launched
        .lock()
        .unwrap()
        .iter()
        .all(|at| *at < clock.origin + Duration::from_secs(30)));
}

#[cfg(windows)]
#[link(name = "ws2_32")]
unsafe extern "system" {
    #[link_name = "socket"]
    fn bound_fixture_socket(family: i32, kind: i32, protocol: i32) -> usize;
    #[link_name = "setsockopt"]
    fn bound_fixture_setsockopt(
        socket: usize,
        level: i32,
        option: i32,
        value: *const u8,
        length: i32,
    ) -> i32;
    #[link_name = "bind"]
    fn bound_fixture_bind(socket: usize, address: *const u8, length: i32) -> i32;
    #[link_name = "getsockname"]
    fn bound_fixture_getsockname(socket: usize, address: *mut u8, length: *mut i32) -> i32;
    #[link_name = "closesocket"]
    fn bound_fixture_close(socket: usize) -> i32;
}
#[cfg(windows)]
struct BoundOnlyPort {
    socket: usize,
    address: std::net::SocketAddr,
}
#[cfg(windows)]
impl BoundOnlyPort {
    fn bind(port: u16) -> Self {
        #[repr(C)]
        struct Address {
            family: u16,
            port: u16,
            address: u32,
            padding: [u8; 8],
        }
        // Initialize the standard library's process-wide Winsock lifetime.
        drop(TcpListener::bind("127.0.0.1:0").unwrap());
        let socket = unsafe { bound_fixture_socket(2, 1, 6) };
        assert_ne!(socket, usize::MAX, "creating the owned TCP socket failed");
        let mut owned = Self {
            socket,
            address: ([127, 0, 0, 1], port).into(),
        };
        let exclusive: i32 = 1;
        assert_eq!(
            unsafe {
                bound_fixture_setsockopt(socket, 0xffff, -5, (&exclusive as *const i32).cast(), 4)
            },
            0,
            "exclusive port ownership failed: {}",
            io::Error::last_os_error()
        );
        let mut address = Address {
            family: 2,
            port: port.to_be(),
            address: u32::from_ne_bytes([127, 0, 0, 1]),
            padding: [0; 8],
        };
        let mut length = std::mem::size_of::<Address>() as i32;
        assert_eq!(
            unsafe { bound_fixture_bind(socket, (&address as *const Address).cast(), length) },
            0,
            "binding the owned non-listening port failed: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            unsafe {
                bound_fixture_getsockname(
                    socket,
                    (&mut address as *mut Address).cast(),
                    &mut length,
                )
            },
            0,
            "reading the owned port failed"
        );
        owned.address = ([127, 0, 0, 1], u16::from_be(address.port)).into();
        owned
    }
}
#[cfg(windows)]
impl Drop for BoundOnlyPort {
    fn drop(&mut self) {
        unsafe {
            bound_fixture_close(self.socket);
        }
    }
}
