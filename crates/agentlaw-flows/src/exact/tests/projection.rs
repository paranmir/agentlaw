use super::*;

#[test]
fn pinned_old_generation_survives_activation_until_last_reader_drops() {
    let dir = tempfile::tempdir().unwrap();
    let old = generation(dir.path(), "old", "lexical-old.sqlite");
    let new = generation(dir.path(), "new", "lexical-new.sqlite");
    let registry = ProjectionGenerations::from_generation(old).unwrap();
    let pinned = registry.pin().unwrap();
    let staged = registry
        .stage(ShadowGeneration { generation: new })
        .unwrap();
    assert_eq!(staged.generation_id(), "new");
    let lexical_path = staged.generation.lexical_file.clone();
    let control = Connection::open_in_memory().unwrap();
    control
        .execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    control.execute("INSERT INTO settings(key,value) VALUES('exact_active_generation',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [staged.exact_file().to_str().unwrap()]).unwrap();
    registry.commit(staged).unwrap();
    let saved: String = control
        .query_row(
            "SELECT value FROM settings WHERE key='exact_active_generation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let restored = ProjectionGenerations::open("new", "epoch", saved, lexical_path).unwrap();
    assert_eq!(restored.pin().unwrap().generation.id, "new");
    assert_eq!(pinned.generation.id, "old");
    assert_eq!(registry.pin().unwrap().generation.id, "new");
    assert!(dir.path().join("exact-old.sqlite").exists());
    assert!(registry.take_retired().is_empty());
    drop(pinned);
    let retired = registry.take_retired();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].0, dir.path().join("exact-old.sqlite"));
}

#[test]
fn persistent_lexical_pointer_mismatch_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let current = generation(dir.path(), "old", "lexical-other.sqlite");
    let registry = ProjectionGenerations::from_generation(current).unwrap();
    assert_eq!(registry.pin().err().unwrap().code, "exact_not_ready");
}

#[test]
fn reopening_rejects_generation_id_for_a_different_file() {
    let dir = tempfile::tempdir().unwrap();
    let current = generation(dir.path(), "actual", "lexical-actual.sqlite");
    let error = ProjectionGenerations::open(
        "different",
        "epoch",
        current.exact_file,
        current.lexical_file,
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "exact_not_ready");
}

#[test]
fn writer_open_does_not_create_a_missing_active_projection() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("exact-missing.sqlite");
    assert!(ExactIndexWriter::open(&missing).is_err());
    assert!(!missing.exists());
}

#[test]
fn work_target_case_and_component_boundaries_match_rebuilt_index() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("canonical"))
            .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "case", "lexical-case.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![position.epoch, position.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let mut writer = ExactIndexWriter::open(&active.exact_file).unwrap();
    let registry = ProjectionGenerations::open(
        "case",
        &position.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    let project = uuid::Uuid::new_v4().to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![agentlaw_storage::Head {
                metadata: serde_json::json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[{"project_id":project,"path":"Src/pay/file.rs","kind":"file","reading":"required"}]}),
                body: "case sensitive target".into(),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![agentlaw_storage::Mutation {
                unit,
                expected_versions: vec![],
                evidence: "fixture".into(),
            }],
        )
        .unwrap();
    let guard = source.read_snapshot().unwrap();
    let stale =
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
    drop(guard);
    let shadow = writer
        .rebuild_shadow(&source, &RequestControl::default())
        .unwrap();
    let fresh_registry = ProjectionGenerations::from_generation(shadow.generation).unwrap();
    let guard = source.read_snapshot().unwrap();
    let fresh = ExactSnapshot::build(
        fresh_registry.pin().unwrap(),
        &guard,
        DeltaBudget::default(),
    )
    .unwrap();
    drop(guard);
    for (path, kind, expected) in [
        ("src/pay", TargetKind::Directory, false),
        ("Src/pay", TargetKind::Directory, true),
        ("src/pay/file.rs", TargetKind::File, false),
        ("src/payment", TargetKind::Directory, false),
    ] {
        let targets = [WorkTarget {
            path: path.into(),
            kind,
        }];
        let left = stale.work_targets(&project, &targets).unwrap();
        let right = fresh.work_targets(&project, &targets).unwrap();
        assert_eq!(left, right, "{path}");
        assert_eq!(!left.is_empty(), expected, "{path}");
    }
}

