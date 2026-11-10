use agentlaw_worker::supervisor::{IndexWork, IndexWorkPort, IndexWorkStatus, ModelRequestId};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Default)]
struct WorkState {
    queue: VecDeque<IndexWork>,
    digests: Vec<String>,
    retry: Option<Instant>,
    awaiting: bool,
    finished: Vec<(ModelRequestId, Result<Vec<f32>, String>)>,
}
#[derive(Clone, Default)]
pub struct FakeRequestIds {
    counter: Arc<Mutex<u64>>,
}
impl FakeRequestIds {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn next(&self) -> ModelRequestId {
        let mut counter = self.counter.lock().unwrap();
        *counter = counter.checked_add(1).expect("model request ID exhausted");
        ModelRequestId(*counter)
    }
}
#[derive(Clone, Default)]
pub struct FakeIndexWork {
    state: Arc<Mutex<WorkState>>,
    ids: FakeRequestIds,
}
impl FakeIndexWork {
    pub fn new() -> Self {
        Self::default()
    }
    /// Several bindings under one supervisor share one allocator.
    pub fn with_ids(ids: FakeRequestIds) -> Self {
        Self {
            state: Arc::default(),
            ids,
        }
    }
    pub fn push(&self, text: &str) -> ModelRequestId {
        let mut s = self.state.lock().unwrap();
        let id = self.ids.next();
        s.queue.push_back(IndexWork {
            id,
            text: text.into(),
        });
        id
    }
    pub fn set_next_retry_at(&self, at: Option<Instant>) {
        self.state.lock().unwrap().retry = at;
    }
    pub fn set_awaiting_model(&self, awaiting: bool) {
        self.state.lock().unwrap().awaiting = awaiting;
    }
    pub fn ready_digests(&self) -> Vec<String> {
        self.state.lock().unwrap().digests.clone()
    }
    pub fn finished(&self) -> Vec<(ModelRequestId, Result<Vec<f32>, String>)> {
        self.state.lock().unwrap().finished.clone()
    }
}
impl IndexWorkPort for FakeIndexWork {
    fn model_ready(&self, digest: &str) {
        let mut s = self.state.lock().unwrap();
        s.digests.push(digest.into());
        s.awaiting = false;
    }
    fn next_work(&self, digest: &str) -> Option<IndexWork> {
        let mut s = self.state.lock().unwrap();
        if s.digests.last().is_some_and(|d| d == digest) {
            s.queue.pop_front()
        } else {
            None
        }
    }
    fn finish(
        &self,
        id: ModelRequestId,
        outcome: Result<Vec<f32>, agentlaw_worker::EmbeddingError>,
    ) {
        self.state
            .lock()
            .unwrap()
            .finished
            .push((id, outcome.map_err(|e| e.to_string())));
    }
    fn status(&self) -> IndexWorkStatus {
        let s = self.state.lock().unwrap();
        IndexWorkStatus {
            runnable: !s.queue.is_empty(),
            next_retry_at: s.retry,
            awaiting_model: s.awaiting,
        }
    }
}

#[cfg(test)]
mod tests;
