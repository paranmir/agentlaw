//! Background semantic lanes for immutable history.
use super::state::{ActiveFile, Lane, LaneKey, ReaderGuard, WorkKind};
use super::{pump, quarantine, source, SemanticSearch, Shared};
use agentlaw_contracts::RequestControl;
use agentlaw_flows::semantic::{
    HistoryIngestHint, HistorySemanticPort, OfferResult, QueryVector, SemanticCandidates,
    SemanticCoverage, SemanticError,
};
use agentlaw_search::vector_file::{
    SourcePosition, VectorFileBatch, VectorFileIdentity, VectorFileKind, VectorFileReadView,
    VectorFileWriter,
};
use agentlaw_search::ScopeFilter;
use agentlaw_storage::published::OwnedPublishedReader;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) struct Completed {
    pub binding: String,
    subject: String,
    position: agentlaw_storage::published::SourcePosition,
    model: String,
    pub(super) active: ActiveFile,
    gate: Arc<Mutex<()>>,
}

impl HistorySemanticPort for SemanticSearch {
    fn offer(&self, hint: HistoryIngestHint) -> OfferResult {
        if agentlaw_storage::validate_id(&hint.subject).is_err() || hint.through.epoch.is_empty() {
            return OfferResult::Busy;
        }
        let shared = &self.indexer.inner;
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(model) = state.schedule.model.clone() else {
            return OfferResult::Busy;
        };
        if state.stopping
            || !state
                .schedule
                .lanes
                .contains_key(&LaneKey::current(&hint.binding))
        {
            return OfferResult::Busy;
        }
        if state.quarantined.iter().any(|(identity, _)| identity.binding == hint.binding && identity.model_digest == model && identity.epoch == hint.through.epoch && matches!(&identity.kind, VectorFileKind::History { subject } if subject == &hint.subject)) {
            return OfferResult::Busy;
        }
        if state.history_completed.iter().any(|item| {
            item.binding == hint.binding
                && item.subject == hint.subject
                && item.model == model
                && item.position.epoch == hint.through.epoch
                && item.position.sequence >= hint.through.sequence
        }) {
            return OfferResult::Accepted;
        }
        let key = LaneKey::history(&hint.binding, &hint.subject);
        if let Some(lane) = state.schedule.lanes.get_mut(&key) {
            let Some(target) = &mut lane.history_target else {
                return OfferResult::Busy;
            };
            if target.epoch != hint.through.epoch {
                return OfferResult::Busy;
            }
            target.sequence = target.sequence.max(hint.through.sequence);
        } else {
            let count = state
                .schedule
                .lanes
                .keys()
                .filter(|key| matches!(key.kind, WorkKind::History { .. }))
                .count();
            if count >= shared.config.history_lanes {
                return OfferResult::Busy;
            }
            let Some(token) = state.next_token.checked_add(1) else {
                return OfferResult::Busy;
            };
            state.next_token = token;
            let io_gate = state.schedule.lanes[&LaneKey::current(&hint.binding)]
                .io_gate
                .clone();
            state.schedule.lanes.insert(
                key,
                Lane {
                    token,
                    io_gate,
                    history_target: Some(hint.through),
                    ..Lane::default()
                },
            );
        }
        drop(state);
        shared.changed.notify_all();
        shared.wake_model();
        OfferResult::Queued
    }

