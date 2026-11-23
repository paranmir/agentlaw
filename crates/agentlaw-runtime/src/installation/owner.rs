//! Ownership of the installation runtime.

use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
};

pub struct OwnerLock(File);

impl OwnerLock {
    pub fn try_acquire(state: &Path) -> io::Result<Option<OwnerLock>> {
        let dir = state.join("service");
        fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(dir.join("owner.lock"))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self(file))),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.raw_os_error() == Some(33) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for OwnerLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn owner_alive(state: &Path) -> io::Result<bool> {
    Ok(OwnerLock::try_acquire(state)?.is_none())
}
