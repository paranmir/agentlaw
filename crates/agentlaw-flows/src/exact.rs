//! Rebuildable, disk-backed exact membership index. No current body inventory is
//! retained in Rust; source publication pages update only changed identities.
use crate::*;
use agentlaw_storage::{
    published::{PublishedChangeSource, SourcePosition},
    CurrentUnit, UnitState,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, Weak},
};
#[cfg(test)]
mod tests;

/// A validated pair of derived files. Readers hold its Arc while using either file.
pub struct ProjectionGeneration {
    id: String,
    epoch: String,
    exact_file: PathBuf,
    lexical_file: PathBuf,
}

pub struct PinnedProjection {
    exact: ExactIndex,
    lexical: agentlaw_search::ReadView,
    minimum_ack: SourcePosition,
    maximum_ack: u64,
    // Drop the read handles before releasing the file-retention reference.
    generation: Arc<ProjectionGeneration>,
}

pub struct ShadowGeneration {
    generation: ProjectionGeneration,
}

pub struct StagedActivation {
    registry_id: uuid::Uuid,
    generation: Arc<ProjectionGeneration>,
}
impl StagedActivation {
    pub fn generation_id(&self) -> &str {
        &self.generation.id
    }
    pub fn exact_file(&self) -> &Path {
        &self.generation.exact_file
    }
    pub fn epoch(&self) -> &str {
        &self.generation.epoch
    }
    pub fn lexical_file(&self) -> &Path {
        &self.generation.lexical_file
    }
    pub fn into_files(self) -> (PathBuf, PathBuf) {
        (
            self.generation.exact_file.clone(),
            self.generation.lexical_file.clone(),
        )
    }
}

// Weak references allow a restarted maintainer in this process to avoid reclaiming
// files still held by a registry, a staged activation, or a pinned reader.
static KNOWN_GENERATIONS: std::sync::OnceLock<
    Mutex<Vec<(Weak<ProjectionGeneration>, PathBuf, PathBuf)>>,
> = std::sync::OnceLock::new();
/// Obtain a common filesystem identity for path comparisons without changing files.
pub fn projection_path_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| -> std::io::Result<PathBuf> {
            let parent = path.parent().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "missing parent")
            })?;
            let name = path
                .file_name()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing name"))?;
            Ok(std::fs::canonicalize(parent)?.join(name))
        })
        .unwrap_or_else(|_| path.to_path_buf())
}
fn register_generation(generation: &Arc<ProjectionGeneration>) {
    let exact = projection_path_key(&generation.exact_file);
    let lexical = projection_path_key(&generation.lexical_file);
    let mut known = KNOWN_GENERATIONS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    known.retain(|(item, _, _)| item.strong_count() != 0);
    known.push((Arc::downgrade(generation), exact, lexical));
}
/// True only if no generation in this process still references the file.
pub fn projection_file_unreferenced(path: &Path) -> bool {
    let key = projection_path_key(path);
    let mut known = KNOWN_GENERATIONS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    known.retain(|(item, _, _)| item.strong_count() != 0);
    !known
        .iter()
        .any(|(_, exact, lexical)| *exact == key || *lexical == key)
}
struct RetiredGeneration {
    generation: Weak<ProjectionGeneration>,
    exact: PathBuf,
    lexical: PathBuf,
}
struct AcquisitionState {
    accepting: bool,
    version: u64,
    in_flight: usize,
}
struct AcquisitionGate {
    state: Mutex<AcquisitionState>,
    drained: Condvar,
}
impl AcquisitionGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(AcquisitionState {
                accepting: true,
                version: 0,
                in_flight: 0,
            }),
            drained: Condvar::new(),
        }
    }
    fn acquire(&self) -> Result<Acquisition<'_>> {
        let mut state = self.state.lock().map_err(|_| not_ready())?;
        if !state.accepting {
            return Err(not_ready());
        }
        state.in_flight = state.in_flight.checked_add(1).ok_or_else(not_ready)?;
        Ok(Acquisition {
            gate: self,
            version: state.version,
        })
    }
    fn stop(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| not_ready())?;
        if state.accepting {
            state.version = state.version.checked_add(1).ok_or_else(not_ready)?;
            state.accepting = false;
        }
        Ok(())
    }
    fn start(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| not_ready())?;
        if !state.accepting {
            state.version = state.version.checked_add(1).ok_or_else(not_ready)?;
            state.accepting = true;
        }
        Ok(())
    }
    fn wait(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| not_ready())?;
        while state.in_flight != 0 {
            state = self.drained.wait(state).map_err(|_| not_ready())?;
        }
        Ok(())
    }
}
struct Acquisition<'a> {
    gate: &'a AcquisitionGate,
    version: u64,
}
impl Acquisition<'_> {
    fn valid(&self) -> Result<bool> {
        let state = self.gate.state.lock().map_err(|_| not_ready())?;
        Ok(state.accepting && state.version == self.version)
    }
}
impl Drop for Acquisition<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.gate.drained.notify_all();
        }
    }
}