#[test]
fn writer_rejects_missing_active_tables_without_recreating_them() {
    for (file, table) in [
        ("exact-damaged.sqlite", "exact_heads"),
        ("lexical-damaged.sqlite", "docs"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let active = generation(dir.path(), "damaged", "lexical-damaged.sqlite");
        let connection = Connection::open(dir.path().join(file)).unwrap();
        connection
            .execute_batch(&format!("DROP TABLE {table}"))
            .unwrap();
        drop(connection);
        assert!(ExactIndexWriter::open(&active.exact_file).is_err());
        let check = Connection::open_with_flags(
            dir.path().join(file),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let count: i64 = check
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn writer_reports_missing_projection_metadata_as_corruption() {
    for (filename, table) in [
        ("exact-meta.sqlite", "exact_position"),
        ("exact-meta.sqlite", "exact_lexical"),
        ("lexical-meta.sqlite", "metadata"),
        ("lexical-meta.sqlite", "analyzer"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let active = generation(dir.path(), "meta", "lexical-meta.sqlite");
        let connection = Connection::open(dir.path().join(filename)).unwrap();
        connection
            .execute(&format!("DELETE FROM {table}"), [])
            .unwrap();
        drop(connection);
        assert_eq!(
            ExactIndexWriter::open(&active.exact_file)
                .err()
                .unwrap()
                .code,
            "exact_index_corrupt"
        );
    }
}

#[test]
fn pinned_exact_uses_memory_for_sqlite_temporary_storage() {
    let dir = tempfile::tempdir().unwrap();
    let generation = generation(dir.path(), "temp", "lexical-temp.sqlite");
    let pinned = ProjectionGenerations::from_generation(generation)
        .unwrap()
        .pin()
        .unwrap();
    let value: i64 = pinned
        .exact
        .connection
        .query_row("PRAGMA temp_store", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, 2);
}

thread_local! {
    static TEMP_OPEN_COUNT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static TEMP_DELETE_COUNT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
static ORIGINAL_VFS_OPEN: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_VFS_DELETE: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
unsafe extern "C" fn observe_sqlite_open(
    vfs: *mut rusqlite::ffi::sqlite3_vfs,
    name: *const std::ffi::c_char,
    file: *mut rusqlite::ffi::sqlite3_file,
    flags: std::ffi::c_int,
    out: *mut std::ffi::c_int,
) -> std::ffi::c_int {
    type Open = unsafe extern "C" fn(
        *mut rusqlite::ffi::sqlite3_vfs,
        *const std::ffi::c_char,
        *mut rusqlite::ffi::sqlite3_file,
        std::ffi::c_int,
        *mut std::ffi::c_int,
    ) -> std::ffi::c_int;
    let original: Open =
        std::mem::transmute(ORIGINAL_VFS_OPEN.load(std::sync::atomic::Ordering::SeqCst));
    let result = original(vfs, name, file, flags, out);
    if result == rusqlite::ffi::SQLITE_OK
        && flags
            & (rusqlite::ffi::SQLITE_OPEN_DELETEONCLOSE
                | rusqlite::ffi::SQLITE_OPEN_TEMP_DB
                | rusqlite::ffi::SQLITE_OPEN_TEMP_JOURNAL)
            != 0
    {
        TEMP_OPEN_COUNT.with(|count| {
            if let Some(n) = count.get() {
                count.set(Some(n + 1));
            }
        });
    }
    result
}
unsafe extern "C" fn observe_sqlite_delete(
    vfs: *mut rusqlite::ffi::sqlite3_vfs,
    name: *const std::ffi::c_char,
    sync_dir: std::ffi::c_int,
) -> std::ffi::c_int {
    type Delete = unsafe extern "C" fn(
        *mut rusqlite::ffi::sqlite3_vfs,
        *const std::ffi::c_char,
        std::ffi::c_int,
    ) -> std::ffi::c_int;
    let original: Delete =
        std::mem::transmute(ORIGINAL_VFS_DELETE.load(std::sync::atomic::Ordering::SeqCst));
    TEMP_DELETE_COUNT.with(|count| {
        if let Some(n) = count.get() {
            count.set(Some(n + 1));
        }
    });
    original(vfs, name, sync_dir)
}

#[test]
fn large_reverse_lookup_creates_no_sqlite_temporary_file() {
    const CHILD: &str = "W2D_SQLITE_VFS_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let isolated_temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exact::tests::projection::large_reverse_lookup_creates_no_sqlite_temporary_file",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("TEMP", isolated_temp.path())
            .env("TMP", isolated_temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let active = generation(dir.path(), "vfs", "lexical-vfs.sqlite");
    let mut writer = ExactIndexWriter::open(&active.exact_file).unwrap();
    let tx = writer.exact.connection.transaction().unwrap();
    {
        let mut insert = tx.prepare("INSERT INTO exact_edges(source,version,target,kind) VALUES(?1,'v','target','required')").unwrap();
        for n in 0..150_000u64 {
            insert
                .execute([format!("{:08x}-0000-4000-8000-000000000000", 150_000 - n)])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    let registry = ProjectionGenerations::from_generation(active).unwrap();
    let pinned = registry.pin().unwrap();
    let temp: i64 = pinned
        .exact
        .connection
        .query_row("PRAGMA temp_store", [], |row| row.get(0))
        .unwrap();
    assert_eq!(temp, 2);
    let snapshot_dir = |path: &Path,
                        sqlite_temp_only: bool|
     -> std::collections::BTreeMap<
        std::ffi::OsString,
        (u64, Option<std::time::SystemTime>),
    > {
        std::fs::read_dir(path)
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name();
                let text = name.to_string_lossy();
                if text.ends_with("-shm") || (sqlite_temp_only && !text.starts_with("etilqs")) {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                if !metadata.is_file() {
                    return None;
                }
                Some((name, (metadata.len(), metadata.modified().ok())))
            })
            .collect()
    };
    let before_db = snapshot_dir(dir.path(), false);
    let before_temp = snapshot_dir(&std::env::temp_dir(), true);
    unsafe {
        let vfs = rusqlite::ffi::sqlite3_vfs_find(std::ptr::null());
        let original = (*vfs).xOpen.expect("SQLite VFS open");
        let original_delete = (*vfs).xDelete.expect("SQLite VFS delete");
        ORIGINAL_VFS_OPEN.store(original as *mut (), std::sync::atomic::Ordering::SeqCst);
        ORIGINAL_VFS_DELETE.store(
            original_delete as *mut (),
            std::sync::atomic::Ordering::SeqCst,
        );
        (*vfs).xOpen = Some(observe_sqlite_open);
        (*vfs).xDelete = Some(observe_sqlite_delete);
        TEMP_OPEN_COUNT.with(|count| count.set(Some(0)));
        TEMP_DELETE_COUNT.with(|count| count.set(Some(0)));
        let result = pinned.exact.reverse_required(&["target".into()]);
        let opened = TEMP_OPEN_COUNT.with(|count| count.replace(None).unwrap());
        let deleted = TEMP_DELETE_COUNT.with(|count| count.replace(None).unwrap());
        (*vfs).xOpen = Some(original);
        (*vfs).xDelete = Some(original_delete);
        assert_eq!(result.unwrap().len(), 150_000);
        assert_eq!(opened, 0, "SQLite opened a temporary file during lookup");
        assert_eq!(deleted, 0, "SQLite deleted a file during lookup");
    }
    assert_eq!(before_db, snapshot_dir(dir.path(), false));
    assert_eq!(before_temp, snapshot_dir(&std::env::temp_dir(), true));
}

#[test]
fn shadow_rebuild_is_private_and_cancelled_rebuild_preserves_active_files() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().unwrap();
    let store =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("canonical"))
            .unwrap();
    let source = store.owned_published_reader();
    let epoch = source.read_snapshot().unwrap().position().epoch.clone();
    let active = generation(dir.path(), "old", "lexical-old.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute("UPDATE exact_position SET epoch=?1", [&epoch])
        .unwrap();
    drop(db);
    let registry =
        ProjectionGenerations::open("old", &epoch, &active.exact_file, &active.lexical_file)
            .unwrap();
    let mut writer = ExactIndexWriter::open(&active.exact_file).unwrap();
    let cancelled = RequestControl::default();
    cancelled.cancel.store(true, Ordering::Release);
    assert_eq!(
        writer
            .rebuild_shadow(&source, &cancelled)
            .err()
            .unwrap()
            .code,
        "cancelled"
    );
    assert_eq!(registry.pin().unwrap().generation.id, "old");
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = cancel.clone();
    let during = RequestControl::new(cancel, move |phase| {
        if phase == "rebuilding_shadow" {
            signal.store(true, Ordering::Release);
        }
    });
    assert_eq!(
        writer.rebuild_shadow(&source, &during).err().unwrap().code,
        "cancelled"
    );
    assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        !name.starts_with("exact-repair-") && !name.starts_with("lexical-rebuild-")
    }));
    assert_eq!(registry.pin().unwrap().generation.id, "old");
    let shadow = writer
        .rebuild_shadow(&source, &RequestControl::default())
        .unwrap();
    assert_eq!(registry.pin().unwrap().generation.id, "old");
    let staged = registry.stage(shadow).unwrap();
    assert_ne!(staged.exact_file(), active.exact_file);
    registry.commit(staged).unwrap();
    assert_ne!(registry.pin().unwrap().generation.id, "old");
}

fn remember(runtime: &mut crate::Runtime, body: &str) {
    let input = serde_json::json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed in projection test","applies_to":["user"]}]}});
    runtime
        .call(crate::parse_request(&input.to_string()).unwrap())
        .unwrap();
}

#[test]
fn stale_projection_adds_published_documents_and_advance_replaces_delta() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "snap", "lexical-snap.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![position.epoch, position.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let registry = ProjectionGenerations::open(
        "snap",
        &position.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    let _writer = ExactIndexWriter::open(&active.exact_file).unwrap();
    remember(&mut runtime, "uniquefractal projection first");
    let guard = source.read_snapshot().unwrap();
    let mut snapshot =
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
    drop(guard);
    let scope = agentlaw_search::ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("uniquefractal")
            .unwrap(),
        1
    );
    remember(&mut runtime, "uniqueorbit projection second");
    let guard = source.read_snapshot().unwrap();
    let failure = snapshot
        .advance(
            &guard,
            DeltaBudget {
                max_identities: 1,
                max_resident_bytes: 64 * 1024 * 1024,
            },
        )
        .unwrap_err();
    assert_eq!(failure.code, "capacity");
    drop(guard);
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("uniqueorbit")
            .unwrap(),
        0
    );
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("uniquefractal")
            .unwrap(),
        1
    );
    let guard = source.read_snapshot().unwrap();
    snapshot.advance(&guard, DeltaBudget::default()).unwrap();
    drop(guard);
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("uniqueorbit")
            .unwrap(),
        1
    );
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("uniquefractal")
            .unwrap(),
        1
    );
}

