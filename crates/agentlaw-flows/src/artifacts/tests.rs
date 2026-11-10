use super::*;
#[test]
fn only_expired_registered_owned_files_are_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let artifact = create_from_reader(tmp.path(), &mut b"{\"value\":1}".as_slice()).unwrap();
    assert!(artifact.expires_at_unix_seconds > now().unwrap());
    let unregistered = tmp.path().join("user-data.json");
    std::fs::write(&unregistered, "preserve").unwrap();
    let connection = registry(tmp.path()).unwrap();
    connection
        .execute("UPDATE response_artifacts SET expires=0,next_attempt=0", [])
        .unwrap();
    let newer = create_from_reader(tmp.path(), &mut b"{}".as_slice()).unwrap();
    assert_eq!(newer.expired_artifacts_removed, 1);
    assert!(!artifact.path.exists());
    assert!(unregistered.exists());
    assert!(newer.path.exists());
}
#[test]
fn registered_traversal_cannot_delete_a_user_file() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside.json");
    std::fs::write(&outside, "preserve").unwrap();
    let directory = tmp.path().join("responses");
    std::fs::create_dir(&directory).unwrap();
    let connection = registry(&directory).unwrap();
    connection
        .execute(
            "INSERT INTO response_artifacts VALUES('../outside',0,0)",
            [],
        )
        .unwrap();
    let artifact = create_from_reader(&directory, &mut b"{}".as_slice()).unwrap();
    assert_eq!(artifact.expired_artifacts_removed, 0);
    assert!(outside.exists());
}

#[test]
fn read_artifact_cleanup_respects_retention_and_owned_names() {
    let tmp = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4();
    let complete = tmp.path().join(format!("response-{id}.json"));
    let temporary = tmp.path().join(format!("response-{id}.tmp"));
    let unrelated = tmp.path().join("response-not-owned.json");
    std::fs::write(&complete, "{}").unwrap();
    std::fs::write(&temporary, "partial").unwrap();
    std::fs::write(&unrelated, "user data").unwrap();
    assert_eq!(
        cleanup_read_artifacts(tmp.path(), std::time::SystemTime::now()).unwrap(),
        0
    );
    let future =
        std::time::SystemTime::now() + std::time::Duration::from_secs(RETENTION_SECONDS + 60);
    assert_eq!(cleanup_read_artifacts(tmp.path(), future).unwrap(), 2);
    assert!(!complete.exists());
    assert!(!temporary.exists());
    assert!(unrelated.exists());
}

#[test]
fn read_artifact_expiry_uses_the_same_file_time_as_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = ReadArtifactWriter::create(dir.path()).unwrap();
    writer.write_all(b"{}").unwrap();
    writer.flush().unwrap();
    let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    writer
        .writer
        .as_ref()
        .unwrap()
        .get_ref()
        .set_modified(earlier)
        .unwrap();
    let artifact = writer.finish().unwrap();
    let modified = std::fs::metadata(&artifact.path)
        .unwrap()
        .modified()
        .unwrap();
    let expected = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + RETENTION_SECONDS;
    assert_eq!(artifact.expires_at_unix_seconds, expected);
    let before = std::time::UNIX_EPOCH + std::time::Duration::from_secs(expected - 1);
    assert_eq!(cleanup_read_artifacts(dir.path(), before).unwrap(), 0);
    assert!(artifact.path.exists());
    assert_eq!(
        cleanup_read_artifacts(
            dir.path(),
            modified + std::time::Duration::from_secs(RETENTION_SECONDS)
        )
        .unwrap(),
        1
    );
}
