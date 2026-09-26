//! Predicate CAS. Baseline adjacency must come from an exact index at source_position.
use super::*;
use published::SourcePosition;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadSet {
    pub source_position: SourcePosition,
    /// All current head versions, keyed by memory ID (not an arbitrary selected head).
    pub observed: BTreeMap<String, Vec<String>>,
    pub absent: Vec<String>,
    /// Complete baseline incoming required edges, keyed by required target memory ID.
    pub reverse_required: BTreeMap<String, Vec<String>>,
    /// Exact canonical applicability objects whose overlap corpus was inspected.
    #[serde(default)]
    pub watched_scopes: Vec<Value>,
}
impl Store {
    pub fn publish_with_readset(
        &self,
        operation_id: &str,
        mutations: Vec<Mutation>,
        read_set: &ReadSet,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<PublishReceipt> {
        self.publish_checked(
            operation_id,
            mutations,
            None,
            None,
            cancel,
            Some(read_set),
            None,
        )
    }
    pub(super) fn validate_read_set_locked(&self, read_set: &ReadSet) -> Result<()> {
        let generation = self.ensure_clean()?;
        let epoch: String = self.load(&self.local.join("source-epoch"))?;
        if epoch != read_set.source_position.epoch || read_set.source_position.sequence > generation
        {
            return Err(Error::CoverageLost);
        }
        for (id, expected) in &read_set.observed {
            let references = match self.read_kind(id, "memory") {
                Ok(unit) => unit.references()?,
                Err(Error::Capacity) => {
                    self.acquire_kind_locked(
                        id,
                        "memory",
                        SourcePosition {
                            epoch: epoch.clone(),
                            sequence: generation,
                        },
                    )?
                    .references
                }
                Err(e) => return Err(e),
            };
            let actual = references
                .into_iter()
                .map(|r| r.observed_version)
                .collect::<BTreeSet<_>>();
            let expected = expected.iter().cloned().collect::<BTreeSet<_>>();
            if actual != expected {
                return Err(Error::Stale(id.clone()));
            }
        }
        for id in &read_set.absent {
            match self.read_kind(id, "memory") {
                Err(Error::NotFound(_)) => {}
                Ok(_) => return Err(Error::Stale(id.clone())),
                Err(e) => return Err(e),
            }
        }
        let baseline = read_set
            .reverse_required
            .iter()
            .map(|(id, refs)| (id.clone(), refs.iter().cloned().collect::<BTreeSet<_>>()))
            .collect::<BTreeMap<_, _>>();
        for (id, refs) in &baseline {
            validate_id(id)?;
            for r in refs {
                validate_id(r)?;
            }
        }
        let mut actual = baseline.clone();
        if read_set.source_position.sequence == generation {
            return Ok(());
        }
        let conn = rusqlite::Connection::open_with_flags(
            self.local.join("journal.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| Error::CoverageLost)?;
        let mut query=conn.prepare("SELECT manifest FROM publications WHERE sequence>?1 AND sequence<=?2 ORDER BY sequence").map_err(|_|Error::CoverageLost)?;
        let mut rows = query.query(rusqlite::params![
            read_set.source_position.sequence.to_be_bytes().as_slice(),
            generation.to_be_bytes().as_slice()
        ])?;
        let mut seen = read_set.source_position.sequence;
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            let manifest: Manifest = serde_json::from_str(&raw)?;
            seen = seen.checked_add(1).ok_or(Error::Capacity)?;
            if manifest.receipt.generation != seen {
                return Err(Error::CoverageLost);
            }
            if actual.is_empty() && read_set.watched_scopes.is_empty() {
                continue;
            }
            for target in manifest.targets {
                if !target.path.starts_with("current/memory/") {
                    continue;
                }
                // The ledger currently retains after-images, not prior applicability.
                // A move out of a watched scope is as relevant as a move into it.
                if !read_set.watched_scopes.is_empty() {
                    return Err(Error::Stale(
                        "overlap corpus changed; prior scope cannot be excluded".into(),
                    ));
                }
                let path = self.safe_path(&target.path)?;
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| Error::Corrupt("current path ID".into()))?;
                let acquired = self.acquire_kind_locked(
                    id,
                    "memory",
                    SourcePosition {
                        epoch: epoch.clone(),
                        sequence: generation,
                    },
                )?;
                let required = acquired.state["current_heads"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|h| {
                        h.get("relations")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                    })
                    .filter(|r| r.get("kind").and_then(Value::as_str) == Some("required"))
                    .filter_map(|r| r.get("target_memory_id").and_then(Value::as_str))
                    .collect::<BTreeSet<_>>();
                for (target, sources) in &mut actual {
                    sources.remove(id);
                    if required.contains(target.as_str()) {
                        sources.insert(id.to_owned());
                    }
                }
            }
        }
        if seen != generation {
            return Err(Error::CoverageLost);
        }
        if actual != baseline {
            return Err(Error::Stale("reverse required adjacency changed".into()));
        }
        Ok(())
    }
}
