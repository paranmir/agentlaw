use super::install_temp_file;
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    sync::mpsc,
    thread,
    time::Duration,
};
use tempfile::NamedTempFile;

fn assert_replacement_while_reader_is_open(
    replace: impl FnOnce(NamedTempFile, &Path) -> io::Result<()>,
) {
    const OLD: &[u8] = b"old selection\x00\nlast original byte!";
    const NEW: &[u8] = b"new selection\n";
    const TIMEOUT: Duration = Duration::from_secs(10);

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("selection");
    fs::write(&target, OLD).unwrap();
    let mut prepared = NamedTempFile::new_in(dir.path()).unwrap();
    prepared.write_all(NEW).unwrap();
    prepared.as_file().sync_all().unwrap();
    let temp_path = prepared.path().to_owned();

    let (opened_tx, opened_rx) = mpsc::sync_channel(1);
    let (replaced_tx, replaced_rx) = mpsc::sync_channel(1);
    let (result, old_bytes) = thread::scope(|scope| {
        let target = &target;
        let reader = scope.spawn(move || {
            let mut file = File::open(target).expect("open the original target for reading");
            opened_tx.try_send(()).unwrap();
            replaced_rx
                .recv_timeout(TIMEOUT)
                .expect("replacement attempt did not finish");
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            bytes
        });

        opened_rx
            .recv_timeout(TIMEOUT)
            .expect("reader did not open the target");
        let result = replace(prepared, target);
        replaced_tx.try_send(()).unwrap();
        (result, reader.join().unwrap())
    });

    assert!(
        result.is_ok(),
        "one replacement attempt must succeed: {result:?}"
    );
    assert_eq!(fs::read(&target).unwrap(), NEW);
    assert_eq!(old_bytes, OLD);
    assert!(!temp_path.try_exists().unwrap(), "temporary path remains");
}

#[test]
fn install_temp_file_replaces_target_held_open_by_reader() {
    assert_replacement_while_reader_is_open(install_temp_file);
}

#[cfg(windows)]
fn install_with_prepared_file_held_open(
    prepared: NamedTempFile,
    target: &Path,
    hold_for: Duration,
) -> (io::Result<()>, Duration) {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::Instant;

    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;

    let path = prepared.path().to_owned();
    let (opened_tx, opened_rx) = mpsc::sync_channel(1);
    thread::scope(|scope| {
        scope.spawn(move || {
            let holder = fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(path)
                .expect("open the prepared file without delete sharing");
            opened_tx.try_send(()).unwrap();
            thread::sleep(hold_for);
            drop(holder);
        });

        opened_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("holder did not open the prepared file");
        let start = Instant::now();
        let result = install_temp_file(prepared, target);
        // Record the return time before the scope joins the holder.
        (result, start.elapsed())
    })
}

#[cfg(windows)]
#[test]
fn install_temp_file_retries_until_prepared_file_is_released() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("selection");
    fs::write(&target, b"old selection").unwrap();
    let mut prepared = NamedTempFile::new_in(dir.path()).unwrap();
    prepared.write_all(b"new selection").unwrap();
    prepared.as_file().sync_all().unwrap();
    let path = prepared.path().to_owned();

    let (result, _) =
        install_with_prepared_file_held_open(prepared, &target, Duration::from_millis(300));

    result.expect("replacement should succeed in the original call after the holder closes");
    assert_eq!(fs::read(&target).unwrap(), b"new selection");
    assert!(!path.try_exists().unwrap(), "temporary path remains");
}

#[cfg(windows)]
#[test]
fn install_temp_file_returns_last_lock_error_at_retry_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("selection");
    fs::write(&target, b"old selection").unwrap();
    let mut prepared = NamedTempFile::new_in(dir.path()).unwrap();
    prepared.write_all(b"new selection").unwrap();
    prepared.as_file().sync_all().unwrap();

    let (result, elapsed) =
        install_with_prepared_file_held_open(prepared, &target, Duration::from_millis(1_300));

    let error = result.expect_err("replacement should expire while the prepared file is held");
    assert!(
        matches!(error.raw_os_error(), Some(5 | 32 | 33)),
        "expected the last Windows lock error, got {error:?}"
    );
    assert!(
        (Duration::from_secs(1)..Duration::from_millis(1_500)).contains(&elapsed),
        "replacement returned after {elapsed:?}, expected 1 second to less than 1.5 seconds"
    );
    assert_eq!(fs::read(&target).unwrap(), b"old selection");
}

#[test]
fn install_temp_file_returns_missing_target_directory_without_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("missing").join("selection");
    let mut prepared = NamedTempFile::new_in(dir.path()).unwrap();
    prepared.write_all(b"new selection").unwrap();
    prepared.as_file().sync_all().unwrap();

    let start = std::time::Instant::now();
    let result = install_temp_file(prepared, &target);
    let elapsed = start.elapsed();

    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    assert!(
        elapsed < Duration::from_millis(200),
        "missing destination directory was reported after {elapsed:?}"
    );
}
