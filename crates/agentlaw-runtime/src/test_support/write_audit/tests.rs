use super::*;
#[test]
fn audit_detects_committed_database_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("audit.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE records(value TEXT);")
        .unwrap();
    let before = WriteAudit::capture(tmp.path()).unwrap();
    db.execute("INSERT INTO records VALUES ('observed')", [])
        .unwrap();
    assert_ne!(WriteAudit::capture(tmp.path()).unwrap(), before);
}
