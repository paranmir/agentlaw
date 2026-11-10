use super::*;
use rusqlite::Connection;
use std::sync::atomic::{AtomicUsize, Ordering};

fn full_disk() -> RuntimeError {
    #[cfg(windows)]
    let code = 112;
    #[cfg(not(windows))]
    let code = 28;
    RuntimeError::Unavailable(std::io::Error::from_raw_os_error(code).to_string())
}

fn sharing_violation() -> RuntimeError {
    #[cfg(windows)]
    let code = 32;
    #[cfg(not(windows))]
    let code = 13;
    RuntimeError::Unavailable(std::io::Error::from_raw_os_error(code).to_string())
}

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
            return Err(full_disk());
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
            return Err(sharing_violation());
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
fn corrupt_page(path: &Path, table: Option<&str>) {
    use std::io::{Seek, SeekFrom, Write};

    let db = Connection::open(path).unwrap();
    let page_size: u64 = db
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap();
    let offset = if let Some(table) = table {
        let root: u64 = db
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        (root - 1) * page_size
    } else {
        100
    };
    let (busy, _, _): (i64, i64, i64) = db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(
        busy, 0,
        "test setup must checkpoint the page before damaging it"
    );
    drop(db);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();
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
    assert_eq!(
        handle.generations().pin().err().unwrap().code,
        "exact_not_ready"
    );
}
#[test]
fn corrupt_start_recovers_retries_pointer_and_restores_saved_pointer() {
    let (_dir, store, active) = fixture();
    let sink = Arc::new(Sink::default());
    sink.fail_pointer.store(1, Ordering::SeqCst);
    corrupt_page(&active.lexical_file, Some("metadata"));
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
    corrupt_page(&active.lexical_file, Some("metadata"));
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
            Err(full_disk())
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
    corrupt_page(&active.lexical_file, Some("metadata"));
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

struct ExitAfterPersist {
    path: PathBuf,
    registry: Mutex<Option<Arc<ProjectionGenerations>>>,
}
impl ActivePointerSink for ExitAfterPersist {
    fn persist_active(&self, _: &str, active: &ActiveGeneration) -> RuntimeResult<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&self.path).unwrap();
        writeln!(
            file,
            "{}\n{}\n{}\n{}",
            active.id,
            active.epoch,
            active.exact_file.display(),
            active.lexical_file.display()
        )
        .unwrap();
        file.sync_all().unwrap();
        assert!(self
            .registry
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .pin()
            .is_err());
        std::process::exit(75);
    }
}
#[test]
fn restart_uses_pointer_persisted_before_interrupted_commit() {
    const CHILD_DIR: &str = "AGENTLAW_TEST_POINTER_CHILD_DIR";
    if let Some(path) = std::env::var_os(CHILD_DIR) {
        let dir = PathBuf::from(path);
        let store = agentlaw_storage::Store::open(dir.join("source"), dir.join("local")).unwrap();
        let pos = store
            .owned_published_reader()
            .read_snapshot()
            .unwrap()
            .position()
            .clone();
        let active = ActiveGeneration {
            id: "old".into(),
            epoch: pos.epoch,
            exact_file: dir.join("exact-old.sqlite"),
            lexical_file: dir.join("lexical-old.sqlite"),
        };
        let pointer = Arc::new(ExitAfterPersist {
            path: dir.join("saved-pointer"),
            registry: Mutex::new(None),
        });
        let handle = ProjectionHandle::start(
            "interrupted".into(),
            store.owned_published_reader(),
            active.clone(),
            pointer.clone(),
            Arc::new(Sink::default()),
            HealthSink::default(),
            ProjectionConfig::new(),
        )
        .unwrap();
        await_condition(|| handle.status().state == ProjectionState::Running);
        *pointer.registry.lock().unwrap() = Some(handle.generations());
        corrupt_page(&active.lexical_file, Some("metadata"));
        handle.wake();
        panic_after_timeout(&handle);
    }
    let (dir, store, active) = fixture();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "projection::tests::restart_uses_pointer_persisted_before_interrupted_commit",
        ])
        .env(CHILD_DIR, dir.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(75));
    let persisted = std::fs::read_to_string(dir.path().join("saved-pointer")).unwrap();
    let mut fields = persisted.lines();
    let saved = ActiveGeneration {
        id: fields.next().unwrap().to_owned(),
        epoch: fields.next().unwrap().to_owned(),
        exact_file: PathBuf::from(fields.next().unwrap()),
        lexical_file: PathBuf::from(fields.next().unwrap()),
    };
    assert_ne!(saved.id, active.id);
    let restarted = start(&store, saved, Arc::new(Sink::default()));
    await_condition(|| restarted.status().state == ProjectionState::Running);
    assert!(restarted.generations().pin().is_ok());
    assert!(restarted.shutdown(Duration::from_secs(5)));
}

fn panic_after_timeout(handle: &ProjectionHandle) -> ! {
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
    panic!("worker exited without persisting its pointer");
}

