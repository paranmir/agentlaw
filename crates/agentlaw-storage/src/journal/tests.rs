use super::*;
use crate::{CurrentUnit, Head, Mutation, UnitState};
use serde_json::json;

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn publish(store: &Store) {
    let mutation = Mutation {
        unit: CurrentUnit {
            entity_id: id(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":id()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: "journal test".into(),
                }],
            },
        },
        expected_versions: vec![],
        evidence: "test".into(),
    };
    store.publish(&id(), vec![mutation]).unwrap();
}

#[cfg(windows)]
fn read_optional(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("cannot inspect journal sidecar: {error}"),
    }
}

#[test]
fn active_writer_prevents_readers_from_creating_a_wal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(root, &local).unwrap();
    let _writer = store.hold_journal_writer().unwrap();
    publish(&store);
    let wal = local.join("journal.sqlite-wal");
    let before = std::fs::metadata(&wal).unwrap();
    let reader = store.owned_published_reader();
    let snapshot = reader.read_snapshot().unwrap();
    let mut after = snapshot.position().clone();
    after.sequence = 0;
    assert!(!snapshot
        .read_published_paths(&after, 16)
        .unwrap()
        .canonical_paths
        .is_empty());
    drop(snapshot);
    let after = std::fs::metadata(wal).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
}

#[test]
fn repair_releases_and_reopens_active_writer_under_source_gate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    let _writer = store.hold_journal_writer().unwrap();
    publish(&store);
    let report = Store::repair_local_journal(&root, &local).unwrap();
    assert_eq!(report.source_generation, 1);
    assert!(local.join("journal.sqlite-wal").exists());
    let reader = store.owned_published_reader();
    let snapshot = reader.read_snapshot().unwrap();
    let mut after = snapshot.position().clone();
    after.sequence = 0;
    assert!(!snapshot
        .read_published_paths(&after, 16)
        .unwrap()
        .canonical_paths
        .is_empty());
    drop(snapshot);
    publish(&store);
    assert_eq!(store.generation().unwrap(), 2);
}

#[test]
fn keeper_registration_waits_until_repair_releases_source_gate() {
    use std::{
        sync::{mpsc, Arc, Mutex},
        thread,
        time::Duration,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    publish(&store);
    let starting = Store::open(&root, &local).unwrap();
    let (paused_tx, paused_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let canonical_local = std::fs::canonicalize(&local).unwrap();
    *repair_hook().lock().unwrap() = Some((
        canonical_local,
        Arc::new(move || {
            paused_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }),
    ));
    let repair_root = root.clone();
    let repair_local = local.clone();
    let repair = thread::spawn(move || Store::repair_local_journal(repair_root, repair_local));
    paused_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (keeper_tx, keeper_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let keeper = thread::spawn(move || {
        started_tx.send(()).unwrap();
        keeper_tx.send(starting.hold_journal_writer()).ok()
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        keeper_rx.recv_timeout(Duration::from_millis(150)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release_tx.send(()).unwrap();
    assert_eq!(repair.join().unwrap().unwrap().source_generation, 1);
    let writer = keeper_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    keeper.join().unwrap();
    *repair_hook().lock().unwrap() = None;
    assert!(local.join("journal.sqlite-wal").exists());
    let reader = store.owned_published_reader();
    let snapshot = reader.read_snapshot().unwrap();
    let mut after = snapshot.position().clone();
    after.sequence = 0;
    assert!(!snapshot
        .read_published_paths(&after, 16)
        .unwrap()
        .canonical_paths
        .is_empty());
    drop(snapshot);
    drop(writer);
}

#[cfg(windows)]
#[test]
fn failed_writer_reopen_blocks_reads_until_retry_succeeds() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("local");
    let store = Store::open(dir.path().join("source"), &local).unwrap();
    let writer = store.hold_journal_writer().unwrap();
    publish(&store);
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    writer.set_waker(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    let gate = store.lock_unchecked().unwrap();
    let paused = pause(&local);
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(local.join("journal.sqlite"))
        .unwrap();
    assert!(paused.resume().is_err());
    wake_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    drop(gate);
    let reader = store.owned_published_reader();
    assert!(matches!(
        reader.read_snapshot(),
        Err(Error::RecoveryRequired(_))
    ));
    drop(locked);
    let db = local.join("journal.sqlite");
    let wal = local.join("journal.sqlite-wal");
    let before_db = std::fs::read(&db).unwrap();
    let before_wal = read_optional(&wal);
    assert!(matches!(
        reader.read_snapshot(),
        Err(Error::RecoveryRequired(_))
    ));
    assert_eq!(std::fs::read(&db).unwrap(), before_db);
    assert_eq!(read_optional(&wal), before_wal);
    store.retry_journal_writer(&writer).unwrap();
    let snapshot = reader.read_snapshot().unwrap();
    let slot = find(&local.join("journal.sqlite")).unwrap();
    assert!(slot.lock().unwrap().connection.is_some());
    let mut after = snapshot.position().clone();
    after.sequence = 0;
    assert!(!snapshot
        .read_published_paths(&after, 16)
        .unwrap()
        .canonical_paths
        .is_empty());
}

#[cfg(windows)]
#[test]
fn drop_reopen_failure_is_reported_before_reads_resume() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("local");
    let store = Store::open(dir.path().join("source"), &local).unwrap();
    let writer = store.hold_journal_writer().unwrap();
    publish(&store);
    let gate = store.lock_unchecked().unwrap();
    let paused = pause(&local);
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(local.join("journal.sqlite"))
        .unwrap();
    drop(paused);
    drop(locked);
    drop(gate);
    let reader = store.owned_published_reader();
    assert!(matches!(
        reader.read_snapshot(),
        Err(Error::RecoveryRequired(_))
    ));
    assert!(matches!(
        reader.read_snapshot(),
        Err(Error::RecoveryRequired(_))
    ));
    store.retry_journal_writer(&writer).unwrap();
    let snapshot = reader.read_snapshot().unwrap();
    let slot = find(&local.join("journal.sqlite")).unwrap();
    assert!(slot.lock().unwrap().connection.is_some());
    let mut after = snapshot.position().clone();
    after.sequence = 0;
    assert!(!snapshot
        .read_published_paths(&after, 16)
        .unwrap()
        .canonical_paths
        .is_empty());
}
