use crate::{ScopeFilter, SearchDocument, SearchIndex};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("agentlaw-existing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn schema(path: &Path) -> Vec<(String, String, String, Option<String>)> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut query = db
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")
        .unwrap();
    query
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn temporary_file_index_limits_cache_and_uses_file_temporary_store() {
    let directory = Fixture::new();
    let index = SearchIndex::open_temporary(directory.path().join("temporary.sqlite")).unwrap();
    assert_eq!(
        index
            .connection
            .query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        -2048
    );
    assert_eq!(
        index
            .connection
            .query_row("PRAGMA temp_store", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        index
            .connection
            .query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let view = index.read_view().unwrap();
    assert_eq!(
        view.connection
            .query_row("PRAGMA temp_store", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn existing_open_rejects_missing_schema_and_metadata_without_repair() {
    for (table, delete_row) in [
        ("docs", false),
        ("metadata", false),
        ("metadata", true),
        ("analyzer", true),
    ] {
        let dir = Fixture::new();
        let path = dir.path().join("lexical.sqlite");
        drop(SearchIndex::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        db.execute_batch(&if delete_row {
            format!("DELETE FROM {table}")
        } else {
            format!("DROP TABLE {table}")
        })
        .unwrap();
        drop(db);
        let before = schema(&path);
        assert!(
            SearchIndex::open_existing(&path).is_err(),
            "{table}, delete_row={delete_row}"
        );
        assert_eq!(schema(&path), before);
        let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, i64::from(delete_row));
        if delete_row {
            let rows: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(rows, 0, "{table} metadata must not be recreated");
        }
    }
}

#[test]
fn existing_open_rejects_absent_file_and_supports_valid_incremental_write() {
    let dir = Fixture::new();
    let path = dir.path().join("lexical.sqlite");
    assert!(SearchIndex::open_existing(&path).is_err());
    assert!(!path.exists());
    drop(SearchIndex::open(&path).unwrap());
    let mut index = SearchIndex::open_existing(&path).unwrap();
    index
        .commit(
            1,
            &[SearchDocument {
                memory_id: "id".into(),
                change_id: "version".into(),
                body: "existing projection".into(),
                scope: "user".into(),
            }],
            &[],
        )
        .unwrap();
    let mut view = index.read_view().unwrap();
    assert_eq!(view.stamp.ack, 1);
    assert_eq!(
        view.lexical_matched(
            "projection",
            &ScopeFilter {
                allowed_scopes: vec!["user".into()]
            }
        )
        .unwrap(),
        1
    );
}
