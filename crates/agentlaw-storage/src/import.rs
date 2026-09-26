//! Approved isolated import publication, with causal preservation and ordinary C6 redo.
use super::*;
impl Store {
    pub fn validate_imported_source(&self, staged: &Store, expected_generation: u64) -> Result<()> {
        if staged
            .import_conflicts()?
            .iter()
            .any(|c| c.selected.is_none())
        {
            return Err(Error::Stale("unresolved structural conflicts: explicitly choose local or incoming, then reconcile lineage using the staged call workflow".into()));
        }
        let old_history = self.spool_history()?;
        let mut incoming = staged.spool_history()?;
        incoming.audit_current(staged)?;
        incoming.assert_history_preserved(&old_history)?;
        let _gate = staged.lock()?;
        if staged.ensure_clean()? != incoming.source_position.sequence {
            return Err(Error::Stale("staged source changed".into()));
        }
        self.validate_import_lineage_locked(staged, expected_generation, &old_history, &incoming)
    }
    fn validate_import_lineage_locked(
        &self,
        staged: &Store,
        expected_generation: u64,
        old_history: &history_spool::HistorySpool,
        incoming: &history_spool::HistorySpool,
    ) -> Result<()> {
        for kind in ["memory", "procedure"] {
            let base = self.root.join("current").join(kind);
            if !base.exists() {
                continue;
            }
            for shard in fs::read_dir(base)? {
                for entry in fs::read_dir(shard?.path())? {
                    let path = entry?.path();
                    if path.extension().is_none_or(|e| e != "md") {
                        continue;
                    }
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| Error::Corrupt("unit filename".into()))?;
                    let old = {
                        let _gate = self.lock()?;
                        if self.ensure_clean()? != expected_generation {
                            return Err(Error::Stale("import active source changed".into()));
                        }
                        self.acquire_kind_locked(id, kind, old_history.source_position.clone())?
                    };
                    let mut new =
                        staged.acquire_kind_locked(id, kind, incoming.source_position.clone())?;
                    if old.state["state"] == "redirect" && new.state["state"] != "redirect" {
                        return Err(Error::Stale(format!("import resurrects redirected {id}; use staged consolidate to preserve the active consolidation")));
                    }
                    let mut visited = BTreeSet::new();
                    while new.state["state"] == "redirect" {
                        let destination = new.state["redirect_to"]
                            .as_str()
                            .ok_or_else(|| Error::Corrupt("redirect destination".into()))?
                            .to_owned();
                        if !visited.insert(destination.clone()) {
                            return Err(Error::Corrupt("redirect cycle".into()));
                        }
                        new = staged.acquire_kind_locked(
                            &destination,
                            kind,
                            incoming.source_position.clone(),
                        )?;
                    }
                    let heads = new.state["current_heads"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|h| h["change_id"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>();
                    let mut required = old.state["current_heads"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|h| h["change_id"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>();
                    if let Some(id) = old.state["consolidation_change_id"].as_str() {
                        required.push(id.into());
                    }
                    for change in required {
                        if !incoming.is_ancestor_of_any(&change, &heads)? {
                            return Err(Error::Stale(format!("import discards active lineage of {id}; use staged call evolve/consolidate to include consolidation/head {change} in the final destination ancestry, then resolve again")));
                        }
                    }
                }
            }
        }
        if self.generation()? != expected_generation {
            return Err(Error::Stale("import active source changed".into()));
        }
        Ok(())
    }
    /// Mechanical review workspace: immutable history union and maximal causal heads.
    /// No body/meaning is chosen when two concurrent heads survive.
    pub fn prepare_import_union(
        &self,
        incoming: &Store,
        root: impl AsRef<Path>,
        local: impl AsRef<Path>,
    ) -> Result<Store> {
        let active_generation = self.generation()?;
        let incoming_generation = incoming.generation()?;
        if root.as_ref().exists() {
            return Err(Error::Corrupt("review workspace must be new".into()));
        }
        fs::create_dir_all(root.as_ref())?;
        fs::create_dir_all(local.as_ref())?;
        let conflict_dir = local.as_ref().join("import-conflicts");
        let mut conflicts = Vec::new();
        self.with_source_read(|source, _| {
            copy_canonical(source, root.as_ref(), false, &conflict_dir, &mut conflicts)
        })?;
        incoming.with_source_read(|source, _| {
            copy_canonical(source, root.as_ref(), true, &conflict_dir, &mut conflicts)
        })?;
        fs::create_dir_all(local.as_ref())?;
        let staged = Store {
            root: fs::canonicalize(root)?,
            local: fs::canonicalize(local)?,
        };
        staged.register_local_binding()?;
        staged.ensure_epoch()?;
        staged.record(
            &staged.local.join("source-fence"),
            &Fence {
                generation: 0,
                operation_id: None,
                manifest_digest: None,
            },
        )?;
        staged.record(&staged.local.join("validation-required"), &true)?;
        let history = staged.spool_history_internal(true)?;
        let position = history.source_position.clone();
        for kind in ["memory", "procedure"] {
            let base = incoming.root.join("current").join(kind);
            if !base.exists() {
                continue;
            }
            for shard in fs::read_dir(base)? {
                for entry in fs::read_dir(shard?.path())? {
                    let path = entry?.path();
                    if path.extension().is_none_or(|e| e != "md") {
                        continue;
                    }
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| Error::Corrupt("incoming filename".into()))?;
                    let next = if kind == "memory" {
                        incoming.acquire_current(id)?
                    } else {
                        incoming.acquire_procedure(id)?
                    };
                    let old = match staged.acquire_kind_locked(id, kind, position.clone()) {
                        Ok(v) => Some(v),
                        Err(Error::NotFound(_)) => None,
                        Err(e) => return Err(e),
                    };
                    let combined = if let Some(mut old) = old {
                        if old.state["state"] == "redirect" || next.state["state"] == "redirect" {
                            if old.state == next.state {
                                old
                            } else {
                                let relative = format!("current/{kind}/{}/{}.md", &id[..2], id);
                                let mut conflict = import_conflict::capture(
                                    &conflict_dir,
                                    "redirect",
                                    &relative,
                                    &staged.root.join(&relative),
                                    &incoming.root.join(&relative),
                                )?;
                                conflict.local_references = old.references.clone();
                                conflict.incoming_references = next.references.clone();
                                conflicts.push(conflict);
                                // Keep the active candidate byte-exact until an
                                // explicit side choice, including valid formatting.
                                continue;
                            }
                        } else {
                            let mut heads = BTreeMap::new();
                            for head in old.state["current_heads"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .chain(next.state["current_heads"].as_array().into_iter().flatten())
                            {
                                let cid = head["change_id"]
                                    .as_str()
                                    .ok_or_else(|| Error::Corrupt("head identity".into()))?;
                                if let Some(prior) = heads.insert(cid.to_owned(), head.clone()) {
                                    if prior != *head {
                                        return Err(Error::Corrupt(
                                            "immutable current metadata collision".into(),
                                        ));
                                    }
                                }
                            }
                            old.bodies.extend(next.bodies);
                            let ids = heads.keys().cloned().collect::<Vec<_>>();
                            for candidate in &ids {
                                for other in &ids {
                                    if candidate != other
                                        && history.is_ancestor_of_any(
                                            candidate,
                                            std::slice::from_ref(other),
                                        )?
                                    {
                                        heads.remove(candidate);
                                        break;
                                    }
                                }
                            }
                            old.state["current_heads"] =
                                Value::Array(heads.into_values().collect());
                            old
                        }
                    } else {
                        next
                    };
                    write_acquired(
                        &staged
                            .root
                            .join(format!("current/{kind}/{}/{}.md", &id[..2], id)),
                        &combined,
                        kind,
                    )?;
                }
            }
        }
        if self.generation()? != active_generation || incoming.generation()? != incoming_generation
        {
            return Err(Error::Stale(
                "source changed during import preparation".into(),
            ));
        }
        staged.audit_source()?;
        staged.record(&staged.local.join("import-conflicts.json"), &conflicts)?;
        staged.record(&staged.local.join("import-review"),&json!({"active_root":self.root,"active_generation":active_generation,"incoming_root":incoming.root,"incoming_generation":incoming_generation}))?;
        fs::remove_file(staged.local.join("validation-required"))?;
        Ok(staged)
    }
    /// The caller reviews/edits a separate source with ordinary Runtime operations.
    /// This publishes only an audited full replacement view preserving every old
    /// immutable frame and every active head as retained head or causal ancestor.
    pub fn publish_imported_source(
        &self,
        operation_id: &str,
        staged: &Store,
        expected_generation: u64,
    ) -> Result<PublishReceipt> {
        self.publish_imported_source_with_fault(operation_id, staged, expected_generation, None)
    }
    pub fn publish_imported_source_with_fault(
        &self,
        operation_id: &str,
        staged: &Store,
        expected_generation: u64,
        fault: Option<FaultPoint>,
    ) -> Result<PublishReceipt> {
        self.publish_imported_source_checked(operation_id, staged, expected_generation, None, fault)
    }
    pub fn publish_imported_source_at(
        &self,
        operation_id: &str,
        staged: &Store,
        expected_generation: u64,
        staged_generation: u64,
    ) -> Result<PublishReceipt> {
        self.publish_imported_source_checked(
            operation_id,
            staged,
            expected_generation,
            Some(staged_generation),
            None,
        )
    }
    fn publish_imported_source_checked(
        &self,
        operation_id: &str,
        staged: &Store,
        expected_generation: u64,
        staged_generation: Option<u64>,
        fault: Option<FaultPoint>,
    ) -> Result<PublishReceipt> {
        validate_id(operation_id)?;
        if self.root == staged.root {
            return Err(Error::Corrupt("import requires isolated source".into()));
        }
        let dir = self.local.join("recovery").join(operation_id);
        fs::create_dir_all(&dir)?;
        let operation = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("execution.lock"))?;
        operation.lock_exclusive()?;
        let _admission = self.admission_lock()?;
        let request_digest = codec::digest(&serde_json::to_vec(
            &json!({"kind":"approved_import","root":staged.root,"basis":expected_generation}),
        )?);
        {
            let _gate = self.lock()?;
            self.recover_locked()?;
            if dir.join("manifest").exists() {
                let m: Manifest = self.load(&dir.join("manifest"))?;
                if m.request_digest != request_digest {
                    return Err(Error::Corrupt("import operation binding".into()));
                }
                if dir.join("published").exists() {
                    return Ok(m.receipt);
                }
            }
            if self.ensure_clean()? != expected_generation {
                return Err(Error::Stale("import active source changed".into()));
            }
        }
        let (mut bytes, mut files) = (0u64, 0u64);
        let mut stack = vec![staged.root.clone()];
        while let Some(path) = stack.pop() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_symlink() {
                    return Err(Error::Corrupt("staged symlink".into()));
                }
                if entry.file_type()?.is_dir() {
                    if entry.file_name() != ".git" {
                        stack.push(entry.path());
                    }
                } else if entry.path().extension().is_some_and(|s| s == "md") {
                    bytes = bytes
                        .checked_add(entry.metadata()?.len())
                        .ok_or(Error::Capacity)?;
                    files = files.checked_add(1).ok_or(Error::Capacity)?;
                }
            }
        }
        self.admit_stream(operation_id, bytes, files)?;
        if staged
            .import_conflicts()?
            .iter()
            .any(|c| c.selected.is_none())
        {
            return Err(Error::Stale(
                "unresolved structural choices; resolve each retained conflict explicitly".into(),
            ));
        }
        let old_history = self.spool_history()?;
        let mut incoming = staged.spool_history()?;
        incoming.audit_current(staged)?;
        incoming.assert_history_preserved(&old_history)?;
        let staged_gate = staged.lock()?;
        if staged_generation.is_some_and(|g| g != incoming.source_position.sequence) {
            return Err(Error::Stale("approved staged generation changed".into()));
        }
        if staged.ensure_clean()? != incoming.source_position.sequence {
            return Err(Error::Stale("staged source changed after audit".into()));
        }
        self.validate_import_lineage_locked(staged, expected_generation, &old_history, &incoming)?;
        let mut targets = Vec::new();
        let mut refs = Vec::new();
        for base in ["current", "history", "catalog"] {
            let mut stack = vec![staged.root.join(base)];
            while let Some(path) = stack.pop() {
                if !path.exists() {
                    continue;
                }
                for entry in fs::read_dir(path)? {
                    let entry = entry?;
                    if entry.file_type()?.is_symlink() {
                        return Err(Error::Corrupt("import symlink".into()));
                    }
                    if entry.file_type()?.is_dir() {
                        stack.push(entry.path());
                        continue;
                    }
                    let source = entry.path();
                    if source.extension().is_none_or(|e| e != "md") {
                        continue;
                    }
                    let relative = source
                        .strip_prefix(&staged.root)
                        .map_err(|_| Error::Corrupt("import relative path".into()))?
                        .to_string_lossy()
                        .replace('\\', "/");
                    let digest = hash_file(&source)?
                        .ok_or_else(|| Error::Corrupt("missing staged file".into()))?;
                    let expected = hash_file(&self.safe_path(&relative)?)?;
                    if expected.as_deref() == Some(&digest) {
                        continue;
                    }
                    if relative.starts_with("history/") {
                        self.prepare_target(&dir, &mut targets, relative, fs::read(source)?)?;
                    } else {
                        let image = format!("image-{}", targets.len());
                        install_reader(&dir.join(&image), &mut File::open(source)?)?;
                        targets.push(Target {
                            path: relative.clone(),
                            expected,
                            image,
                            digest,
                            append: None,
                        });
                        if relative.starts_with("current/") {
                            let parts = relative.split('/').collect::<Vec<_>>();
                            let id = Path::new(parts.last().unwrap())
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .ok_or_else(|| Error::Corrupt("import current name".into()))?;
                            refs.extend(
                                staged
                                    .acquire_kind_locked(
                                        id,
                                        parts[1],
                                        incoming.source_position.clone(),
                                    )?
                                    .references,
                            );
                        }
                    }
                }
            }
        }
        drop(staged_gate);
        let _gate = self.lock()?;
        self.recover_locked()?;
        if self.ensure_clean()? != expected_generation {
            return Err(Error::Stale("import changed before decision".into()));
        }
        for target in &targets {
            if hash_file(&self.safe_path(&target.path)?)? != target.expected {
                return Err(Error::Stale(target.path.clone()));
            }
        }
        let receipt = PublishReceipt {
            operation_id: operation_id.into(),
            generation: expected_generation.checked_add(1).ok_or(Error::Capacity)?,
            references: refs,
            durability: Durability::FileSyncedProcessCrashProtocolPowerLossUnverified,
        };
        let manifest = Manifest {
            imported: true,
            root: self.root.to_string_lossy().into(),
            operation_id: operation_id.into(),
            request_digest,
            prior_generation: expected_generation,
            receipt: receipt.clone(),
            targets,
        };
        self.record(&dir.join("manifest"), &manifest)?;
        hit(fault, FaultPoint::Prepared)?;
        self.check_decision_capacity(&dir, &manifest.targets, None)?;
        let digest = codec::digest(&fs::read(dir.join("manifest"))?);
        self.record(
            &self.local.join("source-fence"),
            &Fence {
                generation: expected_generation,
                operation_id: Some(operation_id.into()),
                manifest_digest: Some(digest.clone()),
            },
        )?;
        hit(fault, FaultPoint::DirtyFence)?;
        self.record(&dir.join("decision"), &digest)?;
        hit(fault, FaultPoint::Decision)?;
        self.redo(&dir, &manifest, fault)?;
        drop(_gate);
        self.automatic_maintenance();
        Ok(receipt)
    }
}
fn copy_canonical(
    source: &Path,
    target: &Path,
    history_only_union: bool,
    conflict_dir: &Path,
    conflicts: &mut Vec<import_conflict::ImportConflict>,
) -> Result<()> {
    for base in ["current", "history", "catalog"] {
        if history_only_union && base == "current" {
            continue;
        }
        let start = source.join(base);
        if fs::symlink_metadata(&start).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::Corrupt("canonical root child symlink".into()));
        }
        if !start.exists() {
            continue;
        }
        let mut stack = vec![start];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                if entry.file_type()?.is_symlink() {
                    return Err(Error::Corrupt("canonical symlink".into()));
                }
                if entry.file_type()?.is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                let from = entry.path();
                if from.extension().is_none_or(|e| e != "md") {
                    continue;
                }
                let mut to = target.join(
                    from.strip_prefix(source)
                        .map_err(|_| Error::Corrupt("copy relative path".into()))?,
                );
                if history_only_union && base == "history" {
                    if to.exists() && hash_file(&to)? == hash_file(&from)? {
                        continue;
                    }
                    to.set_file_name(format!("{}.md", uuid::Uuid::new_v4()));
                }
                if to.exists() {
                    if hash_file(&to)? != hash_file(&from)? {
                        let relative = from
                            .strip_prefix(source)
                            .map_err(|_| Error::Corrupt("catalog conflict path".into()))?
                            .to_string_lossy()
                            .replace('\\', "/");
                        conflicts.push(import_conflict::capture(
                            conflict_dir,
                            "catalog",
                            &relative,
                            &to,
                            &from,
                        )?);
                    }
                    continue;
                }
                install_reader(&to, &mut File::open(from)?)?;
            }
        }
    }
    if !history_only_union {
        for name in ["format.md", ".gitattributes"] {
            if source.join(name).exists() {
                install_reader(&target.join(name), &mut File::open(source.join(name))?)?;
            }
        }
    }
    Ok(())
}
fn write_acquired(path: &Path, current: &acquire::AcquiredCurrent, kind: &str) -> Result<()> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| Error::Corrupt("current parent".into()))?,
    )?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut out = File::create(&temp)?;
    out.write_all(b"<!-- agentlaw-file-v1 kind=current -->\n")?;
    let state = codec::canonical_json(&current.state)?;
    let id = current.state[if kind == "memory" {
        "memory_id"
    } else {
        "procedure_id"
    }]
    .as_str()
    .ok_or_else(|| Error::Corrupt("state ID".into()))?;
    write!(
        out,
        "<!-- agentlaw-record-v1 type=state key={id} bytes={} sha256={} -->\n",
        state.len(),
        codec::digest(&state)
    )?;
    out.write_all(&state)?;
    out.write_all(b"\n<!-- /agentlaw-record-v1 -->\n")?;
    let mut count = 1;
    for head in current.state["current_heads"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let id = head["change_id"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("head ID".into()))?;
        let body = current
            .bodies
            .get(id)
            .ok_or_else(|| Error::Corrupt("missing head body".into()))?;
        write!(
            out,
            "<!-- agentlaw-record-v1 type={} key={id} bytes={} sha256={} -->\n",
            if kind == "memory" {
                "body"
            } else {
                "instructions"
            },
            body.bytes,
            body.sha256
        )?;
        body.copy_to(&mut out)?;
        out.write_all(b"\n<!-- /agentlaw-record-v1 -->\n")?;
        count += 1;
    }
    writeln!(out, "<!-- /agentlaw-file-v1 records={count} -->")?;
    out.sync_all()?;
    drop(out);
    use persistence::Persistence;
    persistence::PlatformPersistence.install_replace(&temp, path)?;
    Ok(())
}