#[test]
fn delta_capacity_is_explicit_and_source_guard_can_be_released() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "cap", "lexical-cap.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![position.epoch, position.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let registry = ProjectionGenerations::open(
        "cap",
        &position.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    remember(&mut runtime, "capacity testing token");
    let guard = source.read_snapshot().unwrap();
    let result = ExactSnapshot::build(
        registry.pin().unwrap(),
        &guard,
        DeltaBudget {
            max_identities: 0,
            max_resident_bytes: 0,
        },
    );
    assert_eq!(result.err().unwrap().code, "capacity");
    let exact_budget = DeltaBudget {
        max_identities: 1,
        max_resident_bytes: "capacity testing token".len() as u64,
    };
    assert!(ExactSnapshot::build(registry.pin().unwrap(), &guard, exact_budget).is_ok());
    drop(guard);
    remember(&mut runtime, "post capacity publication token");
}

#[test]
fn newly_published_rule_is_in_exact_membership_without_rebuilding() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "rule", "lexical-rule.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![position.epoch, position.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let registry = ProjectionGenerations::open(
        "rule",
        &position.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    let input = serde_json::json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Always enforce the new rule.","evidence":"Observed","applies_to":["user"],"is_rule":true}]}});
    let response = runtime
        .call(crate::parse_request(&input.to_string()).unwrap())
        .unwrap();
    let id = response["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let dependent_input = serde_json::json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"Dependent on the new rule.","evidence":"Observed","applies_to":["user"],"required_memory_ids":[id]}]}});
    let dependent = runtime
        .call(crate::parse_request(&dependent_input.to_string()).unwrap())
        .unwrap();
    let dependent_id = dependent["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    let guard = source.read_snapshot().unwrap();
    let snapshot =
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
    drop(guard);
    let context = RequestContext {
        store_binding_id: "test".into(),
        user_id: "user".into(),
        machine_id: "machine".into(),
        project_id: None,
        connection_version: "1".into(),
    };
    assert_eq!(
        snapshot.ids_matching(&context, true, false).unwrap(),
        vec![id.to_string()]
    );
    assert_eq!(
        snapshot.reverse_required(&[id.to_string()]).unwrap(),
        vec![dependent_id.to_string()]
    );
}

#[test]
fn missing_ledger_is_not_an_empty_success() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "gap", "lexical-gap.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![position.epoch, position.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let registry = ProjectionGenerations::open(
        "gap",
        &position.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    remember(&mut runtime, "publication that will lose ledger coverage");
    let journal = Connection::open(dir.path().join("local/canonical/journal.sqlite")).unwrap();
    journal
        .execute(
            "DELETE FROM publications WHERE sequence>?1",
            [position.sequence.to_be_bytes().as_slice()],
        )
        .unwrap();
    drop(journal);
    let guard = source.read_snapshot().unwrap();
    let error = ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default())
        .err()
        .unwrap();
    assert_eq!(error.code, "source_coverage_lost");
}

