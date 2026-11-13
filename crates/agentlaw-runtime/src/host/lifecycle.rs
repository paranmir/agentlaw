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
    on_stopping: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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
                on_stopping: Mutex::new(None),
            }),
        }
    }
    pub fn system(on_warm: Arc<dyn Fn(bool) + Send + Sync>) -> Self {
        Self::new(Arc::new(Instant::now), on_warm)
    }
    pub fn counts(&self) -> ActivityCounts {
        self.inner.activity.counts()
    }
    pub fn set_on_stopping(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        *self
            .inner
            .on_stopping
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(callback);
    }
    fn notify_stopping(&self) {
        if let Some(callback) = self
            .inner
            .on_stopping
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            callback();
        }
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
            self.notify_stopping();
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
            self.notify_stopping();
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
mod tests;
