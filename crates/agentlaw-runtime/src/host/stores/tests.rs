use super::*;
use crate::installation::config::Config;
use std::{sync::mpsc, thread, time::Duration};

// The observer reports "maintain_waiting", "maintain_reserved", or
// "acquire_waiting", or "shutdown_waiting" only after the corresponding
// state transition. The implementation must invoke notify_observer there.
fn observe(stores: &Stores) -> mpsc::Receiver<(BindingRef, &'static str)> {
    let (tx, rx) = mpsc::channel();
    stores.observe(move |binding, event| {
        let _ = tx.send((binding, event));
    });
    rx
}

fn wait_for(
    rx: &mpsc::Receiver<(BindingRef, &'static str)>,
    binding: BindingRef,
    event: &'static str,
) {
    loop {
        let got = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("state transition");
        if got == (binding, event) {
            return;
        }
    }
}

fn fixture() -> (tempfile::TempDir, Arc<Stores>, Arc<ConfigSnapshot>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&local).unwrap();
    drop(agentlaw_storage::Store::open(&root, local.join("canonical")).unwrap());
    let config = Config {
        memory_store_path: root.clone(),
        user_id: "user".into(),
        history_response_limit_bytes: 8192,
        runtime_path: Some(local.clone()),
        response_limit_bytes: 65536,
    };
    let key = agentlaw_storage::binding_identity(&root, &local).unwrap();
    let reference = BindingRef::new(&key, &config.user_id);
    let snapshot = Arc::new(ConfigSnapshot {
        config,
        key,
        reference,
    });
    let registry = Arc::new(ConfigRegistry::new());
    let stores = Arc::new(Stores::new(
        dir.path().to_owned(),
        crate::test_support::store_service::deps(dir.path()),
        registry,
    ));
    (dir, stores, snapshot)
}

#[test]
fn maintenance_guards_serialize_on_one_binding() {
    let (_dir, stores, binding) = fixture();
    let first = stores
        .maintain(&binding, &RequestControl::default())
        .ok()
        .expect("first maintenance");
    let events = observe(&stores);
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let stores = stores.clone();
        let binding = binding.clone();
        thread::spawn(move || {
            let guard = stores.maintain(&binding, &RequestControl::default());
            done_tx.send(guard.is_ok()).unwrap();
        })
    };
    wait_for(&events, binding.reference, "maintain_waiting");
    assert!(done_rx.try_recv().is_err());
    drop(first);
    assert!(done_rx.recv_timeout(Duration::from_secs(10)).unwrap());
    worker.join().unwrap();
}

#[test]
fn audit_guard_and_maintenance_exclude_each_other() {
    let (_dir, stores, binding) = fixture();
    let audit = stores
        .audit_guard(&binding, &RequestControl::default())
        .ok()
        .expect("audit guard");
    let events = observe(&stores);
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let stores = stores.clone();
        let binding = binding.clone();
        thread::spawn(move || {
            let guard = stores.maintain(&binding, &RequestControl::default());
            done_tx.send(guard.is_ok()).unwrap();
        })
    };
    wait_for(&events, binding.reference, "maintain_waiting");
    assert!(done_rx.try_recv().is_err());
    drop(audit);
    assert!(done_rx.recv_timeout(Duration::from_secs(10)).unwrap());
    worker.join().unwrap();
}