#[test]
fn source_epoch_change_and_ack_ahead_are_distinct_failures() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("canonical"))
            .unwrap();
    let source = store.owned_published_reader();
    let guard = source.read_snapshot().unwrap();
    let actual = guard.position().clone();
    let active = generation(dir.path(), "ahead", "lexical-ahead.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![actual.epoch, (actual.sequence + 1).to_string()],
    )
    .unwrap();
    drop(db);
    let registry = ProjectionGenerations::open(
        "ahead",
        &actual.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    assert_eq!(
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default())
            .err()
            .unwrap()
            .code,
        "exact_not_ready"
    );
    let other = generation(dir.path(), "epoch", "lexical-epoch.sqlite");
    let registry =
        ProjectionGenerations::open("epoch", "epoch", &other.exact_file, &other.lexical_file)
            .unwrap();
    assert_eq!(
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default())
            .err()
            .unwrap()
            .code,
        "source_epoch_changed"
    );
}

#[test]
fn either_ack_may_be_ahead_and_delta_still_covers_both_views() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let before = source.read_snapshot().unwrap().position().clone();
    let input = serde_json::json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"ackoverlayuniqueterm","evidence":"Observed","applies_to":["user"],"is_rule":true}]}});
    let response = runtime
        .call(crate::parse_request(&input.to_string()).unwrap())
        .unwrap();
    let id = response["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap()
        .to_string();
    let guard = source.read_snapshot().unwrap();
    let end = guard.position().sequence;
    let context = RequestContext {
        store_binding_id: "test".into(),
        user_id: "user".into(),
        machine_id: "machine".into(),
        project_id: None,
        connection_version: "1".into(),
    };
    let scope = agentlaw_search::ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    for exact_ahead in [false, true] {
        let name = if exact_ahead { "ea" } else { "la" };
        let active = generation(dir.path(), name, &format!("lexical-{name}.sqlite"));
        let exact = Connection::open(&active.exact_file).unwrap();
        exact
            .execute(
                "UPDATE exact_position SET epoch=?1,sequence=?2",
                params![
                    before.epoch,
                    if exact_ahead { end } else { before.sequence }.to_string()
                ],
            )
            .unwrap();
        drop(exact);
        let lexical = Connection::open(&active.lexical_file).unwrap();
        lexical
            .execute(
                "UPDATE metadata SET ack=?1",
                [if exact_ahead { before.sequence } else { end } as i64],
            )
            .unwrap();
        drop(lexical);
        let registry = ProjectionGenerations::open(
            name,
            &before.epoch,
            &active.exact_file,
            &active.lexical_file,
        )
        .unwrap();
        let mut snapshot =
            ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
        assert_eq!(
            snapshot.ids_matching(&context, true, false).unwrap(),
            vec![id.clone()]
        );
        assert_eq!(
            snapshot
                .lexical(&scope)
                .unwrap()
                .matched("ackoverlayuniqueterm")
                .unwrap(),
            1
        );
    }
}

