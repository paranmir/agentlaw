use super::*;

#[test]
fn removes_retired_files_and_sqlite_companions() {
    let dir = tempfile::tempdir().unwrap();
    let exact = dir.path().join("exact.sqlite");
    let lexical = dir.path().join("lexical.sqlite");
    for file in [&exact, &lexical] {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = file.as_os_str().to_owned();
            name.push(suffix);
            std::fs::write(name, b"retired").unwrap();
        }
    }
    RetiredFiles
        .retire("binding", &[exact.clone(), lexical.clone()])
        .unwrap();
    RetiredFiles.retire("binding", &[exact, lexical]).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[cfg(windows)]
#[test]
fn locked_retired_file_can_be_retried() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("exact.sqlite");
    std::fs::write(&file, b"retired").unwrap();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&file)
        .unwrap();
    RetiredFiles
        .retire("binding", std::slice::from_ref(&file))
        .unwrap();
    assert!(file.exists());
    drop(locked);
    RetiredFiles
        .retire("binding", std::slice::from_ref(&file))
        .unwrap();
    assert!(!file.exists());
}
