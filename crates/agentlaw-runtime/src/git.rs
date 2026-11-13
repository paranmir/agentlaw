//! Git commit, import and sync work; store changes go through the store handle.
pub mod control;
pub use control::SyncControl;
pub mod git_ops;
pub mod git_scan;
pub mod handle;
pub mod imports;
pub mod sync;

#[cfg(test)]
mod tests;
