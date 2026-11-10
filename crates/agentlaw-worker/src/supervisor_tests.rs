use super::*;
use crate::{Admission, JobKey, DIMENSIONS};

#[test]
fn live_child_remains_when_demand_disappears() {
    let now = Instant::now();
    let mut lifecycle = ModelLifecycle::default();
    lifecycle.started();
    lifecycle.ready(now);
    assert_eq!(lifecycle.action(now, false, true), LifecycleAction::None);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
}

#[test]
fn model_request_ids_are_shared_and_checked() {
    let ids = ModelRequestIds::new();
    let other = ids.clone();
    assert_eq!(ids.next(), Some(ModelRequestId(1)));
    assert_eq!(other.next(), Some(ModelRequestId(2)));
    *ids.counter.lock().unwrap() = u64::MAX - 1;
    assert_eq!(other.next(), Some(ModelRequestId(u64::MAX)));
    assert_eq!(ids.next(), None);
    assert_eq!(other.next(), None);
}

#[test]
fn model_protocol_preserves_full_unsigned_request_id() {
    let request = Input::Embed {
        id: u64::MAX,
        attempt: 1,
        text: "query".into(),
    };
    let frame = model_child::encode(&request).unwrap();
    let decoded: Input = model_child::read(&mut frame.as_slice()).unwrap();
    assert!(matches!(decoded, Input::Embed { id: u64::MAX, .. }));
}

#[test]
fn lifecycle_retries_on_exit_without_new_demand_and_caps_backoff() {
    let origin = Instant::now();
    let mut lifecycle = ModelLifecycle::default();
    assert_eq!(lifecycle.action(origin, true, true), LifecycleAction::Start);
    lifecycle.started();
    lifecycle.ready(origin);
    let mut now = origin;
    for delay in [1, 2, 4, 8, 16, 30, 30] {
        lifecycle.exited(now, "exit".into());
        assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
        now += Duration::from_secs(delay);
        assert_eq!(lifecycle.action(now, true, true), LifecycleAction::Start);
        lifecycle.started();
    }
    lifecycle.ready(now);
    now += Duration::from_secs(60);
    lifecycle.exited(now, "exit".into());
    assert_eq!(
        lifecycle.action(now + Duration::from_secs(1), true, true),
        LifecycleAction::Start
    );
}

#[test]
fn lifecycle_cancels_retry_and_never_overlaps_owned_child() {
    let now = Instant::now();
    let mut lifecycle = ModelLifecycle::default();
    assert_eq!(lifecycle.action(now, true, false), LifecycleAction::None);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::Start);
    lifecycle.started();
    assert_eq!(lifecycle.action(now, false, true), LifecycleAction::None);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
    lifecycle.stopped();
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::Start);
    lifecycle.started();
    lifecycle.exited(now, "missing model".into());
    assert_eq!(lifecycle.action(now, false, true), LifecycleAction::None);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
    assert_eq!(
        lifecycle.action(now + Duration::from_secs(1), true, true),
        LifecycleAction::Start
    );
}

#[test]
fn queries_share_one_computation_and_cancel_independently() {
    let ids = ModelRequestIds::new();
    let mut queue = QueryQueue::default();
    let first = queue.submit("model", "same", &ids).unwrap();
    assert_eq!(queue.submit("model", "same", &ids).unwrap(), first);
    assert_eq!(queue.take(), Some(first));
    queue.release(first);
    assert_eq!(queue.flights[&first].waiters, 1);
    queue.release(first);
    assert!(
        queue.flights.contains_key(&first),
        "running work owns its slot until it completes"
    );
    queue.complete(first, Err(QueryError::Failed("exit".into())));
    assert!(!queue.flights.contains_key(&first));
}

#[test]
fn completed_query_is_not_overwritten_by_later_child_exit() {
    let ids = ModelRequestIds::new();
    let mut queue = QueryQueue::default();
    let id = queue.submit("model", "text", &ids).unwrap();
    queue.take();
    let mut vector = vec![0.0; DIMENSIONS];
    vector[0] = 1.0;
    let result = validate_query_vector("model".into(), vector).unwrap();
    queue.complete(id, Ok(result.clone()));
    queue.fail_all("child exited");
    assert_eq!(queue.flights[&id].result, Some(Ok(result)));
}

