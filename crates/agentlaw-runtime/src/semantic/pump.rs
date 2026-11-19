//! Per-binding source and vector writer. No source or SQLite I/O holds the RAM lock.
use super::state::{ActiveFile, LaneKey};
use super::{generation as generations, quarantine, source, SemanticRepairState, Shared};
use crate::ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus};
use agentlaw_search::vector_file::{
    CurrentHeads, InventoryCursor, SourcePosition, VectorFileBatch, VectorFileError,
    VectorFileIdentity, VectorFileKind, VectorFileProgress, VectorFileReadView, VectorFileWriter,
};
use agentlaw_storage::published::{OwnedPublishedReader, SourcePosition as StoragePosition};
use std::sync::Arc;
use std::time::SystemTime;

type Result<T> = std::result::Result<T, String>;

fn position(p: &StoragePosition) -> Result<SourcePosition> {
    Ok(SourcePosition {
        epoch: p.epoch.clone(),
        sequence: i64::try_from(p.sequence).map_err(|_| "source sequence exceeds vector range")?,
    })
}
fn storage_position(p: &SourcePosition) -> Result<StoragePosition> {
    Ok(StoragePosition {
        epoch: p.epoch.clone(),
        sequence: u64::try_from(p.sequence).map_err(|_| "negative source sequence")?,
    })
}
fn report(shared: &Shared, binding: &str, state: ComponentState, reason: Option<String>) {
    shared.health.publish(
        ComponentId {
            binding: Some(binding.into()),
            kind: ComponentKind::Semantic,
        },
        ComponentStatus {
            state,
            observed_at: SystemTime::now(),
            reason,
        },
    );
}
fn current(shared: &Shared, name: &str, token: u64, generation: u64, model: &str) -> bool {
    let lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    !lock.stopping
        && lock.schedule.model.as_deref() == Some(model)
        && lock
            .schedule
            .lanes
            .get(&LaneKey::current(name))
            .is_some_and(|l| l.token == token && l.generation == generation && !l.quarantined)
}

pub(super) fn vector_failure(
    shared: &Shared,
    name: &str,
    token: u64,
    generation: u64,
    identity: &VectorFileIdentity,
    gate: &std::sync::Mutex<()>,
    error: VectorFileError,
) -> String {
    let reason = error.to_string();
    if quarantine::is_corrupt(&error) {
        let _guard = gate.lock().unwrap_or_else(|e| e.into_inner());
        quarantine::mark_guarded(
            shared,
            name,
            token,
            identity,
            false,
            Some(generation),
            &reason,
        );
    }
    reason
}

pub(super) fn guarded_commit(
    shared: &Shared,
    name: &str,
    token: u64,
    generation: u64,
    identity: &VectorFileIdentity,
    gate: &std::sync::Mutex<()>,
    writer: &mut VectorFileWriter,
    batch: VectorFileBatch,
) -> Result<()> {
    let _guard = gate.lock().unwrap_or_else(|e| e.into_inner());
    {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let key = match &identity.kind {
            VectorFileKind::Current => LaneKey::current(name),
            VectorFileKind::History { subject } => LaneKey::history(name, subject),
        };
        let lane = state
            .schedule
            .lanes
            .get(&key)
            .filter(|lane| lane.token == token)
            .ok_or("binding closed")?;
        if lane.quarantined || state.quarantined.iter().any(|(id, _)| id == identity) {
            return Err("index corrupt: quarantined".into());
        }
    }
    match writer.commit(batch) {
        Ok(()) => Ok(()),
        Err(error) => {
            let reason = error.to_string();
            if quarantine::is_corrupt(&error) {
                quarantine::mark_guarded(
                    shared,
                    name,
                    token,
                    identity,
                    false,
                    Some(generation),
                    &reason,
                );
            }
            Err(reason)
        }
    }
}

fn committed(shared: &Shared, name: &str, token: u64, generation: u64) {
    let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(lane) = lock
        .schedule
        .lanes
        .get_mut(&LaneKey::current(name))
        .filter(|lane| lane.token == token && lane.generation == generation)
    {
        lane.writer_retry.reset();
    }
}

