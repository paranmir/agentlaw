use super::*;
use std::time::Duration;

fn status(state: ComponentState, reason: Option<&str>) -> ComponentStatus {
    ComponentStatus {
        state,
        observed_at: SystemTime::UNIX_EPOCH + Duration::new(123, 456),
        reason: reason.map(str::to_owned),
    }
}

const KINDS: [ComponentKind; 8] = [
    ComponentKind::Host,
    ComponentKind::Setup,
    ComponentKind::Store,
    ComponentKind::Control,
    ComponentKind::Exact,
    ComponentKind::Semantic,
    ComponentKind::Model,
    ComponentKind::Git,
];

#[test]
fn health_reasons_are_truncated_at_utf8_boundaries() {
    let sink = HealthSink::default();
    let id = ComponentId {
        binding: None,
        kind: ComponentKind::Host,
    };
    let ascii = "a".repeat(300);
    sink.publish(id.clone(), status(ComponentState::Failed, Some(&ascii)));
    assert_eq!(sink.snapshot()[0].1.reason.as_deref(), Some(&ascii[..256]));
    let unicode = "기".repeat(100);
    sink.publish(id, status(ComponentState::Failed, Some(&unicode)));
    let snapshot = sink.snapshot();
    let stored = snapshot[0].1.reason.as_ref().unwrap();
    assert!(stored.len() <= MAX_HEALTH_REASON_BYTES);
    assert_eq!(stored.len(), 255);
    assert!(unicode.starts_with(stored));
}

#[test]
fn health_evicts_oldest_binding_and_preserves_installation_and_published() {
    let sink = HealthSink::default();
    for kind in KINDS {
        sink.publish(
            ComponentId {
                binding: None,
                kind,
            },
            status(ComponentState::Ready, None),
        );
    }
    for i in 0..513 {
        let mut entry = status(ComponentState::Ready, None);
        entry.observed_at += Duration::from_secs(i + 1);
        sink.publish(
            ComponentId {
                binding: Some(format!("{i:04}")),
                kind: ComponentKind::Store,
            },
            entry,
        );
    }
    let snapshot = sink.snapshot();
    assert_eq!(snapshot.len(), MAX_HEALTH_COMPONENTS);
    for kind in KINDS {
        assert!(snapshot
            .iter()
            .any(|(id, _)| id.binding.is_none() && id.kind == kind));
    }
    assert!(!snapshot
        .iter()
        .any(|(id, _)| id.binding.as_deref() == Some("0000")));
    assert!(snapshot
        .iter()
        .any(|(id, _)| id.binding.as_deref() == Some("0512")));

    // The newly published entry is protected even if it is older than all others.
    sink.publish(
        ComponentId {
            binding: Some("new-old".into()),
            kind: ComponentKind::Store,
        },
        status(ComponentState::Ready, None),
    );
    assert!(sink
        .snapshot()
        .iter()
        .any(|(id, _)| id.binding.as_deref() == Some("new-old")));
}

#[test]
fn health_eviction_breaks_timestamp_ties_in_component_order() {
    let sink = HealthSink::default();
    for i in 0..=MAX_HEALTH_COMPONENTS {
        sink.publish(
            ComponentId {
                binding: Some(format!("{i:04}")),
                kind: ComponentKind::Store,
            },
            status(ComponentState::Ready, None),
        );
    }
    let snapshot = sink.snapshot();
    assert_eq!(snapshot.len(), MAX_HEALTH_COMPONENTS);
    assert_eq!(snapshot[0].0.binding.as_deref(), Some("0001"));
}

#[test]
fn health_worst_case_fits_control_reply() {
    let sink = HealthSink::default();
    let reason = "\u{1}".repeat(MAX_HEALTH_REASON_BYTES);
    let mut entry = status(ComponentState::Recovering, Some(&reason));
    entry.observed_at = SystemTime::UNIX_EPOCH + Duration::new(4_000_000_000, 999_999_999);
    for kind in KINDS {
        sink.publish(
            ComponentId {
                binding: None,
                kind,
            },
            entry.clone(),
        );
    }
    for i in 0..MAX_HEALTH_COMPONENTS - KINDS.len() {
        sink.publish(
            ComponentId {
                binding: Some(format!("00000000-0000-0000-0000-{i:012x}")),
                kind: ComponentKind::Semantic,
            },
            entry.clone(),
        );
    }
    let components = sink.snapshot();
    assert_eq!(components.len(), MAX_HEALTH_COMPONENTS);
    let reply = crate::protocol::ControlReply::Health(HealthSnapshot {
        components,
        activity: ActivityCounts {
            executions: u32::MAX,
            canonical_recoveries: u32::MAX,
        },
    });
    let bytes = serde_json::to_vec(&reply).unwrap();
    println!("worst-case health reply: {} bytes", bytes.len());
    assert!(bytes.len() <= crate::protocol::MAX_CONTROL_REPLY_BYTES as usize);
    crate::protocol::write_frame(
        &mut Vec::new(),
        &bytes,
        crate::protocol::MAX_CONTROL_REPLY_BYTES,
    )
    .unwrap();
}

