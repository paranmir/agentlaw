use super::*;

#[test]
fn live_child_remains_when_demand_disappears() {
    let now = Instant::now();
    let mut lifecycle = ModelLifecycle::default();
    lifecycle.started();
    lifecycle.ready(now);
    assert_eq!(lifecycle.action(now, false, true), LifecycleAction::None);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
}

#[test]
fn lifecycle_retries_on_exit_without_new_demand_and_caps_backoff() {
    let origin = Instant::now();
    let mut lifecycle = ModelLifecycle::default();
    assert_eq!(lifecycle.action(origin, true, true), LifecycleAction::Start);
    lifecycle.started();
    lifecycle.ready(origin);
    let mut now = origin;
    for delay in [1, 2, 4, 8, 16, 30, 30] {
        lifecycle.exited(now, "exit".into());
        assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
        now += Duration::from_secs(delay);
        assert_eq!(lifecycle.action(now, true, true), LifecycleAction::Start);
        lifecycle.started();
    }
    lifecycle.ready(now);
    now += Duration::from_secs(60);
    lifecycle.exited(now, "exit".into());
    assert_eq!(
        lifecycle.action(now + Duration::from_secs(1), true, true),
        LifecycleAction::Start
    );
}

#[test]
fn lifecycle_cancels_retry_and_never_overlaps_owned_child() {
    let now = Instant::now();
    let mut lifecycle = ModelLifecycle::default();
    assert_eq!(lifecycle.action(now, true, false), LifecycleAction::None);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::Start);
    lifecycle.started();
    assert_eq!(lifecycle.action(now, false, true), LifecycleAction::None);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
    lifecycle.stopped();
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::Start);
    lifecycle.started();
    lifecycle.exited(now, "missing model".into());
    assert_eq!(lifecycle.action(now, false, true), LifecycleAction::None);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
    assert_eq!(
        lifecycle.action(now + Duration::from_secs(1), true, true),
        LifecycleAction::Start
    );
}

#[test]
fn demand_flap_must_not_bypass_failure_delay() {
    let now = Instant::now();
    let mut lifecycle = ModelLifecycle::default();
    lifecycle.started();
    lifecycle.exited(now, "load failed".into());
    assert_eq!(lifecycle.action(now, false, true), LifecycleAction::None);
    assert_eq!(
        lifecycle.action(now, true, true),
        LifecycleAction::None,
        "a new attach started a replacement before the one-second failure deadline"
    );
}