    fn search_available(
        &self,
        binding: &str,
        subject: &str,
        vector: &QueryVector,
        scopes: &[String],
        limit: usize,
        control: &RequestControl,
    ) -> Result<SemanticCandidates, SemanticError> {
        control.check().map_err(|_| SemanticError::Cancelled)?;
        super::search::validate_vector(vector, self.indexer.inner.config.dimension)?;
        if limit > 10_000 {
            return Err(SemanticError::Failed(
                "semantic result limit exceeds 10000".into(),
            ));
        }
        let shared = &self.indexer.inner;
        let (identity, file, target, readers, gate, token) = {
            let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let current = state
                .schedule
                .lanes
                .get(&LaneKey::current(binding))
                .ok_or_else(|| SemanticError::Unavailable("binding not open".into()))?;
            if state.schedule.model.as_deref() != Some(vector.model.as_str()) {
                return Err(SemanticError::Unavailable(
                    "query and index model mismatch".into(),
                ));
            }
            let epoch = current
                .active
                .as_ref()
                .ok_or_else(|| SemanticError::Unavailable("source epoch unavailable".into()))?
                .identity
                .epoch
                .clone();
            let identity = VectorFileIdentity {
                binding: binding.into(),
                kind: VectorFileKind::History {
                    subject: subject.into(),
                },
                epoch: epoch.clone(),
                model_digest: vector.model.clone(),
                config: shared.config.config_id.clone(),
                generation: "initial".into(),
                dimension: shared.config.dimension,
            };
            if state.quarantined.iter().any(|(id, _)| id == &identity) {
                return Err(SemanticError::Unavailable(
                    "index corrupt: quarantined".into(),
                ));
            }
            let lane = state
                .schedule
                .lanes
                .get(&LaneKey::history(binding, subject));
            let completed = state.history_completed.iter().rev().find(|c| {
                c.binding == binding
                    && c.subject == subject
                    && c.model == vector.model
                    && c.position.epoch == epoch
            });
            let published = lane
                .and_then(|lane| lane.active.as_ref())
                .is_some_and(|active| active.identity == identity)
                || completed.is_some_and(|item| item.active.identity == identity);
            if !published {
                return Err(SemanticError::Unavailable(
                    "history semantic index not built".into(),
                ));
            }
            let target = lane
                .and_then(|l| l.history_target.clone())
                .or_else(|| completed.map(|c| c.position.clone()));
            let gate = lane
                .map(|l| l.io_gate.clone())
                .or_else(|| completed.map(|c| c.gate.clone()))
                .unwrap_or_else(|| current.io_gate.clone());
            let readers = current.readers.clone();
            *readers.count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            let token = current.token;
            let file = completed
                .filter(|c| c.active.identity == identity)
                .map(|c| c.active.path.clone())
                .unwrap_or_else(|| super::identity::path(&shared.config, &identity));
            (identity, file, target, ReaderGuard(readers), gate, token)
        };
        let _readers = readers;
        let view = VectorFileReadView::open(&file, &identity).map_err(|e| {
            if matches!(e, agentlaw_search::vector_file::VectorFileError::Missing) {
                return SemanticError::Unavailable("history semantic index not built".into());
            }
            if quarantine::is_corrupt(&e) {
                quarantine_history(
                    shared,
                    binding,
                    token,
                    &identity,
                    &gate,
                    &e.to_string(),
                    || {
                        VectorFileReadView::open(&file, &identity)
                            .err()
                            .is_some_and(|error| quarantine::is_corrupt(&error))
                    },
                );
            }
            SemanticError::Unavailable(e.to_string())
        })?;
        let ack =
            view.progress()
                .ack
                .as_ref()
                .map(|p| agentlaw_storage::published::SourcePosition {
                    epoch: p.epoch.clone(),
                    sequence: p.sequence as u64,
                });
        let requested = target.or_else(|| ack.clone()).unwrap_or_else(|| {
            agentlaw_storage::published::SourcePosition {
                epoch: identity.epoch.clone(),
                sequence: 0,
            }
        });
        if requested.epoch != identity.epoch {
            return Err(SemanticError::Unavailable(
                "history source epoch mismatch".into(),
            ));
        }
        let available =
            ack.clone()
                .unwrap_or_else(|| agentlaw_storage::published::SourcePosition {
                    epoch: identity.epoch.clone(),
                    sequence: 0,
                });
        let coverage = if ack.is_some()
            && available.sequence >= requested.sequence
            && available.epoch == requested.epoch
        {
            SemanticCoverage::CompleteAt(available)
        } else {
            SemanticCoverage::Partial {
                available,
                requested,
            }
        };
        let result = view
            .search_ann(
                &vector.values,
                &ScopeFilter {
                    allowed_scopes: scopes.to_vec(),
                },
                limit,
            )
            .map_err(|e| {
                if quarantine::is_corrupt(&e) {
                    quarantine_history(
                        shared,
                        binding,
                        token,
                        &identity,
                        &gate,
                        &e.to_string(),
                        || match VectorFileReadView::open(&file, &identity) {
                            Err(error) => quarantine::is_corrupt(&error),
                            Ok(view) => view
                                .search_ann(
                                    &vector.values,
                                    &ScopeFilter {
                                        allowed_scopes: scopes.to_vec(),
                                    },
                                    limit,
                                )
                                .err()
                                .is_some_and(|error| quarantine::is_corrupt(&error)),
                        },
                    );
                }
                SemanticError::Unavailable(e.to_string())
            })?;
        control.check().map_err(|_| SemanticError::Cancelled)?;
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.quarantined.iter().any(|(id, _)| id == &identity)
            || !state
                .schedule
                .lanes
                .get(&LaneKey::current(binding))
                .is_some_and(|lane| {
                    lane.token == token
                        && lane
                            .active
                            .as_ref()
                            .is_some_and(|active| active.identity.epoch == identity.epoch)
                })
            || state.schedule.model.as_deref() != Some(vector.model.as_str())
        {
            return Err(SemanticError::Unavailable(
                "history index changed or corrupt".into(),
            ));
        }
        Ok(SemanticCandidates {
            hits: result.hits,
            coverage,
        })
    }
}

