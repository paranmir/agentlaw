use super::*;
use crate::{ExactVectorIndex, VectorBackend};
struct TempDir(std::path::PathBuf);
impl TempDir {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn tempdir() -> std::io::Result<TempDir> {
    let p = std::env::temp_dir().join(format!("agentlaw-vector-file-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&p)?;
    Ok(TempDir(p))
}
fn identity(kind: VectorFileKind) -> VectorFileIdentity {
    VectorFileIdentity {
        binding: "binding".into(),
        kind,
        epoch: "epoch".into(),
        model_digest: "model".into(),
        config: "config".into(),
        generation: "generation".into(),
        dimension: 2,
    }
}
fn row(change: &str, vector: Vec<f32>) -> VectorRecord {
    VectorRecord {
        memory_id: "m".into(),
        change_id: change.into(),
        section: "s".into(),
        scope: "user".into(),
        vector,
    }
}
fn heads(ids: &[&str]) -> Vec<CurrentHeads> {
    vec![CurrentHeads {
        memory_id: "m".into(),
        change_ids: ids.iter().map(|s| s.to_string()).collect(),
    }]
}
fn pos(n: i64) -> SourcePosition {
    SourcePosition {
        epoch: "epoch".into(),
        sequence: n,
    }
}
fn contents(
    w: &VectorFileWriter,
) -> (
    VectorFileProgress,
    Vec<(String, String, String, String, Vec<u8>)>,
    Vec<(i64, i64, String, String, String, String)>,
) {
    let vectors = w.connection.prepare("SELECT memory,change_id,section,scope,vector FROM vectors ORDER BY memory,change_id,section").unwrap().query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let buckets = w.connection.prepare("SELECT table_id,bucket,scope,memory,change_id,section FROM vector_buckets ORDER BY table_id,bucket,scope,memory,change_id,section").unwrap().query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    (w.progress().unwrap(), vectors, buckets)
}
#[test]
fn late_metadata_failure_rolls_back_all_and_removes_old_buckets() {
    for kind in [
        VectorFileKind::Current,
        VectorFileKind::History {
            subject: "subject".into(),
        },
    ] {
        let d = tempdir().unwrap();
        let id = identity(kind.clone());
        let mut w = VectorFileWriter::open(d.path().join("v"), &id).unwrap();
        w.commit(VectorFileBatch {
            records: vec![row("old", vec![1., 0.]), row("keep", vec![0., 1.])],
            ack: Some(pos(1)),
            ..Default::default()
        })
        .unwrap();
        let before = contents(&w);
        w.connection.execute_batch("CREATE TEMP TRIGGER reject_meta BEFORE UPDATE ON binding_vector_meta BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        let batch = || VectorFileBatch {
            heads: heads(&["new", "keep"]),
            records: vec![row("new", vec![1., 1.])],
            ack: Some(pos(2)),
            inventory: Some(InventoryCursor {
                start: pos(1),
                completed_identity: "m".into(),
            }),
            complete: true,
            completed_changes: if matches!(kind, VectorFileKind::History { .. }) {
                vec![("m".into(), "new".into())]
            } else {
                vec![]
            },
        };
        assert!(w.commit(batch()).is_err());
        assert_eq!(contents(&w), before);
        w.connection
            .execute_batch("DROP TRIGGER reject_meta")
            .unwrap();
        w.commit(batch()).unwrap();
        let old_count: i64 = w
            .connection
            .query_row(
                "SELECT count(*) FROM vector_buckets WHERE memory='m' AND change_id='old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            old_count,
            if matches!(kind, VectorFileKind::History { .. }) {
                12
            } else {
                0
            }
        );
        assert_eq!(
            w.contains("m", "old", "s").unwrap(),
            matches!(kind, VectorFileKind::History { .. })
        );
        assert!(w.contains("m", "keep", "s").unwrap());
        assert_eq!(w.progress().unwrap().ack, Some(pos(2)));
        assert!(w.progress().unwrap().complete);
    }
}
#[test]
fn ann_matches_existing_index_and_preserves_snapshot() {
    let d = tempdir().unwrap();
    let id = identity(VectorFileKind::Current);
    let path = d.path().join("new");
    let mut records = vec![row("opposite", vec![-1., 0.]), row("same", vec![1., 0.])];
    records[0].memory_id = "opposite".into();
    records[1].memory_id = "same".into();
    let mut w = VectorFileWriter::open(&path, &id).unwrap();
    w.commit(VectorFileBatch {
        records: records.clone(),
        ack: Some(pos(1)),
        ..Default::default()
    })
    .unwrap();
    let mut old = ExactVectorIndex::open(d.path().join("old"), "model", "config", 2).unwrap();
    old.commit(1, &records, &[]).unwrap();
    let oldview = old.read_view().unwrap();
    let view = VectorFileReadView::open(&path, &id).unwrap();
    for scope in [
        ScopeFilter {
            allowed_scopes: vec!["user".into()],
        },
        ScopeFilter::default(),
    ] {
        for query in [[1., 0.], [-1., 0.], [0., 1.]] {
            for limit in [0, 1, 10] {
                let (expected, metrics) = oldview.search_ann(&query, &scope, limit).unwrap();
                let actual = view.search_ann(&query, &scope, limit).unwrap();
                assert_eq!(actual.hits, expected);
                assert_eq!(actual.metrics.candidates_scored, metrics.candidates_scored);
                assert_eq!(actual.progress.ack, Some(pos(1)));
            }
        }
    }
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    let before = view.search_ann(&[1., 0.], &scope, 10).unwrap();
    assert_eq!(before.hits.len(), 1);
    assert_eq!(view.search(&[1., 0.], &scope, 10).unwrap().hits.len(), 2);
    w.commit(VectorFileBatch {
        heads: vec![CurrentHeads {
            memory_id: "same".into(),
            change_ids: vec![],
        }],
        ack: Some(pos(2)),
        ..Default::default()
    })
    .unwrap();
    let after = view.search_ann(&[1., 0.], &scope, 10).unwrap();
    assert_eq!(after.hits, before.hits);
    assert_eq!(after.progress.ack, Some(pos(1)));
    assert_eq!(
        VectorFileReadView::open(&path, &id)
            .unwrap()
            .search_ann(&[1., 0.], &scope, 10)
            .unwrap()
            .progress
            .ack,
        Some(pos(2))
    );
}
#[test]
fn current_heads_history_and_atomic_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v.sqlite");
    let id = identity(VectorFileKind::Current);
    let mut w = VectorFileWriter::open(&path, &id).unwrap();
    w.commit(VectorFileBatch {
        heads: heads(&["old"]),
        records: vec![row("old", vec![1., 0.])],
        ack: Some(pos(1)),
        ..Default::default()
    })
    .unwrap();
    w.commit(VectorFileBatch {
        heads: heads(&["new", "other"]),
        records: vec![row("new", vec![0., 1.]), row("other", vec![1., 1.])],
        ..Default::default()
    })
    .unwrap();
    assert!(!w.contains("m", "old", "s").unwrap());
    assert!(w.contains("m", "new", "s").unwrap() && w.contains("m", "other", "s").unwrap());
    let before = w.progress().unwrap();
    assert!(w
        .commit(VectorFileBatch {
            heads: heads(&["bad"]),
            records: vec![row("bad", vec![1.])],
            ack: Some(pos(2)),
            ..Default::default()
        })
        .is_err());
    assert_eq!(before, w.progress().unwrap());
    assert!(w.contains("m", "new", "s").unwrap());
    w.commit(VectorFileBatch {
        records: vec![row("new", vec![0., 1.])],
        ack: Some(pos(2)),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        w.connection
            .query_row(
                "SELECT COUNT(*) FROM vectors WHERE memory='m' AND change_id='new'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let progress = w.progress().unwrap();
    drop(w);
    let w = VectorFileWriter::open(&path, &id).unwrap();
    assert_eq!(progress, w.progress().unwrap());
    assert!(w.contains("m", "new", "s").unwrap());
    let hpath = dir.path().join("h.sqlite");
    let hid = identity(VectorFileKind::History {
        subject: "subject".into(),
    });
    let mut h = VectorFileWriter::open(&hpath, &hid).unwrap();
    h.commit(VectorFileBatch {
        heads: heads(&["new"]),
        records: vec![row("old", vec![1., 0.]), row("new", vec![0., 1.])],
        completed_changes: vec![("m".into(), "old".into())],
        ..Default::default()
    })
    .unwrap();
    assert!(h.contains("m", "old", "s").unwrap());
    assert!(h.is_change_completed("m", "old").unwrap());
}

#[test]
fn history_completion_lookup_survives_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite");
    let id = identity(VectorFileKind::History {
        subject: "subject".into(),
    });
    let mut writer = VectorFileWriter::open(&path, &id).unwrap();
    for n in 0..256 {
        writer
            .commit(VectorFileBatch {
                completed_changes: vec![(format!("change:{n}"), format!("{n}"))],
                ..Default::default()
            })
            .unwrap();
    }
    drop(writer);
    let writer = VectorFileWriter::open(&path, &id).unwrap();
    assert!(writer.is_change_completed("change:0", "0").unwrap());
    assert!(writer.is_change_completed("change:255", "255").unwrap());
    assert!(!writer.is_change_completed("change:256", "256").unwrap());
    let view = VectorFileReadView::open(&path, &id).unwrap();
    assert!(view.is_change_completed("change:255", "255").unwrap());
    assert_eq!(view.progress().commit_id, 256);
}
#[test]
fn inventory_snapshot_and_exact_scores() {
    let d = tempdir().unwrap();
    let path = d.path().join("v");
    let id = identity(VectorFileKind::Current);
    let mut w = VectorFileWriter::open(&path, &id).unwrap();
    let cursor = InventoryCursor {
        start: pos(0),
        completed_identity: "m".into(),
    };
    w.commit(VectorFileBatch {
        records: vec![row("a", vec![1., 0.])],
        inventory: Some(cursor.clone()),
        ..Default::default()
    })
    .unwrap();
    drop(w);
    let mut w = VectorFileWriter::open(&path, &id).unwrap();
    let view = VectorFileReadView::open(&path, &id).unwrap();
    assert!(!view.progress().complete);
    assert_eq!(view.progress().inventory, Some(cursor));
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    let hits = view.search(&[1., 0.], &scope, 10).unwrap().hits;
    let mut exact = ExactVectorIndex::open(d.path().join("exact"), "model", "config", 2).unwrap();
    exact.commit(1, &[row("a", vec![1., 0.])], &[]).unwrap();
    assert_eq!(
        hits,
        exact
            .read_view()
            .unwrap()
            .search(&[1., 0.], &scope, 10)
            .unwrap()
    );
    w.commit(VectorFileBatch {
        heads: heads(&["b"]),
        records: vec![row("b", vec![0., 1.])],
        ack: Some(pos(1)),
        complete: true,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(view.search(&[1., 0.], &scope, 10).unwrap().hits, hits);
    drop(view);
    drop(w);
    let w = VectorFileWriter::open(&path, &id).unwrap();
    assert_eq!(w.progress().unwrap().ack, Some(pos(1)));
    assert!(w.progress().unwrap().complete);
}
#[test]
fn empty_commit_and_old_head_search_after_restart() {
    let d = tempdir().unwrap();
    let p = d.path().join("vector");
    let id = identity(VectorFileKind::Current);
    let mut w = VectorFileWriter::open(&p, &id).unwrap();
    w.commit(VectorFileBatch {
        records: vec![row("old", vec![1., 0.])],
        ..Default::default()
    })
    .unwrap();
    w.commit(VectorFileBatch {
        heads: heads(&["new"]),
        records: vec![row("new", vec![0., 1.])],
        ..Default::default()
    })
    .unwrap();
    w.commit(VectorFileBatch {
        ack: Some(pos(7)),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(w.progress().unwrap().ack, Some(pos(7)));
    let v = VectorFileReadView::open(&p, &id).unwrap();
    let hits = v
        .search(
            &[1., 0.],
            &ScopeFilter {
                allowed_scopes: vec!["user".into()],
            },
            10,
        )
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].change_id, "new");
    assert_eq!(hits[0].score, 0.0);
}
#[test]
fn exact_score_scope_and_tie_equivalence() {
    let d = tempdir().unwrap();
    let id = identity(VectorFileKind::Current);
    let mut records = vec![row("a", vec![1., 0.]), row("b", vec![0., 1.])];
    let mut other = row("c", vec![1., 0.]);
    other.memory_id = "a".into();
    records.push(other);
    let mut hidden = row("d", vec![1., 0.]);
    hidden.memory_id = "hidden".into();
    hidden.scope = "private".into();
    records.push(hidden);
    let path = d.path().join("new");
    let mut w = VectorFileWriter::open(&path, &id).unwrap();
    w.commit(VectorFileBatch {
        records: records.clone(),
        ..Default::default()
    })
    .unwrap();
    let mut exact = ExactVectorIndex::open(d.path().join("old"), "model", "config", 2).unwrap();
    exact.commit(1, &records, &[]).unwrap();
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    let expected = exact
        .read_view()
        .unwrap()
        .search(&[1., 0.], &scope, 10)
        .unwrap();
    let actual = VectorFileReadView::open(&path, &id)
        .unwrap()
        .search(&[1., 0.], &scope, 10)
        .unwrap()
        .hits;
    assert_eq!(actual, expected);
    assert_eq!(
        actual
            .iter()
            .map(|h| h.memory_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "m"]
    );
}
#[test]
fn reader_classifies_without_mutation() {
    let d = tempdir().unwrap();
    let p = d.path().join("v");
    let id = identity(VectorFileKind::Current);
    assert!(matches!(
        VectorFileReadView::open(&p, &id),
        Err(VectorFileError::Missing)
    ));
    assert!(!p.exists());
    std::fs::write(&p, b"not sqlite").unwrap();
    assert!(matches!(
        VectorFileReadView::open(&p, &id),
        Err(VectorFileError::Corrupt(_))
    ));
    assert_eq!(std::fs::read(&p).unwrap(), b"not sqlite");
    std::fs::remove_file(&p).unwrap();
    let _w = VectorFileWriter::open(&p, &id).unwrap();
    let before = std::fs::read(&p).unwrap();
    let wal = p.with_file_name("v-wal");
    let wal_before = std::fs::read(&wal).unwrap();
    let mut files_before: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    files_before.sort();
    for field in 0..4 {
        let mut other = id.clone();
        match field {
            0 => other.binding.push('x'),
            1 => other.epoch.push('x'),
            2 => other.model_digest.push('x'),
            _ => other.generation.push('x'),
        };
        assert!(matches!(
            VectorFileReadView::open(&p, &other),
            Err(VectorFileError::Mismatch)
        ));
    }
    let v = VectorFileReadView::open(&p, &id).unwrap();
    assert!(v
        .search(&[1., 0.], &ScopeFilter::default(), 10)
        .unwrap()
        .hits
        .is_empty());
    assert!(v
        .search_ann(&[1., 0.], &ScopeFilter::default(), 10)
        .unwrap()
        .hits
        .is_empty());
    assert_eq!(
        v.view
            .connection
            .is_readonly(rusqlite::DatabaseName::Main)
            .unwrap(),
        true
    );
    assert_eq!(
        v.view
            .connection
            .pragma_query_value(None, "temp_store", |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(before, std::fs::read(&p).unwrap());
    assert_eq!(wal_before, std::fs::read(&wal).unwrap());
    let mut files_after: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    files_after.sort();
    assert_eq!(files_before, files_after);
}

#[test]
fn writer_drop_preserves_wal_and_explicit_checkpoint_truncates_it() {
    use sha2::{Digest, Sha256};
    let d = tempdir().unwrap();
    let path = d.path().join("vector.sqlite");
    let wal = d.path().join("vector.sqlite-wal");
    let id = identity(VectorFileKind::Current);
    let mut writer = VectorFileWriter::open(&path, &id).unwrap();
    writer.checkpoint().unwrap();
    writer
        .commit(VectorFileBatch {
            records: vec![row("change", vec![1., 0.])],
            ack: Some(pos(1)),
            ..Default::default()
        })
        .unwrap();
    let db_before = Sha256::digest(std::fs::read(&path).unwrap());
    let wal_before = Sha256::digest(std::fs::read(&wal).unwrap());
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    drop(writer);
    assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), db_before);
    assert_eq!(Sha256::digest(std::fs::read(&wal).unwrap()), wal_before);
    let mut writer = VectorFileWriter::open(&path, &id).unwrap();
    writer.checkpoint().unwrap();
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    assert_ne!(Sha256::digest(std::fs::read(&path).unwrap()), db_before);
    assert!(writer.contains("m", "change", "s").unwrap());
}

#[test]
fn busy_checkpoint_preserves_wal_without_error() {
    let d = tempdir().unwrap();
    let path = d.path().join("vector.sqlite");
    let wal = d.path().join("vector.sqlite-wal");
    let id = identity(VectorFileKind::Current);
    let mut writer = VectorFileWriter::open(&path, &id).unwrap();
    writer
        .commit(VectorFileBatch {
            records: vec![row("first", vec![1., 0.])],
            ..Default::default()
        })
        .unwrap();
    let view = VectorFileReadView::open(&path, &id).unwrap();
    writer
        .commit(VectorFileBatch {
            records: vec![row("second", vec![0., 1.])],
            ..Default::default()
        })
        .unwrap();
    writer.checkpoint().unwrap();
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    drop(view);
    writer.checkpoint().unwrap();
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
}

#[test]
fn query_errors_and_corrupt_stored_vectors_are_distinct() {
    let d = tempdir().unwrap();
    let locked = d.path().join("locked.sqlite");
    let owner = Connection::open(&locked).unwrap();
    owner
        .execute_batch("CREATE TABLE example(x); BEGIN EXCLUSIVE;")
        .unwrap();
    let competitor = Connection::open(&locked).unwrap();
    competitor.busy_timeout(std::time::Duration::ZERO).unwrap();
    let busy = competitor
        .execute("INSERT INTO example VALUES(1)", [])
        .unwrap_err();
    assert!(matches!(stored_value_error(busy), VectorFileError::Sql(_)));
    drop(competitor);
    owner.execute_batch("ROLLBACK").unwrap();
    let path = d.path().join("vector.sqlite");
    let id = identity(VectorFileKind::Current);
    let mut writer = VectorFileWriter::open(&path, &id).unwrap();
    writer
        .commit(VectorFileBatch {
            records: vec![row("change", vec![1., 0.])],
            ..Default::default()
        })
        .unwrap();
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    for query in [vec![0., 0.], vec![f32::NAN, 1.], vec![1.]] {
        let view = VectorFileReadView::open(&path, &id).unwrap();
        assert!(matches!(
            view.search_ann(&query, &scope, 10),
            Err(VectorFileError::Invalid(_))
        ));
        assert!(matches!(
            view.search(&query, &scope, 10),
            Err(VectorFileError::Invalid(_))
        ));
    }
    for (i, vector) in [[0., 0.], [f32::NAN, 0.]].into_iter().enumerate() {
        use std::io::{Read, Seek, SeekFrom, Write};
        let copy = d.path().join(format!("damaged-{i}.sqlite"));
        writer
            .connection
            .execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
            .unwrap();
        let db = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let page: u64 = db
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name='vectors'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let size: usize = db.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        drop(db);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&copy)
            .unwrap();
        let offset = (page - 1) * size as u64;
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut bytes = vec![0; size];
        file.read_exact(&mut bytes).unwrap();
        let original: Vec<u8> = [1.0_f32, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let positions: Vec<_> = bytes
            .windows(original.len())
            .enumerate()
            .filter_map(|(i, b)| (b == original).then_some(i))
            .collect();
        assert_eq!(positions.len(), 1);
        file.seek(SeekFrom::Start(offset + positions[0] as u64))
            .unwrap();
        for value in vector {
            file.write_all(&value.to_le_bytes()).unwrap();
        }
        file.sync_all().unwrap();
        drop(file);
        let view = VectorFileReadView::open(&copy, &id).unwrap();
        assert!(matches!(
            view.search_ann(&[1., 0.], &scope, 10),
            Err(VectorFileError::Corrupt(_))
        ));
        assert!(matches!(
            view.search(&[1., 0.], &scope, 10),
            Err(VectorFileError::Corrupt(_))
        ));
    }
    let other = d.path().join("other-format.sqlite");
    let db = Connection::open(&other).unwrap();
    db.execute("ATTACH DATABASE ?1 AS original", [path.to_str().unwrap()])
        .unwrap();
    db.execute_batch(
        "CREATE TABLE binding_vector_meta AS SELECT * FROM original.binding_vector_meta;
        CREATE TABLE vectors AS SELECT * FROM original.vectors;
        CREATE TABLE vector_buckets AS SELECT * FROM original.vector_buckets;
        CREATE TABLE completed_changes AS SELECT * FROM original.completed_changes;
        CREATE TABLE ann_profile(singleton INTEGER PRIMARY KEY, version INTEGER);
        INSERT INTO ann_profile VALUES(1,999);",
    )
    .unwrap();
    drop(db);
    let view = VectorFileReadView::open(&other, &id).unwrap();
    assert!(matches!(
        view.search_ann(&[1., 0.], &scope, 10),
        Err(VectorFileError::Corrupt(_))
    ));
}
