//! Binding-scoped audit and repair coordination.
use super::stores::Stores;
use crate::{
    installation::registry::ConfigSnapshot,
    protocol::{AdminCommand, RejectReason},
};
use agentlaw_contracts::{DomainError, RequestControl};
use serde_json::Value;
use std::sync::Arc;

fn domain(reason: RejectReason) -> DomainError {
    match reason {
        RejectReason::InvalidBody(error) => error,
        other => DomainError::new("store_unavailable", format!("{other:?}")),
    }
}
pub(super) fn run(
    stores: &Stores,
    snapshot: &Arc<ConfigSnapshot>,
    request: AdminCommand,
    control: &RequestControl,
) -> Result<Value, DomainError> {
    match request {
        AdminCommand::Audit => {
            let _guard = stores.audit_guard(snapshot, control).map_err(domain)?;
            crate::store::maintenance::audit(
                stores.installation(),
                snapshot,
                control,
                stores.indexing_status(snapshot),
            )
        }
        AdminCommand::Repair => {
            let mut guard = stores.maintain(snapshot, control).map_err(domain)?;
            crate::store::maintenance::repair(stores.installation(), snapshot, control, || {
                guard.reopen(control).map_err(domain)?;
                guard
                    .service()
                    .ok_or_else(|| DomainError::new("store_unavailable", "Store did not reopen."))?
                    .repair_derived(control)
            })
        }
        _ => {
            return Err(DomainError::new(
                "invalid_input",
                "Not a maintenance operation.",
            ))
        }
    }
}
