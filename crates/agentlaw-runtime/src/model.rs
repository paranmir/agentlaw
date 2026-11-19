//! Connection to the supervised model child.

use crate::host::supervision::StopToken;
use crate::ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus, HealthSink};
use agentlaw_contracts::RequestControl;
use agentlaw_flows::semantic::{
    PiecePlan, PieceVectorReuse, PrefixEmbedding, QueryVector, SemanticError,
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
    _recent_vectors: Arc<crate::semantic::recent_vectors::RecentVectors>,
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
            _recent_vectors: recent_vectors,
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
        _text: &str,
        _control: &RequestControl,
    ) -> Result<PrefixEmbedding, SemanticError> {
        Err(SemanticError::Failed(
            "prefix embedding is not implemented".into(),
        ))
    }
    pub fn plan_pieces(
        &self,
        _records: &[&str],
        _control: &RequestControl,
    ) -> Result<PiecePlan, SemanticError> {
        Err(SemanticError::Failed(
            "piece planning is not implemented".into(),
        ))
    }
    pub fn embed_planned(
        &self,
        _plan: &PiecePlan,
        _record: usize,
        _reuse: &PieceVectorReuse,
        _control: &RequestControl,
    ) -> Result<Vec<QueryVector>, SemanticError> {
        Err(SemanticError::Failed(
            "piece embedding is not implemented".into(),
        ))
    }
    pub fn embed_query(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> Result<QueryVector, SemanticError> {
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
        let result = supervisor.embed_query(text, &control.cancel);
        if matches!(&result, Err(QueryError::Unavailable(_)))
            && supervisor.wait_terminated(Duration::ZERO).is_some()
        {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            state.demand = true;
        }
        result.map(map_vector).map_err(map_error)
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
