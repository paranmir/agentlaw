//! Client leases, idle shutdown and the installation owner lock.

use crate::ports::{ActivityCounts, ActivityGuard, ActivityKind, ActivityTracker, RuntimeError};
use crate::protocol::{HelloReply, RejectReason};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const LEASE_RENEWAL_INTERVAL: Duration = Duration::from_secs(10);
pub const LEASE_TTL: Duration = Duration::from_secs(60);
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Running,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, Copy)]
pub struct PollResult {
    pub state: LifecycleState,
}

#[derive(Debug)]
pub enum AdmissionError {
    Stopping,
    Capacity(RuntimeError),
}
impl AdmissionError {
    pub fn stopping_reason(&self) -> Option<RejectReason> {
        match self {
            Self::Stopping => Some(RejectReason::Stopping),
            Self::Capacity(_) => None,
        }
    }
}

struct State {
    phase: LifecycleState,
    leases: HashMap<String, Instant>,
    idle_since: Option<Instant>,
    finishing: bool,
    checkpoint_done: bool,
}
struct Inner {
    state: Mutex<State>,
    activity: ActivityTracker,
    clock: Clock,
    warm_delivery: Mutex<bool>,
    on_warm: Arc<dyn Fn(bool) + Send + Sync>,
}
#[derive(Clone)]
pub struct Lifecycle {
    inner: Arc<Inner>,
}

impl Lifecycle {
    /// The callback must be a short RAM operation and must not reenter warm delivery.
    pub fn new(clock: Clock, on_warm: Arc<dyn Fn(bool) + Send + Sync>) -> Self {
        let now = clock();
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    phase: LifecycleState::Running,
                    leases: HashMap::new(),
                    idle_since: Some(now),
                    finishing: false,
                    checkpoint_done: false,
                }),
                activity: ActivityTracker::default(),
                clock,
                warm_delivery: Mutex::new(false),
                on_warm,
            }),
        }
    }
    pub fn system(on_warm: Arc<dyn Fn(bool) + Send + Sync>) -> Self {
        Self::new(Arc::new(Instant::now), on_warm)
    }
    pub fn counts(&self) -> ActivityCounts {
        self.inner.activity.counts()
    }
    fn deliver_warm(&self) {
        let mut delivered = self
            .inner
            .warm_delivery
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let warm = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            self.update_idle(&mut state, (self.inner.clock)());
            !state.leases.is_empty()
        };
        if warm != *delivered {
            (self.inner.on_warm)(warm);
            *delivered = warm;
        }
    }
    fn update_idle(&self, state: &mut State, now: Instant) {
        let last_expiry = state
            .leases
            .values()
            .copied()
            .filter(|expiry| *expiry <= now)
            .max();
        state.leases.retain(|_, expiry| *expiry > now);
        let counts = self.inner.activity.counts();
        if !state.leases.is_empty() || counts.executions != 0 || counts.canonical_recoveries != 0 {
            state.idle_since = None;
        } else if state.idle_since.is_none() {
            state.idle_since = Some(last_expiry.unwrap_or(now));
        }
    }
    pub fn lease(&self, client: impl Into<String>) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.inner.clock)();
        self.update_idle(&mut state, now);
        if state.phase == LifecycleState::Running {
            state.leases.insert(client.into(), now + LEASE_TTL);
            state.idle_since = None;
        }
        drop(state);
        self.deliver_warm();
    }
    pub fn begin(&self, kind: ActivityKind) -> Result<LifecycleGuard, AdmissionError> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != LifecycleState::Running {
            return Err(AdmissionError::Stopping);
        }
        let guard = self
            .inner
            .activity
            .begin(kind)
            .map_err(AdmissionError::Capacity)?;
        state.idle_since = None;
        Ok(LifecycleGuard {
            inner: self.inner.clone(),
            guard: Some(guard),
        })
    }
    pub fn poll(&self) -> PollResult {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.inner.clock)();
        self.update_idle(&mut state, now);
        if state.phase == LifecycleState::Running
            && state
                .idle_since
                .is_some_and(|since| now.saturating_duration_since(since) >= DEFAULT_IDLE_TIMEOUT)
        {
            state.phase = LifecycleState::Stopping;
        }
        let result = PollResult { state: state.phase };
        drop(state);
        self.deliver_warm();
        result
    }
    pub fn state(&self) -> LifecycleState {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase
    }
    pub fn hello_reply(&self) -> HelloReply {
        if self.state() == LifecycleState::Running {
            HelloReply::Accepted
        } else {
            HelloReply::Stopping
        }
    }
    pub fn stop(&self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase == LifecycleState::Running {
            state.phase = LifecycleState::Stopping;
        }
    }
    /// Call after draining accepted work; checkpoint and model cleanup run without the lock.
    pub fn finish(&self, checkpoint: impl FnOnce(), model_cleanup: impl FnOnce()) -> bool {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != LifecycleState::Stopping
            || state.finishing
            || self.inner.activity.counts() != Default::default()
        {
            return false;
        }
        state.finishing = true;
        let checkpoint_done = state.checkpoint_done;
        drop(state);
        let _reset = FinishGuard(self.inner.clone());
        if !checkpoint_done {
            checkpoint();
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .checkpoint_done = true;
        }
        model_cleanup();
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase = LifecycleState::Stopped;
        true
    }
}
struct FinishGuard(Arc<Inner>);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finishing = false;
    }
}
#[must_use]
pub struct LifecycleGuard {
    inner: Arc<Inner>,
    guard: Option<ActivityGuard>,
}
impl Drop for LifecycleGuard {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.inner.clock)();
        Lifecycle {
            inner: self.inner.clone(),
        }
        .update_idle(&mut state, now);
        drop(self.guard.take());
        Lifecycle {
            inner: self.inner.clone(),
        }
        .update_idle(&mut state, now);
        drop(state);
        Lifecycle {
            inner: self.inner.clone(),
        }
        .deliver_warm();
    }
}

#[cfg(test)]
mod tests {
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
}
