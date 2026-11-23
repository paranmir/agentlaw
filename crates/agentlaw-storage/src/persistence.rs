//! Platform persistence boundary. File flush != verified power-loss durability.
use crate::{Durability, Result};
#[cfg(unix)]
use std::fs::File;
use std::{
    fs::{self, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
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

/// Install a prepared file, retrying transient Windows sharing errors within one deadline.
pub fn install_temp_file(temp: tempfile::NamedTempFile, target: &Path) -> io::Result<()> {
    const RETRY_TIMEOUT: Duration = Duration::from_secs(1);
    const RETRY_INTERVAL: Duration = Duration::from_millis(50);

    enum StagedFile {
        Temporary(tempfile::TempPath),
        Kept(PathBuf),
    }

    // Close the handle before keep updates attributes at the temporary name.
    let mut staged = StagedFile::Temporary(temp.into_temp_path());
    let deadline = Instant::now() + RETRY_TIMEOUT;
    loop {
        let kept = match staged {
            StagedFile::Temporary(path) => path.keep(),
            StagedFile::Kept(path) => Ok(path),
        };
        let error = match kept {
            Ok(path) => {
                let result = PlatformPersistence.install_replace(&path, target);
                staged = StagedFile::Kept(path);
                match result {
                    Ok(()) => return Ok(()),
                    Err(crate::Error::Io(error)) => error,
                    Err(error) => io::Error::other(error),
                }
            }
            Err(error) => {
                staged = StagedFile::Temporary(error.path);
                error.error
            }
        };
        if is_transient_replace_error(&error) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                std::thread::sleep(RETRY_INTERVAL.min(remaining));
                if Instant::now() < deadline {
                    continue;
                }
            }
        }
        if let StagedFile::Kept(path) = staged {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
}

pub fn is_transient_replace_error(error: &io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(test)]
mod tests;
