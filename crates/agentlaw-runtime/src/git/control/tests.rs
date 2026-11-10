use crate::store::{
    lane::{LaneCommand, SyncOperationRow},
    service::StoreService,
};
use crate::test_support::store_service::{deps, fixture};
use agentlaw_flows::RequestControl;
use std::{
    sync::{atomic::Ordering, mpsc},
    thread,
    time::Duration,
};
#[test]
fn git_sync_control_round_trips_rows_and_receipts() {
    let (dir, identity) = fixture();
    let source = identity.root.clone();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let handle = service.git_handle();
    let sync = handle.sync_control();
    let control = RequestControl::default();
    assert!(!sync.any_active().unwrap());
    assert_eq!(sync.active_for(&source).unwrap(), None);
    sync.persist(
        SyncOperationRow {
            id: "op".into(),
            source: source.to_string_lossy().into_owned(),
            state_json: "{\"phase\":\"captured\"}".into(),
            active: true,
        },
        &control,
    )
    .unwrap();
    assert!(sync.any_active().unwrap());
    assert_eq!(sync.active_for(&source).unwrap().as_deref(), Some("op"));
    assert_eq!(
        sync.operation("op").unwrap().as_deref(),
        Some("{\"phase\":\"captured\"}")
    );
    assert!(sync.reserve("request", "op", "digest", &control).unwrap());
    assert!(!sync.reserve("request", "op", "digest", &control).unwrap());
    sync.complete("request", "{\"status\":\"done\"}".into(), &control)
        .unwrap();
    assert_eq!(
        sync.request("request").unwrap(),
        Some(super::SyncReceipt {
            operation_id: "op".into(),
            digest: "digest".into(),
            response: Some("{\"status\":\"done\"}".into())
        })
    );
}

#[test]
fn git_sync_control_queued_cancel_does_not_persist() {
    let (dir, identity) = fixture();
    let local = identity.local.clone();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let lock = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let sender = service.lane_sender();
    let (tx, rx) = mpsc::channel();
    let first = thread::spawn(move || {
        tx.send(sender.call(
            LaneCommand::SyncPersist(SyncOperationRow {
                id: "block".into(),
                source: "block".into(),
                state_json: "{}".into(),
                active: false,
            }),
            &RequestControl::default(),
        ))
        .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    let handle = service.git_handle();
    let cancel = RequestControl::default();
    let queued_control = cancel.clone();
    let (tx, rx) = mpsc::channel();
    let queued = thread::spawn(move || {
        tx.send(handle.sync_control().persist(
            SyncOperationRow {
                id: "cancelled-handle".into(),
                source: "other".into(),
                state_json: "{}".into(),
                active: false,
            },
            &queued_control,
        ))
        .ok();
    });
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    cancel.cancel.store(true, Ordering::Release);
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Err(e) if e.code == "cancelled")
    );
    lock.execute_batch("COMMIT").unwrap();
    assert!(first.join().is_ok());
    queued.join().unwrap();
    assert_eq!(
        service
            .git_handle()
            .sync_control()
            .operation("cancelled-handle")
            .unwrap(),
        None
    );
}

#[test]
fn sync_control_reads_do_not_wait_for_a_held_lane() {
    let (dir, identity) = fixture();
    let local = identity.local.clone();
    let source = identity.root.clone();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let sender = service.lane_sender();
    sender
        .call(
            LaneCommand::SyncPersist(SyncOperationRow {
                id: "read-op".into(),
                source: source.to_string_lossy().into_owned(),
                state_json: "{\"phase\":\"incoming\"}".into(),
                active: true,
            }),
            &RequestControl::default(),
        )
        .unwrap();
    sender
        .call(
            LaneCommand::SyncReserve {
                id: "read-request".into(),
                operation: "read-op".into(),
                digest: "digest".into(),
            },
            &RequestControl::default(),
        )
        .unwrap();
    let lock = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (block_tx, block_rx) = mpsc::channel();
    let blocker = thread::spawn(move || {
        block_tx
            .send(sender.call(
                LaneCommand::SyncPersist(SyncOperationRow {
                    id: "block".into(),
                    source: "block".into(),
                    state_json: "{}".into(),
                    active: false,
                }),
                &RequestControl::default(),
            ))
            .ok();
    });
    assert!(matches!(
        block_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let handle = service.git_handle();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let control = handle.sync_control();
        let result = (
            control.any_active(),
            control.active_for(&source),
            control.request("read-request"),
            control.operation("read-op"),
        );
        tx.send(result).ok();
    });
    let read = rx.recv_timeout(Duration::from_secs(3));
    lock.execute_batch("COMMIT").unwrap();
    let _ = block_rx.recv_timeout(Duration::from_secs(5));
    blocker.join().unwrap();
    let joined = reader.join();
    assert!(joined.is_ok(), "reader panicked");
    let (active, source_active, receipt, operation) = read.expect("reads waited for the held lane");
    assert!(active.unwrap());
    assert_eq!(source_active.unwrap().as_deref(), Some("read-op"));
    assert_eq!(receipt.unwrap().unwrap().digest, "digest");
    assert_eq!(
        operation.unwrap().as_deref(),
        Some("{\"phase\":\"incoming\"}")
    );
}