struct GenerationState {
    current: Arc<ProjectionGeneration>,
    acquisition: Arc<AcquisitionGate>,
    retired: Vec<RetiredGeneration>,
    untrusted: bool,
}
pub struct ProjectionGenerations {
    id: uuid::Uuid,
    state: Mutex<GenerationState>,
}
fn not_ready() -> DomainError {
    DomainError::new("exact_not_ready", "The exact projection is not ready.")
}
fn projection_corrupt() -> DomainError {
    DomainError::new(
        "exact_index_corrupt",
        "Projection schema or metadata is incomplete; rebuild required.",
    )
}
impl ProjectionGenerations {
    pub fn open(
        id: impl Into<String>,
        epoch: impl Into<String>,
        exact_file: impl Into<PathBuf>,
        lexical_file: impl Into<PathBuf>,
    ) -> Result<Self> {
        let registry = Self::from_generation(ProjectionGeneration {
            id: id.into(),
            epoch: epoch.into(),
            exact_file: exact_file.into(),
            lexical_file: lexical_file.into(),
        })?;
        let generation = registry
            .state
            .lock()
            .map_err(|_| not_ready())?
            .current
            .clone();
        if generation.exact_file.parent() != generation.lexical_file.parent() {
            return Err(not_ready());
        }
        if !generation.exact_file.is_file() || !generation.lexical_file.is_file() {
            return Err(not_ready());
        }
        let filename = generation
            .exact_file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(not_ready)?;
        if filename != format!("exact-{}.sqlite", generation.id)
            && filename != format!("exact-repair-{}.sqlite", generation.id)
        {
            return Err(not_ready());
        }
        drop(registry.pin()?);
        Ok(registry)
    }
    /// Construct a registry for an untrusted active generation without opening it.
    pub fn open_for_recovery(
        id: impl Into<String>,
        epoch: impl Into<String>,
        exact_file: impl Into<PathBuf>,
        lexical_file: impl Into<PathBuf>,
    ) -> Result<Self> {
        let generation = ProjectionGeneration {
            id: id.into(),
            epoch: epoch.into(),
            exact_file: exact_file.into(),
            lexical_file: lexical_file.into(),
        };
        if generation.exact_file.parent() != generation.lexical_file.parent() {
            return Err(not_ready());
        }
        let name = generation
            .exact_file
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(not_ready)?;
        if name != format!("exact-{}.sqlite", generation.id)
            && name != format!("exact-repair-{}.sqlite", generation.id)
        {
            return Err(not_ready());
        }
        let registry = Self::from_generation(generation)?;
        registry.state.lock().map_err(|_| not_ready())?.untrusted = true;
        Ok(registry)
    }
    /// Retain an active descriptor while its files are temporarily inaccessible.
    pub fn open_unavailable(
        id: impl Into<String>,
        epoch: impl Into<String>,
        exact_file: impl Into<PathBuf>,
        lexical_file: impl Into<PathBuf>,
    ) -> Result<Self> {
        let registry = Self::open_for_recovery(id, epoch, exact_file, lexical_file)?;
        registry.state.lock().map_err(|_| not_ready())?.untrusted = false;
        Ok(registry)
    }
    fn from_generation(generation: ProjectionGeneration) -> Result<Self> {
        let generation = Arc::new(generation);
        register_generation(&generation);
        Ok(Self {
            id: uuid::Uuid::new_v4(),
            state: Mutex::new(GenerationState {
                current: generation,
                acquisition: Arc::new(AcquisitionGate::new()),
                retired: Vec::new(),
                untrusted: false,
            }),
        })
    }
    pub fn pin(&self) -> Result<PinnedProjection> {
        for _ in 0..2 {
            let (generation, gate) = {
                let state = self.state.lock().map_err(|_| not_ready())?;
                if state.untrusted {
                    return Err(not_ready());
                }
                (state.current.clone(), state.acquisition.clone())
            };
            let acquisition = match gate.acquire() {
                Ok(acquisition) => acquisition,
                Err(error) => {
                    let state = self.state.lock().map_err(|_| not_ready())?;
                    if !Arc::ptr_eq(&state.current, &generation) {
                        continue;
                    }
                    return Err(error);
                }
            };
            #[cfg(test)]
            tests::oracle::on_acquisition(self);
            let exact = db(Connection::open_with_flags(
                &generation.exact_file,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ))?;
            db(exact.execute_batch("PRAGMA temp_store=MEMORY; BEGIN DEFERRED"))?;
            let (epoch, sequence, pointer): (String, String, String) = db(exact.query_row("SELECT p.epoch,p.sequence,l.filename FROM exact_position p JOIN exact_lexical l ON l.singleton=p.singleton WHERE p.singleton=1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))))?;
            if generation.lexical_file.file_name().and_then(|v| v.to_str())
                != Some(pointer.as_str())
            {
                drop(acquisition);
                #[cfg(test)]
                tests::oracle::on_pointer_mismatch(self);
                continue;
            }
            if epoch != generation.epoch {
                return Err(not_ready());
            }
            let sequence: u64 = sequence.parse().map_err(|_| not_ready())?;
            i64::try_from(sequence).map_err(|_| not_ready())?;
            let lexical =
                agentlaw_search::ReadView::open(&generation.lexical_file).map_err(lexical_error)?;
            let lexical_ack = u64::try_from(lexical.stamp.ack).map_err(|_| not_ready())?;
            let minimum_ack = SourcePosition {
                epoch,
                sequence: sequence.min(lexical_ack),
            };
            let directory = generation
                .exact_file
                .parent()
                .ok_or_else(not_ready)?
                .to_path_buf();
            {
                let state = self.state.lock().map_err(|_| not_ready())?;
                if !Arc::ptr_eq(&state.current, &generation) {
                    continue;
                }
                if state.untrusted || !acquisition.valid()? {
                    return Err(not_ready());
                }
            }
            return Ok(PinnedProjection {
                generation,
                exact: ExactIndex {
                    connection: exact,
                    directory,
                },
                lexical,
                minimum_ack,
                maximum_ack: sequence.max(lexical_ack),
            });
        }
        Err(not_ready())
    }
    /// Reject new pins until a validated shadow replaces the damaged generation.
    pub fn mark_untrusted(&self) -> Result<()> {
        let gate = {
            let mut state = self.state.lock().map_err(|_| not_ready())?;
            state.untrusted = true;
            state.acquisition.stop()?;
            state.acquisition.clone()
        };
        gate.wait()
    }
    /// Allow pins again after the active generation's writer has been reopened.
    pub fn mark_trusted(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| not_ready())?;
        state.acquisition.start()?;
        state.untrusted = false;
        Ok(())
    }
    pub fn stage(&self, shadow: ShadowGeneration) -> Result<StagedActivation> {
        let generation = Arc::new(shadow.generation);
        register_generation(&generation);
        Ok(StagedActivation {
            registry_id: self.id,
            generation,
        })
    }
    pub fn commit(&self, staged: StagedActivation) -> Result<()> {
        if staged.registry_id != self.id {
            return Err(not_ready());
        }
        let mut state = self.state.lock().map_err(|_| not_ready())?;
        state.acquisition.stop()?;
        state.acquisition = Arc::new(AcquisitionGate::new());
        state.untrusted = false;
        let old = std::mem::replace(&mut state.current, staged.generation);
        state.retired.push(RetiredGeneration {
            generation: Arc::downgrade(&old),
            exact: old.exact_file.clone(),
            lexical: old.lexical_file.clone(),
        });
        Ok(())
    }
    /// Returns file pairs only after no pinned reader can still use them.
    pub fn take_retired(&self) -> Vec<(PathBuf, PathBuf)> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        let mut ready = Vec::new();
        state.retired.retain(|item| {
            if item.generation.strong_count() == 0 {
                ready.push((item.exact.clone(), item.lexical.clone()));
                false
            } else {
                true
            }
        });
        ready
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DeltaBudget {
    pub max_identities: u32,
    pub max_resident_bytes: u64,
}
impl Default for DeltaBudget {
    fn default() -> Self {
        Self {
            max_identities: 1024,
            max_resident_bytes: 64 * 1024 * 1024,
        }
    }
}

pub struct ExactSnapshot {
    pinned: PinnedProjection,
    position: SourcePosition,
    removed: Vec<String>,
    units: Vec<CurrentUnit>,
    documents: Vec<agentlaw_search::SearchDocument>,
}
impl ExactSnapshot {
    fn shadows_id(&self, id: &str) -> bool {
        self.removed
            .iter()
            .any(|entry| entry == id || entry.strip_prefix("procedure/") == Some(id))
    }
    pub fn position(&self) -> &SourcePosition {
        &self.position
    }
    pub fn owned_body(&self, id: &str, version: &str) -> Option<&str> {
        let unit = self.units.iter().find(|unit| unit.entity_id == id)?;
        let key = index_id(unit);
        let index = self
            .documents
            .iter()
            .filter(|doc| doc.memory_id == key)
            .position(|doc| doc.change_id == version)?;
        if unit.entity_type == "memory" {
            self.documents
                .iter()
                .filter(|doc| doc.memory_id == key)
                .nth(index)
                .map(|doc| doc.body.as_str())
        } else {
            unit.heads().get(index).map(|head| head.body.as_str())
        }
    }

    pub fn identical_bodies(&self, body: &str) -> Result<Vec<String>> {
        let mut result: BTreeSet<_> = self
            .pinned
            .exact
            .identical_bodies(body)?
            .into_iter()
            .filter(|id| !self.shadows_id(id))
            .collect();
        for unit in &self.units {
            let matches = if unit.entity_type == "memory" {
                self.documents
                    .iter()
                    .any(|doc| doc.memory_id == unit.entity_id && doc.body == body)
            } else {
                unit.heads().iter().any(|head| head.body == body)
            };
            if matches {
                result.insert(unit.entity_id.clone());
            }
        }
        Ok(result.into_iter().collect())
    }

    pub fn procedure_ids(&self, context: &RequestContext) -> Result<Vec<String>> {
        let scopes = allowed_scopes(context);
        let mut result: BTreeSet<_> = self
            .pinned
            .exact
            .procedure_ids(context)?
            .into_iter()
            .filter(|id| !self.removed.contains(&format!("procedure/{id}")))
            .collect();
        for unit in &self.units {
            if unit.entity_type != "learned_procedure" {
                continue;
            }
            for head in unit.heads() {
                let scope = crate::runtime::scope_token(&crate::runtime::scope_value(
                    &head.metadata["applicability"],
                )?);
                if scopes.contains(&scope) {
                    result.insert(unit.entity_id.clone());
                }
            }
        }
        Ok(result.into_iter().collect())
    }

    pub fn work_targets(
        &self,
        project: &str,
        requested: &[WorkTarget],
    ) -> Result<Vec<(String, Reading)>> {
        let encode = |reading: &Reading| match reading {
            Reading::Required => "required",
            Reading::Related => "related",
        };
        let mut result = BTreeSet::new();
        for (id, reading) in self.pinned.exact.work_targets(project, requested)? {
            if !self.removed.contains(&id) {
                result.insert((id, encode(&reading).to_owned()));
            }
        }
        let requests = requested
            .iter()
            .map(|target| Ok((relative_path(&target.path)?, &target.kind)))
            .collect::<Result<Vec<_>>>()?;
        for unit in &self.units {
            for head in unit.heads() {
                for target in head.metadata["work_targets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let target: StoredTarget =
                        serde_json::from_value(target.clone()).map_err(|_| {
                            DomainError::new("source_corrupt", "Published work target is invalid.")
                        })?;
                    if target.project_id != project {
                        continue;
                    }
                    let path = relative_path(&target.path)?;
                    if requests
                        .iter()
                        .any(|(requested, kind)| target_path_matches(requested, kind, &path))
                    {
                        result.insert((unit.entity_id.clone(), encode(&target.reading).to_owned()));
                    }
                }
            }
        }
        Ok(result
            .into_iter()
            .map(|(id, reading)| {
                (
                    id,
                    if reading == "required" {
                        Reading::Required
                    } else {
                        Reading::Related
                    },
                )
            })
            .collect())
    }

    pub fn reverse_required(&self, targets: &[String]) -> Result<Vec<String>> {
        let mut result: BTreeSet<_> = self
            .pinned
            .exact
            .reverse_required(targets)?
            .into_iter()
            .filter(|id| !self.removed.contains(id))
            .collect();
        for unit in &self.units {
            for head in unit.heads() {
                for relation in head.metadata["relations"].as_array().into_iter().flatten() {
                    if relation["kind"] == "required"
                        && relation["target_memory_id"]
                            .as_str()
                            .is_some_and(|id| targets.iter().any(|v| v == id))
                    {
                        result.insert(unit.entity_id.clone());
                    }
                }
            }
        }
        Ok(result.into_iter().collect())
    }
    pub fn related(&self, ids: &[String]) -> Result<Vec<(String, String)>> {
        let mut result: BTreeSet<_> = self
            .pinned
            .exact
            .related(ids)?
            .into_iter()
            .filter(|(id, _)| !self.removed.contains(id))
            .collect();
        for unit in &self.units {
            if !ids.contains(&unit.entity_id) {
                continue;
            }
            for head in unit.heads() {
                for relation in head.metadata["relations"].as_array().into_iter().flatten() {
                    if relation["kind"] == "related" {
                        let target = relation["target_memory_id"].as_str().ok_or_else(|| {
                            DomainError::new("source_corrupt", "Relation target is missing.")
                        })?;
                        result.insert((unit.entity_id.clone(), target.to_string()));
                    }
                }
            }
        }
        Ok(result.into_iter().collect())
    }
    pub fn heads(&self, ids: &[String]) -> Result<Vec<HeadRecord>> {
        let mut result = self.pinned.exact.heads(ids)?;
        result.retain(|head| !self.shadows_id(&head.entity_id));
        for unit in &self.units {
            if !ids.contains(&unit.entity_id) {
                continue;
            }
            let key = index_id(unit);
            for (head, document) in unit.heads().iter().zip(
                self.documents
                    .iter()
                    .filter(|document| document.memory_id == key),
            ) {
                result.push(HeadRecord {
                    entity_id: unit.entity_id.clone(),
                    entity_type: unit.entity_type.clone(),
                    observed_version: document.change_id.clone(),
                    metadata: head.metadata.clone(),
                    excerpt: if unit.entity_type == "memory" {
                        document.body.chars().take(400).collect()
                    } else {
                        head.body.chars().take(400).collect()
                    },
                });
            }
        }
        result.sort_by(|a, b| {
            (&a.entity_id, &a.observed_version).cmp(&(&b.entity_id, &b.observed_version))
        });
        Ok(result)
    }
    pub fn ids_matching(
        &self,
        context: &RequestContext,
        rules: bool,
        tasks: bool,
    ) -> Result<Vec<String>> {
        let scopes = allowed_scopes(context);
        let mut result: BTreeSet<_> = self
            .pinned
            .exact
            .ids_matching(context, rules, tasks)?
            .into_iter()
            .filter(|id| !self.removed.contains(id))
            .collect();
        for unit in &self.units {
            if unit.entity_type != "memory" {
                continue;
            }
            for head in unit.heads() {
                if !scopes.contains(&index_scope(unit, head)?) {
                    continue;
                }
                if (rules && head.metadata["is_rule"].as_bool() == Some(true))
                    || (tasks && head.metadata["in_working_set"].as_bool() == Some(true))
                {
                    result.insert(unit.entity_id.clone());
                }
            }
        }
        Ok(result.into_iter().collect())
    }
    pub fn build(
        pinned: PinnedProjection,
        source: &agentlaw_storage::published::SourceReadGuard<'_>,
        budget: DeltaBudget,
    ) -> Result<Self> {
        let mut snapshot = Self {
            position: pinned.minimum_ack.clone(),
            pinned,
            removed: Vec::new(),
            units: Vec::new(),
            documents: Vec::new(),
        };
        snapshot.advance(source, budget)?;
        Ok(snapshot)
    }
    pub fn advance(
        &mut self,
        source: &agentlaw_storage::published::SourceReadGuard<'_>,
        budget: DeltaBudget,
    ) -> Result<()> {
        let end = source.position();
        if end.epoch != self.pinned.minimum_ack.epoch {
            return Err(DomainError::new(
                "source_epoch_changed",
                "Source epoch changed.",
            ));
        }
        u64_to_i64(end.sequence)?;
        if end.sequence < self.pinned.maximum_ack || end.sequence < self.position.sequence {
            return Err(not_ready());
        }
        let mut cursor = self.pinned.minimum_ack.clone();
        let mut identities = std::collections::BTreeSet::new();
        while cursor.sequence < end.sequence {
            let page = source
                .read_published_paths(&cursor, 1024)
                .map_err(source_error)?;
            for path in page.canonical_paths {
                let kind = if path.starts_with("current/memory/") {
                    "memory"
                } else if path.starts_with("current/procedure/") {
                    "procedure"
                } else {
                    continue;
                };
                let id = Path::new(&path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| {
                        DomainError::new("source_corrupt", "Invalid publication path.")
                    })?;
                identities.insert((kind.to_owned(), id.to_owned()));
                if identities.len() > budget.max_identities as usize {
                    return Err(DomainError::new(
                        "capacity",
                        "Projection delta identity budget exceeded.",
                    ));
                }
            }
            cursor = page.covered_through;
        }
        let mut removed = Vec::new();
        let mut units = Vec::new();
        let mut documents = Vec::new();
        let mut bytes = 0u64;
        for (kind, id) in identities {
            removed.push(if kind == "procedure" {
                format!("procedure/{id}")
            } else {
                id.clone()
            });
            let remaining = budget
                .max_resident_bytes
                .checked_sub(bytes)
                .ok_or_else(|| {
                    DomainError::new("capacity", "Projection delta body budget exceeded.")
                })?;
            let mut unit = match source.read_current_compact(&id, kind == "procedure", remaining) {
                Ok(unit) => unit,
                Err(agentlaw_storage::Error::NotFound(_)) => continue,
                Err(error) => return Err(source_error(error)),
            };
            let refs = unit.references().map_err(source_error)?;
            let key = index_id(&unit);
            let specs = unit
                .heads()
                .iter()
                .zip(refs)
                .map(|(head, reference)| {
                    Ok((
                        reference.observed_version,
                        index_scope(&unit, head)?,
                        if kind == "procedure" {
                            Some(index_body(&unit, head))
                        } else {
                            None
                        },
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            if let UnitState::Live { heads } = &mut unit.state {
                for (head, (version, scope, procedure_body)) in heads.iter_mut().zip(specs) {
                    let body = if kind == "memory" {
                        std::mem::take(&mut head.body)
                    } else {
                        procedure_body.unwrap_or_default()
                    };
                    bytes = bytes
                        .checked_add(if kind == "memory" {
                            body.len() as u64
                        } else {
                            head.body.len() as u64
                        })
                        .ok_or_else(|| {
                            DomainError::new("capacity", "Projection delta body budget exceeded.")
                        })?;
                    if bytes > budget.max_resident_bytes {
                        return Err(DomainError::new(
                            "capacity",
                            "Projection delta body budget exceeded.",
                        ));
                    }
                    documents.push(agentlaw_search::SearchDocument {
                        memory_id: key.clone(),
                        change_id: version,
                        body,
                        scope,
                    });
                }
            }
            units.push(unit);
        }
        self.position = end.clone();
        self.removed = removed;
        self.units = units;
        self.documents = documents;
        Ok(())
    }
    pub fn lexical(
        &mut self,
        scope: &agentlaw_search::ScopeFilter,
    ) -> Result<agentlaw_search::EffectiveCorpus<'_>> {
        self.pinned
            .lexical
            .effective(
                scope,
                agentlaw_search::LexicalDelta {
                    pending: &self.documents,
                    removed: &self.removed,
                    complete: true,
                },
            )
            .map_err(lexical_error)
    }
}

/// Single-owner writer; the store service is responsible for writer exclusivity.
pub struct ExactIndexWriter {
    exact: ExactIndex,
    lexical: agentlaw_search::SearchIndex,
}

fn source_error(error: agentlaw_storage::Error) -> DomainError {
    match error {
        agentlaw_storage::Error::Capacity => {
            DomainError::new("capacity", "Source acquisition exceeded its capacity.")
        }
        agentlaw_storage::Error::CoverageLost => DomainError::new(
            "source_coverage_lost",
            "Source publication coverage is incomplete.",
        ),
        _ => DomainError::new(
            "source_unavailable",
            format!("Cannot read published source: {error}"),
        ),
    }
}

fn sqlite_unavailable(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code,
        rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::ReadOnly | rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked | rusqlite::ErrorCode::SystemIoFailure | rusqlite::ErrorCode::PermissionDenied))
}
fn exact_unavailable() -> DomainError {
    DomainError::new(
        "exact_index_unavailable",
        "The projection is temporarily unavailable.",
    )
}
fn exact_schema_error(error: rusqlite::Error) -> DomainError {
    if sqlite_unavailable(&error) {
        return exact_unavailable();
    }
    let corrupt = match &error {
        rusqlite::Error::SqliteFailure(code, message) => {
            matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) || message
                .as_deref()
                .is_some_and(|s| s.contains("no such table") || s.contains("no such column"))
        }
        rusqlite::Error::QueryReturnedNoRows | rusqlite::Error::InvalidColumnType(..) => true,
        _ => false,
    };
    if corrupt {
        projection_corrupt()
    } else {
        DomainError::new("exact_index_failed", "Cannot access exact projection.")
    }
}
fn lexical_error(error: agentlaw_search::Error) -> DomainError {
    if matches!(&error, agentlaw_search::Error::Sql(err) if sqlite_unavailable(err)) {
        return exact_unavailable();
    }
    let corrupt = match &error {
        agentlaw_search::Error::Invalid(message) => {
            message == "lexical projection is incomplete; rebuild required"
        }
        agentlaw_search::Error::Sql(rusqlite::Error::SqliteFailure(code, message)) => {
            matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) || message
                .as_deref()
                .is_some_and(|s| s.contains("no such table") || s.contains("no such column"))
        }
        agentlaw_search::Error::Sql(
            rusqlite::Error::QueryReturnedNoRows | rusqlite::Error::InvalidColumnType(..),
        ) => true,
        _ => false,
    };
    if corrupt {
        projection_corrupt()
    } else {
        DomainError::new("lexical_index_failed", "Cannot access lexical projection.")
    }
}
impl ExactIndexWriter {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(projection_corrupt())
            }
            _ => return Err(exact_unavailable()),
        }
        let check = db(Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ))?;
        validate_exact_schema(&check)?;
        let filename: String = check
            .query_row(
                "SELECT filename FROM exact_lexical WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(exact_schema_error)?;
        if Path::new(&filename).components().count() != 1
            || Path::new(&filename)
                .file_name()
                .and_then(|value| value.to_str())
                != Some(filename.as_str())
        {
            return Err(projection_corrupt());
        }
        let lexical_path = path.parent().ok_or_else(not_ready)?.join(&filename);
        match std::fs::metadata(&lexical_path) {
            Ok(metadata) if metadata.is_file() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(projection_corrupt())
            }
            _ => return Err(exact_unavailable()),
        }
        let sequence: String = check
            .query_row(
                "SELECT sequence FROM exact_position WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(exact_schema_error)?;
        let sequence = sequence.parse::<u64>().map_err(|_| projection_corrupt())?;
        i64::try_from(sequence).map_err(|_| projection_corrupt())?;
        let lexical =
            agentlaw_search::SearchIndex::open_existing(&lexical_path).map_err(lexical_error)?;
        drop(check);
        let connection = db(Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        ))?;
        if connection
            .is_readonly(rusqlite::DatabaseName::Main)
            .map_err(exact_schema_error)?
        {
            return Err(exact_unavailable());
        }
        db(connection.busy_timeout(std::time::Duration::from_secs(5)))?;
        let exact = ExactIndex {
            connection,
            directory: path.parent().ok_or_else(not_ready)?.to_path_buf(),
        };
        exact.position()?.ok_or_else(not_ready)?;
        Ok(Self { exact, lexical })
    }

    /// Validate both projection schemas without writing.
    pub fn validate_active(&self) -> Result<()> {
        validate_exact_schema(&self.exact.connection)?;
        let (epoch, sequence, filename): (String, String, String) = self.exact.connection.query_row(
            "SELECT p.epoch,p.sequence,l.filename FROM exact_position p JOIN exact_lexical l ON l.singleton=p.singleton WHERE p.singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).map_err(exact_schema_error)?;
        if epoch.is_empty()
            || sequence
                .parse::<u64>()
                .ok()
                .and_then(|value| i64::try_from(value).ok())
                .is_none()
            || filename.is_empty()
            || Path::new(&filename).components().count() != 1
        {
            return Err(projection_corrupt());
        }
        self.lexical.validate_existing().map_err(lexical_error)
    }
    pub fn synchronize(
        &mut self,
        source: &agentlaw_storage::published::OwnedPublishedReader,
        control: &RequestControl,
    ) -> Result<SourcePosition> {
        control.check()?;
        let guard = source.read_snapshot().map_err(source_error)?;
        let end = guard.position().clone();
        u64_to_i64(end.sequence)?;
        let exact_ack = self.exact.position()?.ok_or_else(not_ready)?;
        let lexical_ack = self.lexical.read_view().map_err(lexical_error)?.stamp.ack;
        let lexical_ack = u64::try_from(lexical_ack).map_err(|_| not_ready())?;
        if exact_ack.epoch != end.epoch {
            return Err(DomainError::new(
                "source_epoch_changed",
                "Source epoch changed.",
            ));
        }
        if exact_ack.sequence > end.sequence || lexical_ack > end.sequence {
            return Err(not_ready());
        }
        if exact_ack.sequence == end.sequence && lexical_ack == end.sequence {
            return Ok(end);
        }
        let mut cursor = SourcePosition {
            epoch: end.epoch.clone(),
            sequence: exact_ack.sequence.min(lexical_ack),
        };
        drop(guard);
        while cursor.sequence < end.sequence {
            control.check()?;
            let guard = source.read_snapshot().map_err(source_error)?;
            if guard.position().epoch != end.epoch {
                return Err(DomainError::new(
                    "source_epoch_changed",
                    "Source epoch changed.",
                ));
            }
            let page = guard
                .read_published_paths(&cursor, 1)
                .map_err(source_error)?;
            drop(guard);
            // The source page already deduplicates canonical paths. Keep only one
            // bounded batch of parsed identities in addition to that source page.
            let mut paths = page
                .canonical_paths
                .into_iter()
                .filter_map(|path| {
                    let kind = if path.starts_with("current/memory/") {
                        "memory"
                    } else if path.starts_with("current/procedure/") {
                        "procedure"
                    } else {
                        return None;
                    };
                    Some(
                        Path::new(&path)
                            .file_stem()
                            .and_then(|v| v.to_str())
                            .map(|id| (kind.to_owned(), id.to_owned()))
                            .ok_or_else(|| {
                                DomainError::new("source_corrupt", "Invalid canonical path.")
                            }),
                    )
                })
                .peekable();
            loop {
                control.check()?;
                let guard = source.read_snapshot().map_err(source_error)?;
                if guard.position().epoch != end.epoch {
                    return Err(DomainError::new(
                        "source_epoch_changed",
                        "Source epoch changed.",
                    ));
                }
                let mut acquired = Vec::new();
                let mut changed = Vec::new();
                let mut bytes = 0u64;
                while changed.len() < 1024 {
                    if bytes > DeltaBudget::default().max_resident_bytes {
                        break;
                    }
                    let (kind, id) = match paths.peek() {
                        None => break,
                        Some(Ok(pair)) => pair,
                        Some(Err(_)) => {
                            return Err(DomainError::new(
                                "source_corrupt",
                                "Invalid canonical path.",
                            ))
                        }
                    };
                    let value = if kind == "memory" {
                        guard.acquire_current(id)
                    } else {
                        guard.acquire_procedure(id)
                    };
                    match value {
                        Ok(value) => {
                            let size = value
                                .bodies
                                .values()
                                .try_fold(0u64, |sum, body| sum.checked_add(body.bytes))
                                .ok_or_else(|| {
                                    DomainError::new("capacity", "Writer batch is too large.")
                                })?;
                            if !changed.is_empty()
                                && (bytes > DeltaBudget::default().max_resident_bytes
                                    || size
                                        > DeltaBudget::default()
                                            .max_resident_bytes
                                            .saturating_sub(bytes))
                            {
                                break;
                            }
                            bytes = bytes.checked_add(size).ok_or_else(|| {
                                DomainError::new("capacity", "Writer batch is too large.")
                            })?;
                            acquired.push(value);
                        }
                        Err(agentlaw_storage::Error::NotFound(_)) => {}
                        Err(error) => return Err(source_error(error)),
                    }
                    changed.push(paths.next().expect("peeked source path")?);
                }
                drop(guard);
                let complete = paths.peek().is_none();
                let through = if complete {
                    &page.covered_through
                } else {
                    &cursor
                };
                let units = acquired
                    .iter()
                    .map(acquired_unit)
                    .collect::<Result<Vec<_>>>()?;
                let removed = changed.iter().map(|(kind, id)| {
                    if kind == "procedure" {
                        format!("procedure/{id}")
                    } else {
                        id.clone()
                    }
                });
                control.check()?;
                let current_lexical =
                    u64::try_from(self.lexical.read_view().map_err(lexical_error)?.stamp.ack)
                        .map_err(|_| not_ready())?;
                self.lexical
                    .commit_stream(
                        u64_to_i64(through.sequence.max(current_lexical))?,
                        removed.map(Ok),
                        lazy_head_streams(&units, &acquired),
                    )
                    .map_err(lexical_error)?;
                control.phase("lexical_batch_committed");
                let tx = db(self.exact.connection.transaction())?;
                for (_, id) in &changed {
                    delete_unit(&tx, id)?;
                }
                for (unit, body) in units.iter().zip(&acquired) {
                    apply_unit(&tx, unit, body)?;
                }
                let exact_sequence = exact_ack.sequence.max(through.sequence);
                db(tx.execute(
                    "UPDATE exact_position SET epoch=?1,sequence=?2 WHERE singleton=1",
                    params![end.epoch, exact_sequence.to_string()],
                ))?;
                db(tx.commit())?;
                control.phase("exact_batch_committed");
                if complete {
                    break;
                }
            }
            cursor = page.covered_through;
        }
        Ok(end)
    }

    pub fn rebuild_shadow(
        &mut self,
        source: &agentlaw_storage::published::OwnedPublishedReader,
        control: &RequestControl,
    ) -> Result<ShadowGeneration> {
        Self::rebuild_shadow_in(&self.exact.directory, source, control)
    }

    pub fn rebuild_shadow_in(
        directory: &Path,
        source: &agentlaw_storage::published::OwnedPublishedReader,
        control: &RequestControl,
    ) -> Result<ShadowGeneration> {
        control.check()?;
        std::fs::create_dir_all(directory).map_err(|_| {
            DomainError::new("exact_index_failed", "Cannot create projection directory.")
        })?;
        let id = uuid::Uuid::new_v4().to_string();
        let exact_file = directory.join(format!("exact-repair-{id}.sqlite"));
        let lexical_file = directory.join(format!("lexical-rebuild-{id}.sqlite"));
        let result = (|| {
            let before = source
                .read_snapshot()
                .map_err(source_error)?
                .position()
                .clone();
            let mut exact = ExactIndex::open(&exact_file)?;
            let mut lexical =
                agentlaw_search::SearchIndex::open(&lexical_file).map_err(lexical_error)?;
            control.phase("rebuilding_shadow");
            let mut cursor = None;
            loop {
                control.check()?;
                let (_, ids, more) = source
                    .inventory_page(cursor.as_deref(), 128)
                    .map_err(source_error)?;
                for (kind, id) in ids {
                    control.check()?;
                    let acquired = if kind == "memory" {
                        source.acquire_current(&id)
                    } else {
                        source.acquire_procedure(&id)
                    };
                    match acquired {
                        Ok(acquired) => {
                            let unit = acquired_unit(&acquired)?;
                            let tx = db(exact.connection.transaction())?;
                            apply_unit(&tx, &unit, &acquired)?;
                            db(tx.commit())?;
                            lexical
                                .commit_stream(
                                    0,
                                    std::iter::once(Ok(index_id(&unit))),
                                    head_streams(&unit, &acquired)?.into_iter().flatten(),
                                )
                                .map_err(lexical_error)?;
                        }
                        Err(agentlaw_storage::Error::NotFound(_)) => {}
                        Err(error) => return Err(source_error(error)),
                    }
                    cursor = Some(format!("{kind}/{id}"));
                }
                if !more {
                    break;
                }
            }
            lexical
                .commit(u64_to_i64(before.sequence)?, &[], &[])
                .map_err(lexical_error)?;
            db(exact.connection.execute(
                "INSERT INTO exact_lexical VALUES(1,?1)",
                [lexical_file
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(not_ready)?],
            ))?;
            db(exact.connection.execute(
                "INSERT INTO exact_position VALUES(1,?1,?2)",
                params![before.epoch, before.sequence.to_string()],
            ))?;
            let mut writer = Self { exact, lexical };
            let through = writer.synchronize(source, control)?;
            control.check()?;
            if writer.exact.position()?.as_ref() != Some(&through) {
                return Err(not_ready());
            }
            let view = agentlaw_search::ReadView::open(&lexical_file).map_err(lexical_error)?;
            if view.stamp.ack != u64_to_i64(through.sequence)? {
                return Err(not_ready());
            }
            drop(view);
            for path in [&exact_file, &lexical_file] {
                let connection =
                    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .map_err(exact_schema_error)?;
                let status: String = connection
                    .query_row("PRAGMA quick_check", [], |row| row.get(0))
                    .map_err(exact_schema_error)?;
                if status != "ok" {
                    return Err(projection_corrupt());
                }
            }
            let generation = ProjectionGeneration {
                id,
                epoch: before.epoch,
                exact_file: exact_file.clone(),
                lexical_file: lexical_file.clone(),
            };
            let registry = ProjectionGenerations::from_generation(generation)?;
            drop(registry.pin()?);
            let generation = Arc::try_unwrap(
                registry
                    .state
                    .into_inner()
                    .map_err(|_| not_ready())?
                    .current,
            )
            .map_err(|_| not_ready())?;
            Ok(ShadowGeneration { generation })
        })();
        if result.is_err() {
            for path in [&exact_file, &lexical_file] {
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
                }
            }
        }
        result
    }
}

