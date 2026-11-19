//! Bounded model work and commit coordination shared by both lane kinds.
use super::source::{AcquiredPieces, Piece};
use super::state::{LaneKey, QueuedPiece, WorkKind};
use super::{pump, Shared};
use agentlaw_search::vector_file::{
    CurrentHeads, VectorFileBatch, VectorFileIdentity, VectorFileWriter,
};
use agentlaw_search::VectorRecord;
use sha2::{Digest, Sha256};
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
    let (memory, changes) = match &completion {
        Completion::Current(Some(heads)) => (heads.memory_id.clone(), heads.change_ids.clone()),
        Completion::History { memory, change } => (memory.clone(), vec![change.clone()]),
        Completion::Current(None) => (String::new(), Vec::new()),
    };
    if matches!(key.kind, WorkKind::Current) {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state
            .schedule
            .lanes
            .get(key)
            .is_some_and(|lane| lane.token == token && lane.generation == generation)
        {
            if let Some(revisions) = state.schedule.blocked.get_mut(key) {
                revisions.retain(|(_, id, change)| *id != memory || changes.contains(change));
                let cleared = !revisions.iter().any(|(digest, _, _)| digest == model);
                if revisions.is_empty() {
                    state.schedule.blocked.remove(key);
                }
                if cleared {
                    let lane = state.schedule.lanes.get_mut(key).unwrap();
                    if lane.reason.as_deref()
                        == Some(&agentlaw_worker::EmbeddingError::InputTooLong.to_string())
                    {
                        lane.blocked_generation = None;
                        lane.state = crate::ports::ComponentState::Busy;
                        lane.reason = None;
                        pump::report(shared, &key.binding, lane.state, None);
                    }
                }
            }
        }
    }
    let blocked = |schedule: &super::state::Schedule| {
        schedule.blocked.get(key).is_some_and(|blocked| {
            blocked.iter().any(|(digest, id, change)| {
                digest == model && *id == memory && changes.contains(change)
            })
        })
    };
    let mut exhausted = false;
    let mut first_result = None::<Instant>;
    loop {
        // A Split can finish before the writer reaches its wait, without producing a vector to flush.
        let observed_work = {
            let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(lane) = state.schedule.lanes.get(key) else {
                return Ok(false);
            };
            lane.work_revision
        };
        while !exhausted {
            let pending_piece = {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if blocked(&state.schedule) {
                    exhausted = true;
                    break;
                }
                let Some(lane) = state.schedule.lanes.get_mut(key).filter(|lane| {
                    lane.token == token && lane.generation == generation && !lane.quarantined
                }) else {
                    return Ok(false);
                };
                if lane.queue.len() + lane.running.len() >= shared.config.queue_pieces {
                    break;
                }
                if let Some(window) = &lane.pending_window {
                    #[cfg(test)]
                    if shared
                        .pause_pending_ranges
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        break;
                    }
                    let Some(&(start, end)) = window.ranges.get(window.next_range) else {
                        lane.pending_window = None;
                        continue;
                    };
                    Some(Piece {
                        memory_id: window.piece.memory_id.clone(),
                        change_id: window.piece.change_id.clone(),
                        section: format!("{}:{}", window.piece.section, window.next_range),
                        scope: window.piece.scope.clone(),
                        text: window.piece.text[start as usize..end as usize].to_owned(),
                    })
                } else {
                    if lane
                        .queue
                        .iter()
                        .chain(lane.running.values().map(|(_, work)| work))
                        .any(QueuedPiece::is_split)
                    {
                        break;
                    }
                    None
                }
            };
            let from_window = pending_piece.is_some();
            let piece = match pending_piece {
                Some(piece) => piece,
                None => match pieces.next() {
                    Some(piece) => piece?,
                    None => {
                        exhausted = true;
                        break;
                    }
                },
            };
            let split = !from_window;
            let exists = !split
                && writer
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
                    })?;
            let cached = if from_window && !exists {
                shared
                    .config
                    .recent_vectors
                    .get(model, &Sha256::digest(piece.text.as_bytes()).into())
            } else {
                None
            };
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if blocked(&state.schedule) {
                exhausted = true;
                break;
            }
            let Some(lane) = state.schedule.lanes.get_mut(key).filter(|lane| {
                lane.token == token && lane.generation == generation && !lane.quarantined
            }) else {
                return Ok(false);
            };
            if cached.is_some() && lane.occupied() >= shared.config.pending_results {
                break;
            }
            if from_window {
                let Some(window) = lane.pending_window.as_mut() else {
                    return Ok(false);
                };
                window.next_range += 1;
            }
            if exists {
                continue;
            }
            if let Some(vector) = cached {
                lane.results.push_back((piece, vector));
            } else {
                lane.queue.push_back(QueuedPiece::new(piece, split));
            }
            drop(state);
            shared.wake_model();
        }
        let (records, done, wait) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping || state.schedule.model.as_deref() != Some(model) {
                return Ok(false);
            }
            let permanently_blocked = blocked(&state.schedule);
            let Some(lane) = state.schedule.lanes.get_mut(key).filter(|lane| {
                lane.token == token && lane.generation == generation && !lane.quarantined
            }) else {
                return Ok(false);
            };
            if permanently_blocked {
                lane.blocked_generation = Some(generation);
                let reason = agentlaw_worker::EmbeddingError::InputTooLong.to_string();
                if lane.state != crate::ports::ComponentState::Degraded
                    || lane.reason.as_ref() != Some(&reason)
                {
                    lane.state = crate::ports::ComponentState::Degraded;
                    lane.reason = Some(reason.clone());
                    pump::report(shared, &key.binding, lane.state, Some(reason));
                }
            }
            let now = (shared.clock)();
            if !lane.results.is_empty() && first_result.is_none() {
                first_result = Some(now);
            }
            let done = exhausted
                && lane.queue.is_empty()
                && lane.running.is_empty()
                && lane.pending_window.is_none()
                && !permanently_blocked;
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
                        vector: vector.to_vec(),
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            lane.committing += records.len();
            let wait = first_result
                .map(|at| (at + shared.config.commit_interval).saturating_duration_since(now));
            (records, done, wait)
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
        if observed_work != lane.work_revision {
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
