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
        code: "store_busy".into(),
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