fn validate_exact_schema(connection: &Connection) -> Result<()> {
    let queries: &[&str] = &[
        "SELECT singleton,epoch,sequence FROM exact_position LIMIT 0",
        "SELECT singleton,filename FROM exact_lexical LIMIT 0",
        "SELECT id,kind,redirect FROM exact_units LIMIT 0",
        "SELECT id,version,kind,scope,is_rule,task,metadata FROM exact_heads LIMIT 0",
        "SELECT id,version,digest FROM exact_bodies LIMIT 0",
        "SELECT id,version,excerpt FROM exact_previews LIMIT 0",
        "SELECT source,version,target,kind FROM exact_edges LIMIT 0",
        "SELECT source,version,project,path,kind,reading FROM exact_targets LIMIT 0",
    ];
    for query in queries {
        connection.prepare(query).map_err(exact_schema_error)?;
    }
    let indexes: &[&str] = &[
        "exact_scope",
        "exact_body_digest",
        "reverse_required",
        "target_project",
    ];
    for name in indexes {
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [name],
                |row| row.get(0),
            )
            .map_err(exact_schema_error)?;
        if count != 1 {
            return Err(DomainError::new(
                "exact_index_corrupt",
                "Projection index is missing; rebuild required.",
            ));
        }
    }
    Ok(())
}