fn quarantine_history(
    shared: &Shared,
    binding: &str,
    token: u64,
    identity: &VectorFileIdentity,
    gate: &Mutex<()>,
    reason: &str,
    verify: impl FnOnce() -> bool,
) {
    let _gate = gate.lock().unwrap_or_else(|e| e.into_inner());
    let published = |state: &super::SharedState| {
        let VectorFileKind::History { subject } = &identity.kind else {
            return false;
        };
        state
            .schedule
            .lanes
            .get(&LaneKey::current(binding))
            .is_some_and(|lane| lane.token == token)
            && (state
                .schedule
                .lanes
                .get(&LaneKey::history(binding, subject))
                .and_then(|lane| lane.active.as_ref())
                .is_some_and(|active| active.identity == *identity)
                || state
                    .history_completed
                    .iter()
                    .any(|item| item.active.identity == *identity))
    };
    if !published(&shared.state.lock().unwrap_or_else(|e| e.into_inner())) || !verify() {
        return;
    }
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    if !published(&state) {
        return;
    }
    let current_identity = state.schedule.model.as_deref() == Some(identity.model_digest.as_str())
        && state
            .schedule
            .lanes
            .get(&LaneKey::current(binding))
            .and_then(|lane| lane.active.as_ref())
            .is_some_and(|active| active.identity.epoch == identity.epoch);
    if !state.quarantined.iter().any(|(id, _)| id == identity) {
        state
            .quarantined
            .push((identity.clone(), format!("index corrupt: {reason}")));
    }
    if let VectorFileKind::History { subject } = &identity.kind {
        if let Some(lane) = state
            .schedule
            .lanes
            .get_mut(&LaneKey::history(binding, subject))
            .filter(|lane| {
                current_identity
                    && lane
                        .history_target
                        .as_ref()
                        .is_some_and(|target| target.epoch == identity.epoch)
                    && lane
                        .active
                        .as_ref()
                        .is_none_or(|active| active.identity == *identity)
            })
        {
            lane.invalidate();
            lane.quarantined = true;
        }
    }
    drop(state);
    if !current_identity {
        shared.changed.notify_all();
        return;
    }
    shared.health.publish(
        crate::ports::ComponentId {
            binding: Some(binding.into()),
            kind: crate::ports::ComponentKind::Semantic,
        },
        crate::ports::ComponentStatus {
            state: crate::ports::ComponentState::Degraded,
            observed_at: std::time::SystemTime::now(),
            reason: Some(format!("history index corrupt: {reason}")),
        },
    );
    shared.changed.notify_all();
}

