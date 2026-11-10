//! Semantic index writer, feeder and search port.

mod batch;
mod history;
mod pump;
mod quarantine;
mod search;
mod source;
mod state;
#[cfg(test)]
mod tests;

use crate::ports::{ComponentState, HealthSink, RuntimeError, RuntimeResult};
use agentlaw_search::vector_file::SourcePosition;
use agentlaw_storage::published::OwnedPublishedReader;
use agentlaw_worker::supervisor::{
    IndexWork, IndexWorkPort, IndexWorkStatus, ModelRequestId, ModelRequestIds,
};
use agentlaw_worker::EmbeddingError;
use state::{Lane, LaneKey, Schedule};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use search::SemanticSearch;

/// Configuration shared by the current and future history index lanes.
#[derive(Clone, Debug)]
pub struct SemanticConfig {
    pub indices_root: PathBuf,
    pub config_id: String,
    pub dimension: usize,
    pub chunk_bytes: usize,
    pub commit_pieces: usize,
    pub commit_interval: Duration,
    pub queue_pieces: usize,
    pub pending_results: usize,
    pub history_lanes: usize,
}

impl SemanticConfig {
    pub fn new(indices_root: PathBuf, config_id: String) -> Self {
        Self {
            indices_root,
            config_id,
            dimension: 256,
            chunk_bytes: 4096,
            commit_pieces: 64,
            commit_interval: Duration::from_millis(250),
            queue_pieces: 256,
            pending_results: 128,
            history_lanes: 32,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SemanticProgress {
    pub model_digest: Option<String>,
    pub ack: Option<SourcePosition>,
    pub complete: bool,
    pub queued_pieces: usize,
    pub state: ComponentState,
    pub reason: Option<String>,
}

struct Shared {
    config: SemanticConfig,
    ids: ModelRequestIds,
    health: HealthSink,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    state: Mutex<SharedState>,
    changed: Condvar,
    threads: Mutex<Vec<(String, JoinHandle<()>)>>,
    model_waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pre_commit_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    post_check_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    readview_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    history_init_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    fn wake_model(&self) {
        let wake = self
            .model_waker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

#[derive(Default)]
struct SharedState {
    schedule: Schedule,
    next_token: u64,
    stopping: bool,
    closing: std::collections::BTreeSet<String>,
    retiring_readers: Vec<Arc<state::Readers>>,
    quarantined: Vec<(agentlaw_search::vector_file::VectorFileIdentity, String)>,
    sources: std::collections::BTreeMap<String, OwnedPublishedReader>,
    history_completed: std::collections::VecDeque<history::Completed>,
}

#[derive(Clone)]
pub struct SemanticIndexer {
    inner: Arc<Shared>,
    _owner: Arc<Owner>,
}

/// Binding-scoped preparation facade.
#[derive(Clone)]
pub struct SemanticPreparationHandle {
    indexer: SemanticIndexer,
    binding: String,
    model: crate::model::ModelHandle,
}

impl SemanticPreparationHandle {
    pub fn wake(&self) {
        self.indexer.wake(&self.binding);
    }

    pub fn wait_target(
        &self,
        target: &agentlaw_storage::published::SourcePosition,
        control: &agentlaw_contracts::RequestControl,
    ) -> Result<agentlaw_flows::semantic::SemanticCoverage, agentlaw_flows::semantic::SemanticError>
    {
        self.wait_with_snapshot(
            target,
            control,
            || self.model.snapshot(),
            Duration::from_millis(100),
        )
    }

    fn wait_with_snapshot(
        &self,
        target: &agentlaw_storage::published::SourcePosition,
        control: &agentlaw_contracts::RequestControl,
        mut snapshot: impl FnMut() -> crate::model::ModelSnapshot,
        interval: Duration,
    ) -> Result<agentlaw_flows::semantic::SemanticCoverage, agentlaw_flows::semantic::SemanticError>
    {
        use agentlaw_flows::semantic::{SemanticCoverage, SemanticError};
        use agentlaw_worker::WorkerState;
        control.phase("semantic_preparation");
        let mut reported_ack = None;
        loop {
            control.check().map_err(|_| SemanticError::Cancelled)?;
            let model = snapshot();
            match model.state {
                WorkerState::Starting | WorkerState::Loading | WorkerState::Ready => {}
                WorkerState::Stopped | WorkerState::Failed | WorkerState::Stopping => {
                    return Err(SemanticError::Unavailable(
                        model
                            .cause
                            .unwrap_or_else(|| format!("model is {:?}", model.state)),
                    ));
                }
            }
            let state = self
                .indexer
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let lane = state
                .schedule
                .lanes
                .get(&LaneKey::current(&self.binding))
                .ok_or_else(|| SemanticError::Unavailable("binding not open".into()))?;
            if matches!(
                lane.state,
                ComponentState::Degraded | ComponentState::Failed | ComponentState::Stopped
            ) {
                return Err(SemanticError::Unavailable(
                    lane.reason
                        .clone()
                        .unwrap_or_else(|| format!("semantic index is {:?}", lane.state)),
                ));
            }
            if let Some(active) = &lane.active {
                if active.identity.epoch != target.epoch {
                    return Err(SemanticError::Unavailable("source epoch mismatch".into()));
                }
            }
            if let Some(ack) = &lane.ack {
                if ack.epoch != target.epoch {
                    return Err(SemanticError::Unavailable("source epoch mismatch".into()));
                }
                if reported_ack.as_ref() != Some(ack) {
                    reported_ack = Some(ack.clone());
                    drop(state);
                    control.phase("semantic_preparation_progress");
                    continue;
                }
                let sequence = u64::try_from(ack.sequence)
                    .map_err(|_| SemanticError::Unavailable("negative index position".into()))?;
                if model.state == WorkerState::Ready
                    && model.model_digest.as_deref() == state.schedule.model.as_deref()
                    && model.model_digest.is_some()
                    && lane.complete
                    && sequence >= target.sequence
                {
                    return Ok(SemanticCoverage::CompleteAt(
                        agentlaw_storage::published::SourcePosition {
                            epoch: ack.epoch.clone(),
                            sequence,
                        },
                    ));
                }
            }
            let (_state, _) = self
                .indexer
                .inner
                .changed
                .wait_timeout(state, interval)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

struct Owner(Weak<Shared>);
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopping = true;
            state.schedule.lanes.clear();
            drop(state);
            shared.changed.notify_all();
            let mut threads = shared.threads.lock().unwrap_or_else(|e| e.into_inner());
            for (_, thread) in threads.drain(..) {
                if thread.thread().id() != std::thread::current().id() {
                    let _ = thread.join();
                }
            }
        }
    }
}

impl SemanticIndexer {
    pub fn start(
        config: SemanticConfig,
        ids: ModelRequestIds,
        health: HealthSink,
    ) -> RuntimeResult<Self> {
        Self::start_with_clock(config, ids, health, Arc::new(Instant::now))
    }

    /// The clock is injectable so retry deadlines can be exercised deterministically.
    pub fn start_with_clock(
        config: SemanticConfig,
        ids: ModelRequestIds,
        health: HealthSink,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> RuntimeResult<Self> {
        if config.dimension == 0
            || config.chunk_bytes == 0
            || config.commit_pieces == 0
            || config.commit_interval.is_zero()
            || config.queue_pieces == 0
            || config.pending_results == 0
            || config.config_id.is_empty()
        {
            return Err(RuntimeError::Protocol(
                "invalid semantic configuration".into(),
            ));
        }
        let inner = Arc::new(Shared {
            config,
            ids,
            health,
            clock,
            state: Mutex::new(SharedState::default()),
            changed: Condvar::new(),
            threads: Mutex::new(Vec::new()),
            model_waker: Mutex::new(None),
            #[cfg(test)]
            pre_commit_hook: Mutex::new(None),
            #[cfg(test)]
            post_check_hook: Mutex::new(None),
            #[cfg(test)]
            readview_hook: Mutex::new(None),
            #[cfg(test)]
            history_init_hook: Mutex::new(None),
        });
        let dispatcher = inner.clone();
        let thread = std::thread::Builder::new()
            .name("semantic-history-dispatch".into())
            .spawn(move || history::dispatch(dispatcher))
            .map_err(|error| RuntimeError::Unavailable(error.to_string()))?;
        inner
            .threads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((String::new(), thread));
        Ok(Self {
            _owner: Arc::new(Owner(Arc::downgrade(&inner))),
            inner,
        })
    }

    /// Connects the host's model wake signal after the supervisor is started.
    pub fn set_model_waker(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self
            .inner
            .model_waker
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(wake);
        self.inner.wake_model();
    }

    pub fn open_binding(&self, binding: &str, source: OwnedPublishedReader) -> RuntimeResult<()> {
        if binding.is_empty() {
            return Err(RuntimeError::Protocol("empty binding".into()));
        }
        let mut shared = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if shared.stopping {
            return Err(RuntimeError::Unavailable("semantic indexer stopped".into()));
        }
        if shared
            .schedule
            .lanes
            .contains_key(&LaneKey::current(binding))
        {
            return Err(RuntimeError::Protocol("binding already open".into()));
        }
        if shared.closing.contains(binding) {
            return Err(RuntimeError::Unavailable("binding is closing".into()));
        }
        shared.next_token = shared
            .next_token
            .checked_add(1)
            .ok_or_else(|| RuntimeError::Capacity("binding token exhausted".into()))?;
        let token = shared.next_token;
        let lane = Lane {
            token,
            ..Lane::default()
        };
        shared
            .schedule
            .lanes
            .insert(LaneKey::current(binding), lane);
        shared.sources.insert(binding.to_owned(), source.clone());
        let inner = self.inner.clone();
        let name = binding.to_owned();
        let handle = std::thread::Builder::new()
            .name("semantic-index".into())
            .spawn(move || pump::run(inner, name, token, source));
        match handle {
            Ok(handle) => self
                .inner
                .threads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((binding.to_owned(), handle)),
            Err(error) => {
                shared.schedule.lanes.remove(&LaneKey::current(binding));
                shared.sources.remove(binding);
                return Err(RuntimeError::Unavailable(error.to_string()));
            }
        }
        drop(shared);
        self.inner.changed.notify_all();
        self.inner.wake_model();
        Ok(())
    }

    pub fn close_binding(&self, binding: &str) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lane) = state.schedule.lanes.remove(&LaneKey::current(binding)) else {
            return;
        };
        let history_keys: Vec<_> = state
            .schedule
            .lanes
            .keys()
            .filter(|key| key.binding == binding)
            .cloned()
            .collect();
        for key in history_keys {
            if let Some(lane) = state.schedule.lanes.remove(&key) {
                lane.cancel
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        state.sources.remove(binding);
        state
            .history_completed
            .retain(|item| item.binding != binding);
        let readers = lane.readers;
        state.retiring_readers.push(readers.clone());
        state.closing.insert(binding.to_owned());
        let handles = {
            let mut threads = self.inner.threads.lock().unwrap_or_else(|e| e.into_inner());
            let mut handles = Vec::new();
            while let Some(index) = threads.iter().position(|(name, _)| name == binding) {
                handles.push(threads.swap_remove(index).1);
            }
            handles
        };
        drop(state);
        self.inner.changed.notify_all();
        let mut count = readers.count.lock().unwrap_or_else(|e| e.into_inner());
        while *count != 0 {
            count = readers
                .changed
                .wait(count)
                .unwrap_or_else(|e| e.into_inner());
        }
        drop(count);
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retiring_readers
            .retain(|entry| !Arc::ptr_eq(entry, &readers));
        let mut deferred = false;
        for handle in handles {
            if handle.thread().id() == std::thread::current().id() {
                deferred = true;
                let inner = self.inner.clone();
                let binding = binding.to_owned();
                std::thread::spawn(move || {
                    let _ = handle.join();
                    inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .closing
                        .remove(&binding);
                    inner.changed.notify_all();
                });
                continue;
            }
            let _ = handle.join();
        }
        if deferred {
            return;
        }
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closing
            .remove(binding);
        self.inner.changed.notify_all();
        self.inner.health.publish(
            crate::ports::ComponentId {
                binding: Some(binding.to_owned()),
                kind: crate::ports::ComponentKind::Semantic,
            },
            crate::ports::ComponentStatus {
                state: ComponentState::Stopped,
                observed_at: std::time::SystemTime::now(),
                reason: None,
            },
        );
    }

    pub fn wake(&self, binding: &str) {
        let mut shared = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lane) = shared.schedule.lanes.get_mut(&LaneKey::current(binding)) {
            if !lane.quarantined {
                lane.invalidate();
                lane.dirty = true;
            }
        }
        drop(shared);
        self.inner.changed.notify_all();
    }

    pub fn progress(&self, binding: &str) -> Option<SemanticProgress> {
        let shared = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let lane = shared.schedule.lanes.get(&LaneKey::current(binding))?;
        Some(SemanticProgress {
            model_digest: shared.schedule.model.clone(),
            ack: lane.ack.clone(),
            complete: lane.complete,
            queued_pieces: lane.queue.len(),
            state: lane.state,
            reason: lane.reason.clone(),
        })
    }

    pub fn shutdown(&self, timeout: Duration) -> bool {
        self.inner
            .model_waker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let (bindings, readers) = {
            let mut shared = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            shared.stopping = true;
            let bindings: Vec<_> = shared
                .schedule
                .lanes
                .keys()
                .map(|key| key.binding.clone())
                .collect();
            let readers: Vec<_> = shared
                .schedule
                .lanes
                .values()
                .map(|lane| lane.readers.clone())
                .collect();
            for lane in shared.schedule.lanes.values() {
                lane.cancel
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            shared.sources.clear();
            shared.history_completed.clear();
            shared.retiring_readers.extend(readers);
            let readers = shared.retiring_readers.clone();
            shared.schedule.lanes.clear();
            (bindings, readers)
        };
        for binding in bindings {
            self.inner.health.publish(
                crate::ports::ComponentId {
                    binding: Some(binding),
                    kind: crate::ports::ComponentKind::Semantic,
                },
                crate::ports::ComponentStatus {
                    state: ComponentState::Stopped,
                    observed_at: std::time::SystemTime::now(),
                    reason: None,
                },
            );
        }
        self.inner.changed.notify_all();
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        for readers in readers {
            let mut count = readers.count.lock().unwrap_or_else(|e| e.into_inner());
            while *count != 0 {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                count = readers
                    .changed
                    .wait_timeout(count, remaining)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retiring_readers
                .retain(|entry| !Arc::ptr_eq(entry, &readers));
        }
        let mut threads = self.inner.threads.lock().unwrap_or_else(|e| e.into_inner());
        while threads.iter().any(|(_, h)| !h.is_finished()) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
        let joined = threads
            .drain(..)
            .map(|(_, h)| h.join().is_ok())
            .collect::<Vec<_>>();
        drop(threads);
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        while !state.closing.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            state = self
                .inner
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        joined.into_iter().all(|ok| ok)
    }
}

impl IndexWorkPort for SemanticIndexer {
    fn model_ready(&self, digest: &str) {
        let mut shared = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if shared.schedule.model.as_deref() != Some(digest) {
            shared.schedule.model = Some(digest.to_owned());
            for lane in shared.schedule.lanes.values_mut() {
                lane.invalidate();
                lane.dirty = true;
                lane.ack = None;
                lane.complete = false;
                lane.active = None;
                lane.quarantined = false;
            }
        }
        drop(shared);
        self.inner.changed.notify_all();
    }

    fn next_work(&self, digest: &str) -> Option<IndexWork> {
        let mut shared = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let work = shared.schedule.next(
            digest,
            &self.inner.ids,
            self.inner.config.pending_results,
            (self.inner.clock)(),
        );
        drop(shared);
        if work.is_some() {
            self.inner.changed.notify_all();
        }
        work
    }

    fn finish(&self, id: ModelRequestId, outcome: Result<Vec<f32>, EmbeddingError>) {
        let mut shared = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut notify_model = false;
        let mut failure = None;
        for (name, lane) in shared.schedule.lanes.iter_mut() {
            if let Some((generation, piece)) = lane.running.remove(&id) {
                if generation == lane.generation {
                    let outcome = outcome.and_then(|vector| {
                        if vector.len() != self.inner.config.dimension
                            || vector.iter().any(|x| !x.is_finite())
                            || !vector.iter().any(|x| *x != 0.0)
                        {
                            Err(EmbeddingError::Failed("invalid embedding vector".into()))
                        } else {
                            Ok(vector)
                        }
                    });
                    match outcome {
                        Ok(vector) => {
                            if lane.retry_piece.as_ref().is_some_and(|key| {
                                key.0 == piece.memory_id
                                    && key.1 == piece.change_id
                                    && key.2 == piece.section
                            }) {
                                lane.retry.reset();
                                lane.retry_piece = None;
                            }
                            lane.results.push_back((piece, vector));
                        }
                        Err(error) => {
                            lane.retry_piece = Some((
                                piece.memory_id.clone(),
                                piece.change_id.clone(),
                                piece.section.clone(),
                            ));
                            lane.queue.push_front(piece);
                            lane.retry.fail((self.inner.clock)());
                            lane.state = ComponentState::Degraded;
                            lane.reason = Some(error.to_string());
                            failure = Some((name.binding.clone(), error.to_string()));
                            notify_model = true;
                        }
                    }
                }
                break;
            }
        }
        drop(shared);
        self.inner.changed.notify_all();
        if let Some((name, reason)) = failure {
            self.inner.health.publish(
                crate::ports::ComponentId {
                    binding: Some(name),
                    kind: crate::ports::ComponentKind::Semantic,
                },
                crate::ports::ComponentStatus {
                    state: ComponentState::Degraded,
                    observed_at: std::time::SystemTime::now(),
                    reason: Some(reason),
                },
            );
        }
        if notify_model {
            self.inner.wake_model();
        }
    }

    fn status(&self) -> IndexWorkStatus {
        let shared = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.inner.clock)();
        let runnable = shared.schedule.model.is_some()
            && shared.schedule.lanes.values().any(|lane| {
                !lane.quarantined
                    && !lane.queue.is_empty()
                    && lane.occupied() < self.inner.config.pending_results
                    && lane.retry.ready(now)
            });
        let next_retry_at = shared
            .schedule
            .lanes
            .values()
            .filter(|lane| !lane.quarantined && !lane.queue.is_empty())
            .filter_map(|lane| lane.retry.until)
            .min();
        IndexWorkStatus {
            runnable,
            next_retry_at,
            awaiting_model: shared.schedule.model.is_none()
                && shared.schedule.lanes.values().any(|lane| !lane.quarantined),
        }
    }
}
