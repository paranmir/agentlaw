use super::*;

#[test]
fn checkpoint_mismatch_is_corrupt_and_reader_failure_is_io() {
    let temp = tempfile::tempdir().unwrap();
    let policy = ScratchPolicy::Bounded {
        memory: 0,
        directory: temp.path().to_owned(),
    };
    let delta = br#"{"codec":"utf8-splice-v1","edits":[{"start_byte":"0","delete_bytes":"0","insert_utf8":"abc"}]}"#;
    assert!(matches!(
        verify(&delta[..], &b""[..], &b"abd"[..], &policy),
        Err(Error::Corrupt(_))
    ));
    struct Fail;
    impl Read for Fail {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "reader failure",
            ))
        }
    }
    assert!(matches!(
        verify(&delta[..], &b""[..], Fail, &policy),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
    ));
}

#[test]
fn scratch_spills_only_after_budget_and_preserves_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("history-spools");
    let policy = ScratchPolicy::Bounded {
        memory: 3,
        directory: dir.clone(),
    };
    let mut scratch = Scratch::new(&policy).unwrap();
    scratch.write_all(b"abc").unwrap();
    assert!(!dir.exists());
    scratch.write_all(b"def").unwrap();
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    scratch.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    scratch.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"abcdef");
    scratch.finish().unwrap();
    assert_eq!(fs::read_dir(dir).unwrap().count(), 0);
}

#[test]
fn scratch_removes_open_file_on_unwind() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("history-spools");
    let policy = ScratchPolicy::Bounded {
        memory: 0,
        directory: dir.clone(),
    };
    let result = std::panic::catch_unwind(|| {
        let mut scratch = Scratch::new(&policy).unwrap();
        scratch.write_all(b"x").unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        panic!("exercise unwind");
    });
    assert!(result.is_err());
    assert_eq!(fs::read_dir(dir).unwrap().count(), 0);
}
