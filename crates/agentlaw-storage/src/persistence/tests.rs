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
