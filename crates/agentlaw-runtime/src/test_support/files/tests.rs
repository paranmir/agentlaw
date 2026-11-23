use super::*;

#[cfg(windows)]
#[test]
fn unreadable_content_is_inconclusive_and_readable_rewrite_is_modified() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    fs::write(&path, b"abc").unwrap();
    let time = fs::metadata(&path).unwrap().modified().unwrap();
    let before = DirectorySnapshot::capture(dir.path()).unwrap();
    let mut lock = fs::OpenOptions::new()
        .write(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    use std::io::Write;
    lock.write_all(b"xyz").unwrap();
    lock.set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
    let locked = DirectorySnapshot::capture(dir.path()).unwrap();
    let changes = locked.changes_since(&before);
    assert_eq!(changes.inconclusive, vec![PathBuf::from("file")]);
    assert!(changes.modified.is_empty());
    assert!(!changes.is_empty());
    drop(lock);
    let readable = DirectorySnapshot::capture(dir.path()).unwrap();
    assert_eq!(
        readable.changes_since(&before).modified,
        vec![PathBuf::from("file")]
    );
}

#[test]
fn snapshots_report_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let before = DirectorySnapshot::capture(dir.path()).unwrap();
    assert_eq!(before.changes_since(&before), DirectoryChanges::default());
    let file = dir.path().join("file");
    fs::write(&file, b"abc").unwrap();
    let added = DirectorySnapshot::capture(dir.path()).unwrap();
    assert_eq!(
        added.changes_since(&before).added,
        vec![PathBuf::from("file")]
    );
    fs::write(&file, b"xyz").unwrap();
    let changed = DirectorySnapshot::capture(dir.path()).unwrap();
    assert_eq!(
        changed.changes_since(&added).modified,
        vec![PathBuf::from("file")]
    );
    fs::remove_file(&file).unwrap();
    let removed = DirectorySnapshot::capture(dir.path()).unwrap();
    assert_eq!(
        removed.changes_since(&changed).removed,
        vec![PathBuf::from("file")]
    );
}

#[cfg(windows)]
#[test]
fn snapshot_tolerates_exclusive_file() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("locked");
    fs::write(&file, b"locked").unwrap();
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(file)
        .unwrap();
    DirectorySnapshot::capture(dir.path()).unwrap();
}