#[test]
fn pinned_snapshot_and_lexical_evaluation_do_not_write_projection_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "readonly", "lexical-readonly.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![position.epoch, position.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let registry = ProjectionGenerations::open(
        "readonly",
        &position.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    let _writer = ExactIndexWriter::open(&active.exact_file).unwrap();
    remember(&mut runtime, "readonlydeltauniqueterm");
    let guard = source.read_snapshot().unwrap();
    let files = [&active.exact_file, &active.lexical_file]
        .into_iter()
        .flat_map(|p| [p.clone(), PathBuf::from(format!("{}-wal", p.display()))])
        .collect::<Vec<_>>();
    let before: Vec<_> = files.iter().map(|p| std::fs::read(p).ok()).collect();
    let names = || {
        let mut paths: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| !n.to_string_lossy().ends_with("-shm"))
            .collect();
        paths.sort();
        paths
    };
    let before_names = names();
    let mut snapshot =
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
    assert_eq!(
        snapshot
            .lexical(&agentlaw_search::ScopeFilter {
                allowed_scopes: vec!["user".into()],
            })
            .unwrap()
            .matched("readonlydeltauniqueterm")
            .unwrap(),
        1
    );
    assert_eq!(
        before,
        files
            .iter()
            .map(|p| std::fs::read(p).ok())
            .collect::<Vec<_>>()
    );
    assert_eq!(before_names, names());
}

#[test]
fn writer_synchronizes_new_publications_and_matches_shadow_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let before = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "writer", "lexical-writer.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![before.epoch, before.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let mut writer = ExactIndexWriter::open(&active.exact_file).unwrap();
    remember(&mut runtime, "writer incremental unique phrase");
    let updated = writer
        .synchronize(&source, &RequestControl::default())
        .unwrap();
    assert!(updated.sequence > before.sequence);
    let scope = agentlaw_search::ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    let mut view = agentlaw_search::ReadView::open(&active.lexical_file).unwrap();
    let hits = view
        .effective(
            &scope,
            agentlaw_search::LexicalDelta {
                pending: &[],
                removed: &[],
                complete: true,
            },
        )
        .unwrap()
        .search("incremental unique", 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    let shadow = writer
        .rebuild_shadow(&source, &RequestControl::default())
        .unwrap();
    let mut rebuilt = agentlaw_search::ReadView::open(&shadow.generation.lexical_file).unwrap();
    let rebuilt_hits = rebuilt
        .effective(
            &scope,
            agentlaw_search::LexicalDelta {
                pending: &[],
                removed: &[],
                complete: true,
            },
        )
        .unwrap()
        .search("incremental unique", 10)
        .unwrap();
    assert_eq!(hits, rebuilt_hits);
    assert!(!dir.path().join("exact-maintenance.sqlite").exists());
}