#[test]
fn health_publish_replaces_and_snapshots_are_independent_and_sorted() {
    let sink = HealthSink::default();
    let clone = sink.clone();
    let installation = ComponentId {
        binding: None,
        kind: ComponentKind::Host,
    };
    let scoped = ComponentId {
        binding: Some("project".into()),
        kind: ComponentKind::Store,
    };
    let ready = status(ComponentState::Ready, None);
    let failed = status(ComponentState::Failed, Some("failure"));
    sink.publish(scoped.clone(), ready.clone());
    clone.publish(installation.clone(), ready.clone());
    let before = sink.snapshot();
    clone.publish(scoped.clone(), failed.clone());
    assert_eq!(
        before,
        vec![
            (installation.clone(), ready.clone()),
            (scoped.clone(), ready.clone())
        ]
    );
    assert_eq!(
        sink.snapshot(),
        vec![(installation, ready), (scoped, failed)]
    );
}

#[test]
fn activity_guards_track_each_kind_and_can_move_to_threads() {
    let tracker = ActivityTracker::default();
    let execution = tracker.begin(ActivityKind::Execution).unwrap();
    assert_eq!(
        tracker.counts(),
        ActivityCounts {
            executions: 1,
            canonical_recoveries: 0
        }
    );
    let recovery = tracker
        .clone()
        .begin(ActivityKind::CanonicalRecovery)
        .unwrap();
    assert_eq!(
        tracker.counts(),
        ActivityCounts {
            executions: 1,
            canonical_recoveries: 1
        }
    );
    std::thread::spawn(move || drop(execution)).join().unwrap();
    assert_eq!(
        tracker.counts(),
        ActivityCounts {
            executions: 0,
            canonical_recoveries: 1
        }
    );
    drop(recovery);
    assert_eq!(tracker.counts(), ActivityCounts::default());
}

#[test]
fn activity_overflow_does_not_change_counts_or_block_other_kind() {
    let tracker = ActivityTracker {
        inner: Arc::new(Mutex::new(ActivityCounts {
            executions: u32::MAX,
            canonical_recoveries: 0,
        })),
    };
    assert!(matches!(
        tracker.begin(ActivityKind::Execution),
        Err(RuntimeError::Capacity(_))
    ));
    assert_eq!(tracker.counts().executions, u32::MAX);
    let recovery = tracker.begin(ActivityKind::CanonicalRecovery).unwrap();
    assert_eq!(
        tracker.counts(),
        ActivityCounts {
            executions: u32::MAX,
            canonical_recoveries: 1
        }
    );
    drop(recovery);
    assert_eq!(
        tracker.counts(),
        ActivityCounts {
            executions: u32::MAX,
            canonical_recoveries: 0
        }
    );
}

#[test]
fn health_snapshot_round_trips_json() {
    let snapshot = HealthSnapshot {
        components: vec![(
            ComponentId {
                binding: Some("project".into()),
                kind: ComponentKind::Store,
            },
            status(ComponentState::Degraded, Some("reason")),
        )],
        activity: ActivityCounts {
            executions: 3,
            canonical_recoveries: 2,
        },
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    assert_eq!(
        serde_json::from_str::<HealthSnapshot>(&json).unwrap(),
        snapshot
    );
}

#[test]
fn health_recovers_poisoned_lock_without_losing_state() {
    let sink = HealthSink::default();
    let first = ComponentId {
        binding: None,
        kind: ComponentKind::Host,
    };
    let second = ComponentId {
        binding: None,
        kind: ComponentKind::Store,
    };
    let ready = status(ComponentState::Ready, None);
    sink.publish(first.clone(), ready.clone());
    let clone = sink.clone();
    assert!(std::thread::spawn(move || {
        let _lock = clone.inner.lock().unwrap();
        panic!("poison health lock");
    })
    .join()
    .is_err());
    sink.publish(second.clone(), ready.clone());
    assert_eq!(
        sink.snapshot(),
        vec![(first, ready.clone()), (second, ready)]
    );
}
