use super::*;
use std::sync::atomic::AtomicBool;

fn scratch_fixture(corrupt: bool) -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let unit = CurrentUnit {
        entity_id: "11111111-1111-4111-8111-111111111111".into(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: "small body".into(),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "test".into(),
            }],
        )
        .unwrap();
    if corrupt {
        let writer = fs::read_dir(store.root.join("history"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let path = fs::read_dir(writer)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut frames =
            codec::read("history", &mut BufReader::new(File::open(&path).unwrap())).unwrap();
        let malformed = serde_json::to_vec(&json!({"codec":"utf8-splice-v1","edits":[{"start_byte":"999","delete_bytes":"0","insert_utf8":"small body"}]})).unwrap();
        for frame in &mut frames {
            if frame.kind == "delta_chunk" {
                frame.payload = malformed.clone();
            }
            if frame.kind == "change_descriptor" {
                let mut descriptor: Value = codec::parse_json(&frame.payload).unwrap();
                descriptor["delta"]["bytes"] = json!(malformed.len().to_string());
                descriptor["delta"]["sha256"] = json!(codec::digest(&malformed));
                descriptor["delta"]["chunks"] = json!(1);
                frame.payload = serde_json::to_vec(&descriptor).unwrap();
            }
        }
        codec::write("history", &frames, &mut File::create(path).unwrap()).unwrap();
    }
    (temp, store)
}

#[test]
fn spilling_acquired_history_keeps_prior_component_handles_and_cleans_up() {
    let (_temp, store) = scratch_fixture(false);
    let reader = store.owned_published_reader();
    let mut spool = reader
        .acquire_history(&AtomicBool::new(false))
        .unwrap()
        .into_spool()
        .unwrap();
    let mut prior = None;
    spool
        .visit_metadata_closure("11111111-1111-4111-8111-111111111111", |change| {
            prior = Some(change.body);
            Ok(())
        })
        .unwrap();
    let prior = prior.unwrap();
    spool.spill_to_file().unwrap();
    let path = spool.path().to_path_buf();
    assert!(path.is_file());
    #[cfg(feature = "write-audit")]
    {
        let counts = agentlaw_sqlite::audit::snapshot(path.to_str().unwrap());
        assert_eq!(counts.writes, 1);
        assert_eq!(counts.commits, 1);
    }
    assert_eq!(prior.read_owned().unwrap(), b"small body");
    let mut after = None;
    spool
        .visit_metadata_closure("11111111-1111-4111-8111-111111111111", |change| {
            after = Some(change.body);
            Ok(())
        })
        .unwrap();
    assert_eq!(after.unwrap().read_owned().unwrap(), b"small body");
    drop(spool);
    assert!(!path.exists());
}
fn scratch_paths(dir: &Path) -> Vec<PathBuf> {
    if !dir.exists() {
        return vec![];
    }
    fs::read_dir(dir)
        .unwrap()
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scratch_paths(&path)
            } else if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("delta-")
            {
                vec![path]
            } else {
                vec![]
            }
        })
        .collect()
}
fn acquired_for_scratch(store: &Store, budget: u64) -> AcquiredHistory {
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    guard
        .acquire_history_with_budget(&AtomicBool::new(false), budget)
        .unwrap()
}
#[test]
fn scratch_memory_never_creates_files_during_validation() {
    let (_temp, store) = scratch_fixture(false);
    let acquired = acquired_for_scratch(&store, HISTORY_MEMORY_BUDGET);
    delta_stream::SCRATCH_CREATED.with(|paths| paths.borrow_mut().clear());
    let _spool = acquired.into_spool().unwrap();
    delta_stream::SCRATCH_CREATED.with(|paths| {
        assert!(
            paths.borrow().is_empty(),
            "scratch files created: {:?}",
            paths.borrow()
        )
    });
}
#[test]
fn scratch_spill_stays_inside_history_spools_during_validation() {
    let (_temp, store) = scratch_fixture(false);
    let acquired = acquired_for_scratch(&store, 1);
    delta_stream::SCRATCH_CREATED.with(|paths| paths.borrow_mut().clear());
    let _spool = acquired.into_spool().unwrap();
    delta_stream::SCRATCH_CREATED.with(|paths| assert!(!paths.borrow().is_empty()));
    delta_stream::SCRATCH_CREATED.with(|paths| {
        assert!(
            paths
                .borrow()
                .iter()
                .all(|p| p.starts_with(store.local.join("history-spools"))),
            "scratch files outside spool directory: {:?}",
            paths.borrow()
        )
    });
}
fn assert_corrupt_scratch_cleanup(budget: u64) {
    let (_temp, store) = scratch_fixture(true);
    let acquired = acquired_for_scratch(&store, budget);
    assert!(matches!(acquired.into_spool(), Err(Error::Corrupt(_))));
    assert!(
        scratch_paths(&store.local).is_empty(),
        "leaked scratch at budget {budget}"
    );
    let dir = store.local.join("history-spools");
    assert!(!dir.exists() || fs::read_dir(dir).unwrap().next().is_none());
}
#[test]
fn scratch_corrupt_memory_leaves_no_files() {
    assert_corrupt_scratch_cleanup(HISTORY_MEMORY_BUDGET);
}
#[test]
fn scratch_corrupt_spill_leaves_no_files() {
    assert_corrupt_scratch_cleanup(1);
}
#[test]
fn acquired_and_legacy_spools_have_same_edges() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let id = "11111111-1111-4111-8111-111111111111";
    let mut expected = Vec::new();
    for body in ["first", "second"] {
        let unit = CurrentUnit {
            entity_id: id.into(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: body.into(),
                }],
            },
        };
        let receipt = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit,
                    expected_versions: expected,
                    evidence: "test".into(),
                }],
            )
            .unwrap();
        expected = receipt
            .references
            .into_iter()
            .map(|r| r.observed_version)
            .collect();
    }
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let acquired = guard.acquire_history(&AtomicBool::new(false)).unwrap();
    drop(guard);
    let acquired = acquired.into_spool().unwrap();
    let legacy = store.spool_history().unwrap();
    for sql in ["SELECT child || ':' || parent || ':' || owner FROM edges ORDER BY child,parent"] {
        let read = |spool: &HistorySpool| -> Vec<String> {
            spool
                .conn
                .prepare(sql)
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        let actual = read(&acquired);
        assert_eq!(actual, read(&legacy));
        if sql.contains("edges") {
            assert_eq!(actual.len(), 1);
        }
    }
}
#[test]
fn legacy_scan_failure_removes_database() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let writer = store.root.join("history").join("writer");
    fs::create_dir_all(&writer).unwrap();
    fs::write(writer.join("bad.pack"), b"bad pack").unwrap();
    assert!(store.spool_history().is_err());
    assert_eq!(
        fs::read_dir(store.local.join("history-spools"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn database_spill_releases_near_budget_packs_before_loading() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let mut expected = Vec::new();
    for n in 0..3 {
        let unit = CurrentUnit {
            entity_id: "11111111-1111-4111-8111-111111111111".into(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: format!("{}-{n}", "large history body ".repeat(10_000)),
                }],
            },
        };
        let receipt = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit,
                    expected_versions: expected,
                    evidence: "test".into(),
                }],
            )
            .unwrap();
        expected = receipt
            .references
            .into_iter()
            .map(|r| r.observed_version)
            .collect();
    }
    let writer = fs::read_dir(store.root.join("history"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original = fs::read_dir(&writer)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::copy(&original, writer.join("duplicate.pack")).unwrap();
    let memory = acquired_for_scratch(&store, HISTORY_MEMORY_BUDGET)
        .into_spool()
        .unwrap();
    let raw = acquired_for_scratch(&store, HISTORY_MEMORY_BUDGET)
        .packs
        .iter()
        .map(|p| match p {
            AcquiredPack::Memory(bytes) => bytes.len() as u64,
            AcquiredPack::File(_) => panic!("unexpected spill"),
        })
        .sum::<u64>();
    assert!(raw > 100_000);
    let acquired = acquired_for_scratch(&store, raw + 1);
    assert!(!acquired.spilled);
    assert!(acquired.packs.len() >= 2);
    let spool = acquired
        .into_spool_observed(|history| {
            assert!(history.spilled);
            assert!(history
                .packs
                .iter()
                .all(|p| matches!(p, AcquiredPack::File(path) if path.exists())));
        })
        .unwrap();
    assert!(spool.path().exists());
    for sql in [
        "SELECT child || ':' || parent || ':' || owner FROM edges ORDER BY child,parent",
        "SELECT change_id || ':' || entity_id || ':' || metadata || ':' || descriptor || ':' || checkpoint FROM changes ORDER BY change_id",
        "SELECT kind || ':' || key || ':' || hex(payload) || ':' || sha FROM frames ORDER BY kind,key",
        "SELECT entity_id || ':' || state || ':' || coalesce(target, '') FROM current_units ORDER BY entity_id",
    ] {
        let read = |spool: &HistorySpool| -> Vec<String> {
            spool.conn.prepare(sql).unwrap().query_map([], |row| row.get(0)).unwrap()
                .collect::<std::result::Result<_, _>>().unwrap()
        };
        assert_eq!(read(&spool), read(&memory));
    }
    let dir = store.local.join("history-spools");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    drop(spool);
    assert_eq!(fs::read_dir(dir).unwrap().count(), 0);
}
#[test]
fn cancellation_between_packs_removes_copied_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    let writer = root.join("history").join("writer");
    fs::create_dir_all(&writer).unwrap();
    fs::write(writer.join("first.pack"), b"first").unwrap();
    fs::write(writer.join("second.pack"), b"second").unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let cancel = AtomicBool::new(false);
    let mut copied = 0;
    let result = guard.acquire_history_observed(&cancel, 1, || {
        copied += 1;
        cancel.store(true, std::sync::atomic::Ordering::Release);
    });
    assert!(matches!(result, Err(Error::CancelledBeforeDecision)));
    assert_eq!(copied, 1);
    assert_eq!(
        fs::read_dir(local.join("history-spools")).unwrap().count(),
        0
    );
}
#[test]
fn spilled_history_lives_until_spool_is_dropped() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    let writer = root.join("history").join("writer");
    fs::create_dir_all(&writer).unwrap();
    fs::write(
        writer.join("empty.pack"),
        b"<!-- agentlaw-file-v1 kind=history -->\n<!-- /agentlaw-file-v1 records=0 -->\n",
    )
    .unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let acquired = guard
        .acquire_history_with_budget(&AtomicBool::new(false), 1)
        .unwrap();
    drop(guard);
    let spool = acquired.into_spool().unwrap();
    let dir = local.join("history-spools");
    assert!(spool.path().exists());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    drop(spool);
    assert_eq!(fs::read_dir(dir).unwrap().count(), 0);
}
#[test]
fn spilled_corrupt_history_is_removed_after_decode_failure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    let writer = root.join("history").join("writer");
    fs::create_dir_all(&writer).unwrap();
    fs::write(writer.join("bad.pack"), b"bad pack").unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let acquired = guard
        .acquire_history_with_budget(&AtomicBool::new(false), 1)
        .unwrap();
    assert!(fs::read_dir(local.join("history-spools"))
        .unwrap()
        .next()
        .is_some());
    drop(guard);
    assert!(acquired.into_spool().is_err());
    assert!(fs::read_dir(local.join("history-spools"))
        .unwrap()
        .next()
        .is_none());
}
