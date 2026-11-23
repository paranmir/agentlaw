//! Serves health snapshots from RAM.

use super::lifecycle::Lifecycle;
use crate::ports::{
    ComponentId, ComponentKind, ComponentState, ComponentStatus, HealthSink, HealthSnapshot,
};
use std::time::SystemTime;

pub fn snapshot(health: &HealthSink, lifecycle: &Lifecycle) -> HealthSnapshot {
    HealthSnapshot {
        components: health.snapshot(),
        activity: lifecycle.counts(),
    }
}

pub fn publish_host(health: &HealthSink, state: ComponentState, reason: Option<String>) {
    health.publish(
        ComponentId {
            binding: None,
            kind: ComponentKind::Host,
        },
        ComponentStatus {
            state,
            observed_at: SystemTime::now(),
            reason,
        },
    );
}

#[cfg(test)]
mod tests;
