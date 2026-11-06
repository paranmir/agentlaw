//! C7's read-only source port. RAM wakeups are not the change ledger.
use crate::{CurrentUnit, Error, Fence, Manifest, Result, Store};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs};
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
    pub fn read_snapshot(&self) -> Result<SourceReadGuard<'_>> {
        let gate = self.store.lock_shared()?;
        let sequence = self.store.ensure_clean()?;
        let epoch = self.store.load(&self.store.local.join("source-epoch"))?;
        Ok(SourceReadGuard {
            store: &self.store,
            _gate: gate,
            position: SourcePosition { epoch, sequence },
        })
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
        })
    }
}
impl SourceReadGuard<'_> {
    pub fn position(&self) -> &SourcePosition {
        &self.position
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
            let conn = rusqlite::Connection::open_with_flags(
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
        self.store
            .acquire_kind_locked(id, "memory", self.position.clone())
    }
    pub fn acquire_procedure(&self, id: &str) -> Result<crate::acquire::AcquiredCurrent> {
        self.store
            .acquire_kind_locked(id, "procedure", self.position.clone())
    }
    pub fn acquire_closure(
        &self,
        ids: &[String],
    ) -> Result<(
        SourcePosition,
        Vec<crate::acquire::AcquiredResolvedCurrent>,
        Vec<String>,
    )> {
        self.store
            .acquire_closure_locked(ids, self.position.clone())
    }
    pub fn read_current(&self, id: &str) -> Result<CurrentUnit> {
        self.store.read_kind(id, "memory")
    }
    pub fn read_procedure(&self, id: &str) -> Result<CurrentUnit> {
        self.store.read_kind(id, "procedure")
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
        let mut queue = std::collections::VecDeque::from(ids.to_vec());
        let mut visited = std::collections::BTreeSet::new();
        let mut resolved_ids = std::collections::BTreeSet::new();
        let mut result = Vec::new();
        let mut missing = Vec::new();
        let mut bytes = 0u64;
        while let Some(requested_id) = queue.pop_front() {
            if !visited.insert(requested_id.clone()) {
                continue;
            }
            crate::validate_id(&requested_id)?;
            let mut next = requested_id.clone();
            let mut redirect_path = Vec::new();
            let mut chain = std::collections::BTreeSet::new();
            loop {
                if !chain.insert(next.clone()) {
                    return Err(Error::Corrupt("redirect cycle".into()));
                }
                let current = match self.store.read_kind(&next, "memory") {
                    Ok(unit) => unit,
                    Err(Error::NotFound(_)) => {
                        missing.push(next);
                        break;
                    }
                    Err(error) => return Err(error),
                };
                if let crate::UnitState::Redirect { redirect_to, .. } = &current.state {
                    redirect_path.push(next);
                    next = redirect_to.clone();
                    continue;
                }
                if resolved_ids.insert(current.entity_id.clone()) {
                    for head in current.heads() {
                        bytes = bytes
                            .checked_add(head.body.len() as u64)
                            .ok_or(Error::Capacity)?;
                        if bytes > crate::codec::MAX_FRAME_BYTES {
                            return Err(Error::Capacity);
                        }
                        for relation in head.metadata["relations"].as_array().into_iter().flatten()
                        {
                            if relation["kind"] == "required" {
                                queue.push_back(
                                    relation["target_memory_id"]
                                        .as_str()
                                        .ok_or_else(|| Error::Corrupt("required target".into()))?
                                        .to_owned(),
                                );
                            }
                        }
                    }
                    result.push(crate::ResolvedCurrent {
                        requested_id,
                        redirect_path,
                        current,
                    });
                }
                break;
            }
        }
        missing.sort();
        missing.dedup();
        Ok((self.position.clone(), result, missing))
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
        let conn = rusqlite::Connection::open_with_flags(
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
