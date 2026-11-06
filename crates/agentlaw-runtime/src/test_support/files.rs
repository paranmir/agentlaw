use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSnapshot {
    len: u64,
    modified: SystemTime,
    hash: Option<[u8; 32]>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
/// Compares two moments only. A file created and removed between the captures is not seen, so an empty result is not proof that nothing was written.
pub struct DirectorySnapshot {
    files: BTreeMap<PathBuf, FileSnapshot>,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectoryChanges {
    pub added: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub inconclusive: Vec<PathBuf>,
}
impl DirectoryChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.modified.is_empty()
            && self.inconclusive.is_empty()
    }
}
impl DirectorySnapshot {
    pub fn capture(root: &Path) -> io::Result<Self> {
        fn walk(
            root: &Path,
            dir: &Path,
            files: &mut BTreeMap<PathBuf, FileSnapshot>,
        ) -> io::Result<()> {
            let entries = match fs::read_dir(dir) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                other => other?,
            };
            for entry in entries {
                let entry = match entry {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    other => other?,
                };
                let path = entry.path();
                let metadata = match fs::symlink_metadata(&path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    other => other?,
                };
                if metadata.is_dir() {
                    walk(root, &path, files)?;
                } else if metadata.is_file() {
                    let hash = match fs::File::open(&path) {
                        Ok(mut file) => {
                            let mut h = Sha256::new();
                            let mut buf = [0; 8192];
                            loop {
                                match file.read(&mut buf) {
                                    Ok(0) => break Some(h.finalize().into()),
                                    Ok(n) => h.update(&buf[..n]),
                                    Err(e) if e.kind() == io::ErrorKind::NotFound => break None,
                                    Err(e) => return Err(e),
                                }
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        Err(_) => None,
                    };
                    let modified = match metadata.modified() {
                        Ok(t) => t,
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(e),
                    };
                    files.insert(
                        path.strip_prefix(root).expect("descendant").to_path_buf(),
                        FileSnapshot {
                            len: metadata.len(),
                            modified,
                            hash,
                        },
                    );
                }
            }
            Ok(())
        }
        let mut files = BTreeMap::new();
        walk(root, root, &mut files)?;
        Ok(Self { files })
    }
    pub fn changes_since(&self, earlier: &Self) -> DirectoryChanges {
        let mut changes = DirectoryChanges::default();
        for (path, value) in &self.files {
            match earlier.files.get(path) {
                None => changes.added.push(path.clone()),
                Some(old) if old.len != value.len || old.modified != value.modified => {
                    changes.modified.push(path.clone())
                }
                Some(old) if old.hash.is_none() || value.hash.is_none() => {
                    changes.inconclusive.push(path.clone())
                }
                Some(old) if old.hash != value.hash => changes.modified.push(path.clone()),
                _ => {}
            }
        }
        for path in earlier.files.keys() {
            if !self.files.contains_key(path) {
                changes.removed.push(path.clone());
            }
        }
        changes
    }
}

#[cfg(test)]
mod tests {
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
}
