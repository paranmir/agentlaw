//! C6 publication responsibilities; shares the facade's source gate and persistence protocol.
use super::*;

impl Store {
    pub fn publish_with_resource_limits(
        &self,
        operation_id: &str,
        mutations: Vec<Mutation>,
        limits: &resource::ResourceLimits,
    ) -> Result<PublishReceipt> {
        self.publish_checked(
            operation_id,
            mutations,
            None,
            None,
            None,
            None,
            Some(limits),
        )
    }
    pub fn publish(&self, operation_id: &str, mutations: Vec<Mutation>) -> Result<PublishReceipt> {
        self.publish_with_fault(operation_id, mutations, None)
    }
    pub fn publish_if_generation(
        &self,
        operation_id: &str,
        mutations: Vec<Mutation>,
        expected_generation: u64,
    ) -> Result<PublishReceipt> {
        self.publish_checked(
            operation_id,
            mutations,
            None,
            Some(expected_generation),
            None,
            None,
            None,
        )
    }
    pub fn publish_with_fault(
        &self,
        operation_id: &str,
        mutations: Vec<Mutation>,
        fault: Option<FaultPoint>,
    ) -> Result<PublishReceipt> {
        self.publish_checked(operation_id, mutations, fault, None, None, None, None)
    }
    pub fn publish_if_generation_control(
        &self,
        operation_id: &str,
        mutations: Vec<Mutation>,
        expected_generation: u64,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<PublishReceipt> {
        self.publish_checked(
            operation_id,
            mutations,
            None,
            Some(expected_generation),
            Some(cancel),
            None,
            None,
        )
    }
    pub(super) fn publish_checked(
        &self,
        operation_id: &str,
        mutations: Vec<Mutation>,
        fault: Option<FaultPoint>,
        expected_generation: Option<u64>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        read_set: Option<&ReadSet>,
        limits: Option<&resource::ResourceLimits>,
    ) -> Result<PublishReceipt> {
        validate_id(operation_id)?;
        let operation_dir = self.local.join("recovery").join(operation_id);
        fs::create_dir_all(&operation_dir)?;
        let _operation = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(operation_dir.join("execution.lock"))?;
        _operation.lock_exclusive()?;
        let _admission = self.admission_lock()?;
        let _l = self.lock()?;
        self.recover_locked()?;
        let mut prior = self.ensure_clean()?;
        let mut request_hash = Sha256::new();
        serde_json::to_writer(&mut request_hash, &mutations)?;
        let request_digest = format!("{:x}", request_hash.finalize());
        let dir = self.local.join("recovery").join(operation_id);
        if dir.join("manifest").exists() {
            let m: Manifest = self.load(&dir.join("manifest"))?;
            if m.request_digest != request_digest {
                return Err(Error::Corrupt(
                    "operation identity reused with different request".into(),
                ));
            }
            if dir.join("published").exists() {
                return Ok(m.receipt);
            }
            if let Some(read_set) = read_set {
                self.validate_read_set_locked(read_set)?;
            }
            if prior != m.prior_generation || expected_generation.is_some_and(|g| g != prior) {
                return Err(Error::Stale(
                    "prepared execution source basis changed; revalidation required".into(),
                ));
            }
            for t in &m.targets {
                if hash_file(&self.safe_path(&t.path)?)? != t.expected {
                    return Err(Error::Stale(t.path.clone()));
                }
            }
            let md = codec::digest(&fs::read(dir.join("manifest"))?);
            self.check_decision_capacity(&dir, &m.targets, limits)?;
            self.record(
                &self.local.join("source-fence"),
                &Fence {
                    generation: prior,
                    operation_id: Some(operation_id.into()),
                    manifest_digest: Some(md.clone()),
                },
            )?;
            self.check_cancel_before_decision(cancel, prior)?;
            self.record(&dir.join("decision"), &md)?;
            self.redo(&dir, &m, fault)?;
            return Ok(m.receipt);
        }
        if expected_generation.is_some_and(|g| g != prior) {
            return Err(Error::Stale(
                "source generation changed; dependency predicates must be re-evaluated".into(),
            ));
        }
        if mutations.is_empty() {
            return Err(Error::Corrupt("empty batch".into()));
        }
        if let Some(read_set) = read_set {
            self.validate_read_set_locked(read_set)?;
        }
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(Error::CancelledBeforeDecision);
        }
        self.admit_mutations(operation_id, &mutations, limits)?;
        let proposed_ids = mutations
            .iter()
            .flat_map(|m| m.unit.heads())
            .map(|h| change_id(h).map(str::to_owned))
            .collect::<Result<BTreeSet<_>>>()?;
        journal::reject_existing_changes(self, &proposed_ids)?;
        fs::create_dir_all(&dir)?;
        let mut targets = Vec::new();
        let mut refs = Vec::new();
        let mut ids = BTreeSet::new();
        let mut changes = BTreeSet::new();
        let mut history = Vec::new();
        let mut old_units = BTreeMap::new();
        for mutation in &mutations {
            match self.read_kind(
                &mutation.unit.entity_id,
                if mutation.unit.entity_type == "memory" {
                    "memory"
                } else {
                    "procedure"
                },
            ) {
                Ok(u) => {
                    old_units.insert(u.entity_id.clone(), u);
                }
                Err(Error::Capacity) => {
                    let position = published::SourcePosition {
                        epoch: self.load(&self.local.join("source-epoch"))?,
                        sequence: prior,
                    };
                    let kind = if mutation.unit.entity_type == "memory" {
                        "memory"
                    } else {
                        "procedure"
                    };
                    let old = self
                        .acquire_kind_locked(&mutation.unit.entity_id, kind, position)?
                        .into_owned_admitted()?;
                    old_units.insert(old.entity_id.clone(), old);
                }
                Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        for mutation in &mutations {
            if let UnitState::Redirect {
                redirect_to,
                consolidation_change_id,
            } = &mutation.unit.state
            {
                if mutation.unit.entity_type != "memory"
                    || !old_units.contains_key(&mutation.unit.entity_id)
                {
                    return Err(Error::Corrupt("redirect requires existing memory".into()));
                }
                let dest = mutations
                    .iter()
                    .find(|m| m.unit.entity_id == *redirect_to)
                    .ok_or_else(|| {
                        Error::Corrupt("redirect destination must publish in same batch".into())
                    })?;
                if !old_units.contains_key(redirect_to)
                    || dest.unit.heads().len() != 1
                    || change_id(&dest.unit.heads()[0])? != consolidation_change_id
                    || redirect_to >= &mutation.unit.entity_id
                {
                    return Err(Error::Corrupt(
                        "consolidation must use existing minimum identity and one result head"
                            .into(),
                    ));
                }
            }
        }
        let expected_hashes = mutations
            .iter()
            .map(|m| {
                Ok((
                    m.unit.entity_id.clone(),
                    hash_file(&self.root.join(Self::relative(&m.unit)))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        // Bodies and metadata are now owned. Expensive encoding/recovery image I/O does not block readers.
        drop(_l);
        for mutation in &mutations {
            let unit = &mutation.unit;
            unit.validate()?;
            if !ids.insert(&unit.entity_id) {
                return Err(Error::Corrupt("duplicate batch target".into()));
            }
            let old = old_units.get(&unit.entity_id).cloned();
            let mut actual = old
                .as_ref()
                .map(|u| u.references())
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.observed_version)
                .collect::<Vec<_>>();
            actual.sort();
            let mut expected = mutation.expected_versions.clone();
            expected.sort();
            if actual != expected {
                return Err(Error::Stale(unit.entity_id.clone()));
            }
            let mut parent_units = Vec::new();
            if let Some(o) = old_units.get(&unit.entity_id) {
                parent_units.push(o);
            }
            for source in &mutations {
                if matches!(&source.unit.state,UnitState::Redirect{redirect_to,..} if redirect_to==&unit.entity_id)
                {
                    parent_units.push(
                        old_units
                            .get(&source.unit.entity_id)
                            .ok_or_else(|| Error::Corrupt("missing consolidation source".into()))?,
                    );
                }
            }
            for head in unit.heads() {
                if !changes.insert(change_id(head)?.to_owned()) {
                    return Err(Error::Corrupt("duplicate new change".into()));
                }
                if old.as_ref().is_some_and(|o| {
                    o.heads()
                        .iter()
                        .any(|h| change_id(h).ok() == change_id(head).ok())
                }) {
                    return Err(Error::Corrupt("immutable change reused".into()));
                }
                history.extend(history_frames(
                    unit,
                    head,
                    &parent_units,
                    &mutation.evidence,
                )?);
            }
            refs.extend(unit.references()?);
            self.prepare_target(&dir, &mut targets, Self::relative(unit), unit.encode()?)?;
            targets.last_mut().unwrap().expected = expected_hashes[&unit.entity_id].clone();
        }
        let _l = self.lock()?;
        self.recover_locked()?;
        if let Some(read_set) = read_set {
            self.validate_read_set_locked(read_set)?;
            prior = self.ensure_clean()?;
            journal::reject_existing_changes(self, &proposed_ids)?;
        } else if self.ensure_clean()? != prior {
            return Err(Error::Stale(
                "source changed during preparation; revalidate dependency predicates".into(),
            ));
        }
        for t in &targets {
            if hash_file(&self.safe_path(&t.path)?)? != t.expected {
                return Err(Error::Stale(t.path.clone()));
            }
        }
        // Reserve only the old trailer suffix. Existing immutable history bytes are never rewritten.
        let writer_id = self.writer_id()?;
        let history_dir = self.root.join("history").join(&writer_id);
        let mut pack_path = None;
        let mut frames = Vec::new();
        if history_dir.exists() {
            let mut paths = fs::read_dir(&history_dir)?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            paths.sort();
            if let Some(p) = paths.last() {
                frames = codec::read("history", &mut BufReader::new(File::open(p)?))?;
                pack_path = Some(
                    p.strip_prefix(&self.root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        for frame in history {
            frames.push(frame);
            let mut bytes = Vec::new();
            codec::write("history", &frames, &mut bytes)?;
            if bytes.len() > 16 * 1024 * 1024 {
                let last = frames.pop().unwrap();
                let mut full = Vec::new();
                codec::write("history", &frames, &mut full)?;
                let path = pack_path
                    .take()
                    .unwrap_or_else(|| format!("history/{writer_id}/{}.md", uuid::Uuid::new_v4()));
                self.prepare_target(&dir, &mut targets, path, full)?;
                frames = vec![last];
            }
        }
        if !frames.is_empty() {
            let mut bytes = Vec::new();
            codec::write("history", &frames, &mut bytes)?;
            self.prepare_target(
                &dir,
                &mut targets,
                pack_path
                    .unwrap_or_else(|| format!("history/{writer_id}/{}.md", uuid::Uuid::new_v4())),
                bytes,
            )?;
        }
        let receipt = PublishReceipt {
            operation_id: operation_id.into(),
            generation: prior.checked_add(1).ok_or(Error::Capacity)?,
            references: refs,
            durability: Durability::FileSyncedProcessCrashProtocolPowerLossUnverified,
        };
        let manifest = Manifest {
            imported: false,
            root: self.root.to_string_lossy().into(),
            operation_id: operation_id.into(),
            request_digest,
            prior_generation: prior,
            receipt: receipt.clone(),
            targets,
        };
        self.record(&dir.join("manifest"), &manifest)?;
        hit(fault, FaultPoint::Prepared)?;
        self.check_decision_capacity(&dir, &manifest.targets, limits)?;
        let md = codec::digest(&fs::read(dir.join("manifest"))?);
        self.record(
            &self.local.join("source-fence"),
            &Fence {
                generation: prior,
                operation_id: Some(operation_id.into()),
                manifest_digest: Some(md.clone()),
            },
        )?;
        hit(fault, FaultPoint::DirtyFence)?;
        self.check_cancel_before_decision(cancel, prior)?;
        self.record(&dir.join("decision"), &md)?;
        hit(fault, FaultPoint::Decision)?;
        self.redo(&dir, &manifest, fault)?;
        drop(_l);
        self.automatic_maintenance();
        Ok(receipt)
    }
    fn check_cancel_before_decision(
        &self,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        prior: u64,
    ) -> Result<()> {
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)) {
            self.record(
                &self.local.join("source-fence"),
                &Fence {
                    generation: prior,
                    operation_id: None,
                    manifest_digest: None,
                },
            )?;
            return Err(Error::CancelledBeforeDecision);
        }
        Ok(())
    }
    pub(super) fn prepare_target(
        &self,
        dir: &Path,
        targets: &mut Vec<Target>,
        path: String,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let target = self.root.join(&path);
        let expected = hash_file(&target)?;
        let image = format!("image-{}", targets.len());
        let append = if path.starts_with("history/") && expected.is_some() {
            let old = fs::read(&target)?;
            let offset = old[..old.len().saturating_sub(1)]
                .iter()
                .rposition(|b| *b == b'\n')
                .map(|i| i + 1)
                .ok_or_else(|| Error::Corrupt("history trailer boundary".into()))?;
            if !old[offset..].starts_with(b"<!-- /agentlaw-file-v1 records=")
                || bytes.len() < offset
                || old[..offset] != bytes[..offset]
            {
                return Err(Error::Corrupt("history immutable prefix".into()));
            }
            Some(AppendPlan {
                offset: offset as u64,
                prefix_digest: codec::digest(&old[..offset]),
                old_trailer: old[offset..].to_vec(),
                target_length: bytes.len() as u64,
            })
        } else {
            None
        };
        install(&dir.join(&image), &bytes)?;
        targets.push(Target {
            path,
            expected,
            image,
            digest: codec::digest(&bytes),
            append,
        });
        Ok(())
    }
}
