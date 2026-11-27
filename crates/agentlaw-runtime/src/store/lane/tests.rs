use super::*;

#[test]
fn bounded_queue_processes_one_stores_commands_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let mut writer = ControlWriter::open(&local).unwrap();
    let identity = StoreIdentity {
        root,
        local: local.clone(),
        user_id: uuid::Uuid::new_v4().to_string(),
        machine_id: uuid::Uuid::new_v4().to_string(),
        binding_id: uuid::Uuid::new_v4().to_string(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    writer.bind_identity(&identity).unwrap();
    let lane = MutationLane::start(identity, store, writer).unwrap();
    let lock = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let sender = lane.sender();
    let started = sender.clone();
    let first = thread::spawn(move || {
        started.call(
            LaneCommand::SyncPersist(SyncOperationRow {
                id: "item".into(),
                source: "source".into(),
                state_json: "0".into(),
                active: false,
            }),
            &RequestControl::default(),
        )
    });
    thread::sleep(Duration::from_millis(100));
    let mut receivers = Vec::new();
    for n in 1..=8 {
        let (reply, receiver) = mpsc::channel();
        let work = Work {
            command: LaneCommand::SyncPersist(SyncOperationRow {
                id: "item".into(),
                source: "source".into(),
                state_json: n.to_string(),
                active: false,
            }),
            control: RequestControl::default(),
            limits: None,
            state: Arc::new(Mutex::new(WorkState::Queued)),
            reply,
        };
        assert!(sender.shared.sender.try_send(Message::Work(work)).is_ok());
        receivers.push(receiver);
    }
    let result = sender.call(
        LaneCommand::SyncReserve {
            id: "overflow".into(),
            operation: "op".into(),
            digest: "digest".into(),
        },
        &RequestControl::default(),
    );
    let error = result.err().expect("CT-04e: queue must reject overflow");
    assert_eq!(error.code, "capacity", "FR-06.4 CT-04e: {error:?}");
    assert!(!error.retryable);
    let action = error.next_action.expect("CT-04e: queue recovery guidance");
    let action = action.to_lowercase();
    assert!(
        action.contains("queue") && (action.contains("drain") || action.contains("empty")),
        "{action}"
    );
    assert!(
        action.contains("stor") || action.contains("writ") || action.contains("remember"),
        "{action}"
    );
    lock.execute_batch("COMMIT").unwrap();
    assert!(matches!(first.join().unwrap(), Ok(LaneReply::Done)));
    for receiver in receivers {
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            Ok(LaneReply::Done)
        ));
    }
    let value: String = lock
        .query_row(
            "SELECT state FROM sync_operations WHERE operation_id='item'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, "8");
    lane.close();
}

#[test]
fn cancellation_at_dequeue_never_runs_the_waiting_command() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let mut writer = ControlWriter::open(&local).unwrap();
    let identity = StoreIdentity {
        root,
        local: local.clone(),
        user_id: uuid::Uuid::new_v4().to_string(),
        machine_id: uuid::Uuid::new_v4().to_string(),
        binding_id: uuid::Uuid::new_v4().to_string(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    writer.bind_identity(&identity).unwrap();
    let lane = MutationLane::start(identity, store, writer).unwrap();
    let lock = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let sender = lane.sender();
    let (first_reply, first_receiver) = mpsc::channel();
    let first_state = Arc::new(Mutex::new(WorkState::Queued));
    sender
        .shared
        .sender
        .try_send(Message::Work(Work {
            command: LaneCommand::SyncPersist(SyncOperationRow {
                id: "blocking".into(),
                source: "source".into(),
                state_json: "{}".into(),
                active: false,
            }),
            control: RequestControl::default(),
            limits: None,
            state: first_state.clone(),
            reply: first_reply,
        }))
        .ok()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while *first_state.lock().unwrap() != WorkState::Started {
        assert!(std::time::Instant::now() < deadline);
        thread::yield_now();
    }
    let control = RequestControl::default();
    let (reply, receiver) = mpsc::channel();
    let state = Arc::new(Mutex::new(WorkState::Queued));
    sender
        .shared
        .sender
        .try_send(Message::Work(Work {
            command: LaneCommand::SyncPersist(SyncOperationRow {
                id: "waiting".into(),
                source: "source".into(),
                state_json: "{}".into(),
                active: false,
            }),
            control: control.clone(),
            limits: None,
            state,
            reply,
        }))
        .ok()
        .unwrap();
    // Cancellation is set immediately before the lock releases the previous command;
    // no caller-side polling changes the queued state for this work item.
    control.cancel.store(true, Ordering::Release);
    lock.execute_batch("COMMIT").unwrap();
    assert!(matches!(
        first_receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
        Ok(LaneReply::Done)
    ));
    assert!(
        matches!(receiver.recv_timeout(Duration::from_secs(3)).unwrap(),Err(error) if error.code=="cancelled")
    );
    let found: bool = lock
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_operations WHERE operation_id='waiting')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!found);
}

