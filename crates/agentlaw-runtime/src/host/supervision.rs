//! Restarts failed components without a new request.

use crate::ports::{
    ComponentId, ComponentState, ComponentStatus, HealthSink, RuntimeError, RuntimeResult,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[derive(Default)]
struct Backoff {
    next: u64,
}
impl Backoff {
    fn failure(&mut self, elapsed: Duration) -> Duration {
        if elapsed >= Duration::from_secs(60) {
            self.next = 1;
        }
        let delay = self.next.max(1);
        self.next = (delay * 2).min(30);
        Duration::from_secs(delay)
    }
}
struct StopState {
    stopped: AtomicBool,
    lock: Mutex<()>,
    wake: Condvar,
}
#[derive(Clone)]
pub struct StopToken(
    Arc<StopState>,
    Arc<Mutex<Option<Instant>>>,
    Option<(HealthSink, ComponentId)>,
);
impl StopToken {
    pub fn is_stopping(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }
    /// Marks the beginning of this attempt's stable ready period.
    pub fn ready(&self) {
        let mut ready = self.1.lock().unwrap_or_else(|e| e.into_inner());
        if ready.is_none() {
            *ready = Some(Instant::now());
            if let Some((health, id)) = &self.2 {
                health.publish(
                    id.clone(),
                    ComponentStatus {
                        state: ComponentState::Ready,
                        observed_at: SystemTime::now(),
                        reason: None,
                    },
                );
            }
        }
    }
    fn wait(&self, duration: Duration) {
        let guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = self
            .0
            .wake
            .wait_timeout_while(guard, duration, |_| !self.is_stopping())
            .unwrap_or_else(|e| e.into_inner());
    }
}
struct SupervisorState {
    stopping: bool,
    joining: bool,
    joined: bool,
    threads: BTreeMap<ComponentId, std::thread::JoinHandle<()>>,
}
pub struct Supervisor {
    stop: StopToken,
    state: Mutex<SupervisorState>,
    health: HealthSink,
    joined: Condvar,
}
impl Supervisor {
    pub fn new(health: HealthSink) -> Self {
        Self {
            stop: StopToken(
                Arc::new(StopState {
                    stopped: AtomicBool::new(false),
                    lock: Mutex::new(()),
                    wake: Condvar::new(),
                }),
                Arc::new(Mutex::new(None)),
                None,
            ),
            state: Mutex::new(SupervisorState {
                stopping: false,
                joining: false,
                joined: false,
                threads: BTreeMap::new(),
            }),
            health,
            joined: Condvar::new(),
        }
    }
    /// The factory owns its resources and must return promptly after the stop token is set.
    pub fn spawn<F>(&self, id: ComponentId, factory: F) -> RuntimeResult<()>
    where
        F: Fn(StopToken) -> Result<(), String> + Send + 'static,
    {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping {
            return Err(RuntimeError::Unavailable("supervisor stopping".into()));
        }
        if state.threads.contains_key(&id) {
            return Err(RuntimeError::Unavailable(
                "component already supervised".into(),
            ));
        }
        let token = self.stop.clone();
        let sink = self.health.clone();
        let component = id.clone();
        let handle = std::thread::spawn(move || {
            let mut backoff = Backoff::default();
            loop {
                // The same gate revokes restart permission during shutdown.
                let gate = token.0.lock.lock().unwrap_or_else(|e| e.into_inner());
                if token.is_stopping() {
                    break;
                }
                let attempt = StopToken(
                    token.0.clone(),
                    Arc::new(Mutex::new(None)),
                    Some((sink.clone(), component.clone())),
                );
                sink.publish(
                    component.clone(),
                    ComponentStatus {
                        state: ComponentState::Starting,
                        observed_at: SystemTime::now(),
                        reason: None,
                    },
                );
                drop(gate);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    factory(attempt.clone())
                }));
                if token.is_stopping() {
                    break;
                }
                let reason = match result {
                    Ok(Ok(())) => "component exited".to_owned(),
                    Ok(Err(error)) => error,
                    Err(panic) => panic
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_owned())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "component panicked".into()),
                };
                sink.publish(
                    component.clone(),
                    ComponentStatus {
                        state: ComponentState::Recovering,
                        observed_at: SystemTime::now(),
                        reason: Some(reason),
                    },
                );
                let stable = attempt
                    .1
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .map_or(Duration::ZERO, |ready| ready.elapsed());
                token.wait(backoff.failure(stable));
            }
            sink.publish(
                component,
                ComponentStatus {
                    state: ComponentState::Stopped,
                    observed_at: SystemTime::now(),
                    reason: None,
                },
            );
        });
        state.threads.insert(id, handle);
        Ok(())
    }
    /// Prevents new registrations and restarts, signals every component, then joins all of them.
    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.joining {
            state = self.joined.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        if state.joined {
            return;
        }
        state.stopping = true;
        state.joining = true;
        let guard = self.stop.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.stop.0.stopped.store(true, Ordering::SeqCst);
        self.stop.0.wake.notify_all();
        drop(guard);
        let threads = std::mem::take(&mut state.threads);
        drop(state);
        for (_, handle) in threads {
            let _ = handle.join();
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.joining = false;
        state.joined = true;
        self.joined.notify_all();
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests;
