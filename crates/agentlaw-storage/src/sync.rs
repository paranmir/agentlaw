//! Fixed canonical captures and import-only reconciliation. No Git or semantic choice.
use super::*;
use persistence::{Persistence, PlatformPersistence};
use published::SourcePosition;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct CaptureLimits {
    pub files: u64,
    pub bytes: u64,
    pub elapsed: Duration,
}
impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            files: 100_000,
            bytes: 512 * 1024 * 1024,
            elapsed: Duration::from_secs(5),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capture {
    pub source: PathBuf,
    pub position: SourcePosition,
    pub files: BTreeMap<String, String>,
    pub bytes: u64,
    pub gate_elapsed_ms: u128,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolutionUnit {
    pub unit: CurrentUnit,
    pub parents: Vec<VersionRef>,
    pub evidence: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resolution {
    pub units: Vec<ResolutionUnit>,
    /// Stable project identity is preserved by the writer.
    pub projects: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OverlayGuard {
    pub position: SourcePosition,
    pub paths: BTreeMap<String, Option<String>>,
    pub predicates: BTreeMap<String, u64>,
}

pub fn canonical_files(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut result = Vec::new();
    let mut queue = vec![
        root.join("current"),
        root.join("history"),
        root.join("catalog"),
    ];
    for name in ["format.md", ".gitattributes"] {
        if root.join(name).exists() {
            queue.push(root.join(name));
        }
    }
    while let Some(path) = queue.pop() {
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(Error::Corrupt("canonical symlink".into()));
        }
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                queue.push(entry?.path());
            }
        } else if meta.is_file()
            && (path.extension().is_some_and(|e| e == "md")
                || path.file_name().is_some_and(|e| e == ".gitattributes"))
        {
            result.push((
                path.strip_prefix(root)
                    .map_err(|_| Error::Corrupt("capture path".into()))?
                    .to_string_lossy()
                    .replace('\\', "/"),
                path,
            ));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

impl Store {
    /// Copy only completed canonical content. Completion becomes durable after releasing
    /// the source gate; failed partial directories are never accepted as a cutoff.
    pub fn capture_sync(
        &self,
        target: &Path,
        limits: &CaptureLimits,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Capture> {
        if target.exists() {
            return Err(Error::Corrupt(
                "capture requires new owned directory".into(),
            ));
        }
        fs::create_dir_all(target)?;
        let capture = {
            let _gate = self.lock()?;
            self.recover_locked()?;
            let start = Instant::now();
            let position = SourcePosition {
                epoch: self.load(&self.local.join("source-epoch"))?,
                sequence: self.ensure_clean()?,
            };
            let check = || -> Result<()> {
                if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)) {
                    return Err(Error::CancelledBeforeDecision);
                }
                if start.elapsed() > limits.elapsed {
                    return Err(Error::InsufficientResource {
                        resource: "sync_capture_time_budget_ms".into(),
                        required: start.elapsed().as_millis() as u64,
                        available: limits.elapsed.as_millis() as u64,
                    });
                }
                Ok(())
            };
            let mut files = BTreeMap::new();
            let mut bytes = 0u64;
            // Inventory itself is bounded and checks the cooperative deadline.
            let mut queue = vec![
                self.root.join("current"),
                self.root.join("history"),
                self.root.join("catalog"),
                self.root.join("format.md"),
                self.root.join(".gitattributes"),
            ];
            let mut entries = 0u64;
            while let Some(path) = queue.pop() {
                check()?;
                entries = entries.checked_add(1).ok_or(Error::Capacity)?;
                if entries > limits.files.saturating_mul(4).saturating_add(1024) {
                    return Err(Error::Capacity);
                }
                let meta = match fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                if meta.file_type().is_symlink() {
                    return Err(Error::Corrupt("capture symlink".into()));
                }
                if meta.is_dir() {
                    for entry in fs::read_dir(path)? {
                        check()?;
                        queue.push(entry?.path());
                        if queue.len() as u64 > limits.files.saturating_mul(4) {
                            return Err(Error::Capacity);
                        }
                    }
                    continue;
                }
                if path.extension().is_none_or(|e| e != "md")
                    && path.file_name().is_none_or(|e| e != ".gitattributes")
                {
                    continue;
                }
                if files.len() as u64 >= limits.files
                    || meta.len() > limits.bytes.saturating_sub(bytes)
                {
                    return Err(Error::InsufficientResource {
                        resource: "sync_capture_bytes_or_files".into(),
                        required: bytes.saturating_add(meta.len()),
                        available: limits.bytes,
                    });
                }
                let relative = path
                    .strip_prefix(&self.root)
                    .map_err(|_| Error::Corrupt("capture relative".into()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let to = target.join(&relative);
                fs::create_dir_all(to.parent().unwrap())?;
                let mut input = File::open(path)?;
                let mut output = OpenOptions::new().create_new(true).write(true).open(&to)?;
                let mut h = Sha256::new();
                let mut buffer = [0u8; 65536];
                loop {
                    check()?;
                    let n = input.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    bytes = bytes.checked_add(n as u64).ok_or(Error::Capacity)?;
                    if bytes > limits.bytes {
                        return Err(Error::Capacity);
                    }
                    output.write_all(&buffer[..n])?;
                    h.update(&buffer[..n]);
                }
                files.insert(relative, format!("{:x}", h.finalize()));
            }
            Capture {
                source: self.root.clone(),
                position,
                files,
                bytes,
                gate_elapsed_ms: start.elapsed().as_millis(),
            }
        };
        // Expensive flushes are outside the source gate. The manifest is the only
        // completion authority, not the existence of a partially copied directory.
        for relative in capture.files.keys() {
            PlatformPersistence.sync_file(&target.join(relative))?;
        }
        self.record(&target.with_extension("capture.json"), &capture)?;
        Ok(capture)
    }

    /// Receipt/recovery precedes any obsolete input guards. A completed operation
    /// never needs its old source generation or temporary review workspace.
    pub fn sync_receipt(&self, operation_id: &str) -> Result<Option<PublishReceipt>> {
        validate_id(operation_id)?;
        let _gate = self.lock()?;
        self.recover_locked()?;
        let dir = self.local.join("recovery").join(operation_id);
        if dir.join("published").exists() {
            return Ok(Some(self.load(&dir.join("published"))?));
        }
        Ok(None)
    }
    pub fn validate_reconciled_source(&self, staged: &Store) -> Result<()> {
        let old = self.spool_history()?;
        let mut new = staged.spool_history()?;
        new.audit_current(staged)?;
        new.assert_history_preserved(&old)?;
        let _gate = staged.lock()?;
        self.validate_import_lineage_locked(staged, old.source_position.sequence, &old, &new)
    }

    /// Only isolated imports use historical refs. Ordinary remember_this and all-head
    /// current-parent validation are unchanged. All final files/history publish together.
    pub fn apply_import_resolution(
        &self,
        operation_id: &str,
        resolution: &Resolution,
    ) -> Result<PublishReceipt> {
        self.bind_sync_request(
            operation_id,
            &serde_json::to_vec(&("resolution", resolution))?,
        )?;
        if let Some(receipt) = self.sync_receipt(operation_id)? {
            return Ok(receipt);
        }
        let history = self.spool_history()?;
        let mut targets = BTreeMap::new();
        let mut new_frames = Vec::new();
        let mut references = Vec::new();
        let mut seen_changes = BTreeSet::new();
        for proposed in &resolution.units {
            proposed.unit.validate()?;
            if targets.contains_key(&Self::relative(&proposed.unit)) {
                return Err(Error::Corrupt("duplicate resolution identity".into()));
            }
            let mut parents: BTreeMap<String, CurrentUnit> = BTreeMap::new();
            let mut seen = BTreeSet::new();
            for reference in &proposed.parents {
                let encoded = reference
                    .observed_version
                    .strip_prefix("av1.")
                    .ok_or_else(|| Error::Corrupt("historical ref".into()))?;
                let raw = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .map_err(|_| Error::Corrupt("historical ref encoding".into()))?;
                if raw.len() != 48 {
                    return Err(Error::Corrupt("historical ref length".into()));
                }
                let id = uuid::Uuid::from_slice(&raw[..16])
                    .map_err(|_| Error::Corrupt("historical ref identity".into()))?
                    .to_string();
                if !seen.insert(id.clone()) {
                    return Err(Error::Corrupt("duplicate historical input".into()));
                }
                let state = history.exact_change(&id)?.into_owned()?;
                if state.metadata["entity_id"] != reference.memory_id
                    || state.metadata["observed_version"] != reference.observed_version
                    || state.metadata["entity_type"] != proposed.unit.entity_type
                {
                    return Err(Error::Corrupt(
                        "retained historical reference binding".into(),
                    ));
                }
                let parent = parents
                    .entry(reference.memory_id.clone())
                    .or_insert(CurrentUnit {
                        entity_id: reference.memory_id.clone(),
                        entity_type: proposed.unit.entity_type.clone(),
                        state: UnitState::Live { heads: vec![] },
                    });
                if let UnitState::Live { heads } = &mut parent.state {
                    heads.push(Head {
                        metadata: state.metadata["metadata_after"].clone(),
                        body: state.body,
                    });
                }
            }
            for head in proposed.unit.heads() {
                // Historical consolidation heads belong to their live targets;
                // retain every final alias identity as a consolidation source too.
                // A redirect has no body parent, but its earlier consolidation is
                // already represented by the verified B/C historical inputs.
                for alias in &resolution.units {
                    if matches!(&alias.unit.state,UnitState::Redirect{redirect_to,..} if redirect_to==&proposed.unit.entity_id)
                    {
                        let source = self.read_current(&alias.unit.entity_id)?;
                        let source = match &source.state {
                            UnitState::Live { .. } => source,
                            UnitState::Redirect {
                                consolidation_change_id,
                                ..
                            } => {
                                let consolidation =
                                    history.exact_change(consolidation_change_id)?;
                                let mut heads = Vec::new();
                                for owner in consolidation.metadata["parent_owners"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                {
                                    if owner["entity_id"] == alias.unit.entity_id {
                                        let cid = owner["change_id"].as_str().ok_or_else(|| {
                                            Error::Corrupt("alias historical parent".into())
                                        })?;
                                        let prior = history.exact_change(cid)?.into_owned()?;
                                        heads.push(Head {
                                            metadata: prior.metadata["metadata_after"].clone(),
                                            body: prior.body,
                                        });
                                    }
                                }
                                if heads.is_empty() {
                                    return Err(Error::Corrupt(
                                        "alias lacks direct retained source ancestry".into(),
                                    ));
                                }
                                CurrentUnit {
                                    entity_id: alias.unit.entity_id.clone(),
                                    entity_type: "memory".into(),
                                    state: UnitState::Live { heads },
                                }
                            }
                        };
                        parents
                            .entry(alias.unit.entity_id.clone())
                            .or_insert(source);
                    }
                }
                if proposed.parents.is_empty() || !seen_changes.insert(change_id(head)?.to_owned())
                {
                    return Err(Error::Corrupt(
                        "resolution requires retained parents and new unique changes".into(),
                    ));
                }
                new_frames.extend(history_frames(
                    &proposed.unit,
                    head,
                    &parents.values().collect::<Vec<_>>(),
                    &proposed.evidence,
                )?);
            }
            references.extend(proposed.unit.references()?);
            targets.insert(Self::relative(&proposed.unit), proposed.unit.encode()?);
        }
        journal::reject_existing_changes(self, &seen_changes)?;
        for (id, project) in &resolution.projects {
            validate_id(id)?;
            if project["project_id"] != *id {
                return Err(Error::Corrupt("catalog identity changed".into()));
            }
            let mut bytes = Vec::new();
            codec::write(
                "catalog",
                &[codec::Frame {
                    kind: "project".into(),
                    key: id.clone(),
                    payload: codec::canonical_json(project)?,
                }],
                &mut bytes,
            )?;
            targets.insert(format!("catalog/projects/{}/{}.md", &id[..2], id), bytes);
        }
        add_packs(&mut targets, new_frames)?;
        let guard = OverlayGuard {
            position: history.source_position,
            paths: targets
                .keys()
                .map(|p| Ok((p.clone(), hash_file(&self.safe_path(p)?)?)))
                .collect::<Result<_>>()?,
            predicates: BTreeMap::new(),
        };
        self.publish_sync_files(operation_id, targets, &guard, references, None, None)
    }

    /// Prepare outside the source gate; immutable frames are selected by key/digest,
    /// never by a mutable history shard's bytes. Unrelated later writes are untouched.
    pub fn publish_sync_overlay(
        &self,
        operation_id: &str,
        baseline: &Store,
        resolved: &Store,
        guard: &OverlayGuard,
        fault: Option<FaultPoint>,
    ) -> Result<PublishReceipt> {
        self.publish_sync_overlay_control(operation_id, baseline, resolved, guard, fault, None)
    }
    pub fn publish_sync_overlay_control(
        &self,
        operation_id: &str,
        baseline: &Store,
        resolved: &Store,
        guard: &OverlayGuard,
        fault: Option<FaultPoint>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<PublishReceipt> {
        // Bind the caller's immutable plan, not randomly named history packs.
        // This remains checkable after its temporary views have been removed.
        self.bind_sync_request(
            operation_id,
            &serde_json::to_vec(&("overlay", &baseline.root, &resolved.root, guard))?,
        )?;
        if let Some(receipt) = self.sync_receipt(operation_id)? {
            return Ok(receipt);
        }
        baseline.validate_reconciled_source(resolved)?;
        let old = baseline.spool_history()?;
        let new = resolved.spool_history()?;
        let conn = rusqlite::Connection::open(new.path())?;
        conn.execute(
            "ATTACH DATABASE ?1 AS previous",
            [old.path().to_string_lossy().as_ref()],
        )?;
        let mut query = conn.prepare("SELECT f.kind,f.key,f.payload FROM frames f LEFT JOIN previous.frames p ON p.kind=f.kind AND p.key=f.key WHERE p.key IS NULL ORDER BY f.change_id,f.kind,f.chunk_no")?;
        let mut rows = query.query([])?;
        let mut frames = Vec::new();
        let mut targets = BTreeMap::new();
        let mut pack_bytes = 0usize;
        while let Some(row) = rows.next()? {
            let frame = codec::Frame {
                kind: row.get(0)?,
                key: row.get(1)?,
                payload: row.get(2)?,
            };
            pack_bytes += frame.payload.len() + 256;
            frames.push(frame);
            if pack_bytes >= 8 * 1024 * 1024 {
                add_packs(&mut targets, std::mem::take(&mut frames))?;
                pack_bytes = 0;
            }
        }
        add_packs(&mut targets, frames)?;
        let mut references = Vec::new();
        for (path, expected) in &guard.paths {
            let from = resolved.safe_path(path)?;
            if hash_file(&from)? == *expected {
                continue;
            }
            let bytes = fs::read(&from)?;
            if path.starts_with("current/") {
                references.extend(
                    CurrentUnit::decode(&mut BufReader::new(bytes.as_slice()))?.references()?,
                );
            }
            targets.insert(path.clone(), bytes);
        }
        self.publish_sync_files(operation_id, targets, guard, references, fault, cancel)
    }

    fn bind_sync_request(&self, operation_id: &str, request: &[u8]) -> Result<()> {
        validate_id(operation_id)?;
        let dir = self.local.join("recovery").join(operation_id);
        fs::create_dir_all(&dir)?;
        let execution = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("execution.lock"))?;
        execution.lock_exclusive()?;
        let path = dir.join("sync-request");
        let digest = codec::digest(request);
        if path.exists() {
            let prior: String = self.load(&path)?;
            if prior != digest {
                return Err(Error::Corrupt(
                    "operation identity reused with different sync request".into(),
                ));
            }
        } else {
            if dir.join("manifest").exists() || dir.join("published").exists() {
                return Err(Error::Corrupt(
                    "operation identity belongs to another request".into(),
                ));
            }
            self.record(&path, &digest)?;
        }
        Ok(())
    }

    fn publish_sync_files(
        &self,
        operation_id: &str,
        files: BTreeMap<String, Vec<u8>>,
        guard: &OverlayGuard,
        references: Vec<VersionRef>,
        fault: Option<FaultPoint>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<PublishReceipt> {
        validate_id(operation_id)?;
        let dir = self.local.join("recovery").join(operation_id);
        fs::create_dir_all(&dir)?;
        let operation = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("execution.lock"))?;
        operation.lock_exclusive()?;
        {
            let _gate = self.lock()?;
            self.recover_locked()?;
            if dir.join("published").exists() {
                return self.load(&dir.join("published"));
            }
        }
        let request_digest = codec::digest(&serde_json::to_vec(&(guard, &files))?);
        let mut targets = Vec::new();
        for (path, bytes) in files {
            let image = format!("sync-image-{}", targets.len());
            install(&dir.join(&image), &bytes)?;
            targets.push(Target {
                expected: guard.paths.get(&path).cloned().unwrap_or(None),
                path,
                image,
                digest: codec::digest(&bytes),
                append: None,
            });
        }
        let _admission = self.admission_lock()?;
        let _gate = self.lock()?;
        self.recover_locked()?;
        if dir.join("published").exists() {
            return self.load(&dir.join("published"));
        }
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(Error::CancelledBeforeDecision);
        }
        self.check_overlay_guard_locked(guard)?;
        let prior = self.ensure_clean()?;
        let receipt = PublishReceipt {
            operation_id: operation_id.into(),
            generation: prior.checked_add(1).ok_or(Error::Capacity)?,
            references,
            durability: Durability::FileSyncedProcessCrashProtocolPowerLossUnverified,
        };
        let manifest = Manifest {
            imported: true,
            root: self.root.to_string_lossy().into(),
            operation_id: operation_id.into(),
            request_digest,
            prior_generation: prior,
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
                generation: prior,
                operation_id: Some(operation_id.into()),
                manifest_digest: Some(digest.clone()),
            },
        )?;
        hit(fault, FaultPoint::DirtyFence)?;
        self.check_cancel_before_decision(cancel, prior)?;
        self.record(&dir.join("decision"), &digest)?;
        hit(fault, FaultPoint::Decision)?;
        self.redo(&dir, &manifest, fault)?;
        Ok(receipt)
    }

    /// Bootstrap the reverse dependency registry from a frozen view, with CAS.
    /// Normal C6 redo maintains this registry after it is enabled.
    pub fn prepare_overlay_guard(
        &self,
        capture: &Capture,
        baseline: &Store,
        resolved: &Store,
    ) -> Result<OverlayGuard> {
        let mut paths = BTreeMap::new();
        let mut affected = BTreeSet::new();
        for (path, from) in canonical_files(&resolved.root)? {
            if !path.starts_with("current/") && !path.starts_with("catalog/") {
                continue;
            }
            let expected = capture.files.get(&path).cloned();
            if hash_file(&from)? != expected {
                if path.starts_with("current/") {
                    affected.insert(from.file_stem().unwrap().to_string_lossy().to_string());
                }
                if path.starts_with("catalog/projects/") {
                    affected.insert(format!(
                        "project:{}",
                        from.file_stem().unwrap().to_string_lossy()
                    ));
                }
                paths.insert(path, expected);
            }
        }
        // Freeze the known read closure as well as changed targets. A forward
        // reference may have supplied semantic evidence even if not rewritten;
        // reverse dependents were reviewed as unchanged in the whole solution.
        let mut edges = Vec::new();
        let mut unit_paths = BTreeMap::new();
        for view in [baseline, resolved] {
            view.visit_current(|unit| {
                unit_paths.insert(unit.entity_id.clone(), Self::relative(&unit));
                edges.extend(unit_edges(&unit));
                Ok(())
            })?;
        }
        loop {
            let before = affected.len();
            for (source, target) in &edges {
                if affected.contains(source) || affected.contains(target) {
                    affected.insert(source.clone());
                    affected.insert(target.clone());
                }
            }
            if before == affected.len() {
                break;
            }
        }
        for id in &affected {
            let path = if let Some(project) = id.strip_prefix("project:") {
                validate_id(project)?;
                Some(format!("catalog/projects/{}/{}.md", &project[..2], project))
            } else {
                unit_paths.get(id).cloned()
            };
            if let Some(path) = path {
                paths
                    .entry(path.clone())
                    .or_insert_with(|| capture.files.get(&path).cloned());
            }
        }
        if paths.len().max(affected.len()) > 256 {
            return Err(Error::InsufficientResource {
                resource: "sync_final_guard_read_set_count".into(),
                required: paths.len().max(affected.len()) as u64,
                available: 256,
            });
        }
        let observed_bytes = paths.iter().try_fold(0u64, |sum, (p, _)| {
            Ok::<_, Error>(
                sum.checked_add(fs::metadata(resolved.safe_path(p)?)?.len())
                    .ok_or(Error::Capacity)?,
            )
        })?;
        if observed_bytes > 16 * 1024 * 1024 {
            return Err(Error::InsufficientResource {
                resource: "sync_final_guard_read_set_bytes".into(),
                required: observed_bytes,
                available: 16 * 1024 * 1024,
            });
        }
        // Bootstrap only from the frozen active baseline, not candidate edges.
        let mut baseline_edges = Vec::new();
        baseline.visit_current(|unit| {
            baseline_edges.extend(unit_edges(&unit));
            Ok(())
        })?;
        let _gate = self.lock()?;
        self.recover_locked()?;
        let current = self.ensure_clean()?;
        let conn = journal::open(&self.local.join("journal.sqlite"))?;
        init_registry(&conn)?;
        let registered: Option<Vec<u8>> = conn
            .query_row(
                "SELECT sequence FROM sync_registry WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .ok();
        if registered.is_none() {
            if current != capture.position.sequence {
                return Err(Error::Stale("sync dependency bootstrap changed".into()));
            }
            let tx = conn.unchecked_transaction()?;
            for (source, target) in baseline_edges {
                tx.execute(
                    "INSERT OR IGNORE INTO sync_edges VALUES(?1,?2)",
                    rusqlite::params![source, target],
                )?;
            }
            tx.execute(
                "INSERT INTO sync_registry VALUES(1,?1)",
                [current.to_be_bytes().as_slice()],
            )?;
            tx.commit()?;
        }
        let mut predicates = BTreeMap::new();
        for id in affected {
            let sequence = predicate_stamp(&conn, &id)?;
            if sequence > capture.position.sequence {
                return Err(Error::Stale("sync dependency set changed".into()));
            }
            predicates.insert(id, sequence);
        }
        let guard = OverlayGuard {
            position: capture.position.clone(),
            paths,
            predicates,
        };
        self.check_overlay_guard_locked(&guard)?;
        Ok(guard)
    }
    fn check_overlay_guard_locked(&self, guard: &OverlayGuard) -> Result<()> {
        if self.load::<String>(&self.local.join("source-epoch"))? != guard.position.epoch
            || guard.position.sequence > self.ensure_clean()?
        {
            return Err(Error::CoverageLost);
        }
        for (path, expected) in &guard.paths {
            if hash_file(&self.safe_path(path)?)? != *expected {
                return Err(Error::Stale(path.clone()));
            }
        }
        if !guard.predicates.is_empty() {
            let conn = journal::open(&self.local.join("journal.sqlite"))?;
            init_registry(&conn)?;
            let covered: Vec<u8> = conn
                .query_row(
                    "SELECT sequence FROM sync_registry WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| Error::CoverageLost)?;
            if covered != self.ensure_clean()?.to_be_bytes() {
                return Err(Error::CoverageLost);
            }
            for (target, expected) in &guard.predicates {
                if predicate_stamp(&conn, target)? != *expected {
                    return Err(Error::Stale(format!(
                        "reviewed dependency set changed: {target}"
                    )));
                }
            }
        }
        Ok(())
    }
}
fn add_packs(files: &mut BTreeMap<String, Vec<u8>>, frames: Vec<codec::Frame>) -> Result<()> {
    if frames.is_empty() {
        return Ok(());
    }
    let mut pack = Vec::new();
    let mut size = 0;
    for frame in frames {
        if size + frame.payload.len() + 256 > 16 * 1024 * 1024 && !pack.is_empty() {
            let mut bytes = Vec::new();
            codec::write("history", &pack, &mut bytes)?;
            files.insert(
                format!(
                    "history/{}/{}.md",
                    uuid::Uuid::new_v4(),
                    uuid::Uuid::new_v4()
                ),
                bytes,
            );
            pack.clear();
            size = 0;
        }
        size += frame.payload.len() + 256;
        pack.push(frame);
    }
    let mut bytes = Vec::new();
    codec::write("history", &pack, &mut bytes)?;
    files.insert(
        format!(
            "history/{}/{}.md",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ),
        bytes,
    );
    Ok(())
}
fn init_registry(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS sync_edges(source TEXT NOT NULL,target TEXT NOT NULL,PRIMARY KEY(source,target)); CREATE INDEX IF NOT EXISTS sync_reverse ON sync_edges(target); CREATE TABLE IF NOT EXISTS sync_predicates(target TEXT PRIMARY KEY,sequence BLOB NOT NULL); CREATE TABLE IF NOT EXISTS sync_registry(singleton INTEGER PRIMARY KEY CHECK(singleton=1),sequence BLOB NOT NULL);")?;
    Ok(())
}
fn predicate_stamp(conn: &rusqlite::Connection, id: &str) -> Result<u64> {
    use rusqlite::OptionalExtension;
    let raw: Option<Vec<u8>> = conn
        .query_row(
            "SELECT sequence FROM sync_predicates WHERE target=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|v| {
        v.try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| Error::Corrupt("sync predicate stamp".into()))
    })
    .transpose()
    .map(|v| v.unwrap_or(0))
}
fn unit_edges(unit: &CurrentUnit) -> Vec<(String, String)> {
    let state = json!({"redirect_to":match &unit.state{UnitState::Redirect{redirect_to,..}=>Some(redirect_to),_=>None},"current_heads":unit.heads().iter().map(|h|&h.metadata).collect::<Vec<_>>()});
    state_edges(&unit.entity_id, &state)
}
fn state_edges(entity_id: &str, state: &Value) -> Vec<(String, String)> {
    let mut targets = BTreeSet::new();
    if let Some(target) = state["redirect_to"].as_str() {
        targets.insert(target.into());
    }
    for head in state["current_heads"].as_array().into_iter().flatten() {
        for relation in head["relations"].as_array().into_iter().flatten() {
            if matches!(relation["kind"].as_str(), Some("required" | "related")) {
                if let Some(id) = relation["target_memory_id"].as_str() {
                    targets.insert(id.into());
                }
            }
        }
        if let Some(project) = head["applicability"]["project_id"].as_str() {
            targets.insert(format!("project:{project}"));
        }
        for target in head["work_targets"].as_array().into_iter().flatten() {
            if let Some(project) = target["project_id"].as_str() {
                targets.insert(format!("project:{project}"));
            }
        }
        for id in head["evidence_memory_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            targets.insert(id.into());
        }
    }
    targets
        .into_iter()
        .map(|target| (entity_id.to_owned(), target))
        .collect()
}
pub(super) fn update_registry(
    store: &Store,
    conn: &rusqlite::Connection,
    manifest: &Manifest,
) -> Result<()> {
    use rusqlite::OptionalExtension;
    init_registry(conn)?;
    let enabled: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sync_registry)", [], |r| {
        r.get(0)
    })?;
    if !enabled {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for target in &manifest.targets {
        if !target.path.starts_with("current/") {
            continue;
        }
        let path = store.safe_path(&target.path)?;
        let state = codec::state_prefix(&mut BufReader::new(File::open(path)?))?;
        let entity_id = state[if state["entity_type"] == "memory" {
            "memory_id"
        } else {
            "procedure_id"
        }]
        .as_str()
        .ok_or_else(|| Error::Corrupt("dependency source identity".into()))?;
        let mut query = tx.prepare("SELECT target FROM sync_edges WHERE source=?1")?;
        let old = query
            .query_map([entity_id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        drop(query);
        let new = state_edges(entity_id, &state);
        for id in old
            .into_iter()
            .chain(new.iter().map(|(_, target)| target.clone()))
        {
            let prior: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT sequence FROM sync_predicates WHERE target=?1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()?;
            let seq = manifest.receipt.generation.to_be_bytes();
            if prior.is_none_or(|p| p.as_slice() < seq.as_slice()) {
                tx.execute(
                    "INSERT OR REPLACE INTO sync_predicates VALUES(?1,?2)",
                    rusqlite::params![id, seq.as_slice()],
                )?;
            }
        }
        tx.execute("DELETE FROM sync_edges WHERE source=?1", [entity_id])?;
        for (source, target) in new {
            tx.execute(
                "INSERT OR IGNORE INTO sync_edges VALUES(?1,?2)",
                rusqlite::params![source, target],
            )?;
        }
    }
    tx.execute(
        "UPDATE sync_registry SET sequence=?1 WHERE singleton=1",
        [manifest.receipt.generation.to_be_bytes().as_slice()],
    )?;
    tx.commit()?;
    Ok(())
}
#[cfg(test)]
mod tests;
