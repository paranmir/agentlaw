use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn fixture() -> (Lifecycle, Arc<AtomicU64>) {
    let seconds = Arc::new(AtomicU64::new(0));
    let source = seconds.clone();
    let origin = Instant::now();
    let lifecycle = Lifecycle::new(
        Arc::new(move || origin + Duration::from_secs(source.load(Ordering::SeqCst))),
        Arc::new(|_| {}),
    );
    (lifecycle, seconds)
}

#[test]
fn leases_expire_independently_and_new_demand_resets_idle() {
    let (l, t) = fixture();
    l.lease("a");
    t.store(30, Ordering::SeqCst);
    l.lease("b");
    t.store(60, Ordering::SeqCst);
    l.poll();
    t.store(89, Ordering::SeqCst);
    l.poll();
    t.store(90, Ordering::SeqCst);
    l.poll();
    t.store(689, Ordering::SeqCst);
    l.lease("c");
    t.store(748, Ordering::SeqCst);
    assert_eq!(l.poll().state, LifecycleState::Running);
    t.store(1349, Ordering::SeqCst);
    assert_eq!(l.poll().state, LifecycleState::Stopping);
}

#[test]
fn warm_is_delivered_only_when_live_presence_changes() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let output = events.clone();
    let seconds = Arc::new(AtomicU64::new(0));
    let clock = seconds.clone();
    let origin = Instant::now();
    let l = Lifecycle::new(
        Arc::new(move || origin + Duration::from_secs(clock.load(Ordering::SeqCst))),
        Arc::new(move |warm| output.lock().unwrap().push(warm)),
    );
    l.lease("a");
    l.lease("b");
    seconds.store(60, Ordering::SeqCst);
    l.poll();
    l.poll();
    l.lease("a");
    l.lease("a");
    assert_eq!(*events.lock().unwrap(), vec![true, false, true]);
}

#[test]
fn activity_guards_start_idle_at_drop() {
    for kind in [ActivityKind::Execution, ActivityKind::CanonicalRecovery] {
        let (l, t) = fixture();
        let guard = l.begin(kind).unwrap();
        t.store(1000, Ordering::SeqCst);
        assert_eq!(l.poll().state, LifecycleState::Running);
        drop(guard);
        t.store(1599, Ordering::SeqCst);
        assert_eq!(l.poll().state, LifecycleState::Running);
        t.store(1600, Ordering::SeqCst);
        assert_eq!(l.poll().state, LifecycleState::Stopping);
    }
}

#[test]
fn admission_and_idle_transition_are_atomic() {
    for _ in 0..100 {
        let (l, t) = fixture();
        t.store(600, Ordering::SeqCst);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let other = l.clone();
        let b = barrier.clone();
        let thread = std::thread::spawn(move || {
            b.wait();
            other.begin(ActivityKind::Execution)
        });
        barrier.wait();
        l.poll();
        let admitted = thread.join().unwrap();
        if admitted.is_ok() {
            assert_eq!(l.state(), LifecycleState::Running);
        } else {
            assert_eq!(l.state(), LifecycleState::Stopping);
        }
        l.stop();
        assert!(l.begin(ActivityKind::Execution).is_err());
    }
}

#[test]
fn review_checkpoint_panic_allows_retry() {
    let (l, _) = fixture();
    l.stop();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        l.finish(|| panic!("checkpoint failed"), || {});
    }))
    .is_err());
    assert!(l.finish(|| {}, || {}));
}

#[test]
fn review_completed_checkpoint_is_not_repeated_after_cleanup_panic() {
    let (l, _) = fixture();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    l.stop();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        l.finish(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
            },
            || panic!("cleanup failed"),
        );
    }))
    .is_err());
    assert!(l.finish(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
        },
        || {}
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn review_activity_is_owned_and_rejected_after_stopping() {
    let (l, t) = fixture();
    t.store(599, Ordering::SeqCst);
    let guard = l.begin(ActivityKind::Execution).unwrap();
    assert_eq!(l.counts().executions, 1);
    drop(guard);
    t.store(600, Ordering::SeqCst);
    assert_eq!(l.poll().state, LifecycleState::Running);
    l.stop();
    assert!(l.begin(ActivityKind::Execution).is_err());
}

#[test]
fn review_warm_delivery_is_ordered_across_concurrent_calls() {
    use std::sync::mpsc;
    let t = Arc::new(AtomicU64::new(0));
    let clock = t.clone();
    let origin = Instant::now();
    let events = Arc::new(Mutex::new(Vec::new()));
    let output = events.clone();
    let (cold_tx, cold_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let l = Lifecycle::new(
        Arc::new(move || origin + Duration::from_secs(clock.load(Ordering::SeqCst))),
        Arc::new(move |warm| {
            if !warm {
                cold_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            output.lock().unwrap().push(warm);
        }),
    );
    l.lease("a");
    t.store(60, Ordering::SeqCst);
    let other = l.clone();
    let expiry = std::thread::spawn(move || {
        other.poll();
    });
    cold_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let other = l.clone();
    let renewal = std::thread::spawn(move || other.lease("b"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !l.inner.state.lock().unwrap().leases.contains_key("b") {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    release_tx.send(()).unwrap();
    expiry.join().unwrap();
    renewal.join().unwrap();
    assert_eq!(*events.lock().unwrap(), vec![true, false, true]);
}