fn reclaim(shared: &Shared, name: &str) {
    let readers: Vec<_> = {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .retiring_readers
            .iter()
            .filter(|(binding, _)| binding == name)
            .map(|(_, r)| r.clone())
            .collect()
    };
    for reader in readers {
        if *reader.count.lock().unwrap_or_else(|e| e.into_inner()) != 0 {
            #[cfg(test)]
            if let Some(hook) = shared
                .reclaim_busy_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                hook();
            }
            continue;
        }
        let path = reader
            .retired
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let Some(path) = path else { continue };
        let is_active = {
            let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .schedule
                .lanes
                .get(&LaneKey::current(name))
                .and_then(|l| l.active.as_ref())
                .is_some_and(|active| active.path == path)
        };
        if is_active {
            continue;
        }
        if let generations::Removal::Failed { path, reason } = generations::remove_files(&path) {
            let message = format!("cleanup {}: {reason}", path.display());
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(lane) = state.schedule.lanes.get_mut(&LaneKey::current(name)) {
                lane.cleaned_dir = None;
                lane.cleanup_error = Some(message.clone());
                lane.state = ComponentState::Degraded;
                lane.reason = Some(message.clone());
                if let Some(repair) = lane.repair.as_mut() {
                    repair.state = SemanticRepairState::Failed;
                    repair.reason = Some(message.clone());
                }
            }
            drop(state);
            report(shared, name, ComponentState::Degraded, Some(message));
        }
    }
    shared
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retiring_readers
        .retain(|(_, r)| {
            *r.count.lock().unwrap_or_else(|e| e.into_inner()) != 0
                || r.retired
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some()
        });
}