#[test]
fn query_capacity_and_vector_validation_are_explicit() {
    let ids = ModelRequestIds::new();
    let mut queue = QueryQueue::default();
    for i in 0..64 {
        queue.submit("model", &format!("{i}"), &ids).unwrap();
    }
    assert_eq!(
        queue.submit("model", "overflow", &ids),
        Err(QueryError::Busy)
    );
    assert_eq!(queue.submit("model", "1", &ids), Ok(ModelRequestId(2)));
    assert!(validate_query_vector("model".into(), vec![0.0; DIMENSIONS]).is_err());
    assert!(validate_query_vector("model".into(), vec![f32::NAN; DIMENSIONS]).is_err());
    assert!(validate_query_vector("model".into(), vec![1.0]).is_err());
    let mut vector = vec![0.0; DIMENSIONS];
    vector[0] = 1.0;
    assert_eq!(
        validate_query_vector("actual".into(), vector)
            .unwrap()
            .model_digest,
        "actual"
    );
}

#[derive(Default)]
struct FakeRamIndex {
    calls: std::sync::Mutex<usize>,
    work: std::sync::Mutex<Option<IndexWork>>,
    finished: std::sync::Mutex<Vec<ModelRequestId>>,
}
impl IndexWorkPort for FakeRamIndex {
    fn model_ready(&self, _: &str) {}
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        *self.calls.lock().unwrap() += 1;
        self.work.lock().unwrap().take()
    }
    fn finish(&self, id: ModelRequestId, _: std::result::Result<Vec<f32>, EmbeddingError>) {
        self.finished.lock().unwrap().push(id);
    }
    fn status(&self) -> IndexWorkStatus {
        IndexWorkStatus {
            runnable: self.work.lock().unwrap().is_some(),
            next_retry_at: None,
            awaiting_model: false,
        }
    }
}

#[test]
fn selection_cache_does_not_rewrite_unchanged_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("selection.json");
    let cache = SelectionCache::open(path.clone());
    let selection = crate::execution::Selection {
        cacheable: true,
        backend: crate::execution::ExecutionBackend::Cpu,
        cpu_ns: vec![1, 2, 3],
        gpu_ns: vec![],
        reason: "test".into(),
        host_resident_bytes: None,
        gpu_dedicated_bytes: None,
        gpu_shared_bytes: None,
    };
    cache.save("fingerprint".into(), selection.clone());
    cache.flush();
    assert!(path.exists());
    std::fs::remove_file(&path).unwrap();
    cache.save("fingerprint".into(), selection);
    cache.flush();
    assert!(!path.exists(), "unchanged content was written twice");
}

