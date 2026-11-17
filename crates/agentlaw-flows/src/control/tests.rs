use super::*;

#[test]
fn reader_is_read_only_and_does_not_create_database() {
    let dir = tempfile::tempdir().unwrap();
    assert!(ControlReader::open(dir.path()).is_err());
    assert!(!dir.path().join("control.sqlite").exists());
    let writer = ControlWriter::open(dir.path()).unwrap();
    writer
        .connection
        .execute(
            "INSERT INTO associations VALUES('binding','path','project')",
            [],
        )
        .unwrap();
    let files = || {
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let names_before = files();
    let db_path = dir.path().join("control.sqlite");
    let wal_path = dir.path().join("control.sqlite-wal");
    let db_before = std::fs::read(&db_path).unwrap();
    let wal_before = std::fs::read(&wal_path).unwrap();
    {
        let reader = ControlReader::open(dir.path()).unwrap();
        assert_eq!(
            reader
                .exact_association("binding", "path")
                .unwrap()
                .unwrap()
                .project_id,
            "project"
        );
        assert!(reader
            .connection
            .execute("INSERT INTO settings VALUES('test','value')", [])
            .is_err());
        assert_eq!(names_before, files());
        assert_eq!(db_before, std::fs::read(&db_path).unwrap());
        assert_eq!(wal_before, std::fs::read(&wal_path).unwrap());
        // SQLite read marks in -shm are excluded from the write test.
    }
    assert_eq!(names_before, files());
    assert_eq!(db_before, std::fs::read(db_path).unwrap());
    assert_eq!(wal_before, std::fs::read(wal_path).unwrap());
}

#[test]
fn reader_snapshot_is_stable_while_writer_commits() {
    let dir = tempfile::tempdir().unwrap();
    let writer = ControlWriter::open(dir.path()).unwrap();
    writer
        .connection
        .execute(
            "INSERT INTO associations VALUES('binding','path','old')",
            [],
        )
        .unwrap();
    let reader = ControlReader::open(dir.path()).unwrap();
    assert_eq!(
        reader
            .exact_association("binding", "path")
            .unwrap()
            .unwrap()
            .project_id,
        "old"
    );
    writer
        .connection
        .execute("UPDATE associations SET project='new'", [])
        .unwrap();
    assert_eq!(
        reader
            .exact_association("binding", "path")
            .unwrap()
            .unwrap()
            .project_id,
        "old"
    );
    let fresh = ControlReader::open(dir.path()).unwrap();
    assert_eq!(
        fresh
            .exact_association("binding", "path")
            .unwrap()
            .unwrap()
            .project_id,
        "new"
    );
}

#[test]
fn writer_is_send_and_not_sync() {
    fn send<T: Send>() {}
    send::<ControlWriter>();
    trait AmbiguousIfSync<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
    let _ = <ControlWriter as AmbiguousIfSync<_>>::check;
}