#[test]
fn new_task_and_work_target_are_discovered_in_delta() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let connect = serde_json::json!({"action":"connect_project_memory","connect_project_memory":{"intent":"create","project_path":"C:/work/projection-test","project_name":"Projection"}});
    let connected = runtime
        .call(crate::parse_request(&connect.to_string()).unwrap())
        .unwrap();
    let project = connected["project_connection"]["project_id"]
        .as_str()
        .unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let before = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "task", "lexical-task.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![before.epoch, before.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let registry = ProjectionGenerations::open(
        "task",
        &before.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    let body = "## Objective\nReview.\n## Current position\nReady.\n## Resume point\nContinue.\n## References\nNone.\n";
    let input = serde_json::json!({"action":"remember_this","remember_this":{"project_path":"C:/work/projection-test","memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed","applies_to":["project"],"in_working_set":true,"work_targets":[{"project_id":project,"path":"src/pay/file.rs","kind":"file","reading":"required"}]}]}});
    let task = runtime
        .call(crate::parse_request(&input.to_string()).unwrap())
        .unwrap();
    let id = task["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap()
        .to_string();
    let guard = source.read_snapshot().unwrap();
    let snapshot =
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
    let context = RequestContext {
        store_binding_id: "test".into(),
        user_id: "user".into(),
        machine_id: runtime.machine_id().to_string(),
        project_id: Some(project.to_string()),
        connection_version: "1".into(),
    };
    assert_eq!(
        snapshot.ids_matching(&context, false, true).unwrap(),
        vec![id.clone()]
    );
    let targets = [WorkTarget {
        path: "src/pay".into(),
        kind: TargetKind::Directory,
    }];
    assert_eq!(
        snapshot.work_targets(project, &targets).unwrap(),
        vec![(id, Reading::Required)]
    );
    let outside = [WorkTarget {
        path: "src/payment".into(),
        kind: TargetKind::Directory,
    }];
    assert!(snapshot.work_targets(project, &outside).unwrap().is_empty());
}

#[test]
fn advance_shadows_old_rule_and_old_terms_after_source_evolution() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let before = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "evolve", "lexical-evolve.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![before.epoch, before.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let input = serde_json::json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"obsoleteuniquetoken","evidence":"Observed","applies_to":["user"],"is_rule":true}]}});
    let result = runtime
        .call(crate::parse_request(&input.to_string()).unwrap())
        .unwrap();
    let id = result["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut writer = ExactIndexWriter::open(&active.exact_file).unwrap();
    writer
        .synchronize(&source, &RequestControl::default())
        .unwrap();
    let registry = ProjectionGenerations::open(
        "evolve",
        &before.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    let guard = source.read_snapshot().unwrap();
    let mut snapshot =
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
    drop(guard);
    let mut unit = store.read_current(&id).unwrap();
    let versions = unit
        .references()
        .unwrap()
        .into_iter()
        .map(|r| r.observed_version)
        .collect();
    if let UnitState::Live { heads } = &mut unit.state {
        heads[0].metadata["change_id"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        heads[0].metadata["is_rule"] = serde_json::json!(false);
        heads[0].body = "freshuniquetoken".into();
    }
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![agentlaw_storage::Mutation {
                unit,
                expected_versions: versions,
                evidence: "Rule changed".into(),
            }],
        )
        .unwrap();
    let guard = source.read_snapshot().unwrap();
    snapshot.advance(&guard, DeltaBudget::default()).unwrap();
    drop(guard);
    let context = RequestContext {
        store_binding_id: "test".into(),
        user_id: "user".into(),
        machine_id: "machine".into(),
        project_id: None,
        connection_version: "1".into(),
    };
    assert!(snapshot
        .ids_matching(&context, true, false)
        .unwrap()
        .is_empty());
    let scope = agentlaw_search::ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("obsoleteuniquetoken")
            .unwrap(),
        0
    );
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("freshuniquetoken")
            .unwrap(),
        1
    );
    assert_eq!(
        snapshot.identical_bodies("freshuniquetoken").unwrap(),
        vec![id.clone()]
    );
    let mut unit = store.read_current(&id).unwrap();
    let versions = unit
        .references()
        .unwrap()
        .into_iter()
        .map(|reference| reference.observed_version)
        .collect();
    let machine = runtime.machine_id().to_string();
    if let UnitState::Live { heads } = &mut unit.state {
        heads[0].metadata["change_id"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        heads[0].metadata["is_rule"] = serde_json::json!(true);
        heads[0].metadata["applicability"] =
            serde_json::json!({"scope":"machine","machine_id":machine});
    }
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![agentlaw_storage::Mutation {
                unit,
                expected_versions: versions,
                evidence: "Scope changed".into(),
            }],
        )
        .unwrap();
    let guard = source.read_snapshot().unwrap();
    snapshot.advance(&guard, DeltaBudget::default()).unwrap();
    drop(guard);
    assert!(snapshot
        .ids_matching(&context, true, false)
        .unwrap()
        .is_empty());
    let machine_context = RequestContext {
        machine_id: machine.clone(),
        ..context
    };
    assert_eq!(
        snapshot
            .ids_matching(&machine_context, true, false)
            .unwrap(),
        vec![id]
    );
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("freshuniquetoken")
            .unwrap(),
        0
    );
    let machine_scope = agentlaw_search::ScopeFilter {
        allowed_scopes: vec![format!("machine:{machine}")],
    };
    assert_eq!(
        snapshot
            .lexical(&machine_scope)
            .unwrap()
            .matched("freshuniquetoken")
            .unwrap(),
        1
    );
}