fn cleanup_dir(shared: &Shared, name: &str, token: u64, dir: &std::path::Path, selected: &str) {
    let protected: Vec<_> = {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .retiring_readers
            .iter()
            .filter_map(|(_, r)| r.retired.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .collect()
    };
    let result = if dir.join("active-generation").exists() {
        generations::sync(dir).map(|level| {
            Some(format!(
                "semantic pointer durability: {level:?}; power-loss durability unverified"
            ))
        })
    } else {
        Ok(None)
    };
    let (result, durability) = match result {
        Ok(durability) => (generations::cleanup(dir, selected, &protected), durability),
        Err(error) => (
            Err(format!("semantic pointer namespace sync: {error}")),
            None,
        ),
    };
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(lane) = state
        .schedule
        .lanes
        .get_mut(&LaneKey::current(name))
        .filter(|l| l.token == token)
    {
        if durability.is_some() {
            lane.durability_reason = durability;
        }
        match result {
            Ok(true) => {
                lane.cleaned_dir = Some(dir.to_path_buf());
                lane.cleanup_error = None;
            }
            Ok(false) => {
                lane.cleaned_dir = None;
                lane.cleanup_error = None;
            }
            Err(error) => {
                lane.cleaned_dir = None;
                lane.cleanup_error = Some(error.clone());
                report(shared, name, ComponentState::Degraded, Some(error));
            }
        }
    }
}

pub(super) fn run(shared: Arc<Shared>, name: String, token: u64, source: OwnedPublishedReader) {
    let existing_quarantine = {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .quarantined
            .iter()
            .find(|(identity, _)| {
                identity.binding == name
                    && state.schedule.model.as_deref() == Some(identity.model_digest.as_str())
            })
            .map(|(_, reason)| reason.clone())
    };
    if let Some(reason) = existing_quarantine {
        report(&shared, &name, ComponentState::Degraded, Some(reason));
    } else {
        report(&shared, &name, ComponentState::Starting, None);
    }
    'writer: loop {
        reclaim(&shared, &name);
        let (model, generation) = {
            let lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let Some(lane) = lock
                    .schedule
                    .lanes
                    .get(&LaneKey::current(&name))
                    .filter(|lane| lane.token == token)
                else {
                    let (ready, waiting) = lock
                        .retiring_readers
                        .iter()
                        .filter(|(binding, _)| binding == &name)
                        .fold((false, false), |(ready, waiting), (_, r)| {
                            let count = r.count.lock().unwrap_or_else(|e| e.into_inner());
                            let retired = r
                                .retired
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .is_some();
                            (ready || (*count == 0 && retired), waiting || *count != 0)
                        });
                    #[cfg(test)]
                    if ready || waiting {
                        if let Some(hook) = shared
                            .reclaim_decision_hook
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone()
                        {
                            hook();
                        }
                    }
                    if ready {
                        drop(lock);
                        continue 'writer;
                    }
                    if waiting {
                        drop(shared.changed.wait(lock).unwrap_or_else(|e| e.into_inner()));
                        continue 'writer;
                    }
                    return;
                };
                if lock.stopping {
                    return;
                }
                if lane.quarantined {
                    drop(shared.changed.wait(lock).unwrap_or_else(|e| e.into_inner()));
                    continue 'writer;
                }
                let now = (shared.clock)();
                if let Some(at) = lane.writer_retry.until.filter(|at| *at > now) {
                    let wait = at.saturating_duration_since(now);
                    drop(
                        shared
                            .changed
                            .wait_timeout(lock, wait)
                            .unwrap_or_else(|e| e.into_inner()),
                    );
                    continue 'writer;
                }
                if let Some(model) = &lock.schedule.model {
                    break (model.clone(), lane.generation);
                }
                drop(shared.changed.wait(lock).unwrap_or_else(|e| e.into_inner()));
                continue 'writer;
            }
        };
        match session(&shared, &name, token, generation, &model, &source) {
            Ok(()) => {}
            Err(error) => {
                let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(lane) = lock
                    .schedule
                    .lanes
                    .get_mut(&LaneKey::current(&name))
                    .filter(|lane| lane.token == token && lane.generation == generation)
                {
                    lane.invalidate();
                    lane.dirty = true;
                    if let Some(repair) = lane
                        .repair
                        .as_mut()
                        .filter(|r| r.state == SemanticRepairState::Building)
                    {
                        repair.reason = Some(error.clone());
                        if error.starts_with("repair validation:")
                            || error.starts_with("repair publication:")
                        {
                            repair.state = SemanticRepairState::Failed;
                        } else {
                            lane.writer_retry.fail((shared.clock)());
                        }
                        lane.state = ComponentState::Degraded;
                        lane.reason = Some(error.clone());
                        report(&shared, &name, ComponentState::Degraded, Some(error));
                    } else {
                        lane.state = ComponentState::Degraded;
                        lane.reason = Some(error.clone());
                        lane.writer_retry.fail((shared.clock)());
                        report(&shared, &name, ComponentState::Degraded, Some(error));
                    }
                }
                drop(lock);
                shared.changed.notify_all();
            }
        }
    }
}