#[test]
fn restart_does_not_retire_repair_file_still_held_by_old_reader() {
    let (_dir, store, active) = fixture();
    let first_sink = Arc::new(Sink::default());
    let handle = start(&store, active.clone(), first_sink.clone());
    await_condition(|| handle.status().state == ProjectionState::Running);
    corrupt_page(&active.lexical_file, Some("metadata"));
    handle.wake();
    await_condition(|| first_sink.pointers.lock().unwrap().len() == 1);
    await_condition(|| handle.status().state == ProjectionState::Running);
    let previous = first_sink.pointers.lock().unwrap()[0].clone();
    let pin = handle.generations().pin().unwrap();
    corrupt_page(&previous.lexical_file, Some("metadata"));
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
    for table in [None, Some("metadata")] {
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
        corrupt_page(&active.lexical_file, table);
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
            Err(full_disk())
        } else {
            Ok(())
        }
    }
}
#[test]
fn wake_does_not_bypass_exponential_retry_deadlines() {
    let (_dir, store, active) = fixture();
    corrupt_page(&active.lexical_file, Some("metadata"));
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
        assert_eq!(
            handle.generations().pin().err().unwrap().code,
            "exact_not_ready"
        );
        assert!(sink.pointers.lock().unwrap().is_empty());
        thread::sleep(Duration::from_millis(100));
        drop(guard);
        // The files are accessible again, but no writer has been reopened yet.
        assert_eq!(
            handle.generations().pin().err().unwrap().code,
            "exact_not_ready"
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while handle.status().state != ProjectionState::Running {
            assert!(Instant::now() < deadline, "reopening the writer timed out");
            clock.millis.fetch_add(1000, Ordering::SeqCst);
            handle.inner.changed.notify_all();
            thread::sleep(Duration::from_millis(10));
        }
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

#[cfg(windows)]
#[test]
fn concurrent_pin_observes_generation_only_after_writer_is_open() {
    use std::os::windows::fs::OpenOptionsExt;

    let (_dir, store, active) = fixture();
    let (handle, ready, release, _) = gated_repair(&store, active.clone());
    await_condition(|| handle.status().state == ProjectionState::Running);
    corrupt_page(&active.lexical_file, Some("metadata"));
    handle.wake();
    let next = ready.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_ne!(next.id, active.id);

    let mut sidecars = Vec::new();
    for path in [&next.exact_file, &next.lexical_file] {
        // No reader has pinned this generation yet: this handle is the writer.
        assert!(std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .is_err());
        for suffix in ["-wal", "-shm"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            let sidecar = PathBuf::from(name);
            assert!(sidecar.is_file());
            sidecars.push(sidecar);
        }
    }
    let registry = handle.generations();
    let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
    let (pinned_tx, pinned_rx) = std::sync::mpsc::channel();
    let (drop_tx, drop_rx) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        assert_eq!(registry.pin().err().unwrap().code, "exact_not_ready");
        attempted_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let pin = loop {
            match registry.pin() {
                Ok(pin) => break pin,
                Err(error) if error.code == "exact_not_ready" => {}
                Err(error) => panic!("unexpected pin error: {error}"),
            }
            assert!(Instant::now() < deadline, "generation was not published");
            thread::yield_now();
        };
        pinned_tx.send(()).unwrap();
        drop_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        drop(pin);
    });
    attempted_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    release.send(true).unwrap();
    pinned_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    assert!(sidecars.iter().all(|path| path.is_file()));
    drop_tx.send(()).unwrap();
    reader.join().unwrap();
    await_condition(|| handle.status().state == ProjectionState::Running);
    assert!(handle.shutdown(Duration::from_secs(5)));
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
    let lost = dir.path().join("lost-ledger");
    std::fs::create_dir(&lost).unwrap();
    for name in ["journal.sqlite-wal", "journal.sqlite-shm", "journal.sqlite"] {
        let path = dir.path().join("local").join(name);
        if path.exists() {
            std::fs::rename(&path, lost.join(name)).unwrap();
        }
    }
    assert!(lost.join("journal.sqlite").is_file());
    let source = store.owned_published_reader();
    let guard = source.read_snapshot().unwrap();
    assert_eq!(guard.position(), &target);
    let before = SourcePosition {
        epoch: target.epoch.clone(),
        sequence: target.sequence - 1,
    };
    assert!(matches!(
        guard.read_published_paths(&before, 1),
        Err(agentlaw_storage::Error::CoverageLost)
    ));
    drop(guard);
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
    const CHILD_FILE: &str = "AGENTLAW_TEST_INTERRUPTED_SIDECAR_FILE";
    if let Some(path) = std::env::var_os(CHILD_FILE) {
        let db = Connection::open(path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; BEGIN IMMEDIATE; UPDATE exact_position SET sequence=sequence; COMMIT;").unwrap();
        std::process::exit(74);
    }
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
        corrupt_page(&active.lexical_file, Some("metadata"));
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
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "projection::tests::restart_recovers_ordinary_generations_and_sidecars_after_interruption"])
                .env(CHILD_FILE, &active.exact_file)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(74));
            for suffix in ["-wal", "-shm"] {
                let mut name = active.exact_file.as_os_str().to_os_string();
                name.push(suffix);
                let sidecar = PathBuf::from(name);
                assert!(sidecar.is_file());
                expected.push(sidecar);
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
