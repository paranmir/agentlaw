//! Explicit structural choices apply only to retained isolated review workspaces.
use super::*;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportSide {
    Local,
    Incoming,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportConflict {
    pub conflict_id: String,
    pub kind: String,
    pub canonical_path: String,
    pub local_file: PathBuf,
    pub incoming_file: PathBuf,
    pub local_digest: String,
    pub incoming_digest: String,
    pub local_state: Value,
    pub incoming_state: Value,
    pub local_references: Vec<VersionRef>,
    pub incoming_references: Vec<VersionRef>,
    pub selected: Option<ImportSide>,
}
pub(super) fn capture(
    dir: &Path,
    kind: &str,
    relative: &str,
    local: &Path,
    incoming: &Path,
) -> Result<ImportConflict> {
    let id = uuid::Uuid::new_v4().to_string();
    let owned = dir.join(&id);
    fs::create_dir_all(&owned)?;
    let left = owned.join("local.md");
    let right = owned.join("incoming.md");
    install_reader(&left, &mut File::open(local)?)?;
    install_reader(&right, &mut File::open(incoming)?)?;
    let metadata = |path: &Path| -> Result<Value> {
        let mut state = None;
        codec::scan(
            if kind == "catalog" {
                "catalog"
            } else {
                "current"
            },
            &mut BufReader::new(File::open(path)?),
            |info, payload| {
                if info.kind == "state" || info.kind == "project" {
                    let mut bytes = Vec::new();
                    payload.read_to_end(&mut bytes)?;
                    state = Some(codec::parse_json(&bytes)?);
                }
                Ok(())
            },
        )?;
        state.ok_or_else(|| Error::Corrupt("conflict state missing".into()))
    };
    Ok(ImportConflict {
        conflict_id: id,
        kind: kind.into(),
        canonical_path: relative.into(),
        local_digest: hash_file(&left)?.unwrap(),
        incoming_digest: hash_file(&right)?.unwrap(),
        local_state: metadata(&left)?,
        incoming_state: metadata(&right)?,
        local_file: left,
        incoming_file: right,
        local_references: vec![],
        incoming_references: vec![],
        selected: None,
    })
}
impl Store {
    /// A failed pre-decision choice may be corrected as a new user intent. A
    /// decided operation must retain its execution identity through metadata redo.
    pub fn import_choice_decided(&self, operation_id: &str) -> Result<bool> {
        validate_id(operation_id)?;
        let _gate = self.lock()?;
        Ok(self
            .local
            .join("recovery")
            .join(operation_id)
            .join("decision")
            .exists())
    }
    pub fn import_conflicts(&self) -> Result<Vec<ImportConflict>> {
        let _gate = self.lock()?;
        self.ensure_clean()?;
        let path = self.local.join("import-conflicts.json");
        if !path.exists() {
            return Ok(vec![]);
        }
        self.load(&path)
    }
    pub fn choose_import_conflicts(
        &self,
        operation_id: &str,
        choices: &BTreeMap<String, ImportSide>,
        user_confirmed: bool,
    ) -> Result<PublishReceipt> {
        if !user_confirmed {
            return Err(Error::Corrupt(
                "explicit user confirmation required for structural choice".into(),
            ));
        }
        validate_id(operation_id)?;
        if !self.local.join("import-review").exists() {
            return Err(Error::Corrupt(
                "structural choices require isolated import review workspace".into(),
            ));
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
        let _gate = self.lock()?;
        self.recover_locked()?;
        let prior = self.ensure_clean()?;
        let request_digest = codec::digest(&serde_json::to_vec(choices)?);
        let mut conflicts: Vec<ImportConflict> =
            self.load(&self.local.join("import-conflicts.json"))?;
        for id in choices.keys() {
            if !conflicts.iter().any(|c| &c.conflict_id == id) {
                return Err(Error::NotFound("import conflict ID".into()));
            }
        }
        if choices.is_empty() {
            return Err(Error::Corrupt(
                "no explicit structural choices supplied".into(),
            ));
        }
        if dir.join("manifest").exists() {
            let m: Manifest = self.load(&dir.join("manifest"))?;
            if m.request_digest != request_digest {
                return Err(Error::Corrupt("structural choice operation reused".into()));
            }
            if dir.join("published").exists() {
                for c in &mut conflicts {
                    if let Some(side) = choices.get(&c.conflict_id) {
                        c.selected = Some(side.clone());
                    }
                }
                self.record(&self.local.join("import-conflicts.json"), &conflicts)?;
                return Ok(m.receipt);
            }
            if m.prior_generation != prior {
                return Err(Error::Stale(
                    "prepared structural choice source changed".into(),
                ));
            }
        }
        let mut proposed = BTreeMap::new();
        for c in &conflicts {
            if let Some(side) = choices.get(&c.conflict_id) {
                if c.kind == "redirect" {
                    let state = if *side == ImportSide::Local {
                        &c.local_state
                    } else {
                        &c.incoming_state
                    };
                    let id = state["memory_id"]
                        .as_str()
                        .ok_or_else(|| Error::Corrupt("conflict memory identity".into()))?;
                    proposed.insert(id.to_owned(), state.clone());
                }
            }
        }
        // A selected structural view must itself be inspectable: no cycles/dangling destinations.
        for id in proposed.keys() {
            let mut next = id.clone();
            let mut visited = BTreeSet::new();
            loop {
                if !visited.insert(next.clone()) {
                    return Err(Error::Corrupt(
                        "selected redirect choices create a cycle; choose another side".into(),
                    ));
                }
                let state = if let Some(state) = proposed.get(&next) {
                    state.clone()
                } else {
                    self.acquire_kind_locked(
                        &next,
                        "memory",
                        published::SourcePosition {
                            epoch: self.load(&self.local.join("source-epoch"))?,
                            sequence: prior,
                        },
                    )?
                    .state
                };
                if state["state"] == "live" {
                    break;
                }
                next = state["redirect_to"]
                    .as_str()
                    .ok_or_else(|| Error::Corrupt("selected redirect destination".into()))?
                    .to_owned();
            }
        }
        let mut targets = Vec::new();
        let mut references = Vec::new();
        let mut total = 0u64;
        for c in &conflicts {
            if let Some(side) = choices.get(&c.conflict_id) {
                let path = if *side == ImportSide::Local {
                    &c.local_file
                } else {
                    &c.incoming_file
                };
                total = total
                    .checked_add(path.metadata()?.len())
                    .ok_or(Error::Capacity)?;
                let current = hash_file(&self.safe_path(&c.canonical_path)?)?;
                let expected = if c.selected == Some(ImportSide::Incoming) {
                    &c.incoming_digest
                } else {
                    &c.local_digest
                };
                if current.as_deref() != Some(expected) {
                    return Err(Error::Stale("staged conflict record was edited after preparation/selection; preserve that reconciliation and prepare a fresh review rather than overwriting it with a side choice".into()));
                }
            }
        }
        self.admit_stream(operation_id, total, choices.len() as u64)?;
        for c in &conflicts {
            if let Some(side) = choices.get(&c.conflict_id) {
                let (path, digest, refs) = if *side == ImportSide::Local {
                    (&c.local_file, &c.local_digest, &c.local_references)
                } else {
                    (&c.incoming_file, &c.incoming_digest, &c.incoming_references)
                };
                if hash_file(path)?.as_deref() != Some(digest) {
                    return Err(Error::Corrupt(
                        "retained structural candidate changed".into(),
                    ));
                }
                let image = format!("image-{}", targets.len());
                install_reader(&dir.join(&image), &mut File::open(path)?)?;
                targets.push(Target {
                    path: c.canonical_path.clone(),
                    expected: hash_file(&self.safe_path(&c.canonical_path)?)?,
                    image,
                    digest: digest.clone(),
                    append: None,
                });
                references.extend(refs.clone());
            }
        }
        let receipt = PublishReceipt {
            operation_id: operation_id.into(),
            generation: prior.checked_add(1).ok_or(Error::Capacity)?,
            references,
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
        self.record(&dir.join("decision"), &digest)?;
        self.redo(&dir, &manifest, None)?;
        for c in &mut conflicts {
            if let Some(side) = choices.get(&c.conflict_id) {
                c.selected = Some(side.clone());
            }
        }
        self.record(&self.local.join("import-conflicts.json"), &conflicts)?;
        Ok(receipt)
    }
}
