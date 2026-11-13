//! Administrative export operations.

use crate::protocol::InventoryFilter;
use agentlaw_contracts::{DomainError, RequestControl};
use std::path::Path;

pub fn history_export(
    _source: &Path,
    _memory_id: &str,
    _output: &Path,
    _control: &RequestControl,
) -> Result<serde_json::Value, DomainError> {
    Err(DomainError::new(
        "unavailable",
        "History export is not implemented.",
    ))
}
pub fn procedure_list(
    _source: &Path,
    _local: &Path,
    _filter: &InventoryFilter,
    _output: &Path,
    _control: &RequestControl,
) -> Result<serde_json::Value, DomainError> {
    Err(DomainError::new(
        "unavailable",
        "Procedure list is not implemented.",
    ))
}
pub fn procedure_search(
    _source: &Path,
    _query: &str,
    _limit: Option<usize>,
    _filter: &InventoryFilter,
    _output: &Path,
    _control: &RequestControl,
) -> Result<serde_json::Value, DomainError> {
    Err(DomainError::new(
        "unavailable",
        "Procedure search is not implemented.",
    ))
}

#[cfg(test)]
mod tests;