pub(super) fn dispatch(shared: Arc<Shared>) {
    loop {
        if let Ok(mut threads) = shared.threads.try_lock() {
            let mut index = 0;
            while index < threads.len() {
                if !threads[index].0.is_empty() && threads[index].1.is_finished() {
                    let (_, thread) = threads.swap_remove(index);
                    let _ = thread.join();
                } else {
                    index += 1;
                }
            }
        }
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping {
            return;
        }
        let now = (shared.clock)();
        let key = state.schedule.lanes.iter().find_map(|(key, lane)| {
            (matches!(key.kind, WorkKind::History { .. })
                && !lane.history_started
                && !lane.quarantined
                && lane.writer_retry.ready(now))
            .then(|| key.clone())
        });
        let Some(key) = key else {
            let next = state
                .schedule
                .lanes
                .iter()
                .filter(|(key, lane)| {
                    matches!(key.kind, WorkKind::History { .. })
                        && !lane.history_started
                        && !lane.quarantined
                })
                .filter_map(|(_, lane)| lane.writer_retry.until)
                .min();
            if let Some(at) = next {
                drop(
                    shared
                        .changed
                        .wait_timeout(state, at.saturating_duration_since(now))
                        .unwrap_or_else(|e| e.into_inner()),
                );
            } else {
                drop(
                    shared
                        .changed
                        .wait(state)
                        .unwrap_or_else(|e| e.into_inner()),
                );
            }
            continue;
        };
        let Some(source) = state.sources.get(&key.binding).cloned() else {
            state.schedule.lanes.remove(&key);
            continue;
        };
        let lane = state.schedule.lanes.get_mut(&key).unwrap();
        lane.history_started = true;
        let token = lane.token;
        let worker_shared = shared.clone();
        let worker_key = key.clone();
        match std::thread::Builder::new()
            .name("semantic-history".into())
            .spawn(move || run(worker_shared, worker_key, token, source))
        {
            Ok(thread) => shared
                .threads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((key.binding.clone(), thread)),
            Err(error) => {
                if let Some(lane) = state.schedule.lanes.get_mut(&key) {
                    lane.history_started = false;
                    lane.state = crate::ports::ComponentState::Degraded;
                    lane.reason = Some(error.to_string());
                    lane.writer_retry.fail((shared.clock)());
                }
            }
        }
    }
}

fn active(shared: &Shared, key: &LaneKey, token: u64, generation: u64, model: &str) -> bool {
    let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    !state.stopping
        && state.schedule.model.as_deref() == Some(model)
        && state
            .schedule
            .lanes
            .get(key)
            .is_some_and(|l| l.token == token && l.generation == generation && !l.quarantined)
}

