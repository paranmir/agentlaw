//! Git commit, import and sync work; store changes go through the store handle.
pub mod git_ops;
pub mod git_scan;
pub mod sync;

#[cfg(test)]
mod tests;
