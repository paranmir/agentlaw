//! Per-binding source and vector writer. No source or SQLite I/O holds the RAM lock.
use super::state::{ActiveFile, LaneKey};
use super::{quarantine, source, Shared};
use crate::ports::{ComponentId, ComponentKind, ComponentState, ComponentStatus};
use agentlaw_search::vector_file::{
    CurrentHeads, InventoryCursor, SourcePosition, VectorFileBatch, VectorFileError,
    VectorFileIdentity, VectorFileKind, VectorFileProgress, VectorFileReadView, VectorFileWriter,
};
use agentlaw_storage::published::{OwnedPublishedReader, SourcePosition as StoragePosition};
use sha2::{Digest, Sha256};
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
fn id_path(shared: &Shared, binding: &str, model: &str, epoch: &str) -> std::path::PathBuf {
    let mut hash = Sha256::new();
    for field in [binding, "current", epoch, model, &shared.config.config_id] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hash.update((shared.config.dimension as u64).to_be_bytes());
    hash.update((shared.config.chunk_bytes as u64).to_be_bytes());
    shared
        .config
        .indices_root
        .join(format!("{:x}", hash.finalize()))
        .join("vector.sqlite")
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
    loop {
        let (model, generation) = {
            let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let Some(lane) = lock
                    .schedule
                    .lanes
                    .get(&LaneKey::current(&name))
                    .filter(|lane| lane.token == token)
                else {
                    return;
                };
                if lock.stopping {
                    return;
                }
                if lane.quarantined {
                    lock = shared.changed.wait(lock).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                let now = (shared.clock)();
                if let Some(at) = lane.writer_retry.until.filter(|at| *at > now) {
                    let wait = at.saturating_duration_since(now);
                    lock = shared
                        .changed
                        .wait_timeout(lock, wait)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                    continue;
                }
                if let Some(model) = &lock.schedule.model {
                    break (model.clone(), lane.generation);
                }
                lock = shared.changed.wait(lock).unwrap_or_else(|e| e.into_inner());
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
                    lane.state = ComponentState::Degraded;
                    lane.reason = Some(error.clone());
                    lane.writer_retry.fail((shared.clock)());
                    report(&shared, &name, ComponentState::Degraded, Some(error));
                }
                drop(lock);
                shared.changed.notify_all();
            }
        }
    }
}

fn session(
    shared: &Shared,
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
    let identity = VectorFileIdentity {
        binding: name.into(),
        kind: VectorFileKind::Current,
        epoch: start.epoch.clone(),
        model_digest: model.into(),
        config: shared.config.config_id.clone(),
        generation: "initial".into(),
        dimension: shared.config.dimension,
    };
    let path = id_path(shared, name, model, &start.epoch);
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
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
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
    } else {
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
    result
}

fn session_run(
    shared: &Shared,
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
        lane.ack = progress.ack.clone();
        lane.complete = progress.complete;
        lane.active = Some(ActiveFile {
            path: path.to_path_buf(),
            identity: identity.clone(),
        });
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
            lane.ack = Some(ack.clone());
            lane.complete = progress.complete;
            if caught_up {
                lane.state = ComponentState::Ready;
                report(shared, name, ComponentState::Ready, None);
            }
        }
        shared.changed.notify_all();
        if caught_up {
            let mut lock = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
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
                lock = shared.changed.wait(lock).unwrap_or_else(|e| e.into_inner());
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
    let (heads, pieces) = source::map_acquired(unit, id, procedure, shared.config.chunk_bytes)?;
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