#[test]
fn started_recovery_finishes_result_record_after_caller_cancels() {
    use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
    use serde_json::json;
    let uid = || uuid::Uuid::new_v4().to_string();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let mut writer = ControlWriter::open(&local).unwrap();
    let identity = StoreIdentity {
        root,
        local: local.clone(),
        user_id: uid(),
        machine_id: uid(),
        binding_id: uid(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    writer.bind_identity(&identity).unwrap();
    let mutation = Mutation {
        unit: CurrentUnit {
            entity_id: uid(),
            entity_type: "learned_procedure".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":uid(),"applicability":{"scope":"user"},"origin":{"machine_id":identity.machine_id},"recorded_at_ms":0,"evidence_memory_ids":[]}),
                    body: "procedure".into(),
                }],
            },
        },
        expected_versions: vec![],
        evidence: "test".into(),
    };
    let generation = store.generation().unwrap();
    let token = uid();
    let operation = uid();
    let db = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    db.execute(
        "INSERT INTO authoring(token,binding,basis,used) VALUES(?1,?2,'basis',1)",
        rusqlite::params![token, identity.binding_id],
    )
    .unwrap();
    db.execute("INSERT INTO authoring_executions(token,binding,operation,generation,mutation,submission,status) VALUES(?1,?2,?3,?4,?5,'{}','pending')",rusqlite::params![token,identity.binding_id,operation,generation.to_string(),serde_json::to_string(&mutation).unwrap()]).unwrap();
    let lane = MutationLane::start(identity, store, writer).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let progress_cancel = cancelled.clone();
    let (published_tx, published_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let control = RequestControl::new(cancelled, move |phase| {
        if phase == "published" {
            progress_cancel.store(true, Ordering::Release);
            published_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }
    });
    let caller_control = control.clone();
    let sender = lane.sender();
    let (done_tx, done_rx) = mpsc::channel();
    let caller = thread::spawn(move || {
        done_tx
            .send(sender.call(LaneCommand::Recover, &caller_control))
            .ok();
    });
    published_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_millis(150)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release_tx.send(()).unwrap();
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Ok(LaneReply::Done)
    ));
    caller.join().unwrap();
    assert!(control.cancel.load(Ordering::Acquire));
    let (status, result): (String, Option<String>) = db
        .query_row(
            "SELECT status,result FROM authoring_executions WHERE token=?1",
            [token],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "published");
    assert!(result.is_some());
}

#[test]
fn stale_import_preserves_sync_inputs_changed_code() {
    use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
    use serde_json::json;
    let uid = || uuid::Uuid::new_v4().to_string();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let staged_root = dir.path().join("staged");
    let staged_local = dir.path().join("staged-local");
    let staged =
        Store::open_with_coordination(&staged_root, &staged_local, store.coordination_root())
            .unwrap();
    let staged_generation = staged.generation().unwrap();
    drop(staged);
    let mutation = Mutation {
        unit: CurrentUnit {
            entity_id: uid(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":uid(),"applicability":{"scope":"user"},"origin":{"machine_id":uid()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: "changed".into(),
                }],
            },
        },
        expected_versions: vec![],
        evidence: "test".into(),
    };
    store.publish(&uid(), vec![mutation]).unwrap();
    let mut writer = ControlWriter::open(&local).unwrap();
    let identity = StoreIdentity {
        root,
        local,
        user_id: uid(),
        machine_id: uid(),
        binding_id: uid(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    writer.bind_identity(&identity).unwrap();
    let lane = MutationLane::start(identity, store, writer).unwrap();
    let result = lane.sender().call(
        LaneCommand::PublishImport(ImportPublishInput {
            operation_id: uid(),
            staged_root,
            staged_local,
            expected_generation: 0,
            staged_generation,
        }),
        &RequestControl::default(),
    );
    assert!(matches!(result,Err(error) if error.code=="sync_inputs_changed"));
}