#[test]
fn idle_supervisor_does_not_create_files() {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: "missing-executable".into(),
        model: None,
    };
    let mut supervisor = ModelSupervisor::start(
        config,
        Arc::new(FakeRamIndex::default()),
        ModelRequestIds::new(),
    );
    assert_eq!(supervisor.wait_terminated(Duration::ZERO), None);
    assert!(matches!(
        supervisor.embed_query("query", &AtomicBool::new(false)),
        Err(QueryError::Unavailable(_))
    ));
    supervisor.set_warm(true);
    supervisor.wake();
    supervisor.stop().unwrap();
    assert_eq!(
        supervisor.wait_terminated(Duration::ZERO).as_deref(),
        Some("model supervisor stopped")
    );
    assert_eq!(supervisor.snapshot().state, WorkerState::Stopped);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn idle_supervisor_does_not_create_files_or_start_unconfigured_model() {
    struct Process(Arc<std::sync::atomic::AtomicUsize>);
    impl WorkerProcessPort for Process {
        fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err("unexpected spawn".into())
        }
        fn observe(
            &self,
            _: &Child,
            _: String,
            _: mpsc::Sender<Event>,
            _: Arc<dyn Fn() + Send + Sync>,
        ) -> Result<()> {
            unreachable!()
        }
        fn stop(&self, _: &mut Child) -> Result<()> {
            unreachable!()
        }
        fn control(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<u64>> {
            unreachable!()
        }
    }
    struct Index(mpsc::Sender<()>);
    impl IndexWorkPort for Index {
        fn model_ready(&self, _: &str) {
            unreachable!()
        }
        fn next_work(&self, _: &str) -> Option<IndexWork> {
            unreachable!()
        }
        fn finish(&self, _: ModelRequestId, _: std::result::Result<Vec<f32>, EmbeddingError>) {
            unreachable!()
        }
        fn status(&self) -> IndexWorkStatus {
            let _ = self.0.send(());
            IndexWorkStatus {
                runnable: true,
                next_retry_at: None,
                awaiting_model: false,
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: "missing".into(),
        model: None,
    };
    let (events, incoming) = mpsc::channel();
    let shared = Arc::new(SupervisorShared {
        state: std::sync::Mutex::new(SupervisorState {
            snapshot: ModelSnapshot {
                state: WorkerState::Stopped,
                incarnation: None,
                model_digest: None,
                cause: None,
            },
            warm: true,
            stop: false,
            loading_demand: false,
            queries: QueryQueue::default(),
            terminated: None,
        }),
        signal: Arc::new(RamSignal {
            events: events.clone(),
        }),
        changed: std::sync::Condvar::new(),
        events,
        wake_pending: AtomicBool::new(false),
    });
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (status_tx, status_rx) = mpsc::channel();
    let worker_shared = shared.clone();
    let process = Process(count.clone());
    let thread = std::thread::spawn(move || {
        model_supervision_loop(
            config,
            Arc::new(Index(status_tx)),
            worker_shared,
            process,
            Instant::now,
            incoming,
        )
    });
    let mut supervisor = ModelSupervisor {
        shared,
        thread: Some(thread),
        ids: ModelRequestIds::new(),
        stop_result: None,
    };
    status_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    supervisor.stop().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

fn ready(b: &mut Broker) -> String {
    let inc = b
        .reserve_worker_start("model-a", now())
        .unwrap()
        .incarnation;
    b.transition(&inc, WorkerState::Loading, now()).unwrap();
    b.transition(&inc, WorkerState::Ready, now()).unwrap();
    inc
}
fn enqueue(b: &mut Broker, name: &str, durable: bool, lease: Option<&str>) -> i64 {
    let result = b
        .enqueue(
            &JobKey {
                model_digest: "model-a".into(),
                config_digest: "config".into(),
                memory_id: name.into(),
                change_id: "v1".into(),
                content_digest: name.into(),
                section: "body".into(),
            },
            name,
            durable,
            lease.is_some(),
            lease,
        )
        .unwrap();
    match result {
        Admission::Accepted(id) | Admission::Joined(id) => id,
    }
}
fn state(b: &Broker, id: i64) -> String {
    b.connection
        .query_row("SELECT state FROM jobs WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

#[test]
fn model_failure_requires_current_ready_incarnation_before_becoming_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Broker::open(dir.path().join("broker.sqlite"), 64).unwrap();
    initialize(&b).unwrap();
    diagnostic(&b, "model_load", "old model failure").unwrap();
    diagnostic(&b, "vector_index:other", "independent vector failure").unwrap();
    let inc = b
        .reserve_worker_start("model-a", now())
        .unwrap()
        .incarnation;
    b.transition(&inc, WorkerState::Loading, now()).unwrap();
    assert!(!resolve_diagnostic(&mut b, "model_load", Some(&inc)).unwrap());
    b.transition(&inc, WorkerState::Ready, now()).unwrap();
    assert!(!resolve_diagnostic(&mut b, "model_load", Some("older-incarnation")).unwrap());
    assert!(resolve_diagnostic(&mut b, "model_load", Some(&inc)).unwrap());
    assert!(!resolve_diagnostic(&mut b, "model_load", Some(&inc)).unwrap());
    let before = inspect_runtime(dir.path()).unwrap();
    assert!(!before["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["name"] == "model_load"));
    assert!(before["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["name"] == "vector_index:other"));
    let history = before["diagnostic_history"].as_array().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["name"], "model_load");
    assert_eq!(history[0]["detail"], "old model failure");
    assert!(history[0]["resolved_at_ms"].as_i64().is_some());
    assert_eq!(
        inspect_runtime(dir.path()).unwrap(),
        before,
        "diagnosis must be read-only"
    );
    diagnostic(&b, "model_load", "new model failure").unwrap();
    let after = inspect_runtime(dir.path()).unwrap();
    assert!(after["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["name"] == "model_load" && d["detail"] == "new model failure"));
    assert_eq!(
        after["diagnostic_history"][0]["detail"],
        "old model failure"
    );
}

#[test]
fn lost_worker_retries_only_its_unfinished_claim_and_preserves_results_and_ack() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broker.sqlite");
    let mut b = Broker::open(&path, 64).unwrap();
    initialize(&b).unwrap();
    let inc = ready(&mut b);
    let mut vector = vec![0.; DIMENSIONS];
    vector[0] = 1.;
    let ack = enqueue(&mut b, "already indexed", true, None);
    let job = b.claim(&inc, now()).unwrap().unwrap();
    b.complete(&job, &vector).unwrap();
    b.acknowledge(ack).unwrap();
    let completed = enqueue(&mut b, "finished inference", true, None);
    let job = b.claim(&inc, now()).unwrap().unwrap();
    b.complete(&job, &vector).unwrap();
    let permanent = enqueue(&mut b, "bad input", true, None);
    let job = b.claim(&inc, now()).unwrap().unwrap();
    b.fail(&job, "invalid input", false, now()).unwrap();
    let unfinished = enqueue(&mut b, "interrupted inference", true, None);
    let stale_job = b.claim(&inc, now()).unwrap().unwrap();
    let queued = enqueue(&mut b, "not started", true, None);
    b.renew_lease("foreground", false, now(), 60000).unwrap();
    let waiter = enqueue(&mut b, "foreground query", false, Some("foreground"));

    lose_claims(&mut b, &inc, "native process exited").unwrap();
    assert_eq!(state(&b, ack), "acknowledged");
    assert_eq!(state(&b, completed), "ready_to_index");
    assert_eq!(state(&b, permanent), "failed");
    assert_eq!(state(&b, unfinished), "retry_wait");
    assert_eq!(state(&b, queued), "queued");
    assert_eq!(state(&b, waiter), "failed");
    assert_eq!(b.job_result(completed).unwrap(), Some(vector.clone()));
    let cause: String = b
        .connection
        .query_row(
            "SELECT cause FROM waiter_failures WHERE job=?1 AND lease='foreground'",
            [waiter],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cause, "native process exited");
    drop(b);

    let mut b = Broker::open(&path, 64).unwrap();
    let new_inc = ready(&mut b);
    assert!(matches!(
        b.complete(&stale_job, &vector),
        Err(crate::Error::ObsoleteIncarnation)
    ));
    let retry = b.claim(&new_inc, now()).unwrap().unwrap();
    assert_eq!(retry.id, unfinished);
    assert_eq!(retry.attempts, stale_job.attempts + 1);
    b.complete(&retry, &vector).unwrap();
    assert_eq!(
        state(&b, permanent),
        "failed",
        "a restart is not blanket retry permission"
    );
    assert_eq!(state(&b, ack), "acknowledged");
}

#[test]
fn restart_cooldown_survives_reopen_and_leases_cannot_reset_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broker.sqlite");
    let b = Broker::open(&path, 64).unwrap();
    initialize(&b).unwrap();
    let (mut policy, _) = Recovery::load(&b, "same-config".into()).unwrap();
    for delay in [1000, 2000, 300000] {
        let before = now();
        policy.failed(&b).unwrap();
        let deadline: i64 = b
            .connection
            .query_row(
                "SELECT next_at FROM worker_recovery WHERE config='same-config'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(deadline >= before + delay && deadline <= now() + delay);
    }
    drop(b);
    let b = Broker::open(&path, 64).unwrap();
    b.renew_lease("new-client", false, now(), 60000).unwrap();
    let (policy, slot) = Recovery::load(&b, "same-config".into()).unwrap();
    assert_eq!(policy.streak, 3);
    let Slot::Waiting(until) = slot else {
        panic!("cooldown lost after restart")
    };
    assert!(until.saturating_duration_since(Instant::now()) > Duration::from_secs(290));
    let (other, _) = Recovery::load(&b, "different-config".into()).unwrap();
    assert_eq!(other.streak, 0);
}

#[test]
fn recovery_demand_is_scoped_to_live_leases_or_the_configured_models_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Broker::open(dir.path().join("broker.sqlite"), 64).unwrap();
    initialize(&b).unwrap();
    b.connection
        .execute(
            "INSERT INTO worker_model_configs VALUES('config-a','model-a'),('config-b','model-b')",
            [],
        )
        .unwrap();
    assert!(!demand(&b, "config-a").unwrap());
    b.renew_lease("expired", false, now() - 100, 1).unwrap();
    assert!(!demand(&b, "config-a").unwrap());
    b.renew_lease("op:in-progress", true, now(), 60000).unwrap();
    assert!(demand(&b, "config-a").unwrap());
    b.release_lease("op:in-progress").unwrap();
    enqueue(&mut b, "durable indexing", true, None);
    assert!(demand(&b, "config-a").unwrap());
    assert!(
        !demand(&b, "config-b").unwrap(),
        "another model's work must not keep this model resident"
    );
}

#[test]
fn late_attempt_in_same_incarnation_cannot_replace_retry_result() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Broker::open(dir.path().join("broker.sqlite"), 64).unwrap();
    let inc = ready(&mut b);
    enqueue(&mut b, "retryable input", true, None);
    let first = b.claim(&inc, 10).unwrap().unwrap();
    b.fail(&first, "transient failure", true, 10).unwrap();
    let second = b.claim(&inc, 1010).unwrap().unwrap();
    assert_eq!(second.attempts, 2);
    let mut old = vec![0.; DIMENSIONS];
    old[0] = 1.;
    let mut new = vec![0.; DIMENSIONS];
    new[1] = 1.;
    assert!(b.complete(&first, &old).is_err());
    b.complete(&second, &new).unwrap();
    assert_eq!(b.job_result(second.id).unwrap(), Some(new));
}
