//! C6 current responsibilities; shares the facade's source gate and persistence protocol.
use super::*;

impl Store {
    pub fn read_closure(
        &self,
        ids: &[String],
    ) -> Result<(published::SourcePosition, Vec<ResolvedCurrent>, Vec<String>)> {
        let _gate = self.lock()?;
        let position = published::SourcePosition {
            epoch: self.load(&self.local.join("source-epoch"))?,
            sequence: self.ensure_clean()?,
        };
        let mut queue = std::collections::VecDeque::from(ids.to_vec());
        let mut visited = BTreeSet::new();
        let mut resolved_ids = BTreeSet::new();
        let mut result = Vec::new();
        let mut missing = Vec::new();
        let mut bytes = 0u64;
        while let Some(requested_id) = queue.pop_front() {
            if !visited.insert(requested_id.clone()) {
                continue;
            }
            validate_id(&requested_id)?;
            let mut next = requested_id.clone();
            let mut redirect_path = Vec::new();
            let mut chain = BTreeSet::new();
            loop {
                if !chain.insert(next.clone()) {
                    return Err(Error::Corrupt("redirect cycle".into()));
                }
                let current = match self.read_kind(&next, "memory") {
                    Ok(u) => u,
                    Err(Error::NotFound(_)) => {
                        missing.push(next);
                        break;
                    }
                    Err(e) => return Err(e),
                };
                if let UnitState::Redirect { redirect_to, .. } = &current.state {
                    redirect_path.push(next);
                    next = redirect_to.clone();
                    continue;
                }
                if resolved_ids.insert(current.entity_id.clone()) {
                    for head in current.heads() {
                        bytes = bytes
                            .checked_add(head.body.len() as u64)
                            .ok_or(Error::Capacity)?;
                        if bytes > codec::MAX_FRAME_BYTES {
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
                    result.push(ResolvedCurrent {
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
        Ok((position, result, missing))
    }
    pub(super) fn read_kind(&self, id: &str, kind: &str) -> Result<CurrentUnit> {
        validate_id(id)?;
        let path = self.safe_path(&format!("current/{kind}/{}/{}.md", &id[..2], id))?;
        if !path.exists() {
            return Err(Error::NotFound(id.into()));
        }
        let u = CurrentUnit::decode(&mut BufReader::new(File::open(path)?))?;
        if u.entity_id != id {
            return Err(Error::Corrupt("filename identity".into()));
        }
        Ok(u)
    }
    pub fn read_current(&self, id: &str) -> Result<CurrentUnit> {
        self.read_many(&[id.into()]).map(|mut v| v.remove(0))
    }
    pub fn resolve_current(&self, id: &str) -> Result<ResolvedCurrent> {
        let _l = self.lock()?;
        self.ensure_clean()?;
        let mut next = id.to_string();
        let mut path = Vec::new();
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(next.clone()) {
                return Err(Error::Corrupt("redirect cycle".into()));
            }
            let current = self.read_kind(&next, "memory")?;
            match &current.state {
                UnitState::Redirect { redirect_to, .. } => {
                    path.push(next);
                    next = redirect_to.clone();
                }
                UnitState::Live { .. } => {
                    return Ok(ResolvedCurrent {
                        requested_id: id.into(),
                        redirect_path: path,
                        current,
                    })
                }
            }
        }
    }
    pub fn read_exact(&self, id: &str, observed_version: &str) -> Result<HistoricalState> {
        self.history(id)?
            .into_iter()
            .find(|h| h.metadata["observed_version"] == observed_version)
            .ok_or_else(|| Error::NotFound(format!("{id} exact version")))
    }
    pub fn read_procedure(&self, id: &str) -> Result<CurrentUnit> {
        let _l = self.lock()?;
        self.ensure_clean()?;
        self.read_kind(id, "procedure")
    }
    pub fn read_many(&self, ids: &[String]) -> Result<Vec<CurrentUnit>> {
        let _l = self.lock()?;
        self.ensure_clean()?;
        ids.iter().map(|id| self.read_kind(id, "memory")).collect()
    }
    pub fn generation(&self) -> Result<u64> {
        let _l = self.lock()?;
        self.ensure_clean()
    }
    /// C8 may capture an immutable Git tree while this short cooperative gate is held.
    /// The callback must not recursively call Store methods or perform network/user waits.
    pub fn with_source_read<T>(&self, read: impl FnOnce(&Path, u64) -> Result<T>) -> Result<T> {
        let _l = self.lock()?;
        let generation = self.ensure_clean()?;
        read(&self.root, generation)
    }
    /// Streaming inventory, owned unit bytes acquired while publication is excluded.
    pub fn visit_current(&self, mut visit: impl FnMut(CurrentUnit) -> Result<()>) -> Result<u64> {
        let _l = self.lock()?;
        let generation = self.ensure_clean()?;
        self.visit_current_locked(&mut visit)?;
        Ok(generation)
    }
    pub(super) fn visit_current_locked(
        &self,
        visit: &mut impl FnMut(CurrentUnit) -> Result<()>,
    ) -> Result<()> {
        for kind in ["memory", "procedure"] {
            let base = self.root.join("current").join(kind);
            if !base.exists() {
                continue;
            }
            for shard in fs::read_dir(base)? {
                let shard = shard?;
                if !shard.file_type()?.is_dir() {
                    return Err(Error::Corrupt("current shard".into()));
                }
                for entry in fs::read_dir(shard.path())? {
                    let entry = entry?;
                    if entry.path().extension().is_some_and(|e| e == "tmp") {
                        continue;
                    }
                    if entry.path().extension().is_none_or(|e| e != "md") {
                        return Err(Error::Corrupt("current file".into()));
                    }
                    let unit = CurrentUnit::decode(&mut BufReader::new(File::open(entry.path())?))?;
                    if self.root.join(Self::relative(&unit)) != entry.path() {
                        return Err(Error::Corrupt("current path identity".into()));
                    }
                    visit(unit)?;
                }
            }
        }
        Ok(())
    }
    /// Convenience for bounded test/small-workspace consumers. Large callers must stream.
    pub fn snapshot(&self) -> Result<(u64, Vec<CurrentUnit>)> {
        let mut units = Vec::new();
        let mut bytes = 0u64;
        let generation = self.visit_current(|u| {
            bytes = bytes
                .checked_add(u.heads().iter().map(|h| h.body.len() as u64).sum::<u64>())
                .ok_or(Error::Capacity)?;
            if bytes > codec::MAX_FRAME_BYTES || units.len() >= 100_000 {
                return Err(Error::Capacity);
            }
            units.push(u);
            Ok(())
        })?;
        Ok((generation, units))
    }
}
