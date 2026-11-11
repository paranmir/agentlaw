//! Ownership of the installation runtime.

use std::{io, path::Path};

pub struct OwnerLock;

impl OwnerLock {
    pub fn try_acquire(_state: &Path) -> io::Result<Option<OwnerLock>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "not implemented",
        ))
    }
}

pub fn owner_alive(_state: &Path) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "not implemented",
    ))
}