fn session(
    shared: &Arc<Shared>,
    name: &str,
    token: u64,
    generation: u64,
    model: &str,
    source: &OwnedPublishedReader,
) -> Result<()> {
    let start = source
        .read_snapshot()
        .map_err(|e| e.to_string())?
        .position()
        .clone();
    let start = position(&start)?;
    let mut identity = VectorFileIdentity {
        binding: name.into(),
        kind: VectorFileKind::Current,
        epoch: start.epoch.clone(),
        model_digest: model.into(),
        config: shared.config.config_id.clone(),
        generation: "initial".into(),
        dimension: shared.config.dimension,
    };
    let initial_path = super::identity::path(&shared.config, &identity);
    let dir = initial_path.parent().unwrap();
    let gate = {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lane) = state
            .schedule
            .lanes
            .get(&LaneKey::current(name))
            .filter(|lane| lane.token == token && lane.generation == generation)
        else {
            return Ok(());
        };
        lane.io_gate.clone()
    };
    let io_guard = gate.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let selected = generations::selected(dir)?;
    let repair_id = {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lane) = state
            .schedule
            .lanes
            .get(&LaneKey::current(name))
            .filter(|l| l.token == token && l.generation == generation)
        else {
            return Ok(());
        };
        lane.repair
            .as_ref()
            .filter(|r| r.state == SemanticRepairState::Building && !r.published)
            .map(|r| r.id.clone())
    };
    identity.generation = repair_id.clone().unwrap_or_else(|| selected.clone());
    let path = generations::file(dir, &identity.generation);
    let quarantined = {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .quarantined
            .iter()
            .find(|(id, _)| id == &identity)
            .map(|(_, reason)| reason.clone())
    };
    if let Some(reason) = quarantined {
        quarantine::mark_guarded(
            shared,
            name,
            token,
            &identity,
            false,
            Some(generation),
            &reason,
        );
        return Ok(());
    }
    if path.exists() {
        let view = VectorFileReadView::open(&path, &identity).map_err(|error| {
            let reason = error.to_string();
            if quarantine::is_corrupt(&error) {
                quarantine::mark_guarded(
                    shared,
                    name,
                    token,
                    &identity,
                    false,
                    Some(generation),
                    &reason,
                );
            }
            reason
        })?;
        let progress = view.progress();
        if progress
            .ack
            .as_ref()
            .is_some_and(|p| p.sequence > start.sequence)
            || progress
                .inventory
                .as_ref()
                .is_some_and(|i| i.start.sequence > start.sequence)
            || progress.complete && progress.ack.is_none()
            || progress.ack.is_some() && progress.inventory.is_none()
            || progress
                .ack
                .as_ref()
                .zip(progress.inventory.as_ref())
                .is_some_and(|(ack, inventory)| ack.sequence < inventory.start.sequence)
        {
            quarantine::mark_guarded(
                shared,
                name,
                token,
                &identity,
                false,
                Some(generation),
                "invalid vector progress metadata",
            );
            return Err("invalid vector progress metadata".into());
        }
    } else if repair_id.is_none() {
        let pointer_exists = dir.join("active-generation").exists();
        let other_generation = std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name().to_string_lossy().starts_with("vector-")
                    && e.file_name().to_string_lossy().ends_with(".sqlite")
            });
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lane) = state
            .schedule
            .lanes
            .get_mut(&LaneKey::current(name))
            .filter(|lane| lane.token == token && lane.generation == generation)
        {
            lane.active = None;
            lane.ack = None;
            lane.complete = false;
            if pointer_exists || other_generation {
                lane.invalidate();
                lane.state = ComponentState::Degraded;
                lane.reason = Some("active semantic generation is missing; rebuilding".into());
                lane.repair = Some(super::state::Repair {
                    id: uuid::Uuid::new_v4().to_string(),
                    state: SemanticRepairState::Building,
                    published: false,
                    ack: None,
                    reason: None,
                });
                drop(state);
                shared.changed.notify_all();
                report(
                    shared,
                    name,
                    ComponentState::Degraded,
                    Some("active semantic generation is missing; rebuilding".into()),
                );
                return Ok(());
            }
        }
    }
    if repair_id.is_none() {
        let needs_cleanup = {
            let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(lane) = state
                .schedule
                .lanes
                .get(&LaneKey::current(name))
                .filter(|l| l.token == token && l.generation == generation)
            else {
                return Ok(());
            };
            lane.cleaned_dir.as_deref() != Some(dir)
        };
        if needs_cleanup {
            cleanup_dir(shared, name, token, dir, &selected);
        }
    }
    let mut writer = VectorFileWriter::open(&path, &identity).map_err(|error| {
        let reason = error.to_string();
        if quarantine::is_corrupt(&error) {
            quarantine::mark_guarded(
                shared,
                name,
                token,
                &identity,
                false,
                Some(generation),
                &reason,
            );
        }
        reason
    })?;
    let progress = writer.progress().map_err(|error| {
        let reason = error.to_string();
        if quarantine::is_corrupt(&error) {
            quarantine::mark_guarded(
                shared,
                name,
                token,
                &identity,
                false,
                Some(generation),
                &reason,
            );
        }
        reason
    })?;
    if progress
        .ack
        .as_ref()
        .is_some_and(|p| p.sequence > start.sequence)
        || progress
            .inventory
            .as_ref()
            .is_some_and(|i| i.start.sequence > start.sequence)
    {
        quarantine::mark_guarded(
            shared,
            name,
            token,
            &identity,
            false,
            Some(generation),
            "vector progress exceeds source position",
        );
        return Err("vector progress exceeds source position".into());
    }
    drop(io_guard);
    let result = session_run(
        shared,
        name,
        token,
        generation,
        model,
        source,
        &start,
        &path,
        &identity,
        &gate,
        &mut writer,
        progress,
    );
    if result.is_ok()
        || result
            .as_ref()
            .is_err_and(|error| error == "binding closed")
    {
        let _guard = gate.lock().unwrap_or_else(|e| e.into_inner());
        let should_checkpoint = {
            let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.stopping || state.closing.contains(name))
                && !state.quarantined.iter().any(|(id, _)| id == &identity)
        };
        if should_checkpoint {
            writer.checkpoint().map_err(|error| error.to_string())?;
        }
    }
    drop(writer);
    if repair_id.is_some()
        && result.as_ref().is_err_and(|e| {
            e.starts_with("repair validation:") || e.starts_with("repair publication:")
        })
        && generations::selected(dir).ok().as_deref() != Some(identity.generation.as_str())
    {
        if let generations::Removal::Failed { path, reason } = generations::remove_files(&path) {
            let message = format!("cleanup {}: {reason}", path.display());
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(lane) = state
                .schedule
                .lanes
                .get_mut(&LaneKey::current(name))
                .filter(|l| l.token == token)
            {
                lane.cleaned_dir = None;
                lane.cleanup_error = Some(message.clone());
            }
            drop(state);
            report(shared, name, ComponentState::Degraded, Some(message));
        }
    }
    result
}

