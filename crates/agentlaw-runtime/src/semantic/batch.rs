//! Bounded model work and commit coordination shared by both lane kinds.
use super::source::{AcquiredPieces, Piece};
use super::state::{LaneKey, WorkKind};
use super::{pump, Shared};
use agentlaw_search::vector_file::{
    CurrentHeads, VectorFileBatch, VectorFileIdentity, VectorFileWriter,
};
use agentlaw_search::VectorRecord;
use std::sync::Mutex;
use std::time::Instant;

pub(super) enum Completion {
    Current(Option<CurrentHeads>),
    History { memory: String, change: String },
}

pub(super) fn run(
    shared: &Shared,
    key: &LaneKey,
    token: u64,
    generation: u64,
    model: &str,
    identity: &VectorFileIdentity,
    gate: &Mutex<()>,
    writer: &mut VectorFileWriter,
    mut pieces: AcquiredPieces,
    mut completion: Completion,
) -> Result<bool, String> {
    let mut exhausted = false;
    let mut first_result = None::<Instant>;
    loop {
        while !exhausted {
            let room = {
                let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                let Some(lane) = state.schedule.lanes.get(key).filter(|lane| {
                    lane.token == token && lane.generation == generation && !lane.quarantined
                }) else {
                    return Ok(false);
                };
                #[cfg(test)]
                if shared
                    .pause_pending_ranges
                    .load(std::sync::atomic::Ordering::Acquire)
                    && lane.pending_window.is_some()
                {
                    break;
                }
                lane.queue.len() + lane.running.len() < shared.config.queue_pieces
            };
            if !room {
                break;
            }
            let Some(piece) = pieces.next() else {
                exhausted = true;
                break;
            };
            let piece: Piece = piece?;
            if writer
                .contains(&piece.memory_id, &piece.change_id, &piece.section)
                .map_err(|error| {
                    pump::vector_failure(
                        shared,
                        &key.binding,
                        token,
                        generation,
                        identity,
                        gate,
                        error,
                    )
                })?
            {
                continue;
            }
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(lane) = state.schedule.lanes.get_mut(key).filter(|lane| {
                lane.token == token && lane.generation == generation && !lane.quarantined
            }) else {
                return Ok(false);
            };
            lane.queue.push_back(piece);
            drop(state);
            shared.wake_model();
        }
        let (records, done, wait, snapshot) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping || state.schedule.model.as_deref() != Some(model) {
                return Ok(false);
            }
            let Some(lane) = state.schedule.lanes.get_mut(key).filter(|lane| {
                lane.token == token && lane.generation == generation && !lane.quarantined
            }) else {
                return Ok(false);
            };
            let now = (shared.clock)();
            if !lane.results.is_empty() && first_result.is_none() {
                first_result = Some(now);
            }
            let done = exhausted && lane.queue.is_empty() && lane.running.is_empty();
            let flush = lane.results.len() >= shared.config.commit_pieces
                || (done && !lane.results.is_empty())
                || first_result.is_some_and(|at| {
                    now.saturating_duration_since(at) >= shared.config.commit_interval
                });
            let records = if flush {
                lane.results
                    .drain(..)
                    .map(|(piece, vector)| VectorRecord {
                        memory_id: piece.memory_id,
                        change_id: piece.change_id,
                        section: piece.section,
                        scope: piece.scope,
                        vector,
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            lane.committing += records.len();
            let wait = first_result
                .map(|at| (at + shared.config.commit_interval).saturating_duration_since(now));
            (
                records,
                done,
                wait,
                (lane.queue.len(), lane.running.len(), lane.results.len()),
            )
        };
        shared.changed.notify_all();
        #[cfg(test)]
        if matches!(key.kind, WorkKind::Current) {
            let has_pending = shared
                .state
                .lock()
                .unwrap()
                .schedule
                .lanes
                .get(key)
                .is_some_and(|lane| lane.pending_window.is_some());
            if has_pending {
                let hook = shared.completion_check_hook.lock().unwrap().take();
                if let Some(hook) = hook {
                    hook(done);
                    continue;
                }
            }
        }
        let needs_marker = done
            && match &completion {
                Completion::Current(heads) => heads.is_some(),
                Completion::History { .. } => true,
            };
        if !records.is_empty() || needs_marker {
            #[cfg(test)]
            if matches!(key.kind, WorkKind::Current) {
                let hook = shared
                    .pre_commit_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            let valid = {
                let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                !state.stopping
                    && state.schedule.model.as_deref() == Some(model)
                    && state.schedule.lanes.get(key).is_some_and(|lane| {
                        lane.token == token && lane.generation == generation && !lane.quarantined
                    })
            };
            if !valid {
                return Ok(false);
            }
            #[cfg(test)]
            if matches!(key.kind, WorkKind::Current) {
                let hook = shared
                    .post_check_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            let heads = match &mut completion {
                Completion::Current(heads) => heads.take().into_iter().collect(),
                Completion::History { .. } => Vec::new(),
            };
            let completed_changes = match &completion {
                Completion::History { memory, change } if done => {
                    vec![(memory.clone(), change.clone())]
                }
                _ => Vec::new(),
            };
            let count = records.len();
            pump::guarded_commit(
                shared,
                &key.binding,
                token,
                generation,
                identity,
                gate,
                writer,
                VectorFileBatch {
                    heads,
                    records,
                    completed_changes,
                    ..Default::default()
                },
            )?;
            {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(lane) = state
                    .schedule
                    .lanes
                    .get_mut(key)
                    .filter(|lane| lane.token == token && lane.generation == generation)
                {
                    lane.committing -= count;
                    if matches!(key.kind, WorkKind::Current) {
                        lane.writer_retry.reset();
                    }
                }
            }
            first_result = None;
            shared.changed.notify_all();
            shared.wake_model();
            if done {
                return Ok(true);
            }
            continue;
        }
        if done {
            return Ok(true);
        }
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lane) = state.schedule.lanes.get(key).filter(|lane| {
            lane.token == token && lane.generation == generation && !lane.quarantined
        }) else {
            return Ok(false);
        };
        if snapshot != (lane.queue.len(), lane.running.len(), lane.results.len()) {
            drop(state);
            continue;
        }
        if let Some(wait) = wait {
            drop(
                shared
                    .changed
                    .wait_timeout(state, wait)
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
    }
}
