use agentlaw_storage::{CurrentUnit, Error, FaultPoint, Head, Mutation, Store, UnitState};
use serde_json::json;
use std::{sync::mpsc, time::Duration};

const ID: &str = "11111111-1111-4111-8111-111111111111";

fn mutation(body: &str, versions: Vec<String>) -> Mutation {
    Mutation {
        unit: CurrentUnit {
            entity_id: ID.into(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: body.into(),
                }],
            },
        },
        expected_versions: versions,
        evidence: "test".into(),
    }
}

#[test]
fn snapshot_blocks_publish_and_reads_while_writer_waits() {
    let (test_tx, test_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
        let receipt = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![mutation("before", vec![])],
            )
            .unwrap();
        let reader = store.owned_published_reader();
        let guard = reader.read_snapshot().unwrap();
        let position = guard.position().clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let version = receipt.references[0].observed_version.clone();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(store.publish(
                    &uuid::Uuid::new_v4().to_string(),
                    vec![mutation("after", vec![version])],
                ))
                .unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(guard.read_current(ID).unwrap().heads()[0].body, "before");
        assert_eq!(guard.acquire_current(ID).unwrap().source_position, position);
        assert_eq!(guard.read_closure(&[ID.into()]).unwrap().0, position);
        assert_eq!(guard.acquire_closure(&[ID.into()]).unwrap().0, position);
        assert_eq!(
            guard
                .read_published_paths(&position, 1)
                .unwrap()
                .source_position,
            position
        );
        assert!(guard.list_catalog().unwrap().is_empty());
        assert!(matches!(guard.read_procedure(ID), Err(Error::NotFound(_))));
        assert!(matches!(
            guard.acquire_procedure(ID),
            Err(Error::NotFound(_))
        ));
        drop(guard);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        assert_eq!(reader.read_current(ID).unwrap().heads()[0].body, "after");
        test_tx.send(()).unwrap();
    });
    test_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("snapshot read or publish did not complete");
}

#[test]
fn dirty_snapshot_releases_gate_for_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let reader = store.owned_published_reader();
    let position = reader.read_snapshot().unwrap().position().clone();
    assert!(matches!(
        store.publish_with_fault(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("body", vec![])],
            Some(FaultPoint::Decision)
        ),
        Err(Error::Interrupted(FaultPoint::Decision))
    ));
    assert!(matches!(
        reader.read_snapshot(),
        Err(Error::RecoveryRequired(_))
    ));
    for limit in [0, 1025] {
        assert!(matches!(
            reader.read_published_paths(&position, limit),
            Err(Error::Capacity)
        ));
    }
    assert!(matches!(
        reader.read_published_paths(&position, 1),
        Err(Error::RecoveryRequired(_))
    ));
    assert!(matches!(
        reader.read_catalog("invalid"),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        reader.read_current("invalid"),
        Err(Error::RecoveryRequired(_))
    ));
    assert!(matches!(
        reader.read_procedure("invalid"),
        Err(Error::RecoveryRequired(_))
    ));
    assert!(matches!(
        reader.acquire_current("invalid"),
        Err(Error::RecoveryRequired(_))
    ));
    assert!(matches!(
        reader.acquire_procedure("invalid"),
        Err(Error::RecoveryRequired(_))
    ));
    assert!(matches!(
        reader.read_closure(&["invalid".into()]),
        Err(Error::RecoveryRequired(_))
    ));
    assert!(matches!(
        reader.acquire_closure(&["invalid".into()]),
        Err(Error::RecoveryRequired(_))
    ));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(store.recover()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert_eq!(reader.read_current(ID).unwrap().heads()[0].body, "body");
}

#[test]
fn two_readers_hold_snapshots_concurrently() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let reader = store.owned_published_reader();
    let other = reader.clone();
    let guard = reader.read_snapshot().unwrap();
    let expected = guard.position().clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let second = other.read_snapshot().unwrap();
        tx.send(second.position().clone()).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(3));
    drop(guard);
    worker.join().unwrap();
    assert_eq!(result.expect("second shared guard blocked"), expected);
}

#[test]
fn legacy_acquisition_checks_epoch_before_dirty_fence() {
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("local");
    let store = Store::open(temp.path().join("source"), &local).unwrap();
    let reader = store.owned_published_reader();
    assert!(matches!(
        store.publish_with_fault(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("body", vec![])],
            Some(FaultPoint::Decision)
        ),
        Err(Error::Interrupted(FaultPoint::Decision))
    ));
    std::fs::write(local.join("source-epoch"), b"{}").unwrap();
    assert!(matches!(
        reader.read_snapshot(),
        Err(Error::RecoveryRequired(_))
    ));
    assert!(matches!(reader.acquire_current(ID), Err(Error::Corrupt(_))));
    assert!(matches!(
        reader.acquire_procedure(ID),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        reader.acquire_closure(&[ID.into()]),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        reader.read_closure(&[ID.into()]),
        Err(Error::Corrupt(_))
    ));
}

#[test]
fn wrappers_match_snapshot_reads() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let reader = store.owned_published_reader();
    let start = reader.read_snapshot().unwrap().position().clone();
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("body", vec![])],
        )
        .unwrap();
    store
        .publish_catalog(
            &uuid::Uuid::new_v4().to_string(),
            ID,
            json!({"project_id": ID, "name": "project"}),
        )
        .unwrap();
    let current = reader.read_current(ID).unwrap();
    let acquired = reader.acquire_current(ID).unwrap();
    let closure = reader.read_closure(&[ID.into()]).unwrap();
    let acquired_closure = reader.acquire_closure(&[ID.into()]).unwrap();
    let catalog = reader.list_catalog().unwrap();
    let catalog_entry = reader.read_catalog(ID).unwrap();
    let page = reader.read_published_paths(&start, 1).unwrap();
    let guard = reader.read_snapshot().unwrap();
    assert_eq!(guard.read_current(ID).unwrap(), current);
    assert_eq!(guard.acquire_current(ID).unwrap().state, acquired.state);
    assert_eq!(guard.read_closure(&[ID.into()]).unwrap().0, closure.0);
    assert_eq!(
        serde_json::to_value(guard.read_closure(&[ID.into()]).unwrap()).unwrap(),
        serde_json::to_value(closure).unwrap()
    );
    let guarded_acquired = guard.acquire_closure(&[ID.into()]).unwrap();
    assert_eq!(guarded_acquired.0, acquired_closure.0);
    assert_eq!(
        guarded_acquired.1[0].current.state,
        acquired_closure.1[0].current.state
    );
    assert_eq!(guard.list_catalog().unwrap(), catalog);
    assert_eq!(guard.read_catalog(ID).unwrap(), catalog_entry);
    let first = guard.read_published_paths(&start, 1).unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(page).unwrap()
    );
    let second = guard
        .read_published_paths(&first.covered_through, 1)
        .unwrap();
    assert_eq!(
        first.covered_through.sequence + 1,
        second.covered_through.sequence
    );
    assert_eq!(&second.source_position, guard.position());
    assert_eq!(&second.covered_through, guard.position());
}
