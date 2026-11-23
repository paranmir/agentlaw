//! Persistent-file audit for ordinary read requests.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
pub struct WriteAudit(BTreeMap<PathBuf, (u64, [u8; 32])>);
impl WriteAudit {
    pub fn capture(root: &Path) -> io::Result<Self> {
        fn visit(
            base: &Path,
            dir: &Path,
            files: &mut BTreeMap<PathBuf, (u64, [u8; 32])>,
        ) -> io::Result<()> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                let metadata = entry.metadata()?;
                if metadata.is_dir() {
                    visit(base, &path, files)?;
                } else if metadata.is_file() && !path.to_string_lossy().ends_with("-shm") {
                    let mut file = fs::File::open(&path)?;
                    let mut digest = Sha256::new();
                    let mut buffer = [0; 65536];
                    loop {
                        let n = file.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        digest.update(&buffer[..n]);
                    }
                    files.insert(
                        path.strip_prefix(base).unwrap().to_path_buf(),
                        (metadata.len(), digest.finalize().into()),
                    );
                }
            }
            Ok(())
        }
        let mut files = BTreeMap::new();
        visit(root, root, &mut files)?;
        Ok(Self(files))
    }
}

/// Checks the production read-openers and proves the opened main databases reject writes.
pub fn assert_read_openers(control: &Path, exact: &Path, lexical: &Path) {
    use rusqlite::{Connection, DatabaseName, OpenFlags};
    let flow_control = include_str!("../../../agentlaw-flows/src/control.rs");
    let flow_exact = include_str!("../../../agentlaw-flows/src/exact.rs");
    let search = include_str!("../../../agentlaw-search/src/lib.rs");
    let source = include_str!("../../../agentlaw-storage/src/published.rs");
    let control_reader = flow_control
        .split("impl ControlReader {")
        .nth(1)
        .unwrap()
        .split("pub fn exact_association")
        .next()
        .unwrap();
    let pin = flow_exact
        .split("pub fn pin(&self)")
        .nth(1)
        .unwrap()
        .split("pub fn ")
        .next()
        .unwrap();
    let read_view = search
        .split("impl ReadView {")
        .nth(1)
        .unwrap()
        .split("pub fn effective")
        .next()
        .unwrap();
    assert!(control_reader.contains("SQLITE_OPEN_READ_ONLY"));
    let ledger = source
        .split("pub fn read_published_paths(")
        .nth(2)
        .unwrap()
        .split("pub fn acquire_current(")
        .next()
        .unwrap();
    assert!(ledger.contains("SQLITE_OPEN_READ_ONLY"));
    assert!(pin.contains("SQLITE_OPEN_READ_ONLY") && pin.contains("temp_store=MEMORY"));
    assert!(read_view.contains("SQLITE_OPEN_READ_ONLY") && read_view.contains("temp_store=MEMORY"));
    for path in [control, exact, lexical] {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(db.is_readonly(DatabaseName::Main).unwrap());
        // SQLite may permit BEGIN IMMEDIATE on a read-only handle, but cannot modify main.
        let _ = db.execute_batch("BEGIN IMMEDIATE");
        assert!(db
            .execute_batch("CREATE TABLE main.agentlaw_read_audit_probe(id INTEGER)")
            .is_err());
        let _ = db.execute_batch("ROLLBACK");
    }
}

#[cfg(test)]
mod tests;