#[test]
fn maintenance_reservation_precedes_draining_acquires() {
    let (_dir, stores, binding) = fixture();
    let initial = stores.acquire(&binding).ok().expect("initial acquire");
    let events = observe(&stores);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let maintenance = {
        let stores = stores.clone();
        let binding = binding.clone();
        thread::spawn(move || {
            let guard = stores
                .maintain(&binding, &RequestControl::default())
                .ok()
                .expect("maintain");
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(guard);
        })
    };
    wait_for(&events, binding.reference, "maintain_reserved");
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let acquire = {
        let stores = stores.clone();
        let binding = binding.clone();
        thread::spawn(move || {
            for _ in 0..32 {
                let guard = stores.acquire(&binding).ok().expect("acquire");
                acquired_tx.send(()).unwrap();
                drop(guard);
            }
        })
    };
    wait_for(&events, binding.reference, "acquire_waiting");
    drop(initial);
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("maintenance entered");
    assert!(acquired_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    for _ in 0..32 {
        acquired_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    maintenance.join().unwrap();
    acquire.join().unwrap();
}

#[test]
fn shutdown_waits_for_maintenance_and_does_not_publish_reopened_service() {
    let (_dir, stores, binding) = fixture();
    let mut guard = stores
        .maintain(&binding, &RequestControl::default())
        .ok()
        .expect("maintain");
    stores.stop_opening();
    assert!(matches!(
        stores.maintain(&binding, &RequestControl::default()),
        Err(RejectReason::Stopping)
    ));
    let events = observe(&stores);
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let stores = stores.clone();
        thread::spawn(move || {
            stores.close_all();
            done_tx.send(()).unwrap();
        })
    };
    wait_for(&events, binding.reference, "shutdown_waiting");
    assert!(guard.reopen(&RequestControl::default()).is_ok());
    assert!(matches!(
        stores.acquire(&binding),
        Err(RejectReason::Stopping)
    ));
    assert!(done_rx.try_recv().is_err());
    drop(guard);
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("shutdown finished");
    worker.join().unwrap();
}

#[test]
fn failed_reopen_wakes_waiting_acquire_with_error() {
    let (_dir, stores, binding) = fixture();
    let mut guard = stores
        .maintain(&binding, &RequestControl::default())
        .ok()
        .expect("maintain");
    let events = observe(&stores);
    let (tx, rx) = mpsc::channel();
    let worker = {
        let stores = stores.clone();
        let binding = binding.clone();
        thread::spawn(move || {
            tx.send(stores.acquire(&binding).err()).unwrap();
        })
    };
    wait_for(&events, binding.reference, "acquire_waiting");
    std::fs::write(
        binding
            .config
            .runtime_path
            .as_ref()
            .unwrap()
            .join("control-initialized.json"),
        b"invalid json",
    )
    .unwrap();
    assert!(guard.reopen(&RequestControl::default()).is_err());
    drop(guard);
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Some(RejectReason::StoreUnavailable(_))
    ));
    worker.join().unwrap();
}

#[test]
fn cancelled_waiting_acquire_does_not_interrupt_maintenance() {
    let (_dir, stores, binding) = fixture();
    let mut guard = stores
        .maintain(&binding, &RequestControl::default())
        .ok()
        .expect("maintain");
    let events = observe(&stores);
    let cancel = Arc::new(AtomicBool::new(false));
    let control = RequestControl::new(cancel.clone(), |_| {});
    let (tx, rx) = mpsc::channel();
    let worker = {
        let stores = stores.clone();
        let binding = binding.clone();
        thread::spawn(move || {
            tx.send(stores.acquire_for_execution(&binding, &control).err())
                .unwrap();
        })
    };
    wait_for(&events, binding.reference, "acquire_waiting");
    cancel.store(true, Ordering::Release);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .code,
        "cancelled"
    );
    assert!(guard.reopen(&RequestControl::default()).is_ok());
    drop(guard);
    worker.join().unwrap();
}

#[test]
fn maintenance_does_not_block_another_binding() {
    let (_dir, stores, binding) = fixture();
    let (_other_dir, _other_stores, other) = fixture();
    let guard = stores
        .maintain(&binding, &RequestControl::default())
        .ok()
        .expect("maintain");
    let (tx, rx) = mpsc::channel();
    let worker = {
        let stores = stores.clone();
        thread::spawn(move || {
            tx.send(stores.acquire(&other).is_ok()).unwrap();
        })
    };
    assert!(rx
        .recv_timeout(Duration::from_secs(10))
        .expect("other binding acquire"));
    drop(guard);
    worker.join().unwrap();
}
