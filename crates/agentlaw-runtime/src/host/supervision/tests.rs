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
