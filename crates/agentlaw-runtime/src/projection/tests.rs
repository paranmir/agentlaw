use super::*;
use rusqlite::Connection;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Sink {
    pointers: Mutex<Vec<ActiveGeneration>>,
    retired: Mutex<Vec<Vec<PathBuf>>>,
    fail_pointer: AtomicUsize,
    fail_retire: AtomicUsize,
}
impl ActivePointerSink for Sink {
    fn persist_active(&self, _: &str, value: &ActiveGeneration) -> RuntimeResult<()> {
        if self
            .fail_pointer
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(RuntimeError::Unavailable("injected pointer error".into()));
        }
        self.pointers.lock().unwrap().push(value.clone());
        Ok(())
    }
}
impl RetiredFileSink for Sink {
    fn retire(&self, _: &str, files: &[PathBuf]) -> RuntimeResult<()> {
        self.retired.lock().unwrap().push(files.to_vec());
        if self
            .fail_retire
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(RuntimeError::Unavailable(
                "injected retirement error".into(),
            ));
        }
        Ok(())
    }
}
fn fixture() -> (tempfile::TempDir, agentlaw_storage::Store, ActiveGeneration) {
    let dir = tempfile::tempdir().unwrap();
    let store =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("local")).unwrap();
    let pos = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    let exact = dir.path().join("exact-old.sqlite");
    let lexical = dir.path().join("lexical-old.sqlite");
    let _ = agentlaw_flows::exact::ExactIndex::open(&exact).unwrap();
    let _ = agentlaw_search::SearchIndex::open(&lexical).unwrap();
    let db = Connection::open(&exact).unwrap();
    db.execute(
        "INSERT INTO exact_position VALUES(1,?1,?2)",
        rusqlite::params![pos.epoch, pos.sequence.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO exact_lexical VALUES(1,'lexical-old.sqlite')",
        [],
    )
    .unwrap();
    drop(db);
    let active = ActiveGeneration {
        id: "old".into(),
        epoch: pos.epoch,
        exact_file: exact,
        lexical_file: lexical,
    };
    (dir, store, active)
}
fn start(
    store: &agentlaw_storage::Store,
    active: ActiveGeneration,
    sink: Arc<Sink>,
) -> ProjectionHandle {
    ProjectionHandle::start(
        "test".into(),
        store.owned_published_reader(),
        active,
        sink.clone(),
        sink,
        HealthSink::default(),
        ProjectionConfig {
            retry_initial: Duration::from_millis(30),
            retry_max: Duration::from_millis(100),
            ..ProjectionConfig::new()
        },
    )
    .unwrap()
}
fn await_condition(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < end, "timed out");
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn publication_wake_advances_and_cancelled_wait_returns() {
    let (_dir, store, active) = fixture();
    let handle = start(&store, active, Arc::new(Sink::default()));
    await_condition(|| handle.status().state == ProjectionState::Running);
    let unit = agentlaw_storage::CurrentUnit {
        entity_id: uuid::Uuid::new_v4().to_string(),
        entity_type: "memory".into(),
        state: agentlaw_storage::UnitState::Live {
            heads: vec![agentlaw_storage::Head {
                metadata: serde_json::json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: "projection fixture".into(),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![agentlaw_storage::Mutation {
                unit,
                expected_versions: vec![],
                evidence: "test".into(),
            }],
        )
        .unwrap();
    let target = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    handle.wake();
    assert!(matches!(
        handle.wait_exact(&target, &RequestControl::default()),
        ExactPreparation::Complete { .. }
    ));
    let future = SourcePosition {
        sequence: target.sequence + 1,
        ..target
    };
    let cancel = RequestControl::default();
    cancel.cancel.store(true, Ordering::Release);
    assert!(matches!(
        handle.wait_exact(&future, &cancel),
        ExactPreparation::Incomplete { .. }
    ));
    assert!(handle.shutdown(Duration::from_secs(5)));
}
#[test]
fn corrupt_start_recovers_retries_pointer_and_restores_saved_pointer() {
    let (_dir, store, active) = fixture();
    let sink = Arc::new(Sink::default());
    sink.fail_pointer.store(1, Ordering::SeqCst);
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute_batch("DROP TABLE exact_heads").unwrap();
    drop(db);
    let handle = start(&store, active.clone(), sink.clone());
    await_condition(|| !sink.pointers.lock().unwrap().is_empty());
    await_condition(|| handle.status().state == ProjectionState::Running);
    let next = sink.pointers.lock().unwrap()[0].clone();
    assert_ne!(active.id, next.id);
    assert!(handle.generations().pin().is_ok());
    assert!(handle.shutdown(Duration::from_secs(5)));
    let restored = start(&store, next, sink);
    await_condition(|| restored.status().state == ProjectionState::Running);
    assert!(restored.generations().pin().is_ok());
    assert!(restored.shutdown(Duration::from_secs(5)));
}

#[test]
fn pinned_generation_is_retired_only_after_drop_and_failed_sink_is_retried() {
    let (_dir, store, active) = fixture();
    let sink = Arc::new(Sink::default());
    sink.fail_retire.store(1, Ordering::SeqCst);
    let handle = start(&store, active.clone(), sink.clone());
    await_condition(|| handle.status().state == ProjectionState::Running);
    let pin = handle.generations().pin().unwrap();
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute_batch("DROP TABLE exact_heads").unwrap();
    drop(db);
    handle.wake();
    await_condition(|| !sink.pointers.lock().unwrap().is_empty());
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(sink
        .retired
        .lock()
        .unwrap()
        .iter()
        .all(|batch| !batch.contains(&active.exact_file)));
    drop(pin);
    await_condition(|| {
        sink.retired
            .lock()
            .unwrap()
            .iter()
            .filter(|batch| batch.contains(&active.exact_file))
            .count()
            >= 2
    });
    assert!(handle.shutdown(Duration::from_secs(5)));
}

#[test]
fn unavailable_source_blocks_and_waiter_returns_incomplete() {
    let (_dir, store, active) = fixture();
    let handle = start(&store, active, Arc::new(Sink::default()));
    await_condition(|| handle.status().state == ProjectionState::Running);
    // This fixture owns its isolated local directory; remove its epoch to inject source failure.
    let local = _dir.path().join("local/source-epoch");
    let saved = std::fs::read(&local).unwrap();
    std::fs::remove_file(&local).unwrap();
    handle.wake();
    await_condition(|| handle.status().state == ProjectionState::Blocked);
    let through = handle.status().through.unwrap();
    let target = SourcePosition {
        sequence: through.sequence + 1,
        ..through
    };
    assert!(matches!(
        handle.wait_exact(&target, &RequestControl::default()),
        ExactPreparation::Incomplete { .. }
    ));
    std::fs::write(local, saved).unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(handle.shutdown(Duration::from_secs(5)));
}

struct FakeClock {
    base: Instant,
    millis: std::sync::atomic::AtomicU64,
}
impl ProjectionClock for FakeClock {
    fn now(&self) -> Instant {
        self.base + Duration::from_millis(self.millis.load(Ordering::SeqCst))
    }
}
#[test]
fn injected_clock_controls_blocked_retry_deadline() {
    let (dir, store, active) = fixture();
    let sink = Arc::new(Sink::default());
    let clock = Arc::new(FakeClock {
        base: Instant::now(),
        millis: std::sync::atomic::AtomicU64::new(0),
    });
    let config = ProjectionConfig {
        clock: clock.clone(),
        ..ProjectionConfig::new()
    };
    let handle = ProjectionHandle::start(
        "clock".into(),
        store.owned_published_reader(),
        active,
        sink.clone(),
        sink,
        HealthSink::default(),
        config,
    )
    .unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    let epoch = dir.path().join("local/source-epoch");
    let bytes = std::fs::read(&epoch).unwrap();
    std::fs::remove_file(&epoch).unwrap();
    handle.wake();
    await_condition(|| handle.status().state == ProjectionState::Blocked);
    std::fs::write(epoch, bytes).unwrap();
    clock.millis.store(999, Ordering::SeqCst);
    handle.inner.changed.notify_all();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(handle.status().state, ProjectionState::Blocked);
    clock.millis.store(1000, Ordering::SeqCst);
    handle.inner.changed.notify_all();
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(handle.shutdown(Duration::from_secs(5)));
}
#[test]
fn idle_projection_files_are_unchanged_and_wait_observes_cancellation() {
    let (_dir, store, active) = fixture();
    let handle = start(&store, active.clone(), Arc::new(Sink::default()));
    await_condition(|| handle.status().state == ProjectionState::Running);
    let paths: Vec<_> = [&active.exact_file, &active.lexical_file]
        .into_iter()
        .flat_map(|p| [p.clone(), PathBuf::from(format!("{}-wal", p.display()))])
        .collect();
    let before: Vec<_> = paths.iter().map(|p| std::fs::read(p).ok()).collect();
    let through = handle.status().through.unwrap();
    let target = SourcePosition {
        sequence: through.sequence + 1,
        ..through
    };
    let control = RequestControl::default();
    let signal = control.clone();
    let h = handle.clone();
    let waiter = thread::spawn(move || h.wait_exact(&target, &control));
    thread::sleep(Duration::from_millis(150));
    let start = Instant::now();
    signal.cancel.store(true, Ordering::Release);
    assert!(matches!(
        waiter.join().unwrap(),
        ExactPreparation::Incomplete { .. }
    ));
    assert!(start.elapsed() < Duration::from_secs(1));
    let after: Vec<_> = paths.iter().map(|p| std::fs::read(p).ok()).collect();
    assert_eq!(before, after);
    assert!(handle.shutdown(Duration::from_secs(5)));
}

struct GatePointer {
    reached: std::sync::mpsc::Sender<ActiveGeneration>,
    release: Mutex<std::sync::mpsc::Receiver<bool>>,
}
impl ActivePointerSink for GatePointer {
    fn persist_active(&self, _: &str, active: &ActiveGeneration) -> RuntimeResult<()> {
        self.reached.send(active.clone()).unwrap();
        if self.release.lock().unwrap().recv().unwrap() {
            Ok(())
        } else {
            Err(RuntimeError::Unavailable("injected".into()))
        }
    }
}
#[test]
fn persistence_precedes_commit_and_failure_keeps_registry_untrusted() {
    let (_dir, store, active) = fixture();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let pointer = Arc::new(GatePointer {
        reached: ready_tx,
        release: Mutex::new(release_rx),
    });
    let sink = Arc::new(Sink::default());
    let handle = ProjectionHandle::start(
        "order".into(),
        store.owned_published_reader(),
        active.clone(),
        pointer,
        sink.clone(),
        HealthSink::default(),
        ProjectionConfig {
            retry_initial: Duration::from_millis(30),
            retry_max: Duration::from_millis(100),
            ..ProjectionConfig::new()
        },
    )
    .unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute_batch("DROP TABLE exact_heads").unwrap();
    drop(db);
    handle.wake();
    let first = ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(handle.generations().pin().is_err());
    release_tx.send(false).unwrap();
    let second = ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_ne!(first.id, second.id);
    assert!(handle.generations().pin().is_err());
    release_tx.send(true).unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(handle.generations().pin().is_ok());
    assert!(handle.shutdown(Duration::from_secs(5)));
}

#[derive(Default)]
struct CrashAfterPersist {
    saved: Mutex<Option<ActiveGeneration>>,
}
impl ActivePointerSink for CrashAfterPersist {
    fn persist_active(&self, _: &str, active: &ActiveGeneration) -> RuntimeResult<()> {
        *self.saved.lock().unwrap() = Some(active.clone());
        panic!("simulated interruption after pointer persistence")
    }
}
#[test]
fn restart_uses_pointer_persisted_before_interrupted_commit() {
    let (_dir, store, active) = fixture();
    let crash = Arc::new(CrashAfterPersist::default());
    let sink = Arc::new(Sink::default());
    let handle = ProjectionHandle::start(
        "interrupted".into(),
        store.owned_published_reader(),
        active.clone(),
        crash.clone(),
        sink.clone(),
        HealthSink::default(),
        ProjectionConfig::new(),
    )
    .unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute_batch("DROP TABLE exact_heads").unwrap();
    drop(db);
    handle.wake();
    await_condition(|| {
        handle
            .inner
            .thread
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_finished()
    });
    let saved = crash.saved.lock().unwrap().clone().unwrap();
    assert_ne!(saved.id, active.id);
    assert!(handle.generations().pin().is_err());
    // The worker has exited before RAM commit; this start consumes the persisted value.
    let restarted = start(&store, saved, sink);
    await_condition(|| restarted.status().state == ProjectionState::Running);
    assert!(restarted.generations().pin().is_ok());
    assert!(restarted.shutdown(Duration::from_secs(5)));
}

#[test]
fn restart_does_not_retire_repair_file_still_held_by_old_reader() {
    let (_dir, store, active) = fixture();
    let first_sink = Arc::new(Sink::default());
    let handle = start(&store, active.clone(), first_sink.clone());
    await_condition(|| handle.status().state == ProjectionState::Running);
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute_batch("DROP TABLE exact_heads").unwrap();
    drop(db);
    handle.wake();
    await_condition(|| first_sink.pointers.lock().unwrap().len() == 1);
    await_condition(|| handle.status().state == ProjectionState::Running);
    let previous = first_sink.pointers.lock().unwrap()[0].clone();
    let pin = handle.generations().pin().unwrap();
    let db = Connection::open(&previous.exact_file).unwrap();
    db.execute_batch("DROP TABLE exact_heads").unwrap();
    drop(db);
    handle.wake();
    await_condition(|| first_sink.pointers.lock().unwrap().len() == 2);
    await_condition(|| handle.status().state == ProjectionState::Running);
    let mut current = first_sink.pointers.lock().unwrap()[1].clone();
    current.exact_file = std::fs::canonicalize(&current.exact_file).unwrap();
    current.lexical_file = std::fs::canonicalize(&current.lexical_file).unwrap();
    #[cfg(windows)]
    assert!(current.exact_file.to_string_lossy().starts_with(r"\\?\"));
    assert!(handle.shutdown(Duration::from_secs(5)));
    let second_sink = Arc::new(Sink::default());
    let restarted = start(&store, current, second_sink.clone());
    await_condition(|| restarted.status().state == ProjectionState::Running);
    thread::sleep(Duration::from_millis(150));
    let protected = [
        agentlaw_flows::exact::projection_path_key(&previous.exact_file),
        agentlaw_flows::exact::projection_path_key(&previous.lexical_file),
    ];
    assert!(second_sink
        .retired
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .all(|path| {
            !protected.contains(&agentlaw_flows::exact::projection_path_key(
                &projection_base(path),
            ))
        }));
    drop(pin);
    await_condition(|| {
        second_sink.retired.lock().unwrap().iter().any(|batch| {
            batch.iter().any(|path| {
                agentlaw_flows::exact::projection_path_key(path)
                    == agentlaw_flows::exact::projection_path_key(&previous.exact_file)
            })
        })
    });
    thread::sleep(Duration::from_millis(100));
    let deliveries = second_sink
        .retired
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .filter(|path| {
            agentlaw_flows::exact::projection_path_key(path)
                == agentlaw_flows::exact::projection_path_key(&previous.exact_file)
        })
        .count();
    assert_eq!(deliveries, 1);
    assert!(second_sink
        .retired
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .any(|path| {
            agentlaw_flows::exact::projection_path_key(path)
                == agentlaw_flows::exact::projection_path_key(&previous.lexical_file)
        }));
    assert!(restarted.shutdown(Duration::from_secs(5)));
}

#[test]
fn running_lexical_corruption_blocks_pins_and_rebuilds() {
    for statement in ["DROP TABLE docs", "DELETE FROM metadata"] {
        let (_dir, store, active) = fixture();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let pointer = Arc::new(GatePointer {
            reached: ready_tx,
            release: Mutex::new(release_rx),
        });
        let retired = Arc::new(Sink::default());
        let handle = ProjectionHandle::start(
            "lexical".into(),
            store.owned_published_reader(),
            active.clone(),
            pointer,
            retired,
            HealthSink::default(),
            ProjectionConfig::new(),
        )
        .unwrap();
        await_condition(|| handle.status().state == ProjectionState::Running);
        let unit = agentlaw_storage::CurrentUnit {
            entity_id: uuid::Uuid::new_v4().to_string(),
            entity_type: "memory".into(),
            state: agentlaw_storage::UnitState::Live {
                heads: vec![agentlaw_storage::Head {
                    metadata: serde_json::json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: "lexical corruption fixture".into(),
                }],
            },
        };
        store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![agentlaw_storage::Mutation {
                    unit,
                    expected_versions: vec![],
                    evidence: "test".into(),
                }],
            )
            .unwrap();
        let target = store
            .owned_published_reader()
            .read_snapshot()
            .unwrap()
            .position()
            .clone();
        handle.wake();
        assert!(matches!(
            handle.wait_exact(&target, &RequestControl::default()),
            ExactPreparation::Complete { .. }
        ));
        let db = Connection::open(&active.lexical_file).unwrap();
        db.execute_batch(statement).unwrap();
        drop(db);
        handle.wake();
        let next = ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_ne!(next.id, active.id);
        assert_eq!(handle.status().state, ProjectionState::Recovering);
        let through = handle.status().through.unwrap();
        assert!(matches!(
            handle.wait_exact(&through, &RequestControl::default()),
            ExactPreparation::Incomplete { .. }
        ));
        assert!(handle.generations().pin().is_err());
        release_tx.send(true).unwrap();
        await_condition(|| handle.status().state == ProjectionState::Running);
        assert!(handle.generations().pin().is_ok());
        assert!(ready_rx.try_recv().is_err());
        assert!(handle.shutdown(Duration::from_secs(5)));
    }
}

#[test]
fn startup_recovers_missing_or_malformed_projection_files() {
    for case in 0..4 {
        let (_dir, store, active) = fixture();
        match case {
            0 => std::fs::remove_file(&active.exact_file).unwrap(),
            1 => std::fs::remove_file(&active.lexical_file).unwrap(),
            2 => std::fs::write(&active.exact_file, b"invalid sqlite bytes").unwrap(),
            _ => std::fs::write(&active.lexical_file, b"invalid sqlite bytes").unwrap(),
        }
        let sink = Arc::new(Sink::default());
        let handle = start(&store, active.clone(), sink.clone());
        await_condition(|| handle.status().state == ProjectionState::Running);
        let saved = sink.pointers.lock().unwrap();
        assert_eq!(saved.len(), 1);
        assert_ne!(saved[0].id, active.id);
        drop(saved);
        assert!(handle.generations().pin().is_ok());
        await_condition(|| !sink.retired.lock().unwrap().is_empty());
        let retired = sink.retired.lock().unwrap().concat();
        assert!(retired.contains(&active.exact_file));
        assert!(retired.contains(&active.lexical_file));
        assert!(handle.shutdown(Duration::from_secs(5)));
    }
}

struct TimedPointer {
    clock: Arc<FakeClock>,
    attempts: Mutex<Vec<u64>>,
}
impl ActivePointerSink for TimedPointer {
    fn persist_active(&self, _: &str, _: &ActiveGeneration) -> RuntimeResult<()> {
        let mut attempts = self.attempts.lock().unwrap();
        attempts.push(self.clock.millis.load(Ordering::SeqCst));
        if attempts.len() <= 7 {
            Err(RuntimeError::Unavailable("injected".into()))
        } else {
            Ok(())
        }
    }
}
#[test]
fn wake_does_not_bypass_exponential_retry_deadlines() {
    let (_dir, store, active) = fixture();
    let db = Connection::open(&active.exact_file).unwrap();
    db.execute_batch("DROP TABLE exact_heads").unwrap();
    drop(db);
    let clock = Arc::new(FakeClock {
        base: Instant::now(),
        millis: std::sync::atomic::AtomicU64::new(0),
    });
    let pointer = Arc::new(TimedPointer {
        clock: clock.clone(),
        attempts: Mutex::new(Vec::new()),
    });
    let sink = Arc::new(Sink::default());
    let handle = ProjectionHandle::start(
        "backoff".into(),
        store.owned_published_reader(),
        active,
        pointer.clone(),
        sink,
        HealthSink::default(),
        ProjectionConfig {
            clock: clock.clone(),
            ..ProjectionConfig::new()
        },
    )
    .unwrap();
    await_condition(|| pointer.attempts.lock().unwrap().len() == 1);
    await_condition(|| handle.status().state == ProjectionState::Blocked);
    for _ in 0..5 {
        handle.wake();
    }
    thread::sleep(Duration::from_millis(100));
    assert_eq!(*pointer.attempts.lock().unwrap(), vec![0]);
    for (index, deadline) in [1000, 3000, 7000, 15000, 31000, 61000, 91000]
        .into_iter()
        .enumerate()
    {
        clock.millis.store(deadline - 1, Ordering::SeqCst);
        handle.inner.changed.notify_all();
        thread::sleep(Duration::from_millis(20));
        assert_eq!(pointer.attempts.lock().unwrap().len(), index + 1);
        clock.millis.store(deadline, Ordering::SeqCst);
        handle.inner.changed.notify_all();
        await_condition(|| pointer.attempts.lock().unwrap().len() == index + 2);
        if index < 6 {
            await_condition(|| {
                matches!(
                    handle.status().state,
                    ProjectionState::Blocked | ProjectionState::Failed
                )
            });
        }
    }
    assert_eq!(
        *pointer.attempts.lock().unwrap(),
        vec![0, 1000, 3000, 7000, 15000, 31000, 61000, 91000]
    );
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(handle.shutdown(Duration::from_secs(5)));
}

#[test]
fn startup_recovers_when_projection_directory_is_missing() {
    let (dir, store, mut active) = fixture();
    let missing = dir.path().join("not-yet-created");
    active.exact_file = missing.join("exact-old.sqlite");
    active.lexical_file = missing.join("lexical-old.sqlite");
    let sink = Arc::new(Sink::default());
    let handle = start(&store, active, sink.clone());
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(missing.is_dir());
    assert_eq!(sink.pointers.lock().unwrap().len(), 1);
    assert!(handle.generations().pin().is_ok());
    assert!(handle.shutdown(Duration::from_secs(5)));
}

#[cfg(windows)]
#[test]
fn windows_sharing_violations_retry_without_rebuild() {
    use std::os::windows::fs::OpenOptionsExt;
    for (share_mode, lexical) in [(0, false), (0, true), (1, false), (1, true)] {
        let (_dir, store, active) = fixture();
        let path = if lexical {
            &active.lexical_file
        } else {
            &active.exact_file
        };
        let guard = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(share_mode)
            .open(path)
            .unwrap();
        let clock = Arc::new(FakeClock {
            base: Instant::now(),
            millis: std::sync::atomic::AtomicU64::new(0),
        });
        let sink = Arc::new(Sink::default());
        let handle = ProjectionHandle::start(
            "sharing".into(),
            store.owned_published_reader(),
            active.clone(),
            sink.clone(),
            sink.clone(),
            HealthSink::default(),
            ProjectionConfig {
                clock: clock.clone(),
                ..ProjectionConfig::new()
            },
        )
        .unwrap();
        assert_eq!(handle.status().state, ProjectionState::Blocked);
        assert!(sink.pointers.lock().unwrap().is_empty());
        thread::sleep(Duration::from_millis(100));
        drop(guard);
        clock.millis.store(1100, Ordering::SeqCst);
        handle.inner.changed.notify_all();
        await_condition(|| handle.status().state == ProjectionState::Running);
        assert!(handle.generations().pin().is_ok());
        assert!(sink.pointers.lock().unwrap().is_empty());
        assert!(handle.shutdown(Duration::from_secs(5)));
    }
}

fn publish_repair_fixture(store: &agentlaw_storage::Store) -> SourcePosition {
    let unit = agentlaw_storage::CurrentUnit {
        entity_id: uuid::Uuid::new_v4().to_string(),
        entity_type: "memory".into(),
        state: agentlaw_storage::UnitState::Live {
            heads: vec![agentlaw_storage::Head {
                metadata: serde_json::json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: "rebuild after source interruption".into(),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![agentlaw_storage::Mutation {
                unit,
                expected_versions: vec![],
                evidence: "recovery fixture".into(),
            }],
        )
        .unwrap();
    store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone()
}

fn gated_repair(
    store: &agentlaw_storage::Store,
    active: ActiveGeneration,
) -> (
    ProjectionHandle,
    std::sync::mpsc::Receiver<ActiveGeneration>,
    std::sync::mpsc::Sender<bool>,
    HealthSink,
) {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let pointer = Arc::new(GatePointer {
        reached: ready_tx,
        release: Mutex::new(release_rx),
    });
    let health = HealthSink::default();
    let handle = ProjectionHandle::start(
        "repair-test".into(),
        store.owned_published_reader(),
        active,
        pointer,
        Arc::new(Sink::default()),
        health.clone(),
        ProjectionConfig::new(),
    )
    .unwrap();
    (handle, ready_rx, release_tx, health)
}

fn expect_repair_reason(
    handle: &ProjectionHandle,
    ready: &std::sync::mpsc::Receiver<ActiveGeneration>,
    health: &HealthSink,
    old: &ActiveGeneration,
    reason: &str,
) -> ActiveGeneration {
    let next = ready.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_ne!(next.id, old.id);
    assert_eq!(handle.status().state, ProjectionState::Recovering);
    assert!(handle.generations().pin().is_err());
    let entries = health.snapshot();
    let status = entries
        .iter()
        .find(|(id, _)| id.binding.as_deref() == Some("repair-test"))
        .unwrap();
    assert_eq!(status.1.state, ComponentState::Recovering);
    assert!(status.1.reason.as_deref().unwrap().contains(reason));
    next
}

#[test]
fn corrupt_exact_data_page_triggers_shadow_rebuild() {
    use std::io::{Seek, SeekFrom, Write};

    let (_dir, store, active) = fixture();
    let target = publish_repair_fixture(&store);
    let db = Connection::open(&active.exact_file).unwrap();
    let transaction = db.unchecked_transaction().unwrap();
    for n in 0..2000 {
        transaction
            .execute(
                "INSERT INTO exact_units(id,kind,redirect) VALUES(?1,'memory',NULL)",
                [format!("fixture-{n:08}")],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let root: u64 = db
        .query_row(
            "SELECT rootpage FROM sqlite_master WHERE name='exact_units'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let page_size: u64 = db
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap();
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop(db);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&active.exact_file)
        .unwrap();
    file.seek(SeekFrom::Start((root - 1) * page_size)).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let check = Connection::open_with_flags(
        &active.exact_file,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let schema: i64 = check
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
        .unwrap();
    assert!(schema > 0);
    let error = check
        .query_row("SELECT kind FROM exact_units LIMIT 1", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap_err();
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(ref code, _) if code.code == rusqlite::ErrorCode::DatabaseCorrupt)
    );
    drop(check);
    drop(agentlaw_flows::exact::ExactIndexWriter::open(&active.exact_file).unwrap());
    let (handle, ready, release, health) = gated_repair(&store, active.clone());
    let _ = expect_repair_reason(&handle, &ready, &health, &active, "exact_index_corrupt");
    release.send(true).unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(matches!(
        handle.wait_exact(&target, &RequestControl::default()),
        ExactPreparation::Complete { .. }
    ));
    assert!(handle.generations().pin().is_ok());
    assert!(handle.shutdown(Duration::from_secs(5)));
}

#[test]
fn source_epoch_recreated_after_loss_rebuilds_generation() {
    let (dir, store, active) = fixture();
    let (handle, ready, release, health) = gated_repair(&store, active.clone());
    await_condition(|| handle.status().state == ProjectionState::Running);
    std::fs::remove_file(dir.path().join("local/source-epoch")).unwrap();
    let reopened =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("local")).unwrap();
    let target = reopened
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .clone();
    assert_ne!(target.epoch, active.epoch);
    handle.wake();
    let next = expect_repair_reason(&handle, &ready, &health, &active, "source_epoch_changed");
    assert_eq!(next.epoch, target.epoch);
    release.send(true).unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(matches!(
        handle.wait_exact(&target, &RequestControl::default()),
        ExactPreparation::Complete { .. }
    ));
    assert!(handle.shutdown(Duration::from_secs(5)));
}

#[test]
fn lost_unapplied_publication_ledger_rebuilds_generation() {
    let (dir, store, active) = fixture();
    let (handle, ready, release, health) = gated_repair(&store, active.clone());
    await_condition(|| handle.status().state == ProjectionState::Running);
    let target = publish_repair_fixture(&store);
    let journal = Connection::open(dir.path().join("local/journal.sqlite")).unwrap();
    assert_eq!(journal.execute("DELETE FROM publications", []).unwrap(), 1);
    drop(journal);
    handle.wake();
    let next = expect_repair_reason(&handle, &ready, &health, &active, "source_coverage_lost");
    assert_eq!(next.epoch, target.epoch);
    release.send(true).unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(matches!(
        handle.wait_exact(&target, &RequestControl::default()),
        ExactPreparation::Complete { .. }
    ));
    assert!(handle.generations().pin().is_ok());
    assert!(handle.shutdown(Duration::from_secs(5)));
}

#[test]
fn restart_recovers_ordinary_generations_and_sidecars_after_interruption() {
    for hold_reader in [true, false] {
        let (_dir, store, active) = fixture();
        let first_sink = Arc::new(Sink::default());
        first_sink.fail_retire.store(100, Ordering::SeqCst);
        let first = start(&store, active.clone(), first_sink.clone());
        await_condition(|| first.status().state == ProjectionState::Running);
        let pinned = if hold_reader {
            Some(first.generations().pin().unwrap())
        } else {
            None
        };
        let db = Connection::open(&active.exact_file).unwrap();
        db.execute_batch("DROP TABLE exact_heads").unwrap();
        drop(db);
        first.wake();
        await_condition(|| first_sink.pointers.lock().unwrap().len() == 1);
        await_condition(|| first.status().state == ProjectionState::Running);
        let current = first_sink.pointers.lock().unwrap()[0].clone();
        if hold_reader {
            assert!(first_sink.retired.lock().unwrap().is_empty());
        } else {
            await_condition(|| !first_sink.retired.lock().unwrap().is_empty());
        }
        assert!(first.shutdown(Duration::from_secs(5)));
        drop(first);

        let mut expected = vec![active.exact_file.clone(), active.lexical_file.clone()];
        if !hold_reader {
            // Simulate sidecars left by an interrupted process after the databases close.
            for base in [&active.exact_file, &active.lexical_file] {
                for suffix in ["-wal", "-shm"] {
                    let mut name = base.as_os_str().to_os_string();
                    name.push(suffix);
                    let sidecar = PathBuf::from(name);
                    if !sidecar.exists() {
                        std::fs::write(&sidecar, b"interrupted sidecar").unwrap();
                    }
                    expected.push(sidecar);
                }
            }
        }
        let second_sink = Arc::new(Sink::default());
        let second = start(&store, current.clone(), second_sink.clone());
        await_condition(|| second.status().state == ProjectionState::Running);
        if hold_reader {
            thread::sleep(Duration::from_millis(150));
            assert!(second_sink
                .retired
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .all(|path| {
                    let key = agentlaw_flows::exact::projection_path_key(&projection_base(path));
                    key != agentlaw_flows::exact::projection_path_key(&active.exact_file)
                        && key != agentlaw_flows::exact::projection_path_key(&active.lexical_file)
                }));
            drop(pinned);
        }
        await_condition(|| {
            let files = second_sink.retired.lock().unwrap().concat();
            expected.iter().all(|path| files.contains(path))
        });
        let files = second_sink.retired.lock().unwrap().concat();
        assert!(files.iter().all(|path| {
            let key = agentlaw_flows::exact::projection_path_key(&projection_base(path));
            key != agentlaw_flows::exact::projection_path_key(&current.exact_file)
                && key != agentlaw_flows::exact::projection_path_key(&current.lexical_file)
        }));
        assert!(second.shutdown(Duration::from_secs(5)));
    }
}
