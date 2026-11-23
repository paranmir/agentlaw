use agentlaw_storage::{Error, Store};
use std::sync::atomic::AtomicBool;

#[test]
fn acquired_history_defers_corruption_and_cleans_up() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    let writer = root.join("history").join("writer");
    std::fs::create_dir_all(&writer).unwrap();
    std::fs::write(writer.join("broken.pack"), b"not a pack").unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let acquired = guard.acquire_history(&AtomicBool::new(false)).unwrap();
    drop(guard);
    assert!(acquired.into_spool().is_err());
    let dir = local.join("history-spools");
    assert!(!dir.exists() || std::fs::read_dir(dir).unwrap().next().is_none());
}

#[test]
fn acquired_history_cancelled_before_copy() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("source"), &temp.path().join("local")).unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    assert!(matches!(
        guard.acquire_history(&AtomicBool::new(true)),
        Err(Error::CancelledBeforeDecision)
    ));
}

fn unit(body: &str, change: &str) -> agentlaw_storage::CurrentUnit {
    use agentlaw_storage::{CurrentUnit, Head, UnitState};
    CurrentUnit {
        entity_id: "11111111-1111-4111-8111-111111111111".into(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: serde_json::json!({"change_id": change,"applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: body.into(),
            }],
        },
    }
}
fn publish(store: &Store, body: &str, expected: Vec<String>) {
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![agentlaw_storage::Mutation {
                unit: unit(body, &uuid::Uuid::new_v4().to_string()),
                expected_versions: expected,
                evidence: "test evidence".into(),
            }],
        )
        .unwrap();
}
fn bodies(
    spool: &mut agentlaw_storage::history_spool::HistorySpool,
) -> Vec<(serde_json::Value, String, String, serde_json::Value)> {
    let mut result = Vec::new();
    spool
        .visit_closure("11111111-1111-4111-8111-111111111111", |change| {
            let state = change.into_owned()?;
            result.push((state.metadata, state.body, state.evidence, state.delta));
            Ok(())
        })
        .unwrap();
    result
}
#[test]
fn acquired_history_matches_legacy_and_keeps_snapshot_after_publish() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    publish(&store, "before", vec![]);
    let first = store
        .read_current("11111111-1111-4111-8111-111111111111")
        .unwrap();
    publish(
        &store,
        "second",
        first
            .references()
            .unwrap()
            .into_iter()
            .map(|r| r.observed_version)
            .collect(),
    );
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let position = guard.position().clone();
    let acquired = guard.acquire_history(&AtomicBool::new(false)).unwrap();
    drop(guard);
    let mut legacy = store.spool_history().unwrap();
    let expected = bodies(&mut legacy);
    let current = store
        .read_current("11111111-1111-4111-8111-111111111111")
        .unwrap();
    publish(
        &store,
        "after",
        current
            .references()
            .unwrap()
            .into_iter()
            .map(|r| r.observed_version)
            .collect(),
    );
    let mut snapshot = acquired.into_spool().unwrap();
    assert_eq!(snapshot.source_position, position);
    assert!(!snapshot.cache_reused);
    assert_eq!(bodies(&mut snapshot), expected);
}

#[test]
fn acquired_empty_history_does_not_create_spool_files() {
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("local");
    let store = Store::open(&temp.path().join("source"), &local).unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let acquired = guard.acquire_history(&AtomicBool::new(false)).unwrap();
    drop(guard);
    let _spool = acquired.into_spool().unwrap();
    for name in ["history-spools", "history-cache", "acquisitions"] {
        let dir = local.join(name);
        assert!(
            !dir.exists() || std::fs::read_dir(dir).unwrap().next().is_none(),
            "{name}"
        );
    }
}

#[test]
fn acquired_history_guard_blocks_publisher_but_owned_bytes_do_not() {
    use std::sync::mpsc;
    use std::time::Duration;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    publish(&store, "before", vec![]);
    let expected = store
        .read_current("11111111-1111-4111-8111-111111111111")
        .unwrap()
        .references()
        .unwrap()
        .into_iter()
        .map(|r| r.observed_version)
        .collect();
    let writer = Store::open(&root, &local).unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let acquired = guard.acquire_history(&AtomicBool::new(false)).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        publish(&writer, "after", expected);
        done_tx.send(()).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
    drop(guard);
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    thread.join().unwrap();
    let mut spool = acquired.into_spool().unwrap();
    let old = bodies(&mut spool);
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].1, "before");
}

#[test]
fn acquired_history_matches_legacy_across_pack_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    publish(&store, "a change split across two packs", vec![]);
    let writer = std::fs::read_dir(root.join("history"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let pack = std::fs::read_dir(&writer)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let frames = agentlaw_storage::codec::read(
        "history",
        &mut std::io::BufReader::new(std::fs::File::open(&pack).unwrap()),
    )
    .unwrap();
    assert!(frames.len() > 1);
    let midpoint = frames.len() / 2;
    agentlaw_storage::codec::write(
        "history",
        &frames[..midpoint],
        &mut std::fs::File::create(&pack).unwrap(),
    )
    .unwrap();
    agentlaw_storage::codec::write(
        "history",
        &frames[midpoint..],
        &mut std::fs::File::create(writer.join("second.pack")).unwrap(),
    )
    .unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let acquired = guard.acquire_history(&AtomicBool::new(false)).unwrap();
    drop(guard);
    let mut actual = acquired.into_spool().unwrap();
    let mut legacy = store.spool_history().unwrap();
    assert_eq!(bodies(&mut actual), bodies(&mut legacy));
}

#[test]
fn owned_reader_acquires_history_without_retaining_the_gate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    publish(&store, "before", vec![]);
    let reader = store.owned_published_reader();
    let acquired = reader.acquire_history(&AtomicBool::new(false)).unwrap();
    let current = store
        .read_current("11111111-1111-4111-8111-111111111111")
        .unwrap();
    let expected = current
        .references()
        .unwrap()
        .into_iter()
        .map(|r| r.observed_version)
        .collect();
    publish(&store, "after", expected);
    let mut spool = acquired.into_spool().unwrap();
    let snapshot = bodies(&mut spool);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].1, "before");
    assert!(matches!(
        reader.acquire_history(&AtomicBool::new(true)),
        Err(Error::CancelledBeforeDecision)
    ));
}
