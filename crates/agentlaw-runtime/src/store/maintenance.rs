//! Store audit and repair operations.

use agentlaw_contracts::{DomainError, RequestControl};
use std::path::Path;

pub fn audit(_source: &Path, _control: &RequestControl) -> Result<serde_json::Value, DomainError> {
    Err(DomainError::new("unavailable", "Audit is not implemented."))
}

pub fn repair(
    _source: &Path,
    _control: &RequestControl,
    _reopen: impl FnOnce() -> Result<(), DomainError>,
) -> Result<serde_json::Value, DomainError> {
    Err(DomainError::new(
        "unavailable",
        "Repair is not implemented.",
    ))
}