fn session_run(
    shared: &Arc<Shared>,
    name: &str,
    token: u64,
    generation: u64,
    model: &str,
    source: &OwnedPublishedReader,
    start: &SourcePosition,
    path: &std::path::Path,
    identity: &VectorFileIdentity,
    gate: &std::sync::Mutex<()>,
    mut writer: &mut VectorFileWriter,
    mut progress: VectorFileProgress,
) -> Result<()> {
    if progress.inventory.is_none() && progress.ack.is_none() {
        let inventory = InventoryCursor {
            start: start.clone(),
            completed_identity: String::new(),
        };
        guarded_commit(
            shared,
            name,
            token,
            generation,
            &identity,
            &gate,
            &mut writer,
            VectorFileBatch {
                inventory: Some(inventory),
                ..Default::default()
            },
        )?;
        committed(shared, name, token, generation);
        progress = writer
            .progress()
            .map_err(|e| vector_failure(shared, name, token, generation, &identity, &gate, e))?;
    }
    {
        let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lane) = lock
            .schedule
            .lanes
            .get_mut(&LaneKey::current(name))
            .filter(|l| l.token == token && l.generation == generation)
        else {
            return Ok(());
        };
        if lane.repair.as_ref().is_some_and(|r| {
            r.state == SemanticRepairState::Building && !r.published && r.id == identity.generation
        }) {
            lane.repair.as_mut().unwrap().ack = progress.ack.clone();
        } else {
            lane.ack = progress.ack.clone();
            lane.complete = progress.complete;
            lane.active = Some(ActiveFile {
                path: path.to_path_buf(),
                identity: identity.clone(),
            });
        }
        lane.state = ComponentState::Busy;
        lane.reason = None;
        lane.dirty = false;
        report(shared, name, ComponentState::Busy, None);
    }
    shared.changed.notify_all();
    if progress.ack.is_none() {
        let mut cursor = progress
            .inventory
            .clone()
            .ok_or("missing inventory metadata")?;
        loop {
            if !current(shared, name, token, generation, model) {
                return Ok(());
            }
            let after = (!cursor.completed_identity.is_empty())
                .then_some(cursor.completed_identity.as_str());
            let (page_pos, items, more) =
                source.inventory_page(after, 1).map_err(|e| e.to_string())?;
            if page_pos.epoch != cursor.start.epoch {
                return Err("source epoch changed during inventory".into());
            }
            if cursor.completed_identity.is_empty() {
                // No identity has been enumerated. Start replay at the actual first page.
                cursor.start = position(&page_pos)?;
            }
            for (kind, id) in items {
                let guard = source.read_snapshot().map_err(|e| e.to_string())?;
                if guard.position().epoch != cursor.start.epoch {
                    return Err("source epoch changed during inventory".into());
                }
                let unit = if kind == "procedure" {
                    guard.acquire_procedure(&id)
                } else {
                    guard.acquire_current(&id)
                }
                .map_err(|e| e.to_string())?;
                drop(guard);
                if !unit_work(
                    shared,
                    name,
                    token,
                    generation,
                    model,
                    &identity,
                    &gate,
                    &mut writer,
                    unit,
                    &id,
                    kind == "procedure",
                    &mut Vec::new(),
                )? {
                    return Ok(());
                }
                cursor.completed_identity = format!("{kind}/{id}");
                guarded_commit(
                    shared,
                    name,
                    token,
                    generation,
                    &identity,
                    &gate,
                    &mut writer,
                    VectorFileBatch {
                        inventory: Some(cursor.clone()),
                        ..Default::default()
                    },
                )?;
                committed(shared, name, token, generation);
            }
            if !more {
                break;
            }
        }
        guarded_commit(
            shared,
            name,
            token,
            generation,
            &identity,
            &gate,
            &mut writer,
            VectorFileBatch {
                ack: Some(cursor.start.clone()),
                ..Default::default()
            },
        )?;
        committed(shared, name, token, generation);
        progress.ack = Some(cursor.start);
    }
    let mut ack = progress.ack.ok_or("missing ack")?;
    loop {
        if !current(shared, name, token, generation, model) {
            return Ok(());
        }
        let guard = source.read_snapshot().map_err(|e| e.to_string())?;
        let page = guard
            .read_published_paths(&storage_position(&ack)?, 1)
            .map_err(|e| e.to_string())?;
        let covered = position(&page.covered_through)?;
        let paths = page.canonical_paths;
        drop(guard);
        let mut publication_heads = Vec::new();
        for path in paths {
            let procedure = path.starts_with("current/procedure/");
            if !procedure && !path.starts_with("current/memory/") {
                continue;
            }
            let id = std::path::Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("invalid current path")?;
            let guard = source.read_snapshot().map_err(|e| e.to_string())?;
            if guard.position().epoch != ack.epoch {
                return Err("source epoch changed during replay".into());
            }
            let unit = if procedure {
                guard.acquire_procedure(id)
            } else {
                guard.acquire_current(id)
            }
            .map_err(|e| e.to_string())?;
            drop(guard);
            if !unit_work(
                shared,
                name,
                token,
                generation,
                model,
                &identity,
                &gate,
                &mut writer,
                unit,
                id,
                procedure,
                &mut publication_heads,
            )? {
                return Ok(());
            }
        }
        let caught_up = covered.sequence
            == i64::try_from(page.source_position.sequence)
                .map_err(|_| "source sequence overflow")?;
        if covered != ack || (!progress.complete && caught_up) {
            guarded_commit(
                shared,
                name,
                token,
                generation,
                &identity,
                &gate,
                &mut writer,
                VectorFileBatch {
                    heads: publication_heads,
                    ack: Some(covered.clone()),
                    complete: caught_up || progress.complete,
                    ..Default::default()
                },
            )?;
            committed(shared, name, token, generation);
            ack = covered;
            progress.complete |= caught_up;
        }
        if caught_up {
            let is_repair = {
                let lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                lock.schedule
                    .lanes
                    .get(&LaneKey::current(name))
                    .is_some_and(|lane| {
                        lane.token == token
                            && lane.generation == generation
                            && lane.repair.as_ref().is_some_and(|r| {
                                r.state == SemanticRepairState::Building
                                    && !r.published
                                    && r.id == identity.generation
                            })
                    })
            };
            if is_repair {
                writer.checkpoint().map_err(|e| e.to_string())?;
                #[cfg(test)]
                if let Some(hook) = shared
                    .repair_validation_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                {
                    hook(path);
                }
                let view = VectorFileReadView::open(path, identity).map_err(|e| {
                    if quarantine::is_corrupt(&e) {
                        format!("repair validation: {e}")
                    } else {
                        e.to_string()
                    }
                })?;
                view.validate().map_err(|e| {
                    if quarantine::is_corrupt(&e) {
                        format!("repair validation: {e}")
                    } else {
                        e.to_string()
                    }
                })?;
                let verified = view.progress();
                if !verified.complete
                    || verified.ack.as_ref() != Some(&ack)
                    || verified.inventory.as_ref().is_none_or(|i| {
                        i.start.epoch != ack.epoch || i.start.sequence > ack.sequence
                    })
                    || ack.epoch != start.epoch
                    || ack.sequence < start.sequence
                {
                    return Err("repair validation: invalid repair progress".into());
                }
                drop(view);
                let _io = gate.lock().unwrap_or_else(|e| e.into_inner());
                {
                    let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    if lock.stopping || lock.schedule.model.as_deref() != Some(model) {
                        return Ok(());
                    }
                    let Some(lane) =
                        lock.schedule
                            .lanes
                            .get_mut(&LaneKey::current(name))
                            .filter(|l| {
                                l.token == token
                                    && l.generation == generation
                                    && l.repair.as_ref().is_some_and(|r| {
                                        r.state == SemanticRepairState::Building
                                            && !r.published
                                            && r.id == identity.generation
                                    })
                            })
                    else {
                        return Ok(());
                    };
                    if lane
                        .active
                        .as_ref()
                        .is_some_and(|active| active.path == path && active.identity == *identity)
                    {
                        let repair = lane.repair.as_mut().unwrap();
                        repair.published = true;
                        repair.state = SemanticRepairState::Complete;
                        shared.changed.notify_all();
                        return Ok(());
                    }
                    lane.transition = Some(identity.generation.clone());
                }
                #[cfg(test)]
                if let Some(hook) = shared
                    .repair_transition_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                {
                    hook();
                }
                let publication =
                    generations::publish(path.parent().unwrap(), &identity.generation);
                #[cfg(test)]
                if matches!(&publication, Ok(generations::Publication::Replaced(_))) {
                    generations::crash_point("before_ram");
                }
                let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                let valid = !lock.stopping && lock.schedule.model.as_deref() == Some(model);
                let Some(lane) = lock
                    .schedule
                    .lanes
                    .get_mut(&LaneKey::current(name))
                    .filter(|l| {
                        l.token == token
                            && l.transition.as_deref() == Some(identity.generation.as_str())
                    })
                else {
                    return Ok(());
                };
                lane.transition = None;
                if !valid
                    || lane.generation != generation
                    || !lane.repair.as_ref().is_some_and(|r| {
                        r.state == SemanticRepairState::Building
                            && !r.published
                            && r.id == identity.generation
                    })
                {
                    return Ok(());
                }
                let level = match publication {
                    Err(error) => return Err(format!("repair publication: {error}")),
                    Ok(generations::Publication::ReplacedButUnsynced(error)) => {
                        return Err(format!("semantic pointer namespace sync: {error}"))
                    }
                    Ok(generations::Publication::Replaced(level)) => level,
                };
                let old = lane.active.replace(ActiveFile {
                    path: path.to_path_buf(),
                    identity: identity.clone(),
                });
                let readers = std::mem::replace(
                    &mut lane.readers,
                    Arc::new(super::state::Readers::default()),
                );
                *lane.readers.wake.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Arc::downgrade(shared));
                lane.repair.as_mut().unwrap().ack = Some(ack.clone());
                lane.repair.as_mut().unwrap().reason = None;
                lane.repair.as_mut().unwrap().published = true;
                lane.ack = Some(ack.clone());
                lane.complete = true;
                lane.active_quarantined = false;
                lane.quarantined = false;
                lane.durability_reason = Some(format!(
                    "semantic pointer durability: {level:?}; power-loss durability unverified"
                ));
                if let Some(old) = old.filter(|old| old.path != path && old.identity != *identity) {
                    readers.retire(old.path);
                    lock.retiring_readers.push((name.to_owned(), readers));
                }
                drop(lock);
                #[cfg(test)]
                if let Some(hook) = shared
                    .repair_published_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                {
                    hook();
                }
                #[cfg(test)]
                generations::crash_point("after_ram");
                cleanup_dir(
                    shared,
                    name,
                    token,
                    path.parent().unwrap(),
                    &identity.generation,
                );
                let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(lane) = lock
                    .schedule
                    .lanes
                    .get_mut(&LaneKey::current(name))
                    .filter(|l| l.token == token)
                {
                    let error = lane.cleanup_error.clone();
                    if let Some(repair) = lane.repair.as_mut().filter(|r| {
                        r.id == identity.generation
                            && r.published
                            && r.state == SemanticRepairState::Building
                    }) {
                        repair.state = if error.is_some() {
                            SemanticRepairState::Failed
                        } else {
                            SemanticRepairState::Complete
                        };
                        repair.reason = error;
                    }
                }
            }
        }
        {
            let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(lane) = lock
                .schedule
                .lanes
                .get_mut(&LaneKey::current(name))
                .filter(|l| l.token == token && l.generation == generation)
            else {
                return Ok(());
            };
            if let Some(repair) = lane.repair.as_mut().filter(|r| {
                r.state == SemanticRepairState::Building
                    && !r.published
                    && r.id == identity.generation
            }) {
                repair.ack = Some(ack.clone());
            } else {
                lane.ack = Some(ack.clone());
                lane.complete = progress.complete;
            }
            if caught_up {
                lane.state = if lane.cleanup_error.is_some() {
                    ComponentState::Degraded
                } else {
                    ComponentState::Ready
                };
                lane.reason = lane.cleanup_error.clone();
                report(
                    shared,
                    name,
                    lane.state,
                    lane.cleanup_error
                        .clone()
                        .or_else(|| lane.durability_reason.clone()),
                );
            }
        }
        shared.changed.notify_all();
        if caught_up {
            super::identity::reclaim_previous(&shared.config, identity);
            loop {
                reclaim(shared, name);
                let lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if lock.stopping
                    || lock.schedule.model.as_deref() != Some(model)
                    || !lock
                        .schedule
                        .lanes
                        .get(&LaneKey::current(name))
                        .is_some_and(|l| l.token == token && l.generation == generation)
                {
                    return Ok(());
                }
                let eligible = lock.retiring_readers.iter().any(|(binding, r)| {
                    binding == name
                        && *r.count.lock().unwrap_or_else(|e| e.into_inner()) == 0
                        && r.retired
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .is_some()
                });
                if eligible {
                    drop(lock);
                    continue;
                }
                drop(shared.changed.wait(lock).unwrap_or_else(|e| e.into_inner()));
            }
        }
    }
}

fn unit_work(
    shared: &Shared,
    name: &str,
    token: u64,
    generation: u64,
    model: &str,
    identity: &VectorFileIdentity,
    gate: &std::sync::Mutex<()>,
    writer: &mut VectorFileWriter,
    unit: agentlaw_storage::acquire::AcquiredCurrent,
    id: &str,
    procedure: bool,
    publication_heads: &mut Vec<CurrentHeads>,
) -> Result<bool> {
    let (heads, pieces) = source::map_acquired(unit, id, procedure, shared.config.window_bytes())?;
    let completed_heads = heads.clone();
    let finished = super::batch::run(
        shared,
        &LaneKey::current(name),
        token,
        generation,
        model,
        identity,
        gate,
        writer,
        pieces,
        super::batch::Completion::Current(Some(heads)),
    )?;
    if finished {
        publication_heads.push(completed_heads);
    }
    Ok(finished)
}
