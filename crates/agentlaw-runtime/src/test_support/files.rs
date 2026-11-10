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
mod tests;
