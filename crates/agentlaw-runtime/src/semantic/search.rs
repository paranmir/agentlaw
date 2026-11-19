//! Read-only current semantic search adapter.
use super::SemanticIndexer;
use super::{
    quarantine,
    state::{LaneKey, ReaderGuard},
};
use crate::model::ModelHandle;
use crate::ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus};
use agentlaw_contracts::RequestControl;
use agentlaw_flows::semantic::{
    PiecePlan, PieceVectorReuse, PrefixEmbedding, QueryVector, SemanticCandidates,
    SemanticCoverage, SemanticError, SemanticPort, SemanticQuery,
};
use agentlaw_search::{vector_file::VectorFileReadView, ScopeFilter};
use std::time::SystemTime;

#[derive(Clone)]
pub struct SemanticSearch {
    pub(super) indexer: SemanticIndexer,
    pub(super) model: ModelHandle,
}

impl SemanticSearch {
    pub fn new(indexer: SemanticIndexer, model: ModelHandle) -> Self {
        Self { indexer, model }
    }
    pub fn preparation(&self, binding: &str) -> super::SemanticPreparationHandle {
        super::SemanticPreparationHandle {
            indexer: self.indexer.clone(),
            binding: binding.to_owned(),
            model: self.model.clone(),
        }
    }
}

