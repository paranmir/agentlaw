//! RAM-only isolation for identities known to be corrupt.
use super::{state::LaneKey, Shared};
use crate::ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus};
use agentlaw_search::vector_file::{VectorFileError, VectorFileIdentity, VectorFileKind};
use std::time::SystemTime;

fn corrupt_sql(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase))
        || matches!(error, rusqlite::Error::SqliteFailure(_, Some(message)) if message.contains("no such table") || message.contains("no such column") || message.contains("malformed"))
}

pub(super) fn is_corrupt(error: &VectorFileError) -> bool {
    match error {
        VectorFileError::Corrupt(_) | VectorFileError::Mismatch => true,
        VectorFileError::Sql(error) => corrupt_sql(error),
        VectorFileError::Search(agentlaw_search::Error::Sql(error)) => corrupt_sql(error),
        _ => false,
    }
}

/// Caller owns the lane's I/O gate, so no writer can cross this transition.
pub(super) fn mark_guarded(
    shared: &Shared,
    binding: &str,
    token: u64,
    identity: &VectorFileIdentity,
    require_active: bool,
    generation: Option<u64>,
    reason: &str,
) -> bool {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let key = match &identity.kind {
        VectorFileKind::Current => LaneKey::current(binding),
        VectorFileKind::History { subject } => LaneKey::history(binding, subject),
    };
    let Some(lane) = state.schedule.lanes.get_mut(&key).filter(|lane| {
        lane.token == token && generation.is_none_or(|generation| lane.generation == generation)
    }) else {
        return false;
    };
    if require_active
        && !lane
            .active
            .as_ref()
            .is_some_and(|active| active.identity == *identity)
    {
        return false;
    }
    let message = format!("index corrupt: {reason}");
    lane.invalidate();
    lane.quarantined = true;
    lane.state = ComponentState::Degraded;
    lane.reason = Some(message.clone());
    if !state
        .quarantined
        .iter()
        .any(|(existing, _)| existing == identity)
    {
        state.quarantined.push((identity.clone(), message.clone()));
    }
    drop(state);
    shared.health.publish(
        ComponentId {
            binding: Some(binding.to_owned()),
            kind: ComponentKind::Semantic,
        },
        ComponentStatus {
            state: ComponentState::Degraded,
            observed_at: SystemTime::now(),
            reason: Some(message),
        },
    );
    shared.changed.notify_all();
    true
}
