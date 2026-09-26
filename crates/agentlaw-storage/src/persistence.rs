//! Platform persistence boundary. File flush != verified power-loss durability.
use crate::{Durability, Result};
#[cfg(unix)]
use std::fs::File;
use std::{
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
};
pub trait Persistence {
    fn durability(&self) -> Durability;
    fn write_all_at(&self, path: &Path, offset: u64, bytes: &[u8]) -> Result<()>;
    fn sync_file(&self, path: &Path) -> Result<()>;
    fn install_replace(&self, temp: &Path, target: &Path) -> Result<()>;
    fn sync_namespace(&self, parent: &Path) -> Result<Durability>;
}
pub struct PlatformPersistence;
impl Persistence for PlatformPersistence {
    fn durability(&self) -> Durability {
        Durability::FileSyncedProcessCrashProtocolPowerLossUnverified
    }
    fn write_all_at(&self, path: &Path, offset: u64, bytes: &[u8]) -> Result<()> {
        offset
            .checked_add(u64::try_from(bytes.len()).map_err(|_| crate::Error::Capacity)?)
            .ok_or(crate::Error::Capacity)?;
        let mut file = OpenOptions::new().write(true).open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)?;
        Ok(())
    }
    fn sync_file(&self, path: &Path) -> Result<()> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?
            .sync_all()?;
        Ok(())
    }
    fn install_replace(&self, temp: &Path, target: &Path) -> Result<()> {
        fs::rename(temp, target)?;
        Ok(())
    }
    fn sync_namespace(&self, parent: &Path) -> Result<Durability> {
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = parent;
        // Windows directory-entry power-loss persistence has not been established.
        Ok(self.durability())
    }
}
