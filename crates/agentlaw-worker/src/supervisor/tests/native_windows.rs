use super::*;

#[test]
fn killed_parent_releases_model_child_before_replacement() {
    use windows_sys::Win32::Foundation::CloseHandle as CloseNativeHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "supervisor::native_tests::job_lifetime_helper",
        ])
        .env("AGENTLAW_JOB_ROLE", "parent")
        .env("AGENTLAW_JOB_DIR", dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    until(|| dir.path().join("parent-ready").exists());
    let pid: u32 = std::fs::read_to_string(dir.path().join("child-ready"))
        .unwrap()
        .parse()
        .unwrap();
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!handle.is_null());
    parent.kill().unwrap();
    parent.wait().unwrap();
    assert_eq!(unsafe { WaitForSingleObject(handle, 5000) }, 0);
    unsafe {
        CloseNativeHandle(handle);
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join("model.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    FileExt::unlock(&lock).unwrap();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: "unused".into(),
        model: None,
    };
    let (events, _) = mpsc::channel();
    let signal = Arc::new(RamSignal {
        events: events.clone(),
    });
    let mut replacement = start_ram_child(
        &LifetimeProcess(dir.path().into()),
        &config,
        &events,
        &signal,
        Instant::now(),
        None,
    )
    .unwrap_or_else(|e| panic!("{}", e.cause));
    NativeProcess.stop(&mut replacement.child).unwrap();
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
        &Arc::new(RamSignal {
            events: events.clone(),
        }),
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
        signal: Arc::new(RamSignal {
            events: events.clone(),
        }),
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
        cancelled_waiter_supervisor.embed_piece("shared", &cancel_waiter)
    });
    let first_waiter = std::thread::spawn(move || {
        first_waiter_supervisor.embed_piece("shared", &AtomicBool::new(false))
    });
    let second_waiter = std::thread::spawn(move || {
        second_waiter_supervisor.embed_piece("shared", &AtomicBool::new(false))
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
        std::thread::spawn(move || query_supervisor.embed_piece("hello", &AtomicBool::new(false)));
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
            truncated: false,
            id: 2,
            attempt: 1,
            result: Ok(old_vector),
        },
    ))
    .unwrap();
    tx.send(Event::Output(
        new,
        Output::Result {
            truncated: false,
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
        query_supervisor.embed_piece("bad input", &AtomicBool::new(false))
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
            truncated: false,
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

#[test]
fn missing_model_diagnostic_survives_stdout_before_exit() {
    let mut fixture = Recheck::new(true, false, false);
    let incarnation = fixture
        .observed
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let cause = missing_model_cause(fixture._dir.path());
    fixture
        .events
        .send(Event::Output(
            incarnation,
            Output::Unavailable {
                cause: cause.clone(),
            },
        ))
        .unwrap();
    until(|| fixture.owner().snapshot().cause.as_deref() == Some(cause.as_str()));
    fixture.shutdown().unwrap();
}

#[test]
fn missing_model_diagnostic_survives_exit_before_stdout() {
    let mut fixture = Recheck::new(true, false, false);
    let incarnation = fixture
        .observed
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let cause = missing_model_cause(fixture._dir.path());
    fixture
        .events
        .send(Event::Exit(incarnation.clone()))
        .unwrap();
    {
        let handles = fixture.handles.lock().unwrap();
        let handle = handles[0].as_raw_handle();
        unsafe {
            assert_ne!(TerminateProcess(handle, 71), 0);
        }
    }
    until(|| fixture.owner().snapshot().cause.as_deref() == Some("model child exited"));
    fixture
        .events
        .send(Event::Output(
            incarnation,
            Output::Unavailable {
                cause: cause.clone(),
            },
        ))
        .unwrap();
    until(|| fixture.owner().snapshot().cause.as_deref() == Some(cause.as_str()));
    fixture.shutdown().unwrap();
}
