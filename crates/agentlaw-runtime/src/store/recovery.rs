//! Resumes interrupted work when a store opens.

use std::path::PathBuf;

use crate::ports::{RuntimeError, RuntimeResult};
use crate::projection::RetiredFileSink;

/// Removes retired generation files as soon as their last reader releases them.
pub struct RetiredFiles;

impl RetiredFileSink for RetiredFiles {
    fn retire(&self, _binding: &str, files: &[PathBuf]) -> RuntimeResult<()> {
        for file in files {
            for suffix in ["", "-wal", "-shm"] {
                let mut name = file.as_os_str().to_owned();
                name.push(suffix);
                match std::fs::remove_file(PathBuf::from(name)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    #[cfg(windows)]
                    Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => {}
                    Err(error) => {
                        return Err(RuntimeError::Unavailable(format!(
                            "Cannot remove retired projection file: {error}"
                        )))
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
