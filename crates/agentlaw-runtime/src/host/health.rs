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
mod tests {
    use super::*;
    use crate::ports::ActivityKind;
    use std::sync::Arc;

    #[test]
    fn snapshot_reads_lifecycle_owned_activity() {
        let lifecycle = Lifecycle::system(Arc::new(|_| {}));
        let health = HealthSink::default();
        let guard = lifecycle.begin(ActivityKind::CanonicalRecovery).unwrap();
        publish_host(&health, ComponentState::Ready, None);
        let observed = snapshot(&health, &lifecycle);
        assert_eq!(observed.activity.canonical_recoveries, 1);
        assert_eq!(observed.components[0].1.state, ComponentState::Ready);
        drop(guard);
        assert_eq!(
            snapshot(&health, &lifecycle).activity.canonical_recoveries,
            0
        );
    }
}
