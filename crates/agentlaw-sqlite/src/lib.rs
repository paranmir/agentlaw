//! SQLite connection opening shared by the storage, search, flows and runtime owners.
use rusqlite::{Connection, OpenFlags, Result};
use std::path::Path;
#[cfg(feature = "write-audit")]
pub mod audit;

pub const fn audit_enabled() -> bool {
    cfg!(feature = "write-audit")
}

fn opened(connection: Connection) -> Connection {
    #[cfg(feature = "write-audit")]
    audit::install(&connection);
    connection
}
pub fn open(path: impl AsRef<Path>) -> Result<Connection> {
    Connection::open(path).map(opened)
}
pub fn open_with_flags(path: impl AsRef<Path>, flags: OpenFlags) -> Result<Connection> {
    Connection::open_with_flags(path, flags).map(opened)
}
pub fn open_in_memory() -> Result<Connection> {
    Connection::open_in_memory().map(opened)
}
