//! C7's read-only source port. RAM wakeups are not the change ledger.
use crate::{CurrentUnit, Error, Fence, Manifest, Result, Store};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum EntityKind {
    Memory,
    Procedure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrefixMatch {
    Absent,
    Unique(String),
    Ambiguous,
}

pub enum PublicationEvidence {
    Undecided,
    Inconclusive,
    Decided,
    Published,
}

impl PrefixMatch {
    pub fn count(ids: impl IntoIterator<Item = String>) -> Self {
        let mut ids = ids.into_iter();
        let Some(first) = ids.next() else {
            return Self::Absent;
        };
        if ids.any(|id| id != first) {
            Self::Ambiguous
        } else {
            Self::Unique(first)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrefixCondition {
    pub kind: EntityKind,
    pub prefix: String,
    pub selected: String,
}

impl Store {
    pub(crate) fn match_identity_prefix_locked(
        &self,
        kind: EntityKind,
        prefix: &str,
    ) -> Result<PrefixMatch> {
        if !(8..=32).contains(&prefix.len())
            || !prefix
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(Error::PublicationInvariant(
                "invalid identity prefix condition",
            ));
        }
        let kind = match kind {
            EntityKind::Memory => "memory",
            EntityKind::Procedure => "procedure",
        };
        let shard = &prefix[..2];
        let relative = format!("current/{kind}/{shard}");
        let directory = self.root.join(&relative);
        let source_read = |source| Error::SourceRead {
            path: directory.clone(),
            source,
        };
        let path = self.safe_path(&relative).map_err(|error| match error {
            Error::Io(source) => source_read(source),
            other => other,
        })?;
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PrefixMatch::Absent)
            }
            Err(error) => return Err(source_read(error)),
        };
        let mut selected = None;
        for entry in entries {
            let entry = entry.map_err(source_read)?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "tmp") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|id| {
                    path.extension().is_some_and(|ext| ext == "md") && id.starts_with(shard)
                })
                .ok_or_else(|| Error::Corrupt("invalid current source filename".into()))?;
            crate::validate_id(id)?;
            if !entry
                .file_type()
                .map_err(|source| Error::SourceRead {
                    path: path.clone(),
                    source,
                })?
                .is_file()
            {
                return Err(Error::Corrupt(
                    "current source entry is not a regular file".into(),
                ));
            }
            if id.replace('-', "").starts_with(prefix) {
                if selected.is_some() {
                    return Ok(PrefixMatch::Ambiguous);
                }
                selected = Some(id.to_owned());
            }
        }
        Ok(selected.map_or(PrefixMatch::Absent, PrefixMatch::Unique))
    }

    pub(crate) fn validate_prefixes_locked(&self, conditions: &[PrefixCondition]) -> Result<()> {
        for (index, condition) in conditions.iter().enumerate() {
            let actual = self.match_identity_prefix_locked(condition.kind, &condition.prefix)?;
            if !matches!(&actual, PrefixMatch::Unique(id) if id == &condition.selected) {
                return Err(Error::PrefixSelection { index, actual });
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcePosition {
    pub epoch: String,
    pub sequence: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedChange {
    pub operation_id: String,
    pub sequence: u64,
    /// Latest units at source_position, deduplicated across this page; NOT historical snapshots.
    pub current_units: Vec<CurrentUnit>,
    pub canonical_paths: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedChanges {
    pub source_position: SourcePosition,
    pub covered_through: SourcePosition,
    pub changes: Vec<PublishedChange>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedPaths {
    pub source_position: SourcePosition,
    pub covered_through: SourcePosition,
    pub canonical_paths: Vec<String>,
}
pub trait PublishedChangeSource {
    fn position(&self) -> Result<SourcePosition>;
    fn read_published_changes(
        &self,
        after: &SourcePosition,
        limit: u32,
    ) -> Result<PublishedChanges>;
}
pub struct PublishedReader<'a> {
    pub(super) store: &'a Store,
}
pub struct OwnedPublishedReader {
    pub(super) store: Store,
}
/// A clean source position protected by a shared source gate.
pub struct SourceReadGuard<'a> {
    pub(crate) store: &'a Store,
    pub(crate) _gate: fs::File,
    pub(crate) position: SourcePosition,
    recall: bool,
}
impl Clone for OwnedPublishedReader {
    fn clone(&self) -> Self {
        Self {
            store: Store {
                root: self.store.root.clone(),
                local: self.store.local.clone(),
                coordination: self.store.coordination.clone(),
            },
        }
    }
}
impl OwnedPublishedReader {
    pub fn publication_evidence(&self, operation: &str) -> Result<PublicationEvidence> {
        crate::validate_id(operation)?;
        let store = &self.store;
        let _guard = store.lock_shared()?;
        let dir = store.local.join("recovery").join(operation);
        match store.load::<crate::PublishReceipt>(&dir.join("published")) {
            Ok(_) => return Ok(PublicationEvidence::Published),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match store.load::<String>(&dir.join("decision")) {
            Ok(decision) => {
                let manifest_path = dir.join("manifest");
                let bytes = fs::read(&manifest_path)?;
                let manifest: Manifest = store.load(&manifest_path)?;
                if manifest.operation_id == operation && decision == crate::codec::digest(&bytes) {
                    return Ok(PublicationEvidence::Decided);
                }
                return Ok(PublicationEvidence::Inconclusive);
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let fence: Fence = store.load(&store.local.join("source-fence"))?;
        Ok(if fence.operation_id.as_deref() == Some(operation) {
            PublicationEvidence::Inconclusive
        } else {
            PublicationEvidence::Undecided
        })
    }
    /// Construct an isolated import review using the binding's coordinated source.
    pub fn prepare_import_union(
        &self,
        incoming: &Store,
        root: &std::path::Path,
        local: &std::path::Path,
    ) -> Result<Store> {
        self.store.prepare_import_union(incoming, root, local)
    }
    /// Validate a staged review against this binding's current source position.
    pub fn validate_imported_source(&self, staged: &Store, expected_generation: u64) -> Result<()> {
        self.store
            .validate_imported_source(staged, expected_generation)
    }
    pub fn with_source_read<T>(
        &self,
        read: impl FnOnce(&std::path::Path, u64) -> Result<T>,
    ) -> Result<T> {
        self.store.with_source_read(read)
    }
    pub fn visit_history_closure(
        &self,
        entity_id: &str,
        visit: impl FnMut(crate::history_spool::SpoolChange) -> Result<()>,
    ) -> Result<u64> {
        self.store.visit_history_closure(entity_id, visit)
    }
    pub fn visit_current(&self, visit: impl FnMut(CurrentUnit) -> Result<()>) -> Result<u64> {
        self.store.visit_current(visit)
    }
    pub fn acquire_history(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<crate::history_spool::AcquiredHistory> {
        self.read_snapshot()?.acquire_history(cancel)
    }
    pub fn read_snapshot(&self) -> Result<SourceReadGuard<'_>> {
        self.read_snapshot_with_policy(false)
    }
    pub fn read_snapshot_for_recall(&self) -> Result<SourceReadGuard<'_>> {
        self.read_snapshot_with_policy(true)
    }
    fn read_snapshot_with_policy(&self, recall: bool) -> Result<SourceReadGuard<'_>> {
        let gate = self.store.lock_shared()?;
        let sequence = self.store.ensure_clean()?;
        let path = self.store.local.join("source-epoch");
        let epoch = if recall {
            self.store
                .load::<String>(&path)
                .and_then(|epoch| {
                    crate::validate_id(&epoch)?;
                    Ok(epoch)
                })
                .map_err(|error| Error::LocalRecallEpoch(Box::new(error)))?
        } else {
            self.store.load(&path)?
        };
        Ok(SourceReadGuard {
            store: &self.store,
            _gate: gate,
            position: SourcePosition { epoch, sequence },
            recall,
        })
    }
    /// Project discovery does not require the epoch used for indexed recall.
    pub fn list_catalog_for_discovery(&self) -> Result<Vec<serde_json::Value>> {
        let _gate = self.store.lock_shared()?;
        self.store.ensure_clean()?;
        self.store.list_catalog_locked()
    }
    pub fn read_published_paths(
        &self,
        after: &SourcePosition,
        limit: u32,
    ) -> Result<PublishedPaths> {
        if limit == 0 || limit > 1024 {
            return Err(Error::Capacity);
        }
        self.read_snapshot()?.read_published_paths(after, limit)
    }
    // These legacy acquisition paths read the epoch before checking the fence.
    fn read_snapshot_epoch_first(&self) -> Result<SourceReadGuard<'_>> {
        let gate = self.store.lock_shared()?;
        let epoch = self.store.load(&self.store.local.join("source-epoch"))?;
        let sequence = self.store.ensure_clean()?;
        Ok(SourceReadGuard {
            store: &self.store,
            _gate: gate,
            position: SourcePosition { epoch, sequence },
            recall: false,
        })
    }
}
impl SourceReadGuard<'_> {
    pub fn match_identity_prefix(&self, kind: EntityKind, prefix: &str) -> Result<PrefixMatch> {
        self.store.match_identity_prefix_locked(kind, prefix)
    }

    pub fn validate_prefixes(&self, conditions: &[PrefixCondition]) -> Result<()> {
        self.store.validate_prefixes_locked(conditions)
    }
    pub fn position(&self) -> &SourcePosition {
        &self.position
    }
    pub fn validate_read_set(&self, read_set: &crate::ReadSet) -> Result<()> {
        self.store.validate_read_set_locked(read_set)
    }
    /// A bounded ledger page at this guard's pinned source position.
    pub fn read_published_paths(
        &self,
        after: &SourcePosition,
        limit: u32,
    ) -> Result<PublishedPaths> {
        if limit == 0 || limit > 1024 {
            return Err(Error::Capacity);
        }
        let s = self.store;
        let generation = self.position.sequence;
        let epoch = self.position.epoch.clone();
        if epoch != after.epoch || after.sequence > generation {
            return Err(Error::CoverageLost);
        }
        let through = after
            .sequence
            .saturating_add(u64::from(limit))
            .min(generation);
        let mut paths = std::collections::BTreeSet::new();
        if through > after.sequence {
            let conn = agentlaw_sqlite::open_with_flags(
                s.local.join("journal.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|_| Error::CoverageLost)?;
            let mut q=conn.prepare("SELECT manifest FROM publications WHERE sequence>?1 AND sequence<=?2 ORDER BY sequence").map_err(|_|Error::CoverageLost)?;
            let mut rows = q.query(rusqlite::params![
                after.sequence.to_be_bytes().as_slice(),
                through.to_be_bytes().as_slice()
            ])?;
            let mut seen = after.sequence;
            while let Some(row) = rows.next()? {
                let raw: String = row.get(0)?;
                let m: Manifest = serde_json::from_str(&raw)?;
                seen += 1;
                if m.receipt.generation != seen {
                    return Err(Error::CoverageLost);
                }
                paths.extend(m.targets.into_iter().map(|t| t.path));
            }
            if seen != through {
                return Err(Error::CoverageLost);
            }
        }
        Ok(PublishedPaths {
            source_position: SourcePosition {
                epoch: epoch.clone(),
                sequence: generation,
            },
            covered_through: SourcePosition {
                epoch,
                sequence: through,
            },
            canonical_paths: paths.into_iter().collect(),
        })
    }
    pub fn acquire_current(&self, id: &str) -> Result<crate::acquire::AcquiredCurrent> {
        self.acquire_kind(id, "memory")
    }
    pub fn acquire_procedure(&self, id: &str) -> Result<crate::acquire::AcquiredCurrent> {
        self.acquire_kind(id, "procedure")
    }
    fn acquire_kind(&self, id: &str, kind: &str) -> Result<crate::acquire::AcquiredCurrent> {
        if self.recall {
            self.store
                .acquire_kind_with_origin(id, kind, self.position.clone())
        } else {
            self.store
                .acquire_kind_locked(id, kind, self.position.clone())
        }
    }
    pub fn acquire_closure(
        &self,
        ids: &[String],
    ) -> Result<(
        SourcePosition,
        Vec<crate::acquire::AcquiredResolvedCurrent>,
        Vec<String>,
    )> {
        if self.recall {
            self.store
                .acquire_closure_with_origin(ids, self.position.clone())
        } else {
            self.store
                .acquire_closure_locked(ids, self.position.clone())
        }
    }
    pub fn read_current(&self, id: &str) -> Result<CurrentUnit> {
        self.store.read_kind(id, "memory")
    }
    pub fn read_procedure(&self, id: &str) -> Result<CurrentUnit> {
        self.store.read_kind(id, "procedure")
    }
    /// Read an owned current unit without duplicating its bodies during decoding.
    /// Reject before allocating a body when their cumulative size exceeds the budget.
    pub fn read_current_compact(
        &self,
        id: &str,
        procedure: bool,
        body_budget: u64,
    ) -> Result<CurrentUnit> {
        crate::validate_id(id)?;
        let kind = if procedure { "procedure" } else { "memory" };
        let path = self
            .store
            .safe_path(&format!("current/{kind}/{}/{}.md", &id[..2], id))?;
        if !path.exists() {
            return Err(Error::NotFound(id.into()));
        }
        let mut reader = std::io::BufReader::new(fs::File::open(path)?);
        let unit = crate::codec::read_current_owned(&mut reader, body_budget)?;
        if unit.entity_id != id {
            return Err(Error::Corrupt("filename identity".into()));
        }
        Ok(unit)
    }
    pub fn read_catalog(&self, id: &str) -> Result<serde_json::Value> {
        self.store.read_catalog_locked(id)
    }
    pub fn list_catalog(&self) -> Result<Vec<serde_json::Value>> {
        self.store.list_catalog_locked()
    }
    pub fn read_closure(
        &self,
        ids: &[String],
    ) -> Result<(SourcePosition, Vec<crate::ResolvedCurrent>, Vec<String>)> {
        self.store.read_closure_locked(ids, self.position.clone())
    }
}
impl OwnedPublishedReader {
    pub fn acquire_current(&self, id: &str) -> Result<crate::acquire::AcquiredCurrent> {
        self.read_snapshot_epoch_first()?.acquire_current(id)
    }
    pub fn acquire_procedure(&self, id: &str) -> Result<crate::acquire::AcquiredCurrent> {
        self.read_snapshot_epoch_first()?.acquire_procedure(id)
    }
    pub fn acquire_closure(
        &self,
        ids: &[String],
    ) -> Result<(
        SourcePosition,
        Vec<crate::acquire::AcquiredResolvedCurrent>,
        Vec<String>,
    )> {
        self.read_snapshot_epoch_first()?.acquire_closure(ids)
    }
    pub fn read_current(&self, id: &str) -> Result<CurrentUnit> {
        self.read_snapshot()?.read_current(id)
    }
    pub fn read_procedure(&self, id: &str) -> Result<CurrentUnit> {
        self.read_snapshot()?.read_procedure(id)
    }
    /// Cursor is `memory/<uuid>` or `procedure/<uuid>`. Filename discovery does not
    /// retain a source gate across consumer work; replay the ledger from the first page's position.
    pub fn inventory_page(
        &self,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<(SourcePosition, Vec<(String, String)>, bool)> {
        if limit == 0 || limit > 4096 {
            return Err(Error::Capacity);
        }
        let position = self.position()?;
        let mut candidates = std::collections::BTreeSet::new();
        for kind in ["memory", "procedure"] {
            for byte in 0..256u16 {
                let dir = self
                    .store
                    .root
                    .join("current")
                    .join(kind)
                    .join(format!("{byte:02x}"));
                if !dir.exists() {
                    continue;
                }
                let prefix = format!("{kind}/{byte:02x}");
                if after_id.is_some_and(|a| a > prefix.as_str() && !a.starts_with(&prefix)) {
                    continue;
                }
                for entry in std::fs::read_dir(dir)? {
                    let path = entry?.path();
                    if path.extension().is_none_or(|s| s != "md") {
                        continue;
                    }
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| Error::Corrupt("inventory filename".into()))?;
                    crate::validate_id(id)?;
                    let key = format!("{kind}/{id}");
                    if after_id.is_some_and(|a| key.as_str() <= a) {
                        continue;
                    }
                    candidates.insert((key, kind.to_string(), id.to_string()));
                    if candidates.len() > limit as usize + 1 {
                        candidates.pop_last();
                    }
                }
                if candidates.len() > limit as usize {
                    break;
                }
            }
            if candidates.len() > limit as usize {
                break;
            }
        }
        let more = candidates.len() > limit as usize;
        if more {
            candidates.pop_last();
        }
        Ok((
            position,
            candidates
                .into_iter()
                .map(|(_, kind, id)| (kind, id))
                .collect(),
            more,
        ))
    }
    pub fn read_closure(
        &self,
        ids: &[String],
    ) -> Result<(SourcePosition, Vec<crate::ResolvedCurrent>, Vec<String>)> {
        self.read_snapshot_epoch_first()?.read_closure(ids)
    }
    pub fn read_catalog(&self, id: &str) -> Result<serde_json::Value> {
        crate::validate_id(id)?;
        self.read_snapshot()?.read_catalog(id)
    }
    pub fn list_catalog(&self) -> Result<Vec<serde_json::Value>> {
        self.read_snapshot()?.list_catalog()
    }
}
impl PublishedChangeSource for OwnedPublishedReader {
    fn position(&self) -> Result<SourcePosition> {
        self.store.published_reader().position()
    }
    fn read_published_changes(
        &self,
        after: &SourcePosition,
        limit: u32,
    ) -> Result<PublishedChanges> {
        self.store
            .published_reader()
            .read_published_changes(after, limit)
    }
}
impl PublishedChangeSource for PublishedReader<'_> {
    fn position(&self) -> Result<SourcePosition> {
        let _gate = self.store.lock()?;
        Ok(SourcePosition {
            epoch: self.store.load(&self.store.local.join("source-epoch"))?,
            sequence: self.store.ensure_clean()?,
        })
    }
    fn read_published_changes(
        &self,
        after: &SourcePosition,
        limit: u32,
    ) -> Result<PublishedChanges> {
        if limit == 0 || limit > 1024 {
            return Err(Error::Capacity);
        }
        let s = self.store;
        let _gate = s.lock()?;
        let generation = s.ensure_clean()?;
        let epoch: String = s.load(&s.local.join("source-epoch"))?;
        if epoch != after.epoch || after.sequence > generation {
            return Err(Error::CoverageLost);
        }
        let through = after
            .sequence
            .checked_add(u64::from(limit))
            .ok_or(Error::Capacity)?
            .min(generation);
        let source_position = SourcePosition {
            epoch: epoch.clone(),
            sequence: generation,
        };
        let covered_through = SourcePosition {
            epoch,
            sequence: through,
        };
        if through == after.sequence {
            return Ok(PublishedChanges {
                source_position,
                covered_through,
                changes: vec![],
            });
        }
        let conn = agentlaw_sqlite::open_with_flags(
            s.local.join("journal.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| Error::CoverageLost)?;
        let mut query = conn
            .prepare("SELECT manifest FROM publications WHERE sequence>?1 AND sequence<=?2 ORDER BY sequence")
            .map_err(|_| Error::CoverageLost)?;
        let mut rows = query.query(rusqlite::params![
            after.sequence.to_be_bytes().as_slice(),
            through.to_be_bytes().as_slice()
        ])?;
        let mut selected = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            let m: Manifest = serde_json::from_str(&raw)?;
            if m.receipt.generation > after.sequence && m.receipt.generation <= through {
                if selected.insert(m.receipt.generation, m).is_some() {
                    return Err(Error::CoverageLost);
                }
            }
        }
        let mut latest = BTreeMap::new();
        for (sequence, m) in &selected {
            for t in &m.targets {
                if t.path.starts_with("current/") {
                    latest.insert(t.path.clone(), *sequence);
                }
            }
        }
        let mut changes = Vec::new();
        for sequence in (after.sequence + 1)..=through {
            let m = selected.remove(&sequence).ok_or(Error::CoverageLost)?;
            let mut units = Vec::new();
            let mut paths = Vec::new();
            for target in m.targets {
                paths.push(target.path.clone());
                if target.path.starts_with("current/")
                    && latest.get(&target.path) == Some(&sequence)
                {
                    let path = s.safe_path(&target.path)?;
                    let unit =
                        CurrentUnit::decode(&mut std::io::BufReader::new(fs::File::open(path)?))?;
                    units.push(unit);
                }
            }
            changes.push(PublishedChange {
                operation_id: m.operation_id,
                sequence,
                current_units: units,
                canonical_paths: paths,
            });
        }
        let _: Fence = s.fence()?;
        Ok(PublishedChanges {
            source_position,
            covered_through,
            changes,
        })
    }
}
