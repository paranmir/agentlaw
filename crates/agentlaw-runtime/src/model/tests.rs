use super::*;
use crate::host::{lifecycle::Lifecycle, supervision::Supervisor};
use agentlaw_worker::supervisor::{IndexWork, IndexWorkStatus, ModelRequestId};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

struct PanicOnce {
    calls: AtomicUsize,
    fail: AtomicBool,
}
impl IndexWorkPort for PanicOnce {
    fn model_ready(&self, _: &str) {}
    fn next_work(&self, _: &str) -> Option<IndexWork> {
        None
    }
    fn finish(&self, _: ModelRequestId, _: Result<Vec<f32>, agentlaw_worker::EmbeddingError>) {}
    fn status(&self) -> IndexWorkStatus {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(!self.fail.swap(false, Ordering::SeqCst), "index failed");
        IndexWorkStatus {
            runnable: false,
            next_retry_at: None,
            awaiting_model: false,
        }
    }
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn config() -> RuntimeConfig {
    RuntimeConfig {
        state_dir: std::env::temp_dir(),
        executable: std::env::current_exe().unwrap(),
        model: None,
    }
}
fn model_id() -> ComponentId {
    ComponentId {
        binding: None,
        kind: ComponentKind::Model,
    }
}

#[test]
fn mapping_preserves_worker_results() {
    assert_eq!(map_error(QueryError::Busy), SemanticError::Busy);
    assert_eq!(map_error(QueryError::Cancelled), SemanticError::Cancelled);
    assert_eq!(
        map_error(QueryError::Unavailable("offline".into())),
        SemanticError::Unavailable("offline".into())
    );
    assert_eq!(
        map_error(QueryError::Failed("bad".into())),
        SemanticError::Failed("bad".into())
    );
    let vector = map_vector(agentlaw_worker::supervisor::QueryVector {
        model_digest: "digest".into(),
        vector: vec![1.0, 2.0],
    });
    assert_eq!(vector.model, "digest");
    assert_eq!(&*vector.values, &[1.0, 2.0]);
}

#[test]
fn cancelled_without_supervisor_creates_no_demand() {
    let handle = ModelHandle::new();
    let control = RequestControl::default();
    control.cancel.store(true, Ordering::Release);
    assert_eq!(
        handle.embed_query("cancelled", &control),
        Err(SemanticError::Cancelled)
    );
    assert!(!handle.inner.lock().unwrap().demand);
}

#[test]
fn terminated_supervisor_retains_query_demand() {
    let handle = ModelHandle::new();
    let index = Arc::new(PanicOnce {
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(true),
    });
    let old = Arc::new(ModelSupervisor::start(
        config(),
        index.clone(),
        ModelRequestIds::new(),
    ));
    assert!(old.wait_terminated(Duration::from_secs(5)).is_some());
    handle.inner.lock().unwrap().supervisor = Some(old.clone());
    assert!(matches!(
        handle.embed_query("query", &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    assert!(handle.inner.lock().unwrap().demand);
    handle.inner.lock().unwrap().supervisor = None;
    assert!(Arc::try_unwrap(old).ok().unwrap().stop().is_err());
    assert_replacement_receives_demand(handle, index);
}

fn assert_replacement_receives_demand(handle: ModelHandle, index: Arc<PanicOnce>) {
    let health = HealthSink::default();
    let host = Supervisor::new(health.clone());
    let component_handle = handle.clone();
    let component_health = health.clone();
    host.spawn(model_id(), move |stop| {
        run_model_component(
            config(),
            index.clone(),
            ModelRequestIds::new(),
            component_health.clone(),
            component_handle.clone(),
            stop,
        )
    })
    .unwrap();
    wait_for(|| handle.inner.lock().unwrap().supervisor.is_some());
    let state = handle.inner.lock().unwrap();
    let replacement = state.supervisor.as_ref().unwrap();
    assert!(!replacement.is_warm());
    assert!(replacement.has_query_demand());
    drop(state);
    host.shutdown();
}

#[test]
fn absent_supervisor_delivers_demand_to_cold_replacement() {
    let handle = ModelHandle::new();
    assert!(matches!(
        handle.embed_query("query", &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    assert_replacement_receives_demand(
        handle,
        Arc::new(PanicOnce {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        }),
    );
}

fn observe_replacement_demand(retain_in_old: bool) -> (bool, bool) {
    let health = HealthSink::default();
    let host = Supervisor::new(health.clone());
    let handle = ModelHandle::new();
    let index = Arc::new(PanicOnce {
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let ids = ModelRequestIds::new();
    let h = handle.clone();
    let sink = health.clone();
    let work = index.clone();
    host.spawn(model_id(), move |stop| {
        run_model_component(
            config(),
            work.clone(),
            ids.clone(),
            sink.clone(),
            h.clone(),
            stop,
        )
    })
    .unwrap();
    wait_for(|| handle.inner.lock().unwrap().supervisor.is_some());
    let old = handle.inner.lock().unwrap().supervisor.clone().unwrap();
    let old_weak = Arc::downgrade(&old);
    index.fail.store(true, Ordering::SeqCst);
    old.wake();
    assert!(old.wait_terminated(Duration::from_secs(5)).is_some());
    wait_for(|| handle.inner.lock().unwrap().supervisor.is_none());
    // Reproduce the RAM left by a query during failure cleanup. The component
    // cannot complete handoff until this outstanding supervisor reference drops.
    if retain_in_old {
        old.request_load();
    }
    drop(old);
    wait_for(|| {
        handle
            .inner
            .lock()
            .unwrap()
            .supervisor
            .as_ref()
            .is_some_and(|s| Arc::as_ptr(s) != old_weak.as_ptr())
    });
    let state = handle.inner.lock().unwrap();
    let replacement = state.supervisor.as_ref().unwrap();
    let observed = (replacement.is_warm(), replacement.has_query_demand());
    drop(state);
    host.shutdown();
    observed
}

#[test]
fn replacement_inherits_unconsumed_old_supervisor_demand() {
    assert_eq!(observe_replacement_demand(true), (false, true));
}

#[test]
fn idle_supervisor_replacement_does_not_invent_demand() {
    assert_eq!(observe_replacement_demand(false), (false, false));
}

#[test]
fn initial_unconfigured_health_never_claims_ready() {
    let health = HealthSink::default();
    let host = Supervisor::new(health.clone());
    let handle = ModelHandle::new();
    let index = Arc::new(PanicOnce {
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let component_health = health.clone();
    let component_handle = handle.clone();
    host.spawn(model_id(), move |stop| {
        run_model_component(
            config(),
            index.clone(),
            ModelRequestIds::new(),
            component_health.clone(),
            component_handle.clone(),
            stop,
        )
    })
    .unwrap();
    wait_for(|| {
        health.snapshot().iter().any(|(id, status)| {
            *id == model_id()
                && matches!(
                    status.state,
                    ComponentState::Ready | ComponentState::Degraded
                )
        })
    });
    let status = health
        .snapshot()
        .into_iter()
        .find(|(id, _)| *id == model_id())
        .unwrap()
        .1;
    host.shutdown();
    assert_eq!(status.state, ComponentState::Degraded);
    assert_eq!(status.reason.as_deref(), Some("model not configured"));
}

#[test]
fn restart_warm_health_and_shutdown() {
    let health = HealthSink::default();
    let host = Supervisor::new(health.clone());
    let handle = ModelHandle::new();
    let lifecycle_handle = handle.clone();
    let lifecycle = Lifecycle::new(
        Arc::new(Instant::now),
        Arc::new(move |warm| lifecycle_handle.set_warm(warm)),
    );
    lifecycle.lease("client");
    let index = Arc::new(PanicOnce {
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let component_handle = handle.clone();
    let component_index = index.clone();
    let component_health = health.clone();
    let ids = ModelRequestIds::new();
    host.spawn(model_id(), move |stop| {
        run_model_component(
            config(),
            component_index.clone(),
            ids.clone(),
            component_health.clone(),
            component_handle.clone(),
            stop,
        )
    })
    .unwrap();
    wait_for(|| handle.inner.lock().unwrap().supervisor.is_some());
    assert!(handle.inner.lock().unwrap().warm);
    assert!(handle
        .inner
        .lock()
        .unwrap()
        .supervisor
        .as_ref()
        .unwrap()
        .is_warm());
    assert!(matches!(
        handle.embed_query("hello", &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    let cancelled = RequestControl::default();
    cancelled.cancel.store(true, Ordering::Release);
    assert_eq!(
        handle.embed_query("hello", &cancelled),
        Err(SemanticError::Cancelled)
    );
    wait_for(|| {
        health
            .snapshot()
            .iter()
            .any(|(id, status)| *id == model_id() && status.state == ComponentState::Degraded)
    });
    let first = health
        .snapshot()
        .into_iter()
        .find(|(id, _)| *id == model_id())
        .unwrap()
        .1;
    std::thread::sleep(Duration::from_millis(250));
    let same = health
        .snapshot()
        .into_iter()
        .find(|(id, _)| *id == model_id())
        .unwrap()
        .1;
    assert_eq!(first, same);
    index.fail.store(true, Ordering::SeqCst);
    handle.wake();
    wait_for(|| handle.inner.lock().unwrap().supervisor.is_none());
    assert!(handle.snapshot().cause.unwrap().contains("index failed"));
    assert!(matches!(
        handle.embed_query("during", &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    wait_for(|| {
        health
            .snapshot()
            .iter()
            .any(|(id, s)| *id == model_id() && s.state == ComponentState::Recovering)
    });
    wait_for(|| handle.inner.lock().unwrap().supervisor.is_some());
    assert!(handle.inner.lock().unwrap().warm);
    assert!(handle
        .inner
        .lock()
        .unwrap()
        .supervisor
        .as_ref()
        .unwrap()
        .is_warm());
    assert!(matches!(
        handle.embed_query("after", &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    wait_for(|| {
        health
            .snapshot()
            .iter()
            .any(|(id, s)| *id == model_id() && s.state == ComponentState::Degraded)
    });
    host.shutdown();
    assert!(handle.inner.lock().unwrap().supervisor.is_none());
    assert!(matches!(
        handle.embed_query("stopped", &RequestControl::default()),
        Err(SemanticError::Unavailable(_))
    ));
    let calls = index.calls.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(calls, index.calls.load(Ordering::SeqCst));
    assert!(health
        .snapshot()
        .iter()
        .any(|(id, s)| *id == model_id() && s.state == ComponentState::Stopped));
}

#[test]
fn persisted_backlog_starts_one_model_without_frontend_demand_and_completes_coverage() {
    use crate::semantic::{SemanticConfig, SemanticIndexer};
    use crate::test_support::{model_assets, model_child_executable, model_transcript};
    use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
    let dir = tempfile::tempdir().unwrap();
    let assets_root = dir.path().join("assets");
    let assets = model_assets(&assets_root);
    let executable = model_child_executable();
    let store = Store::open(dir.path().join("source"), dir.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit: CurrentUnit {
                    entity_id: uuid::Uuid::new_v4().to_string(),
                    entity_type: "memory".into(),
                    state: UnitState::Live {
                        heads: vec![Head {
                            metadata: serde_json::json!({
                "change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},
                "origin":{"machine_id":uuid::Uuid::new_v4().to_string(),"user_id":"user"},
                "recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                            body: "Persisted source awaiting semantic indexing.".into(),
                        }],
                    },
                },
                expected_versions: vec![],
                evidence: "Published before runtime startup".into(),
            }],
        )
        .unwrap();
    drop(store);
    let store = Store::open(dir.path().join("source"), dir.path().join("local")).unwrap();
    let ids = ModelRequestIds::new();
    let health = HealthSink::default();
    let indexer = SemanticIndexer::start(
        SemanticConfig::new(dir.path().join("indices"), "test".into()),
        ids.clone(),
        health.clone(),
    )
    .unwrap();
    let handle = ModelHandle::new();
    let wake_handle = handle.clone();
    indexer.set_model_waker(Arc::new(move || wake_handle.wake()));
    indexer
        .open_binding("backlog", store.owned_published_reader())
        .unwrap();
    let supervisor = Supervisor::new(health.clone());
    let worker_index = indexer.clone();
    let worker_handle = handle.clone();
    std::fs::create_dir_all(dir.path().join("worker")).unwrap();
    let runtime_config = RuntimeConfig {
        state_dir: dir.path().join("worker"),
        executable,
        model: Some(assets),
    };
    supervisor
        .spawn(model_id(), move |stop| {
            run_model_component(
                runtime_config.clone(),
                Arc::new(worker_index.clone()),
                ids.clone(),
                health.clone(),
                worker_handle.clone(),
                stop,
            )
        })
        .unwrap();
    wait_for(|| {
        indexer.progress("backlog").is_some_and(|p| {
            p.complete
                && p.ack
                    .is_some_and(|a| a.sequence == receipt.generation as i64)
        })
    });
    assert_eq!(model_transcript(&assets_root, "starts.jsonl").len(), 1);
    assert!(!model_transcript(&assets_root, "requests.jsonl").is_empty());
    supervisor.shutdown();
    assert!(indexer.shutdown(Duration::from_secs(10)));
}

#[test]
fn prefix_query_caps_transport_at_a_utf8_boundary_and_combines_truncation() {
    use crate::test_support::{model_assets, model_child_executable, model_transcript};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("assets");
    let assets = model_assets(&root);
    let handle = ModelHandle::new();
    handle.set_warm(true);
    let health = HealthSink::default();
    let host = Supervisor::new(health.clone());
    let worker = handle.clone();
    let index = Arc::new(PanicOnce {
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let config = RuntimeConfig {
        state_dir: temp.path().into(),
        executable: model_child_executable(),
        model: Some(assets),
    };
    host.spawn(model_id(), move |stop| {
        run_model_component(
            config.clone(),
            index.clone(),
            ModelRequestIds::new(),
            health.clone(),
            worker.clone(),
            stop,
        )
    })
    .unwrap();
    wait_for(|| handle.snapshot().state == WorkerState::Ready);

    let limit = 4 * 1024 * 1024;
    let expected = "x".repeat(limit - 1);
    let oversized = format!("{expected}한 remainder");
    let control = RequestControl::default();
    let result = handle.embed_prefix(&oversized, &control);
    let short = handle.embed_prefix("token-truncated query", &control);
    host.shutdown();

    assert!(result.unwrap().truncated);
    assert!(short.unwrap().truncated);
    let requests = model_transcript(&root, "requests.jsonl");
    assert_eq!(requests.len(), 2);
    let delivered = requests[0].as_str().unwrap();
    assert_eq!(delivered.len(), limit - 1);
    assert_eq!(delivered, expected);
    assert_eq!(requests[1], "token-truncated query");
}