impl SemanticPort for SemanticSearch {
    fn embed_prefix(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> Result<PrefixEmbedding, SemanticError> {
        let prefix = self.model.embed_prefix(text, control)?;
        validate_vector(&prefix.vector, self.indexer.inner.config.dimension)?;
        if self.model.snapshot().model_digest.as_deref() != Some(prefix.vector.model.as_str()) {
            return Err(SemanticError::Unavailable(
                "model changed during query embedding".into(),
            ));
        }
        control.check().map_err(|_| SemanticError::Cancelled)?;
        Ok(prefix)
    }
    fn plan_pieces(
        &self,
        records: &[&str],
        control: &RequestControl,
    ) -> Result<PiecePlan, SemanticError> {
        self.model.plan_pieces(records, control)
    }
    fn embed_planned(
        &self,
        plan: &PiecePlan,
        record: usize,
        reuse: &PieceVectorReuse,
        control: &RequestControl,
    ) -> Result<Vec<QueryVector>, SemanticError> {
        self.model.embed_planned(plan, record, reuse, control)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn embed_query(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> Result<QueryVector, SemanticError> {
        self.embed_prefix(text, control).map(|prefix| prefix.vector)
    }

    fn search_available(
        &self,
        query: SemanticQuery<'_>,
        control: &RequestControl,
    ) -> Result<SemanticCandidates, SemanticError> {
        control.check().map_err(|_| SemanticError::Cancelled)?;
        let vector = query
            .vector
            .ok_or_else(|| SemanticError::Unavailable("query embedding is unavailable".into()))?;
        if query.limit > 10_000 {
            return Err(SemanticError::Failed(
                "semantic result limit exceeds 10000".into(),
            ));
        }
        validate_vector(vector, self.indexer.inner.config.dimension)?;
        let (token, active, io_gate, _reader) = {
            let state = self
                .indexer
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let lane = state
                .schedule
                .lanes
                .get(&LaneKey::current(query.binding))
                .ok_or_else(|| SemanticError::Unavailable("binding not open".into()))?;
            if lane.quarantined || lane.active_quarantined {
                return Err(SemanticError::Unavailable(
                    lane.reason
                        .clone()
                        .unwrap_or_else(|| "index corrupt".into()),
                ));
            }
            let active = lane
                .active
                .clone()
                .ok_or_else(|| SemanticError::Unavailable("semantic index not available".into()))?;
            if state.schedule.model.as_deref() != Some(vector.model.as_str())
                || active.identity.model_digest != vector.model
            {
                return Err(SemanticError::Unavailable(
                    "query and index model mismatch".into(),
                ));
            }
            let readers = lane.readers.clone();
            *readers.count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            (
                lane.token,
                active,
                lane.io_gate.clone(),
                ReaderGuard(readers),
            )
        };
        if active.identity.epoch != query.source.epoch {
            return Err(SemanticError::Unavailable("source epoch mismatch".into()));
        }
        let view = VectorFileReadView::open(&active.path, &active.identity).map_err(|e| {
            self.file_error(
                query.binding,
                token,
                &active,
                &io_gate,
                e.to_string(),
                quarantine::is_corrupt(&e),
                || {
                    VectorFileReadView::open(&active.path, &active.identity)
                        .err()
                        .is_some_and(|error| quarantine::is_corrupt(&error))
                },
            )
        })?;
        #[cfg(test)]
        {
            let hook = self
                .indexer
                .inner
                .readview_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let progress = view.progress();
        if invalid_progress(progress) {
            return Err(self.file_error(
                query.binding,
                token,
                &active,
                &io_gate,
                "invalid vector progress metadata".into(),
                true,
                || match VectorFileReadView::open(&active.path, &active.identity) {
                    Ok(view) => invalid_progress(view.progress()),
                    Err(error) => quarantine::is_corrupt(&error),
                },
            ));
        }
        // Before the first ack, sequence zero denotes no publications applied.
        let available = match progress.ack.as_ref() {
            Some(ack) if ack.epoch == query.source.epoch => {
                agentlaw_storage::published::SourcePosition {
                    epoch: ack.epoch.clone(),
                    sequence: u64::try_from(ack.sequence).map_err(|_| {
                        SemanticError::Unavailable("negative index position".into())
                    })?,
                }
            }
            Some(_) => return Err(SemanticError::Unavailable("source epoch mismatch".into())),
            None => agentlaw_storage::published::SourcePosition {
                epoch: active.identity.epoch.clone(),
                sequence: 0,
            },
        };
        let coverage = if progress.ack.is_some()
            && progress.complete
            && available.sequence >= query.source.sequence
        {
            SemanticCoverage::CompleteAt(available)
        } else {
            SemanticCoverage::Partial {
                available,
                requested: query.source.clone(),
            }
        };
        control.check().map_err(|_| SemanticError::Cancelled)?;
        let result = view
            .search_ann(
                &vector.values,
                &ScopeFilter {
                    allowed_scopes: query.scopes.to_vec(),
                },
                query.limit,
            )
            .map_err(|e| {
                self.file_error(
                    query.binding,
                    token,
                    &active,
                    &io_gate,
                    e.to_string(),
                    quarantine::is_corrupt(&e),
                    || match VectorFileReadView::open(&active.path, &active.identity) {
                        Ok(view) => view
                            .search_ann(
                                &vector.values,
                                &ScopeFilter {
                                    allowed_scopes: query.scopes.to_vec(),
                                },
                                query.limit,
                            )
                            .err()
                            .is_some_and(|error| quarantine::is_corrupt(&error)),
                        Err(error) => quarantine::is_corrupt(&error),
                    },
                )
            })?;
        control.check().map_err(|_| SemanticError::Cancelled)?;
        {
            let state = self
                .indexer
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(lane) = state
                .schedule
                .lanes
                .get(&LaneKey::current(query.binding))
                .filter(|lane| lane.token == token && lane.quarantined)
            {
                return Err(SemanticError::Unavailable(
                    lane.reason
                        .clone()
                        .unwrap_or_else(|| "index corrupt".into()),
                ));
            }
            if state.schedule.model.as_deref() != Some(vector.model.as_str())
                || !state
                    .schedule
                    .lanes
                    .get(&LaneKey::current(query.binding))
                    .is_some_and(|lane| {
                        lane.token == token
                            && !lane.quarantined
                            && lane.active.as_ref().is_some_and(|a| {
                                a.identity.epoch == active.identity.epoch
                                    && a.identity.model_digest == active.identity.model_digest
                            })
                    })
            {
                return Err(SemanticError::Unavailable(
                    "semantic index changed during query".into(),
                ));
            }
        }
        Ok(SemanticCandidates {
            hits: result.hits,
            coverage,
        })
    }
}

impl SemanticSearch {
    fn file_error(
        &self,
        binding: &str,
        token: u64,
        active: &super::state::ActiveFile,
        io_gate: &std::sync::Arc<std::sync::Mutex<()>>,
        reason: String,
        corrupt: bool,
        verify: impl FnOnce() -> bool,
    ) -> SemanticError {
        if corrupt {
            let _io = io_gate.lock().unwrap_or_else(|e| e.into_inner());
            let matches = {
                let state = self
                    .indexer
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                state
                    .schedule
                    .lanes
                    .get(&LaneKey::current(binding))
                    .is_some_and(|lane| {
                        lane.token == token
                            && lane.active.as_ref().is_some_and(|current| {
                                current.identity == active.identity && current.path == active.path
                            })
                    })
            };
            if matches
                && verify()
                && quarantine::mark_guarded(
                    &self.indexer.inner,
                    binding,
                    token,
                    &active.identity,
                    true,
                    None,
                    &reason,
                )
            {
                return SemanticError::Unavailable(format!("index corrupt: {reason}"));
            }
            return SemanticError::Unavailable(reason);
        }
        let mut state = self
            .indexer
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(lane) = state
            .schedule
            .lanes
            .get(&LaneKey::current(binding))
            .filter(|lane| lane.token == token && lane.quarantined)
        {
            return SemanticError::Unavailable(
                lane.reason
                    .clone()
                    .unwrap_or_else(|| "index corrupt".into()),
            );
        }
        if let Some(lane) = state
            .schedule
            .lanes
            .get_mut(&LaneKey::current(binding))
            .filter(|lane| {
                lane.token == token
                    && lane.active.as_ref().is_some_and(|current| {
                        current.identity == active.identity && current.path == active.path
                    })
            })
        {
            lane.state = ComponentState::Degraded;
            lane.reason = Some(reason.clone());
            self.indexer.inner.health.publish(
                ComponentId {
                    binding: Some(binding.to_owned()),
                    kind: ComponentKind::Semantic,
                },
                ComponentStatus {
                    state: ComponentState::Degraded,
                    observed_at: SystemTime::now(),
                    reason: Some(reason.clone()),
                },
            );
        }
        drop(state);
        self.indexer.inner.changed.notify_all();
        SemanticError::Unavailable(reason)
    }
}

fn invalid_progress(progress: &agentlaw_search::vector_file::VectorFileProgress) -> bool {
    progress.complete && progress.ack.is_none()
        || progress.ack.is_some() && progress.inventory.is_none()
        || progress
            .ack
            .as_ref()
            .zip(progress.inventory.as_ref())
            .is_some_and(|(ack, inventory)| ack.sequence < inventory.start.sequence)
}

pub(super) fn validate_vector(vector: &QueryVector, dimension: usize) -> Result<(), SemanticError> {
    if vector.model.is_empty()
        || vector.values.len() != dimension
        || vector.values.iter().any(|v| !v.is_finite())
        || !vector.values.iter().any(|v| *v != 0.0)
    {
        return Err(SemanticError::Failed("invalid query embedding".into()));
    }
    Ok(())
}
