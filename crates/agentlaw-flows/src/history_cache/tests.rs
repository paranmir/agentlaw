use super::*;
use crate::RequestControl;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::Duration;

#[test]
fn history_cache_same_key_waits_for_first_build() {
    let guards = Arc::new(HistoryCacheGuards::new());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let g = guards.clone();
    let first = std::thread::spawn(move || {
        g.with_build("same", &RequestControl::default(), || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        })
    });
    started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let (second_tx, second_rx) = mpsc::channel();
    let g = guards.clone();
    let second = std::thread::spawn(move || {
        g.with_build("same", &RequestControl::default(), || {
            second_tx.send(()).unwrap();
            Ok(())
        })
    });
    assert!(second_rx.recv_timeout(Duration::from_millis(200)).is_err());
    release_tx.send(()).unwrap();
    second_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert!(guards.keys.lock().unwrap().is_empty());
}
#[test]
fn history_cache_serializes_keys_and_cancelled_waiter_does_not_build() {
    let guards = Arc::new(HistoryCacheGuards::new());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let g = guards.clone();
    let first = std::thread::spawn(move || {
        g.with_build("a", &RequestControl::default(), || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(())
        })
    });
    started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let (other_tx, other_rx) = mpsc::channel();
    let g = guards.clone();
    let other = std::thread::spawn(move || {
        g.with_build("b", &RequestControl::default(), || {
            other_tx.send(()).unwrap();
            Ok(())
        })
    });
    other_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let control = RequestControl::default();
    let cancel = control.cancel.clone();
    let (built_tx, built_rx) = mpsc::channel();
    let g = guards.clone();
    let waiter = std::thread::spawn(move || {
        g.with_build("a", &control, || {
            built_tx.send(()).unwrap();
            Ok(())
        })
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let waiting = guards
            .keys
            .lock()
            .unwrap()
            .get("a")
            .is_some_and(|state| Arc::strong_count(state) >= 3);
        if waiting {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "waiter did not reach the key"
        );
        std::thread::yield_now();
    }
    cancel.store(true, Ordering::Release);
    assert_eq!(waiter.join().unwrap().unwrap_err().code, "cancelled");
    assert!(built_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
    other.join().unwrap().unwrap();
    assert!(guards.keys.lock().unwrap().is_empty());
}

#[test]
fn history_cache_panic_releases_key() {
    let guards = HistoryCacheGuards::new();
    let _ = std::panic::catch_unwind(|| {
        let _: crate::Result<()> =
            guards.with_build("a", &RequestControl::default(), || panic!("test"));
    });
    let (done_tx, done_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        guards
            .with_build("a", &RequestControl::default(), || Ok(()))
            .unwrap();
        done_tx.send(()).unwrap();
    });
    done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    thread.join().unwrap();
}
