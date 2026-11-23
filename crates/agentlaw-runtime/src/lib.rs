//! The per-installation owner of store work, derived indexes and the model child.
pub mod client;
pub mod git;
// git_scan.rs hashes its own bytes as the scanner identity, so it keeps this path.
pub(crate) use git::git_ops;
pub mod host;
pub mod installation;
pub mod model;
pub mod ports;
pub mod projection;
pub mod protocol;
pub mod semantic;
pub mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod transport;
