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
mod tests {
    use super::*;
    use crate::ports::ComponentKind;

    #[test]
    fn restart_backoff_caps_and_resets_after_stability() {
        let mut backoff = Backoff::default();
        let actual: Vec<_> = (0..7).map(|_| backoff.failure(Duration::ZERO)).collect();
        assert_eq!(actual, [1, 2, 4, 8, 16, 30, 30].map(Duration::from_secs));
        assert_eq!(
            backoff.failure(Duration::from_secs(60)),
            Duration::from_secs(1)
        );
        assert_eq!(backoff.failure(Duration::ZERO), Duration::from_secs(2));
    }

    #[test]
    fn panic_is_reported_and_shutdown_waits_for_component() {
        let sink = HealthSink::default();
        let supervisor = Supervisor::new(sink.clone());
        let id = ComponentId {
            binding: None,
            kind: ComponentKind::Exact,
        };
        let launches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = launches.clone();
        supervisor
            .spawn(id.clone(), move |stop| {
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("component failed");
                }
                while !stop.is_stopping() {
                    std::thread::yield_now();
                }
                Ok(())
            })
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let mut observed_failure = false;
        while launches.load(Ordering::SeqCst) < 2 && Instant::now() < until {
            observed_failure |= sink.snapshot().iter().any(|(key, status)| {
                *key == id
                    && status
                        .reason
                        .as_deref()
                        .is_some_and(|r| r.contains("component failed"))
            });
            std::thread::yield_now();
        }
        assert_eq!(launches.load(Ordering::SeqCst), 2);
        assert!(observed_failure);
        supervisor.shutdown();
        let count = launches.load(Ordering::SeqCst);
        assert_eq!(count, 2);
    }

    #[test]
    fn stopping_cancels_restart_and_joins_other_component() {
        use std::sync::mpsc;
        let supervisor = Arc::new(Supervisor::new(HealthSink::default()));
        let launches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = launches.clone();
        let (failed_tx, failed_rx) = mpsc::channel();
        supervisor
            .spawn(
                ComponentId {
                    binding: None,
                    kind: ComponentKind::Exact,
                },
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    failed_tx.send(()).unwrap();
                    Err("failed".into())
                },
            )
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        supervisor
            .spawn(
                ComponentId {
                    binding: None,
                    kind: ComponentKind::Semantic,
                },
                move |token| {
                    started_tx.send(()).unwrap();
                    while !token.is_stopping() {
                        std::thread::yield_now();
                    }
                    release_rx.lock().unwrap().recv().unwrap();
                    Ok(())
                },
            )
            .unwrap();
        failed_rx.recv().unwrap();
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let owner = supervisor.clone();
        let join = std::thread::spawn(move || {
            owner.shutdown();
            done_tx.send(()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        join.join().unwrap();
        assert_eq!(launches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn review_shutdown_releases_registry_before_join() {
        use std::sync::mpsc;
        let supervisor = Arc::new(Supervisor::new(HealthSink::default()));
        let weak = Arc::downgrade(&supervisor);
        let (started_tx, started_rx) = mpsc::channel();
        let (unlocked_tx, unlocked_rx) = mpsc::channel();
        supervisor.spawn(ComponentId { binding: None, kind: ComponentKind::Exact }, move |token| {
            started_tx.send(()).unwrap();
            while !token.is_stopping() { std::thread::yield_now(); }
            let owner = weak.upgrade().unwrap();
            let rejected = matches!(owner.spawn(ComponentId { binding: None, kind: ComponentKind::Git }, |_| Ok(())), Err(RuntimeError::Unavailable(reason)) if reason == "supervisor stopping");
            unlocked_tx.send(rejected).unwrap();
            Ok(())
        }).unwrap();
        started_rx.recv().unwrap();
        supervisor.shutdown();
        assert!(unlocked_rx.recv().unwrap());
    }

    #[test]
    fn review_drop_revokes_restart_permission() {
        use std::sync::mpsc;
        let supervisor = Supervisor::new(HealthSink::default());
        let (token_tx, token_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        supervisor
            .spawn(
                ComponentId {
                    binding: None,
                    kind: ComponentKind::Exact,
                },
                move |token| {
                    token_tx.send(token.clone()).unwrap();
                    while !token.is_stopping() {
                        std::thread::yield_now();
                    }
                    done_tx.send(()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        let token = token_rx.recv().unwrap();
        drop(supervisor);
        let stopped = token.is_stopping();
        token.0.stopped.store(true, Ordering::SeqCst);
        token.0.wake.notify_all();
        done_rx.recv().unwrap();
        assert!(stopped);
    }

    #[test]
    fn review_ready_publishes_health() {
        use std::sync::mpsc;
        let health = HealthSink::default();
        let supervisor = Supervisor::new(health.clone());
        let id = ComponentId {
            binding: None,
            kind: ComponentKind::Exact,
        };
        let (ready_tx, ready_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let go_rx = Mutex::new(go_rx);
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        supervisor
            .spawn(id.clone(), move |token| {
                entered_tx.send(()).unwrap();
                go_rx.lock().unwrap().recv().unwrap();
                token.ready();
                ready_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                Ok(())
            })
            .unwrap();
        entered_rx.recv().unwrap();
        let starting = health.snapshot().into_iter().find(|(key, _)| *key == id);
        go_tx.send(()).unwrap();
        ready_rx.recv().unwrap();
        let status = health.snapshot().into_iter().find(|(key, _)| *key == id);
        release_tx.send(()).unwrap();
        supervisor.shutdown();
        assert_eq!(starting.unwrap().1.state, ComponentState::Starting);
        assert_eq!(status.unwrap().1.state, ComponentState::Ready);
    }

    #[test]
    fn concurrent_shutdown_calls_both_wait_for_join() {
        use std::sync::mpsc;
        let supervisor = Arc::new(Supervisor::new(HealthSink::default()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        supervisor
            .spawn(
                ComponentId {
                    binding: None,
                    kind: ComponentKind::Exact,
                },
                move |token| {
                    started_tx.send(()).unwrap();
                    while !token.is_stopping() {
                        std::thread::yield_now();
                    }
                    release_rx.lock().unwrap().recv().unwrap();
                    Ok(())
                },
            )
            .unwrap();
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let owner = supervisor.clone();
        let first_tx = done_tx.clone();
        let first = std::thread::spawn(move || {
            owner.shutdown();
            first_tx.send(()).unwrap();
        });
        while !supervisor.stop.is_stopping() {
            std::thread::yield_now();
        }
        let owner = supervisor.clone();
        let second = std::thread::spawn(move || {
            owner.shutdown();
            done_tx.send(()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        first.join().unwrap();
        second.join().unwrap();
    }
}