fn delete_unit(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<()> {
    for (table, key) in [
        ("exact_units", "id"),
        ("exact_heads", "id"),
        ("exact_bodies", "id"),
        ("exact_previews", "id"),
        ("exact_edges", "source"),
        ("exact_targets", "source"),
    ] {
        db(tx.execute(&format!("DELETE FROM {table} WHERE {key}=?1"), [id]))?;
    }
    Ok(())
}
fn db<T>(r: std::result::Result<T, rusqlite::Error>) -> Result<T> {
    r.map_err(exact_schema_error)
}
fn u64_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        DomainError::new(
            "source_counter_overflow",
            "Index source generation exceeds the supported counter.",
        )
    })
}
pub struct ExactIndex {
    connection: Connection,
    directory: PathBuf,
}
#[derive(Clone, Debug)]
pub struct HeadRecord {
    pub entity_id: String,
    pub entity_type: String,
    pub observed_version: String,
    pub metadata: Value,
    pub excerpt: String,
}
impl ExactIndex {
    fn maintenance_gate(&self) -> Result<Connection> {
        let gate = db(Connection::open(
            self.directory.join("exact-maintenance.sqlite"),
        ))?;
        db(gate.busy_timeout(std::time::Duration::from_secs(30)))?;
        db(gate.execute_batch("BEGIN IMMEDIATE"))?;
        Ok(gate)
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let directory = path
            .as_ref()
            .parent()
            .ok_or_else(|| DomainError::new("exact_index_failed", "Index directory is missing."))?
            .to_path_buf();
        let connection = db(Connection::open(path))?;
        db(connection.busy_timeout(std::time::Duration::from_secs(5)))?;
        db(connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA temp_store=FILE;
        CREATE TABLE IF NOT EXISTS exact_position(singleton INTEGER PRIMARY KEY CHECK(singleton=1),epoch TEXT NOT NULL,sequence TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS exact_lexical(singleton INTEGER PRIMARY KEY CHECK(singleton=1),filename TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS exact_units(id TEXT PRIMARY KEY,kind TEXT NOT NULL,redirect TEXT);
        CREATE TABLE IF NOT EXISTS exact_heads(id TEXT NOT NULL,version TEXT NOT NULL,kind TEXT NOT NULL,scope TEXT NOT NULL,is_rule INTEGER,task INTEGER,metadata TEXT NOT NULL,PRIMARY KEY(id,version));
        CREATE INDEX IF NOT EXISTS exact_scope ON exact_heads(scope,is_rule,task);
        CREATE TABLE IF NOT EXISTS exact_bodies(id TEXT NOT NULL,version TEXT NOT NULL,digest TEXT NOT NULL,PRIMARY KEY(id,version));
        CREATE TABLE IF NOT EXISTS exact_previews(id TEXT NOT NULL,version TEXT NOT NULL,excerpt TEXT NOT NULL,PRIMARY KEY(id,version));
        CREATE INDEX IF NOT EXISTS exact_body_digest ON exact_bodies(digest);
        CREATE TABLE IF NOT EXISTS exact_edges(source TEXT NOT NULL,version TEXT NOT NULL,target TEXT NOT NULL,kind TEXT NOT NULL,PRIMARY KEY(source,version,target,kind));
        CREATE INDEX IF NOT EXISTS reverse_required ON exact_edges(target,kind);
        CREATE TABLE IF NOT EXISTS exact_targets(source TEXT NOT NULL,version TEXT NOT NULL,project TEXT NOT NULL,path TEXT NOT NULL,kind TEXT NOT NULL,reading TEXT NOT NULL,PRIMARY KEY(source,version,project,path,kind,reading));
        CREATE INDEX IF NOT EXISTS target_project ON exact_targets(project,path);
    "))?;
        Ok(Self {
            connection,
            directory,
        })
    }
    fn lexical(&self, epoch: &str) -> Result<agentlaw_search::SearchIndex> {
        let name: Option<String> = db(self
            .connection
            .query_row(
                "SELECT filename FROM exact_lexical WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional())?;
        let name = name
            .unwrap_or_else(|| format!("lexical-{:x}.sqlite", Sha256::digest(epoch.as_bytes())));
        agentlaw_search::SearchIndex::open(self.directory.join(name)).map_err(|_| {
            DomainError::new(
                "lexical_index_failed",
                "The derived lexical index is unavailable.",
            )
        })
    }
    pub fn position(&self) -> Result<Option<SourcePosition>> {
        let value: Option<(String, String)> = db(self
            .connection
            .query_row(
                "SELECT epoch,sequence FROM exact_position WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional())?;
        value
            .map(|(epoch, seq)| {
                Ok(SourcePosition {
                    epoch,
                    sequence: seq.parse().map_err(|_| {
                        DomainError::new("exact_index_corrupt", "Invalid exact-index watermark.")
                    })?,
                })
            })
            .transpose()
    }
    pub fn identical_bodies(&self, body: &str) -> Result<Vec<String>> {
        let mut statement = db(self
            .connection
            .prepare("SELECT DISTINCT id FROM exact_bodies WHERE digest=?1 ORDER BY id"))?;
        let rows = db(statement
            .query_map([format!("{:x}", Sha256::digest(body.as_bytes()))], |r| {
                r.get::<_, String>(0)
            }))?;
        db(rows.collect())
    }
    pub fn synchronize(
        &mut self,
        source: &agentlaw_storage::Store,
        control: &RequestControl,
    ) -> Result<SourcePosition> {
        let _maintenance = self.maintenance_gate()?;
        control.phase("synchronizing");
        let reader = source.owned_published_reader();
        let now = reader.position().map_err(|_| {
            DomainError::new("source_unavailable", "Cannot inspect source position.")
        })?;
        let mut cursor = self.position()?.ok_or_else(|| {
            DomainError::new(
                "exact_index_rebuild_required",
                "A verified initial inventory is required.",
            )
        })?;
        if cursor.epoch != now.epoch {
            return Err(DomainError::new(
                "exact_index_rebuild_required",
                "Source epoch changed.",
            ));
        }
        while cursor.sequence < now.sequence {
            control.check()?;
            let page = reader.read_published_paths(&cursor, 1).map_err(|_| {
                DomainError::new(
                    "exact_index_coverage_unknown",
                    "Canonical publication coverage is incomplete.",
                )
            })?;
            let mut acquired = Vec::new();
            for path in &page.canonical_paths {
                let procedure = path.starts_with("current/procedure/");
                if !procedure && !path.starts_with("current/memory/") {
                    continue;
                }
                let id = std::path::Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| DomainError::new("source_corrupt", "Invalid canonical path."))?;
                let unit = if procedure {
                    reader.acquire_procedure(id)
                } else {
                    reader.acquire_current(id)
                }
                .map_err(|_| {
                    DomainError::new("source_unavailable", "Cannot acquire changed identity.")
                })?;
                if unit.source_position != page.source_position {
                    return Err(DomainError::new(
                        "source_changed",
                        "Source changed during incremental acquisition.",
                    ));
                }
                acquired.push(unit);
            }
            if reader.position().map_err(|_| {
                DomainError::new("source_unavailable", "Cannot verify source position.")
            })? != page.source_position
            {
                return Err(DomainError::new(
                    "source_changed",
                    "Source changed during incremental acquisition.",
                ));
            }
            let units = acquired
                .iter()
                .map(acquired_unit)
                .collect::<Result<Vec<_>>>()?;
            let mut lexical = self.lexical(&cursor.epoch)?;
            let through = u64_to_i64(page.covered_through.sequence)?;
            let stamp = lexical
                .read_view()
                .map_err(|_| DomainError::new("lexical_index_failed", "Cannot pin lexical index."))?
                .stamp;
            if stamp.ack < through {
                let mut streams = Vec::new();
                for (unit, body) in units.iter().zip(&acquired) {
                    streams.extend(head_streams(unit, body)?);
                }
                lexical
                    .commit_stream(
                        through,
                        units.iter().map(|u| Ok(index_id(u))),
                        streams.into_iter().flatten(),
                    )
                    .map_err(|_| {
                        DomainError::new(
                            "lexical_index_failed",
                            "Cannot commit acquired lexical records.",
                        )
                    })?;
            }
            let tx = db(self.connection.transaction())?;
            for (unit, body) in units.iter().zip(&acquired) {
                apply_unit(&tx, unit, body)?;
            }
            db(tx.execute("INSERT INTO exact_position VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence",params![page.covered_through.epoch,page.covered_through.sequence.to_string()]))?;
            db(tx.commit())?;
            cursor = page.covered_through;
        }
        Ok(cursor)
    }
    pub fn lexical_search(
        &self,
        query: &str,
        context: &RequestContext,
        limit: usize,
    ) -> Result<Vec<agentlaw_search::Hit>> {
        self.search_scopes(query, &allowed_scopes(context), limit)
    }
    pub fn all_scopes(&self) -> Result<Vec<String>> {
        let mut st = db(self
            .connection
            .prepare("SELECT DISTINCT scope FROM exact_heads ORDER BY scope"))?;
        let rows = db(st.query_map([], |r| r.get(0)))?;
        db(rows.collect())
    }
    pub fn search_scopes(
        &self,
        query: &str,
        scopes: &[String],
        limit: usize,
    ) -> Result<Vec<agentlaw_search::Hit>> {
        let position = self.position()?.ok_or_else(|| {
            DomainError::new(
                "exact_index_coverage_unknown",
                "No verified source position is indexed.",
            )
        })?;
        let lexical = self.lexical(&position.epoch)?;
        let mut view = lexical.read_view().map_err(|_| {
            DomainError::new("lexical_index_failed", "Cannot pin lexical search view.")
        })?;
        if view.stamp.ack < u64_to_i64(position.sequence)? {
            return Err(DomainError::new(
                "lexical_index_incomplete",
                "Lexical index has not committed the required source position.",
            ));
        }
        view.search(
            query,
            &agentlaw_search::ScopeFilter {
                allowed_scopes: scopes.to_vec(),
            },
            limit,
            &[],
            &[],
            true,
        )
        .map_err(|_| DomainError::new("search_failed", "Indexed lexical search failed."))
    }
    pub fn rebuild(
        &mut self,
        source: &agentlaw_storage::Store,
        control: &RequestControl,
    ) -> Result<SourcePosition> {
        let maintenance = self.maintenance_gate()?;
        control.phase("rebuilding_exact_metadata");
        let reader = source.owned_published_reader();
        let before = reader.position().map_err(|_| {
            DomainError::new(
                "source_unavailable",
                "Cannot inspect source before rebuild.",
            )
        })?;
        let filename = format!("lexical-rebuild-{}.sqlite", uuid::Uuid::new_v4());
        let mut lexical = agentlaw_search::SearchIndex::open(self.directory.join(&filename))
            .map_err(|_| {
                DomainError::new(
                    "lexical_index_failed",
                    "Cannot prepare a private rebuild index.",
                )
            })?;
        let tx = db(self.connection.transaction())?;
        db(tx.execute_batch("DELETE FROM exact_units;DELETE FROM exact_heads;DELETE FROM exact_edges;DELETE FROM exact_targets;DELETE FROM exact_bodies;DELETE FROM exact_previews;"))?;
        let mut cursor = None;
        loop {
            control.check()?;
            let (_, ids, more) = reader.inventory_page(cursor.as_deref(), 128).map_err(|_| {
                DomainError::new(
                    "exact_index_rebuild_failed",
                    "Cannot enumerate canonical identities.",
                )
            })?;
            for (kind, id) in ids {
                control.check()?;
                let acquired = if kind == "memory" {
                    reader.acquire_current(&id)
                } else {
                    reader.acquire_procedure(&id)
                }
                .map_err(|_| {
                    DomainError::new(
                        "exact_index_rebuild_failed",
                        "Cannot acquire canonical identity.",
                    )
                })?;
                let unit = acquired_unit(&acquired)?;
                apply_unit(&tx, &unit, &acquired)?;
                lexical
                    .commit_stream(
                        0,
                        std::iter::once(Ok(index_id(&unit))),
                        head_streams(&unit, &acquired)?.into_iter().flatten(),
                    )
                    .map_err(|_| {
                        DomainError::new(
                            "lexical_index_failed",
                            "Cannot populate private rebuild index.",
                        )
                    })?;
                cursor = Some(format!("{kind}/{id}"));
            }
            if !more {
                break;
            }
        }
        lexical
            .commit(u64_to_i64(before.sequence)?, &[], &[])
            .map_err(|_| {
                DomainError::new("lexical_index_failed", "Cannot seal private rebuild index.")
            })?;
        db(tx.execute("INSERT INTO exact_lexical VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET filename=excluded.filename",[filename]))?;
        db(tx.execute("INSERT INTO exact_position VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence",params![before.epoch,before.sequence.to_string()]))?;
        db(tx.commit())?;
        drop(maintenance);
        self.synchronize(source, control)
    }
    pub fn ids_matching(
        &self,
        context: &RequestContext,
        rules: bool,
        tasks: bool,
    ) -> Result<Vec<String>> {
        let scopes = allowed_scopes(context);
        let mut result = BTreeSet::new();
        for scope in scopes {
            let mut st=db(self.connection.prepare("SELECT DISTINCT id FROM exact_heads WHERE kind='memory' AND scope=?1 AND ((?2=1 AND is_rule=1) OR (?3=1 AND task=1)) ORDER BY id"))?;
            let rows = db(st.query_map(params![scope, rules, tasks], |r| r.get::<_, String>(0)))?;
            result.extend(db(rows.collect::<std::result::Result<Vec<_>, _>>())?);
        }
        Ok(result.into_iter().collect())
    }
    pub fn reverse_required(&self, targets: &[String]) -> Result<Vec<String>> {
        let mut result = BTreeSet::new();
        let mut st=db(self.connection.prepare("SELECT DISTINCT source FROM exact_edges WHERE target=?1 AND kind='required' ORDER BY source"))?;
        for target in targets {
            let rows = db(st.query_map([target], |r| r.get::<_, String>(0)))?;
            result.extend(db(rows.collect::<std::result::Result<Vec<_>, _>>())?);
        }
        Ok(result.into_iter().collect())
    }
    pub fn related(&self, ids: &[String]) -> Result<Vec<(String, String)>> {
        let mut result = BTreeSet::new();
        let mut st=db(self.connection.prepare("SELECT DISTINCT target FROM exact_edges WHERE source=?1 AND kind='related' ORDER BY target"))?;
        for id in ids {
            for target in db(st.query_map([id], |r| r.get::<_, String>(0)))? {
                result.insert((id.clone(), db(target)?));
            }
        }
        Ok(result.into_iter().collect())
    }
    pub fn work_targets(
        &self,
        project: &str,
        requested: &[WorkTarget],
    ) -> Result<Vec<(String, Reading)>> {
        let mut result = BTreeSet::new();
        for target in requested {
            let path = relative_path(&target.path)?;
            let mut st=db(self.connection.prepare("SELECT DISTINCT source,reading,path FROM exact_targets WHERE project=?1 AND (path=?2 COLLATE BINARY OR (?3=1 AND substr(path,1,length(?2)+1)=?2 || '/' COLLATE BINARY)) ORDER BY source"))?;
            let rows = db(st.query_map(
                params![project, path, target.kind == TargetKind::Directory],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            ))?;
            for row in rows {
                let (id, reading, stored) = db(row)?;
                if target_path_matches(&path, &target.kind, &stored) {
                    result.insert((id, reading));
                }
            }
        }
        result
            .into_iter()
            .map(|(id, reading)| {
                Ok((
                    id,
                    match reading.as_str() {
                        "required" => Reading::Required,
                        "related" => Reading::Related,
                        _ => {
                            return Err(DomainError::new(
                                "exact_index_corrupt",
                                "Unknown indexed reading relation.",
                            ))
                        }
                    },
                ))
            })
            .collect()
    }
    pub fn heads(&self, ids: &[String]) -> Result<Vec<HeadRecord>> {
        let mut result = Vec::new();
        let mut st = db(self.connection.prepare(
            "SELECT h.id,h.kind,h.version,h.metadata,p.excerpt FROM exact_heads h JOIN exact_previews p USING(id,version) WHERE h.id=?1 ORDER BY h.version",
        ))?;
        for id in ids {
            let rows = db(st.query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            }))?;
            for row in rows {
                let (id, kind, version, metadata, excerpt) = db(row)?;
                result.push(HeadRecord {
                    entity_id: id,
                    entity_type: kind,
                    observed_version: version,
                    excerpt,
                    metadata: serde_json::from_str(&metadata).map_err(|_| {
                        DomainError::new(
                            "exact_index_corrupt",
                            "Indexed metadata cannot be decoded.",
                        )
                    })?,
                });
            }
        }
        Ok(result)
    }
    pub fn procedure_ids(&self, context: &RequestContext) -> Result<Vec<String>> {
        let mut result = BTreeSet::new();
        for scope in allowed_scopes(context) {
            let mut st=db(self.connection.prepare("SELECT DISTINCT id FROM exact_heads WHERE kind='learned_procedure' AND scope=?1 ORDER BY id"))?;
            result.extend(db(db(st.query_map([scope], |r| r.get::<_, String>(0)))?
                .collect::<std::result::Result<Vec<_>, _>>())?);
        }
        Ok(result.into_iter().collect())
    }
}
// Paths are case-sensitive and directory matches end at a component boundary;
// the SQL prefix condition above implements the same predicate.
fn target_path_matches(requested: &str, kind: &TargetKind, stored: &str) -> bool {
    stored == requested
        || (*kind == TargetKind::Directory
            && stored
                .strip_prefix(requested)
                .is_some_and(|tail| tail.starts_with('/')))
}
pub fn allowed_scopes(context: &RequestContext) -> Vec<String> {
    let mut scopes = vec!["user".into(), format!("machine:{}", context.machine_id)];
    if let Some(project) = &context.project_id {
        scopes.push(format!("project:{project}"));
        scopes.push(format!("project:{project}:machine:{}", context.machine_id));
    }
    scopes
}
fn index_id(unit: &CurrentUnit) -> String {
    if unit.entity_type == "learned_procedure" {
        format!("procedure/{}", unit.entity_id)
    } else {
        unit.entity_id.clone()
    }
}
fn acquired_unit(acquired: &agentlaw_storage::acquire::AcquiredCurrent) -> Result<CurrentUnit> {
    let v = &acquired.state;
    let procedure = v["entity_type"] == "learned_procedure";
    let id = v[if procedure {
        "procedure_id"
    } else {
        "memory_id"
    }]
    .as_str()
    .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired ID."))?;
    let state = if v["state"] == "redirect" {
        UnitState::Redirect {
            redirect_to: v["redirect_to"]
                .as_str()
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing redirect target."))?
                .into(),
            consolidation_change_id: v["consolidation_change_id"]
                .as_str()
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing redirect change."))?
                .into(),
        }
    } else {
        UnitState::Live {
            heads: v["current_heads"]
                .as_array()
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired heads."))?
                .iter()
                .map(|metadata| agentlaw_storage::Head {
                    metadata: metadata.clone(),
                    body: String::new(),
                })
                .collect(),
        }
    };
    Ok(CurrentUnit {
        entity_id: id.into(),
        entity_type: if procedure {
            "learned_procedure"
        } else {
            "memory"
        }
        .into(),
        state,
    })
}
struct HeadStream {
    template: agentlaw_search::SearchDocument,
    reader: Option<std::fs::File>,
    pending: Vec<u8>,
    done: bool,
    emitted: bool,
}
impl Iterator for HeadStream {
    type Item = agentlaw_search::Result<agentlaw_search::SearchDocument>;
    fn next(&mut self) -> Option<Self::Item> {
        use std::io::Read;
        if self.done {
            return None;
        }
        let result = (|| {
            if let Some(reader) = &mut self.reader {
                let mut buf = [0u8; 65536];
                let n = reader.read(&mut buf).map_err(|_| {
                    agentlaw_search::Error::Invalid("acquired body read failed".into())
                })?;
                self.pending.extend_from_slice(&buf[..n]);
                let valid = match std::str::from_utf8(&self.pending) {
                    Ok(s) => s.len(),
                    Err(e) if e.error_len().is_none() && n != 0 => e.valid_up_to(),
                    Err(_) => {
                        return Err(agentlaw_search::Error::Invalid("body UTF-8 failed".into()))
                    }
                };
                if n == 0 && self.pending.is_empty() && self.emitted {
                    self.done = true;
                    return Ok(None);
                }
                let body = String::from_utf8(self.pending.drain(..valid).collect())
                    .map_err(|_| agentlaw_search::Error::Invalid("body UTF-8 failed".into()))?;
                self.emitted = true;
                if n == 0 {
                    self.done = true
                }
                Ok(Some(agentlaw_search::SearchDocument {
                    body,
                    ..self.template.clone()
                }))
            } else {
                self.done = true;
                Ok(Some(self.template.clone()))
            }
        })();
        match result {
            Ok(value) => value.map(Ok),
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
fn head_streams(
    unit: &CurrentUnit,
    acquired: &agentlaw_storage::acquire::AcquiredCurrent,
) -> Result<Vec<HeadStream>> {
    unit.heads()
        .iter()
        .enumerate()
        .map(|(index, head)| head_stream(unit, acquired, index, head))
        .collect()
}
fn head_stream(
    unit: &CurrentUnit,
    acquired: &agentlaw_storage::acquire::AcquiredCurrent,
    index: usize,
    head: &agentlaw_storage::Head,
) -> Result<HeadStream> {
    let change = &acquired
        .references
        .get(index)
        .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired version."))?
        .observed_version;
    let reader = if unit.entity_type == "memory" {
        Some(
            acquired
                .bodies
                .get(head.metadata["change_id"].as_str().unwrap_or(""))
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired body."))?
                .open()
                .map_err(|_| {
                    DomainError::new("source_unavailable", "Cannot open acquired body.")
                })?,
        )
    } else {
        None
    };
    Ok(HeadStream {
        template: agentlaw_search::SearchDocument {
            memory_id: index_id(unit),
            change_id: change.clone(),
            body: index_body(unit, head),
            scope: index_scope(unit, head)?,
        },
        reader,
        pending: Vec::new(),
        done: false,
        emitted: false,
    })
}
fn lazy_head_streams<'a>(
    units: &'a [CurrentUnit],
    acquired: &'a [agentlaw_storage::acquire::AcquiredCurrent],
) -> impl Iterator<Item = agentlaw_search::Result<agentlaw_search::SearchDocument>> + 'a {
    let mut index = 0;
    let mut head_index = 0;
    let mut current: Option<HeadStream> = None;
    std::iter::from_fn(move || loop {
        if let Some(stream) = &mut current {
            if let Some(item) = stream.next() {
                return Some(item);
            }
            current = None;
        }
        let unit = units.get(index)?;
        let body = acquired.get(index)?;
        if head_index == unit.heads().len() {
            index += 1;
            head_index = 0;
            continue;
        }
        let head = &unit.heads()[head_index];
        let selected = head_index;
        head_index += 1;
        match head_stream(unit, body, selected, head) {
            Ok(next) => current = Some(next),
            Err(error) => return Some(Err(agentlaw_search::Error::Invalid(error.to_string()))),
        }
    })
}
fn index_body(unit: &CurrentUnit, head: &agentlaw_storage::Head) -> String {
    if unit.entity_type == "learned_procedure" {
        format!(
            "{}\n{}",
            head.metadata["name"].as_str().unwrap_or(""),
            head.metadata["use_when"].as_str().unwrap_or("")
        )
    } else {
        head.body.clone()
    }
}
fn index_scope(unit: &CurrentUnit, head: &agentlaw_storage::Head) -> Result<String> {
    let scope = crate::runtime::scope_token(&crate::runtime::scope_value(
        &head.metadata["applicability"],
    )?);
    Ok(if unit.entity_type == "learned_procedure" {
        format!("procedure:{scope}")
    } else {
        scope
    })
}
fn apply_unit(
    tx: &rusqlite::Transaction<'_>,
    unit: &CurrentUnit,
    acquired: &agentlaw_storage::acquire::AcquiredCurrent,
) -> Result<()> {
    for table in [
        "exact_heads",
        "exact_edges",
        "exact_targets",
        "exact_bodies",
        "exact_previews",
    ] {
        let key = if table == "exact_heads" || table == "exact_bodies" || table == "exact_previews"
        {
            "id"
        } else {
            "source"
        };
        db(tx.execute(
            &format!("DELETE FROM {table} WHERE {key}=?1"),
            [&unit.entity_id],
        ))?;
    }
    let redirect = match &unit.state {
        UnitState::Redirect { redirect_to, .. } => Some(redirect_to),
        _ => None,
    };
    db(tx.execute("INSERT INTO exact_units VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,redirect=excluded.redirect",params![unit.entity_id,unit.entity_type,redirect]))?;
    for (index, head) in unit.heads().iter().enumerate() {
        let version = &acquired
            .references
            .get(index)
            .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired version."))?
            .observed_version;
        db(tx.execute(
            "INSERT INTO exact_bodies VALUES(?1,?2,?3)",
            params![
                unit.entity_id,
                version,
                acquired
                    .bodies
                    .get(head.metadata["change_id"].as_str().unwrap_or(""))
                    .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired body."))?
                    .sha256
            ],
        ))?;
        let applicability = crate::runtime::scope_value(&head.metadata["applicability"])?;
        {
            use std::io::Read;
            let mut prefix = Vec::with_capacity(1600);
            acquired
                .bodies
                .get(head.metadata["change_id"].as_str().unwrap_or(""))
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired body."))?
                .open()
                .map_err(|_| DomainError::new("source_unavailable", "Cannot read preview."))?
                .take(1600)
                .read_to_end(&mut prefix)
                .map_err(|_| DomainError::new("source_unavailable", "Cannot read preview."))?;
            let text = match std::str::from_utf8(&prefix) {
                Ok(text) => text,
                Err(e) if e.error_len().is_none() => {
                    std::str::from_utf8(&prefix[..e.valid_up_to()])
                        .map_err(|_| DomainError::new("source_corrupt", "Invalid body UTF-8."))?
                }
                Err(_) => return Err(DomainError::new("source_corrupt", "Invalid body UTF-8.")),
            };
            let excerpt: String = text.chars().take(400).collect();
            db(tx.execute(
                "INSERT INTO exact_previews VALUES(?1,?2,?3)",
                params![unit.entity_id, version, excerpt],
            ))?;
        }
        let scope = crate::runtime::scope_token(&applicability);
        let is_rule = head.metadata.get("is_rule").and_then(Value::as_bool);
        let task = head.metadata.get("in_working_set").and_then(Value::as_bool);
        db(tx.execute(
            "INSERT INTO exact_heads VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                unit.entity_id,
                version,
                unit.entity_type,
                scope,
                is_rule,
                task,
                head.metadata.to_string()
            ],
        ))?;
        if unit.entity_type == "memory" {
            for relation in head.metadata["relations"].as_array().ok_or_else(|| {
                DomainError::new(
                    "source_corrupt",
                    "Published memory relation metadata is missing.",
                )
            })? {
                let target = relation["target_memory_id"].as_str().ok_or_else(|| {
                    DomainError::new("source_corrupt", "Published relation target is missing.")
                })?;
                let kind = relation["kind"].as_str().ok_or_else(|| {
                    DomainError::new("source_corrupt", "Published relation kind is missing.")
                })?;
                db(tx.execute(
                    "INSERT INTO exact_edges VALUES(?1,?2,?3,?4)",
                    params![unit.entity_id, version, target, kind],
                ))?;
            }
            for target in head.metadata["work_targets"].as_array().ok_or_else(|| {
                DomainError::new(
                    "source_corrupt",
                    "Published work-target metadata is missing.",
                )
            })? {
                let target: StoredTarget =
                    serde_json::from_value(target.clone()).map_err(|_| {
                        DomainError::new("source_corrupt", "Published work target is invalid.")
                    })?;
                db(tx.execute(
                    "INSERT INTO exact_targets VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        unit.entity_id,
                        version,
                        target.project_id,
                        relative_path(&target.path)?,
                        match target.kind {
                            TargetKind::File => "file",
                            TargetKind::Directory => "directory",
                        },
                        match target.reading {
                            Reading::Required => "required",
                            Reading::Related => "related",
                        }
                    ],
                ))?;
            }
        }
    }
    Ok(())
}