#[test]
fn consolidation_redirect_removes_old_lexical_heads() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let before = source.read_snapshot().unwrap().position().clone();
    let active = generation(dir.path(), "redirect", "lexical-redirect.sqlite");
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![before.epoch, before.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    let create = serde_json::json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"redirectobsoleteunique","evidence":"Observed","applies_to":["user"]}]}});
    let first = runtime
        .call(crate::parse_request(&create.to_string()).unwrap())
        .unwrap()["results"][0]["memory_ref"]
        .clone();
    let pending = runtime
        .call(crate::parse_request(&create.to_string()).unwrap())
        .unwrap();
    let confirm = serde_json::json!({"action":"remember_this","remember_this":{"pending_action":"continue","pending_batch_ref":pending["pending_batch_ref"],"resolution_ref":pending["resolution_ref"]}});
    let second = runtime
        .call(crate::parse_request(&confirm.to_string()).unwrap())
        .unwrap()["results"][0]["memory_ref"]
        .clone();
    let mut writer = ExactIndexWriter::open(&active.exact_file).unwrap();
    writer
        .synchronize(&source, &RequestControl::default())
        .unwrap();
    let registry = ProjectionGenerations::open(
        "redirect",
        &before.epoch,
        &active.exact_file,
        &active.lexical_file,
    )
    .unwrap();
    let pinned = registry.pin().unwrap();
    let consolidate = serde_json::json!({"action":"remember_this","remember_this":{"memories":[{"operation":"consolidate","consolidation_refs":[first,second],"what_to_remember":"redirectcombinedunique","evidence":"Confirmed"}]}});
    assert_eq!(
        runtime
            .call(crate::parse_request(&consolidate.to_string()).unwrap())
            .unwrap()["status"],
        "remembered"
    );
    let guard = source.read_snapshot().unwrap();
    let mut snapshot = ExactSnapshot::build(pinned, &guard, DeltaBudget::default()).unwrap();
    assert!(snapshot
        .units
        .iter()
        .any(|unit| matches!(unit.state, UnitState::Redirect { .. })));
    let scope = agentlaw_search::ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("redirectobsoleteunique")
            .unwrap(),
        0
    );
    assert_eq!(
        snapshot
            .lexical(&scope)
            .unwrap()
            .matched("redirectcombinedunique")
            .unwrap(),
        1
    );
}

#[test]
fn recovery_registry_rejects_pins_until_valid_shadow_is_committed() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("canonical"))
            .unwrap();
    let source = store.owned_published_reader();
    let pos = source.read_snapshot().unwrap().position().clone();
    let old = generation(dir.path(), "damaged", "lexical-damaged.sqlite");
    let db = Connection::open(&old.exact_file).unwrap();
    db.execute_batch("DROP TABLE exact_heads").unwrap();
    drop(db);
    let registry = ProjectionGenerations::open_for_recovery(
        "damaged",
        &pos.epoch,
        &old.exact_file,
        &old.lexical_file,
    )
    .unwrap();
    assert!(registry.pin().is_err());
    let shadow =
        ExactIndexWriter::rebuild_shadow_in(dir.path(), &source, &RequestControl::default())
            .unwrap();
    let staged = registry.stage(shadow).unwrap();
    assert_eq!(staged.epoch(), pos.epoch);
    assert!(staged.lexical_file().exists());
    registry.commit(staged).unwrap();
    assert!(registry.pin().is_ok());
    assert_eq!(
        registry.take_retired(),
        vec![(old.exact_file, old.lexical_file)]
    );
}