fn run(shared: Arc<Shared>, key: LaneKey, token: u64, source: OwnedPublishedReader) {
    loop {
        let (generation, model, target, cancel, gate) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let Some(lane) = state
                    .schedule
                    .lanes
                    .get(&key)
                    .filter(|lane| lane.token == token)
                else {
                    return;
                };
                if state.stopping || lane.quarantined {
                    return;
                }
                let now = (shared.clock)();
                if let Some(at) = lane.writer_retry.until.filter(|at| *at > now) {
                    state = shared
                        .changed
                        .wait_timeout(state, at.saturating_duration_since(now))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                    continue;
                }
                if let Some(model) = &state.schedule.model {
                    break (
                        lane.generation,
                        model.clone(),
                        lane.history_target.clone().unwrap(),
                        lane.cancel.clone(),
                        lane.io_gate.clone(),
                    );
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        let result = process(
            &shared, &key, token, generation, &model, &target, &cancel, &gate, &source,
        );
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lane) = state
            .schedule
            .lanes
            .get_mut(&key)
            .filter(|lane| lane.token == token)
        else {
            return;
        };
        if lane.quarantined {
            return;
        }
        if lane.generation != generation {
            continue;
        }
        match result {
            Ok((position, active_file)) => {
                lane.ack = Some(SourcePosition {
                    epoch: position.epoch.clone(),
                    sequence: position.sequence as i64,
                });
                lane.writer_retry.reset();
                let target = lane.history_target.as_ref().unwrap();
                if target.epoch == position.epoch && target.sequence <= position.sequence {
                    let lane = state.schedule.lanes.remove(&key).unwrap();
                    let WorkKind::History { subject } = &key.kind else {
                        unreachable!()
                    };
                    state
                        .history_completed
                        .retain(|item| !(item.binding == key.binding && item.subject == *subject));
                    state.history_completed.push_back(Completed {
                        binding: key.binding.clone(),
                        subject: subject.clone(),
                        position,
                        model,
                        active: active_file,
                        gate: lane.io_gate,
                    });
                    while state.history_completed.len() > shared.config.history_lanes {
                        state.history_completed.pop_front();
                    }
                    let identity = state
                        .history_completed
                        .back()
                        .unwrap()
                        .active
                        .identity
                        .clone();
                    drop(state);
                    super::identity::reclaim_previous(&shared.config, &identity);
                    shared.changed.notify_all();
                    return;
                }
            }
            Err(reason) => {
                lane.invalidate();
                lane.state = crate::ports::ComponentState::Degraded;
                lane.reason = Some(reason);
                lane.writer_retry.fail((shared.clock)());
            }
        }
        drop(state);
        shared.changed.notify_all();
    }
}

