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
fn reader_matches_runtime_association() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&root).unwrap();
    let runtime = Runtime::open(&root, &local, "user").unwrap();
    runtime
        .control
        .execute(
            "INSERT INTO associations VALUES(?1,'path','project')",
            [&runtime.binding_id],
        )
        .unwrap();
    let reader = ControlReader::open(&local).unwrap();
    let expected = ContextRepository::exact_association(&runtime, "path")
        .unwrap()
        .unwrap();
    let actual = reader
        .exact_association(&runtime.binding_id, "path")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(expected).unwrap(),
        serde_json::to_value(actual).unwrap()
    );
}

#[test]
fn reader_matches_runtime_pending_and_authoring_results() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&root).unwrap();
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let context = ContextRepository::request_context(&runtime, None).unwrap();
    let batch = crate::write::PendingBatch::new(&context, vec![]);
    let reference = batch.reference.clone();
    let retained = Retained {
        diagnostics: vec![],
        batch,
        context,
        issues: vec![],
        basis: String::new(),
        resolution_ref: None,
        review_refs: vec![],
        approved_review_refs: vec![],
        overlap_approved: false,
        operation_id: None,
        mutations: vec![],
        expected_generation: None,
        publication_basis: None,
    };
    runtime.save_pending(&retained, None).unwrap();
    runtime.control.execute("INSERT INTO authoring_executions VALUES(?1,?2,'op','1','{}','submission','published','result')", params!["token", runtime.binding_id]).unwrap();
    let reader = ControlReader::open(&local).unwrap();
    let expected = runtime
        .remember(
            serde_json::from_value(
                json!({"pending_batch_ref": reference, "pending_action": "inspect"}),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        reader
            .pending_inspect(&runtime.binding_id, &reference)
            .unwrap(),
        expected
    );
    assert_eq!(
        reader
            .authoring_result(&runtime.binding_id, "token")
            .unwrap(),
        authoring_result(&runtime.control, &runtime.binding_id, "token").unwrap()
    );
}

#[test]
fn reader_pending_notices_match_runtime_recall() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&root).unwrap();
    let runtime = Runtime::open(&root, &local, "user").unwrap();
    for (id, binding, project, status) in [
        (
            "b",
            runtime.binding_id.as_str(),
            Some("project-a"),
            "pending",
        ),
        ("a", runtime.binding_id.as_str(), None, "pending"),
        (
            "c",
            runtime.binding_id.as_str(),
            Some("project-b"),
            "pending",
        ),
        ("d", runtime.binding_id.as_str(), None, "discarded"),
        ("e", "other-binding", None, "pending"),
    ] {
        runtime.control.execute(
            "INSERT INTO pending(id,version,binding,payload,status) VALUES(?1,'version',?2,?3,?4)",
            params![id, binding, json!({"context":{"project_id":project}}).to_string(), status],
        ).unwrap();
    }
    let reader = ControlReader::open(&local).unwrap();
    for (project, expected_ids) in [
        (None, vec!["a"]),
        (Some("project-a"), vec!["a", "b"]),
        (Some("project-b"), vec!["a", "c"]),
    ] {
        let context =
            ContextRepository::request_context(&runtime, project.map(str::to_owned)).unwrap();
        let response = runtime
            .recall_with_context(RecallRequest::default(), context)
            .unwrap();
        let actual = reader
            .pending_notices(&runtime.binding_id, project)
            .unwrap();
        assert_eq!(json!(actual), response["pending_batches"]);
        let ids: Vec<_> = actual
            .iter()
            .map(|v| v["pending_batch_ref"]["pending_batch_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, expected_ids);
    }
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