#[test]
fn writerless_shadow_matches_writer_rebuild_and_can_be_consumed_for_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        crate::Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let store = agentlaw_storage::Store::open(
        dir.path().join("source"),
        dir.path().join("local/canonical"),
    )
    .unwrap();
    let source = store.owned_published_reader();
    let pos = source.read_snapshot().unwrap().position().clone();
    let old = generation(dir.path(), "comparison", "lexical-comparison.sqlite");
    let db = Connection::open(&old.exact_file).unwrap();
    db.execute(
        "UPDATE exact_position SET epoch=?1,sequence=?2",
        params![pos.epoch, pos.sequence.to_string()],
    )
    .unwrap();
    drop(db);
    remember(&mut runtime, "writerless comparison unique");
    let mut writer = ExactIndexWriter::open(&old.exact_file).unwrap();
    let first = writer
        .rebuild_shadow(&source, &RequestControl::default())
        .unwrap();
    let second =
        ExactIndexWriter::rebuild_shadow_in(dir.path(), &source, &RequestControl::default())
            .unwrap();
    let first_registry = ProjectionGenerations::from_generation(first.generation).unwrap();
    let second_registry = ProjectionGenerations::from_generation(second.generation).unwrap();
    let guard = source.read_snapshot().unwrap();
    let a = ExactSnapshot::build(
        first_registry.pin().unwrap(),
        &guard,
        DeltaBudget::default(),
    )
    .unwrap();
    let b = ExactSnapshot::build(
        second_registry.pin().unwrap(),
        &guard,
        DeltaBudget::default(),
    )
    .unwrap();
    assert_eq!(
        a.identical_bodies("writerless comparison unique").unwrap(),
        b.identical_bodies("writerless comparison unique").unwrap()
    );
    assert_eq!(
        a.identical_bodies("writerless comparison unique")
            .unwrap()
            .len(),
        1
    );
    drop(a);
    drop(b);
    drop(guard);
    let other =
        ExactIndexWriter::rebuild_shadow_in(dir.path(), &source, &RequestControl::default())
            .unwrap();
    let staged = first_registry.stage(other).unwrap();
    let (exact, lexical) = staged.into_files(); // consumes staged; it cannot also be passed to commit
    assert!(exact.exists() && lexical.exists());
}

#[test]
fn sqlite_schema_errors_distinguish_locking_from_corrupt_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.sqlite");
    let owner = Connection::open(&path).unwrap();
    owner
        .execute_batch("CREATE TABLE fixture(id INTEGER); BEGIN EXCLUSIVE")
        .unwrap();
    let reader = Connection::open(&path).unwrap();
    reader.busy_timeout(std::time::Duration::ZERO).unwrap();
    let error = reader
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_err();
    assert_eq!(exact_schema_error(error).code, "exact_index_unavailable");
    drop(reader);
    owner.execute_batch("ROLLBACK").unwrap();
    drop(owner);
    std::fs::write(&path, b"not a SQLite database").unwrap();
    let damaged = Connection::open(&path).unwrap();
    let error = damaged
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_err();
    assert_eq!(exact_schema_error(error).code, "exact_index_corrupt");
}

#[test]
fn pin_reports_lexical_metadata_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let active = generation(dir.path(), "corrupt-pin", "lexical-corrupt-pin.sqlite");
    let lexical = Connection::open(&active.lexical_file).unwrap();
    lexical.execute("DELETE FROM metadata", []).unwrap();
    drop(lexical);
    assert_eq!(
        ProjectionGenerations::open(
            &active.id,
            &active.epoch,
            &active.exact_file,
            &active.lexical_file,
        )
        .err()
        .unwrap()
        .code,
        "exact_index_corrupt"
    );
    let registry = ProjectionGenerations::from_generation(active).unwrap();
    assert_eq!(registry.pin().err().unwrap().code, "exact_index_corrupt");
}

#[cfg(windows)]
#[test]
fn pin_reports_lexical_sharing_violation_as_unavailable() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempfile::tempdir().unwrap();
    let active = generation(dir.path(), "locked-pin", "lexical-locked-pin.sqlite");
    let registry = ProjectionGenerations::from_generation(active).unwrap();
    let lexical = dir.path().join("lexical-locked-pin.sqlite");
    let guard = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(lexical)
        .unwrap();
    assert_eq!(
        registry.pin().err().unwrap().code,
        "exact_index_unavailable"
    );
    drop(guard);
    assert!(registry.pin().is_ok());
}

#[test]
fn lexical_snapshot_reports_missing_schema_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("local")).unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let mut active = generation(dir.path(), "bad-query", "lexical-bad-query.sqlite");
    active.epoch = position.epoch.clone();
    let exact = Connection::open(&active.exact_file).unwrap();
    exact
        .execute("UPDATE exact_position SET epoch=?1", [&position.epoch])
        .unwrap();
    drop(exact);
    let lexical = Connection::open(&active.lexical_file).unwrap();
    lexical.execute_batch("DROP TABLE docs").unwrap();
    drop(lexical);
    let registry = ProjectionGenerations::from_generation(active).unwrap();
    let guard = source.read_snapshot().unwrap();
    let mut snapshot =
        ExactSnapshot::build(registry.pin().unwrap(), &guard, DeltaBudget::default()).unwrap();
    let error = snapshot
        .lexical(&agentlaw_search::ScopeFilter {
            allowed_scopes: vec!["user".into()],
        })
        .err()
        .unwrap();
    assert_eq!(error.code, "exact_index_corrupt");
}
