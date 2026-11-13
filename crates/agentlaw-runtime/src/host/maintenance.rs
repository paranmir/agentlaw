//! Exclusive and read-only management operations for a binding.

use super::stores::Stores;
use crate::installation::registry::ConfigSnapshot;
use agentlaw_contracts::{DomainError, RequestControl};
use std::sync::Arc;

pub(super) fn audit(
    _stores: &Stores,
    _binding: &Arc<ConfigSnapshot>,
    _control: &RequestControl,
) -> Result<serde_json::Value, DomainError> {
    Err(DomainError::new("unavailable", "Audit is not implemented."))
}

pub(super) fn repair(
    _stores: &Stores,
    _binding: &Arc<ConfigSnapshot>,
    _control: &RequestControl,
) -> Result<serde_json::Value, DomainError> {
    Err(DomainError::new(
        "unavailable",
        "Repair is not implemented.",
    ))
}
