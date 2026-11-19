//! Connection to the supervised model child.

use crate::host::supervision::StopToken;
use crate::ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus, HealthSink};
use agentlaw_contracts::RequestControl;
use agentlaw_flows::semantic::{
    PiecePlan, PieceRecord, PieceVectorReuse, PrefixEmbedding, QueryVector, SemanticError,
};
use agentlaw_worker::supervisor::{IndexWorkPort, ModelRequestIds, ModelSupervisor, QueryError};
use agentlaw_worker::{RuntimeConfig, WorkerState};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub use agentlaw_worker::supervisor::ModelSnapshot;

struct State {
    supervisor: Option<Arc<ModelSupervisor>>,
    previous: Option<ModelSupervisor>,
    warm: bool,
    demand: bool,
    cause: String,
}

/// Stable RAM handle shared by lifecycle and semantic callers across restarts.
#[derive(Clone)]
pub struct ModelHandle {
    inner: Arc<Mutex<State>>,
    recent_vectors: Arc<crate::semantic::recent_vectors::RecentVectors>,
}
impl Default for ModelHandle {
    fn default() -> Self {
        Self::new()
    }
}
impl ModelHandle {
    pub fn new() -> Self {
        Self::with_recent_vectors(Arc::default())
    }
    pub(crate) fn with_recent_vectors(
        recent_vectors: Arc<crate::semantic::recent_vectors::RecentVectors>,
    ) -> Self {
        Self {
            recent_vectors: recent_vectors,
            inner: Arc::new(Mutex::new(State {
                supervisor: None,
                previous: None,
                warm: false,
                demand: false,
                cause: "model supervisor not running".into(),
            })),
        }
    }
    pub fn embed_prefix(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> Result<PrefixEmbedding, SemanticError> {
        let end = text.floor_char_boundary(text.len().min(4 * 1024 * 1024));
        let (vector, truncated) = self.query(control, |supervisor| {
            supervisor.embed_prefix(text, &text[..end], &control.cancel)
        })?;
        Ok(PrefixEmbedding {
            vector: map_vector(vector),
            truncated: truncated || end < text.len(),
        })
    }

    pub fn plan_pieces(
        &self,
        records: &[&str],
        control: &RequestControl,
    ) -> Result<PiecePlan, SemanticError> {
        use sha2::{Digest, Sha256};
        control.check().map_err(|_| SemanticError::Cancelled)?;
        let mut model = None;
        let mut planned = Vec::with_capacity(records.len());
        for &text in records {
            let mut offset = 0;
            let mut pieces = Vec::new();
            for window in crate::semantic::pieces::windows(text) {
                let (digest, ranges) = self.query(control, |supervisor| {
                    supervisor.split(window, &control.cancel)
                })?;
                if model.as_ref().is_some_and(|expected| *expected != digest) {
                    return Err(SemanticError::Failed(
                        "model changed during piece planning".into(),
                    ));
                }
                model.get_or_insert(digest);
                if !crate::semantic::pieces::valid_ranges(window, &ranges) {
                    return Err(SemanticError::Failed("invalid model piece ranges".into()));
                }
                for (start, end) in ranges {
                    let range = offset + start as usize..offset + end as usize;
                    pieces.push((range.clone(), Sha256::digest(text[range].as_bytes()).into()));
                }
                offset += window.len();
            }
            planned.push(PieceRecord::new(Arc::from(text), pieces));
        }
        let model = model
            .or_else(|| self.snapshot().model_digest)
            .unwrap_or_default();
        Ok(PiecePlan::new(model, planned))
    }

    pub fn embed_planned(
        &self,
        plan: &PiecePlan,
        record: usize,
        reuse: &PieceVectorReuse,
        control: &RequestControl,
    ) -> Result<Vec<QueryVector>, SemanticError> {
        control.check().map_err(|_| SemanticError::Cancelled)?;
        let model = plan.model();
        if self.snapshot().model_digest.as_deref() != Some(model) {
            return Err(SemanticError::Failed("piece plan model changed".into()));
        }
        let record = plan
            .record(record)
            .ok_or_else(|| SemanticError::Failed("missing planned record".into()))?;
        let mut vectors = Vec::new();
        for (text, digest) in record.pieces() {
            control.check().map_err(|_| SemanticError::Cancelled)?;
            if self.snapshot().model_digest.as_deref() != Some(model) {
                return Err(SemanticError::Failed("piece plan model changed".into()));
            }
            let text = text.ok_or_else(|| SemanticError::Failed("invalid planned range".into()))?;
            let values = match reuse
                .get(model, *digest)
                .or_else(|| self.recent_vectors.get(model, digest))
            {
                Some(values) => values,
                None => {
                    let vector = self.query(control, |supervisor| {
                        supervisor.embed_piece(text, &control.cancel)
                    })?;
                    if vector.model_digest != model {
                        return Err(SemanticError::Failed("piece plan model changed".into()));
                    }
                    let values: Arc<[f32]> = vector.vector.into();
                    self.recent_vectors
                        .insert(model, *digest, values.clone())
                        .map_err(|error| SemanticError::Failed(error.to_string()))?;
                    values
                }
            };
            reuse.insert(model, *digest, values.clone());
            vectors.push(QueryVector {
                model: model.to_owned(),
                values,
            });
        }
        Ok(vectors)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn embed_query(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> Result<QueryVector, SemanticError> {
        self.query(control, |supervisor| {
            supervisor.embed_piece(text, &control.cancel)
        })
        .map(map_vector)
    }

    fn query<T>(
        &self,
        control: &RequestControl,
        submit: impl FnOnce(&ModelSupervisor) -> Result<T, QueryError>,
    ) -> Result<T, SemanticError> {
        if control.cancel.load(Ordering::Acquire) {
            return Err(SemanticError::Cancelled);
        }
        let supervisor = {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match &state.supervisor {
                Some(supervisor) => supervisor.clone(),
                None => {
                    state.demand = true;
                    return Err(SemanticError::Unavailable(state.cause.clone()));
                }
            }
        };
        let result = submit(&supervisor);
        if matches!(&result, Err(QueryError::Unavailable(_)))
            && supervisor.wait_terminated(Duration::ZERO).is_some()
        {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            state.demand = true;
        }
        result.map_err(map_error)
    }
    pub fn snapshot(&self) -> ModelSnapshot {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &state.supervisor {
            Some(supervisor) => supervisor.snapshot(),
            None => ModelSnapshot {
                state: WorkerState::Stopped,
                incarnation: None,
                model_digest: None,
                cause: Some(state.cause.clone()),
            },
        }
    }
    pub fn set_warm(&self, warm: bool) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.warm = warm;
        if let Some(supervisor) = &state.supervisor {
            supervisor.set_warm(warm);
        }
    }
    pub fn wake(&self) {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(supervisor) = &state.supervisor {
            supervisor.wake();
        }
    }
}
fn map_vector(vector: agentlaw_worker::supervisor::QueryVector) -> QueryVector {
    QueryVector {
        model: vector.model_digest,
        values: vector.vector.into(),
    }
}
fn map_error(error: QueryError) -> SemanticError {
    match error {
        QueryError::Busy => SemanticError::Busy,
        QueryError::Unavailable(reason) => SemanticError::Unavailable(reason),
        QueryError::Cancelled => SemanticError::Cancelled,
        QueryError::Failed(reason) => SemanticError::Failed(reason),
    }
}

/// One host-supervised attempt. The host owns retry scheduling.
pub fn run_model_component(
    config: RuntimeConfig,
    index: Arc<dyn IndexWorkPort>,
    ids: ModelRequestIds,
    health: HealthSink,
    handle: ModelHandle,
    stop: StopToken,
) -> Result<(), String> {
    let model_configured = config.model.is_some();
    let mut supervisor = Arc::new(ModelSupervisor::start(config, index, ids));
    {
        let mut state = handle.inner.lock().unwrap_or_else(|e| e.into_inner());
        supervisor.set_warm(state.warm);
        let previous_demand = state
            .previous
            .take()
            .is_some_and(|old| old.has_query_demand());
        if state.demand || previous_demand {
            supervisor.request_load();
            state.demand = false;
        }
        state.supervisor = Some(supervisor.clone());
    }
    let mut last = None;
    let mut ready_marked = false;
    let result = loop {
        if stop.is_stopping() {
            break Ok(());
        }
        let snapshot = supervisor.snapshot();
        if snapshot.state == WorkerState::Failed {
            break Err(snapshot
                .cause
                .unwrap_or_else(|| "model start failed".into()));
        }
        let (state, reason) = match snapshot.state {
            WorkerState::Ready => (ComponentState::Ready, None),
            WorkerState::Starting | WorkerState::Loading => {
                (ComponentState::Starting, snapshot.cause)
            }
            WorkerState::Failed => (ComponentState::Degraded, snapshot.cause),
            WorkerState::Stopped if !model_configured => (
                ComponentState::Degraded,
                Some("model not configured".into()),
            ),
            WorkerState::Stopped | WorkerState::Stopping => {
                (ComponentState::Starting, snapshot.cause)
            }
        };
        let current = (state, reason);
        if last.as_ref() != Some(&current) {
            if current.0 == ComponentState::Ready && !ready_marked {
                stop.ready();
                ready_marked = true;
            } else {
                health.publish(
                    ComponentId {
                        binding: None,
                        kind: ComponentKind::Model,
                    },
                    ComponentStatus {
                        state: current.0,
                        observed_at: SystemTime::now(),
                        reason: current.1.clone(),
                    },
                );
            }
            last = Some(current);
        }
        if let Some(reason) = supervisor.wait_terminated(Duration::from_secs(1)) {
            break Err(reason);
        }
    };
    {
        let mut state = handle.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.supervisor = None;
        state.cause = result
            .as_ref()
            .err()
            .cloned()
            .unwrap_or_else(|| "model supervisor stopped".into());
    }
    supervisor.request_stop();
    let mut supervisor = loop {
        match Arc::try_unwrap(supervisor) {
            Ok(supervisor) => break supervisor,
            Err(remaining) => {
                std::thread::sleep(Duration::from_millis(5));
                supervisor = remaining;
            }
        }
    };
    let stopped = supervisor.stop();
    handle
        .inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .previous = Some(supervisor);
    match (result, stopped) {
        (Err(reason), Err(stop_error)) if reason != stop_error => {
            Err(format!("{reason}; stop: {stop_error}"))
        }
        (Err(reason), _) => Err(reason),
        (Ok(()), stopped) => stopped,
    }
}

#[cfg(test)]
mod tests;
