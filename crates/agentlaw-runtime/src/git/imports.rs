//! In-memory progress and persisted import inventory.
use agentlaw_contracts::{DomainError, Result};
use std::{path::Path, time::Duration};

#[derive(Clone, Default)]
pub struct ProgressRegistry;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportStatus {
    InProgress,
    Completed,
    HandoffPending,
    ReviewWaiting,
    Incomplete,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEntry {
    pub import_ref: String,
    pub status: ImportStatus,
}
impl ProgressRegistry {
    pub fn update(
        &self,
        _import_ref: &str,
        _phase: &str,
        _completed: u64,
        _unit: &str,
        _elapsed: Duration,
    ) -> Result<()> {
        Err(DomainError::new(
            "unavailable",
            "Import progress is not implemented.",
        ))
    }
    pub fn finish(&self, _import_ref: &str) -> Result<()> {
        Err(DomainError::new(
            "unavailable",
            "Import progress is not implemented.",
        ))
    }
}
pub fn list(_local: &Path, _progress: &ProgressRegistry) -> Result<Vec<ImportEntry>> {
    Err(DomainError::new(
        "unavailable",
        "Import list is not implemented.",
    ))
}
#[cfg(test)]
mod tests;
