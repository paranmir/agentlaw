//! Isolated import review execution.
use super::service::StoreService;
use agentlaw_contracts::{DomainError, RequestControl, Result};

pub fn call(
    _service: &StoreService,
    _request: serde_json::Value,
    _control: &RequestControl,
) -> Result<serde_json::Value> {
    Err(DomainError::new(
        "unavailable",
        "Import review is not implemented.",
    ))
}
