#[test]
fn feature_controls_hooks_and_counts_rolled_back_write_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("audit.sqlite");
    let db = agentlaw_sqlite::open(&path).unwrap();
    #[cfg(not(feature = "write-audit"))]
    {
        assert!(!agentlaw_sqlite::audit_enabled());
        unsafe {
            assert!(
                rusqlite::ffi::sqlite3_commit_hook(db.handle(), None, std::ptr::null_mut())
                    .is_null()
            );
            assert!(
                rusqlite::ffi::sqlite3_rollback_hook(db.handle(), None, std::ptr::null_mut())
                    .is_null()
            );
        }
    }
    #[cfg(feature = "write-audit")]
    {
        assert!(agentlaw_sqlite::audit_enabled());
        let name = db.path().unwrap();
        let before = agentlaw_sqlite::audit::snapshot(name);
        db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
        let after = agentlaw_sqlite::audit::snapshot(name);
        assert_eq!(after.writes - before.writes, 1);
        assert_eq!(after.rollbacks - before.rollbacks, 1);
        assert_eq!(after.commits - before.commits, 0);
    }
    #[cfg(not(feature = "write-audit"))]
    db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
}