fn process(
    shared: &Shared,
    key: &LaneKey,
    token: u64,
    generation: u64,
    model: &str,
    target: &agentlaw_storage::published::SourcePosition,
    cancel: &std::sync::atomic::AtomicBool,
    gate: &Mutex<()>,
    source: &OwnedPublishedReader,
) -> Result<(agentlaw_storage::published::SourcePosition, ActiveFile), String> {
    let WorkKind::History { subject } = &key.kind else {
        unreachable!()
    };
    let acquired = source.acquire_history(cancel).map_err(|e| e.to_string())?;
    let mut spool = acquired.into_spool().map_err(|e| e.to_string())?;
    let mut position = spool.source_position.clone();
    if position.epoch != target.epoch || position.sequence < target.sequence {
        return Err("history source position unavailable".into());
    }
    let sequence =
        i64::try_from(position.sequence).map_err(|_| "history source position overflow")?;
    let identity = VectorFileIdentity {
        binding: key.binding.clone(),
        kind: VectorFileKind::History {
            subject: subject.clone(),
        },
        epoch: position.epoch.clone(),
        model_digest: model.into(),
        config: shared.config.config_id.clone(),
        generation: "initial".into(),
        dimension: shared.config.dimension,
    };
    let file = super::identity::path(&shared.config, &identity);
    let mut writer = {
        let _gate = gate.lock().unwrap_or_else(|e| e.into_inner());
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let quarantined = state
            .quarantined
            .iter()
            .find(|(id, _)| id == &identity)
            .map(|(_, reason)| reason.clone());
        drop(state);
        if let Some(reason) = quarantined {
            quarantine::mark_guarded(
                shared,
                &key.binding,
                token,
                &identity,
                false,
                Some(generation),
                &reason,
            );
            return Err(reason);
        }
        if !file.exists() {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .history_completed
                .retain(|item| item.active.identity != identity);
            if let Some(lane) = state
                .schedule
                .lanes
                .get_mut(key)
                .filter(|lane| lane.token == token && lane.generation == generation)
            {
                lane.active = None;
                lane.ack = None;
            }
        }
        std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
        #[cfg(test)]
        let opened = VectorFileWriter::open_with_initialization_observer(&file, &identity, || {
            let hook = shared
                .history_init_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(hook) = hook {
                hook();
            }
        });
        #[cfg(not(test))]
        let opened = VectorFileWriter::open(&file, &identity);
        opened.map_err(|e| {
            if quarantine::is_corrupt(&e) {
                quarantine::mark_guarded(
                    shared,
                    &key.binding,
                    token,
                    &identity,
                    false,
                    Some(generation),
                    &e.to_string(),
                );
            }
            e.to_string()
        })?
    };
    let progress = writer.progress().map_err(|e| {
        pump::vector_failure(shared, &key.binding, token, generation, &identity, gate, e)
    })?;
    let sequence = progress
        .ack
        .as_ref()
        .map_or(sequence, |ack| ack.sequence.max(sequence));
    position.sequence = u64::try_from(sequence).map_err(|_| "invalid history ack")?;
    {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lane) = state
            .schedule
            .lanes
            .get_mut(key)
            .filter(|lane| lane.token == token && lane.generation == generation)
        else {
            return Err("history lane changed".into());
        };
        lane.active = Some(ActiveFile {
            path: file.clone(),
            identity: identity.clone(),
        });
        lane.ack = progress.ack.clone();
    }
    let mut failure = None;
    let visited = spool.visit_closure(subject, |change| {
        let id = change.metadata["change_id"]
            .as_str()
            .ok_or_else(|| agentlaw_storage::Error::Corrupt("history change identity".into()))?
            .to_owned();
        let memory = format!("change:{id}");
        let completed = match writer.is_change_completed(&memory, &id) {
            Ok(completed) => completed,
            Err(error) => {
                failure = Some(pump::vector_failure(
                    shared,
                    &key.binding,
                    token,
                    generation,
                    &identity,
                    gate,
                    error,
                ));
                return Err(agentlaw_storage::Error::CancelledBeforeDecision);
            }
        };
        if completed {
            return Ok(());
        }
        if let Err(reason) = change_work(
            shared,
            key,
            token,
            generation,
            model,
            &identity,
            gate,
            &mut writer,
            change,
            &memory,
            &id,
        ) {
            failure = Some(reason);
            return Err(agentlaw_storage::Error::CancelledBeforeDecision);
        }
        Ok(())
    });
    if let Some(reason) = failure {
        return Err(reason);
    }
    visited.map_err(|e| e.to_string())?;
    if !active(shared, key, token, generation, model) {
        return Err("history lane changed".into());
    }
    let ack = SourcePosition {
        epoch: position.epoch.clone(),
        sequence,
    };
    pump::guarded_commit(
        shared,
        &key.binding,
        token,
        generation,
        &identity,
        gate,
        &mut writer,
        VectorFileBatch {
            ack: Some(ack),
            complete: true,
            ..Default::default()
        },
    )?;
    Ok((
        position,
        ActiveFile {
            path: file,
            identity,
        },
    ))
}

fn change_work(
    shared: &Shared,
    key: &LaneKey,
    token: u64,
    generation: u64,
    model: &str,
    identity: &VectorFileIdentity,
    gate: &Mutex<()>,
    writer: &mut VectorFileWriter,
    change: agentlaw_storage::history_spool::SpoolChange,
    memory: &str,
    id: &str,
) -> Result<(), String> {
    let pieces = source::history_pieces(change, shared.config.window_bytes())?;
    let finished = super::batch::run(
        shared,
        key,
        token,
        generation,
        model,
        identity,
        gate,
        writer,
        pieces,
        super::batch::Completion::History {
            memory: memory.into(),
            change: id.into(),
        },
    )?;
    if finished {
        Ok(())
    } else {
        Err("history lane changed".into())
    }
}
