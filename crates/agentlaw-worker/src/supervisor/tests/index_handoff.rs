use super::*;
use std::sync::Mutex;

struct TestPort {
    work: Mutex<Option<IndexWork>>,
    finished: Mutex<
        Option<(
            ModelRequestId,
            std::result::Result<Vec<f32>, EmbeddingError>,
        )>,
    >,
}

impl IndexWorkPort for TestPort {
    fn model_ready(&self, _: &str) {}

    fn next_work(&self, _: &str) -> Option<IndexWork> {
        self.work.lock().unwrap().take()
    }

    fn finish(&self, id: ModelRequestId, outcome: std::result::Result<Vec<f32>, EmbeddingError>) {
        *self.finished.lock().unwrap() = Some((id, outcome));
    }

    fn status(&self) -> IndexWorkStatus {
        IndexWorkStatus {
            runnable: false,
            next_retry_at: None,
            awaiting_model: false,
        }
    }
}

#[test]
fn query_load_handoff_respects_ready_and_loading() {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: std::env::current_exe().unwrap(),
        model: None,
    };
    let index = Arc::new(TestPort {
        work: Mutex::new(None),
        finished: Mutex::new(None),
    });
    let mut supervisor = ModelSupervisor::start(config, index, ModelRequestIds::new());
    {
        let mut state = supervisor.shared.state.lock().unwrap();
        state.snapshot.state = WorkerState::Ready;
    }
    supervisor.request_load();
    let after_ready = supervisor.has_query_demand();
    {
        let mut state = supervisor.shared.state.lock().unwrap();
        state.snapshot.state = WorkerState::Loading;
        state.loading_demand = false;
    }
    supervisor.request_load();
    let during_loading = supervisor.has_query_demand();
    supervisor.stop().unwrap();
    assert!(
        !after_ready,
        "demand handed to a Ready model must be consumed"
    );
    assert!(during_loading, "loading model must retain demand");
}

#[test]
fn index_work_port_crosses_threads_and_records_outcome() {
    let concrete = Arc::new(TestPort {
        work: Mutex::new(Some(IndexWork {
            id: ModelRequestId(42),
            text: Arc::from("queued text"),
        })),
        finished: Mutex::new(None),
    });
    let port: Arc<dyn IndexWorkPort> = concrete.clone();
    std::thread::spawn(move || {
        let work = port.next_work("model").unwrap();
        assert_eq!(&*work.text, "queued text");
        port.finish(work.id, Ok(vec![1.0, 2.0]));
    })
    .join()
    .unwrap();
    let finished = concrete.finished.lock().unwrap().take().unwrap();
    assert_eq!(finished.0, ModelRequestId(42));
    assert_eq!(finished.1.unwrap(), vec![1.0, 2.0]);
}
