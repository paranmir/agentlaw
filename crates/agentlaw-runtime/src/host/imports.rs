//! Coordination of import review requests.
use crate::{installation::registry::ConfigSnapshot, protocol::AdminCommand};
use agentlaw_contracts::{DomainError, RequestControl, Result};

pub(super) fn run(
    _snapshot: &ConfigSnapshot,
    _command: AdminCommand,
    _control: &RequestControl,
) -> Result<serde_json::Value> {
    Err(DomainError::new(
        "unavailable",
        "Import coordination is not implemented.",
    ))
}

#[cfg(test)]
type BoundaryHook = std::sync::Arc<dyn Fn(&str, &str) -> Result<()> + Send + Sync>;

// Test seam for the coordinator's real lifetime and publication boundaries.
// The implementation must call the hook after canonical publication, while a
// lifetime guard is held, and on prepare start/completion and staged call entry.
#[cfg(test)]
fn run_observed(
    snapshot: &ConfigSnapshot,
    command: AdminCommand,
    control: &RequestControl,
    _stores: &super::stores::Stores,
    _progress: &crate::git::imports::ProgressRegistry,
    _hook: BoundaryHook,
) -> Result<serde_json::Value> {
    run(snapshot, command, control)
}

#[cfg(test)]
mod tests;
