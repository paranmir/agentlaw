//! Canonical per-unit Markdown and journal-independent, redo-only publication.
//! Readers and writers sharing a local root cooperate through an OS file lock.
//! Power-loss durability is deliberately NOT asserted by this implementation.
pub mod acquire;
pub mod codec;
mod current;
mod delta_stream;
pub mod domain;
mod history;
pub mod history_spool;
mod import;
pub mod import_conflict;
mod journal;
pub mod journal_repair;
pub mod maintenance;
pub mod persistence;
mod publication;
mod read_set;
pub use read_set::ReadSet;
pub mod published;
mod recovery;
pub mod resource;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("journal: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("corrupt/unsupported canonical data: {0}")]
    Corrupt(String),
    #[error("source recovery required: {0}")]
    RecoveryRequired(String),
    #[error("all current heads changed: {0}")]
    Stale(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bounded in-memory operation exceeded; spool adapter required")]
    Capacity,
    #[error("insufficient_resource: {resource} needs {required} bytes; {available} available")]
    InsufficientResource {
        resource: String,
        required: u64,
        available: u64,
    },
    #[error("resource capacity could not be measured: {0}")]
    ResourceUnknown(String),
    #[error("this canonical root is already pinned to another machine-local control directory; explicit recovery/rebinding is required")]
    LocalBindingMismatch,
    #[error("published-change coverage is unavailable or belongs to a different source epoch; rebuild is required")]
    CoverageLost,
    #[error("cancelled before canonical decision; source unchanged")]
    CancelledBeforeDecision,
    #[error("injected publication interruption at {0:?}")]
    Interrupted(FaultPoint),
}
pub fn validate_id(id: &str) -> Result<()> {
    let u = uuid::Uuid::parse_str(id).map_err(|_| Error::Corrupt("UUID".into()))?;
    if u.get_version_num() != 4 || u.get_variant() != uuid::Variant::RFC4122 || u.to_string() != id
    {
        return Err(Error::Corrupt("noncanonical UUIDv4".into()));
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Head {
    pub metadata: Value,
    pub body: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CurrentUnit {
    pub entity_id: String,
    pub entity_type: String,
    pub state: UnitState,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum UnitState {
    Live {
        heads: Vec<Head>,
    },
    Redirect {
        redirect_to: String,
        consolidation_change_id: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mutation {
    pub unit: CurrentUnit,
    pub expected_versions: Vec<String>,
    pub evidence: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionRef {
    pub memory_id: String,
    pub observed_version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedCurrent {
    pub requested_id: String,
    pub redirect_path: Vec<String>,
    pub current: CurrentUnit,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Durability {
    FileSyncedProcessCrashProtocolPowerLossUnverified,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishReceipt {
    pub operation_id: String,
    pub generation: u64,
    pub references: Vec<VersionRef>,
    pub durability: Durability,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    Prepared,
    DirtyFence,
    Decision,
    Installed(usize),
    Published,
    Journal,
    Clean,
}
impl CurrentUnit {
    pub fn heads(&self) -> &[Head] {
        match &self.state {
            UnitState::Live { heads } => heads,
            _ => &[],
        }
    }
    pub fn references(&self) -> Result<Vec<VersionRef>> {
        self.heads()
            .iter()
            .map(|h| {
                Ok(VersionRef {
                    memory_id: self.entity_id.clone(),
                    observed_version: version(self, h)?,
                })
            })
            .collect()
    }
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.entity_id)?;
        if !matches!(self.entity_type.as_str(), "memory" | "learned_procedure") {
            return Err(Error::Corrupt("entity type".into()));
        }
        let mut ids = BTreeSet::new();
        match &self.state {
            UnitState::Live { heads } => {
                domain::NonEmptyHeads::new(heads.clone())?;
                if heads.is_empty() {
                    return Err(Error::Corrupt("empty heads".into()));
                }
                for h in heads {
                    let id = change_id(h)?;
                    validate_id(id)?;
                    if !ids.insert(id) {
                        return Err(Error::Corrupt("duplicate head".into()));
                    }
                    for key in ["applicability", "origin"] {
                        if !h.metadata[key].is_object() {
                            return Err(Error::Corrupt(format!("missing {key}")));
                        }
                    }
                    if !h.metadata["recorded_at_ms"].is_i64() {
                        return Err(Error::Corrupt("timestamp".into()));
                    }
                    validate_id(
                        h.metadata["origin"]["machine_id"]
                            .as_str()
                            .ok_or_else(|| Error::Corrupt("origin machine identity".into()))?,
                    )?;
                    if self.entity_type == "memory" {
                        domain::validate_memory_metadata(&h.metadata)?;
                    }
                }
            }
            UnitState::Redirect {
                redirect_to,
                consolidation_change_id,
            } => {
                validate_id(redirect_to)?;
                validate_id(consolidation_change_id)?;
                if redirect_to == &self.entity_id {
                    return Err(Error::Corrupt("self redirect".into()));
                }
            }
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let id_key = if self.entity_type == "memory" {
            "memory_id"
        } else {
            "procedure_id"
        };
        let mut state = json!({"entity_type":self.entity_type,id_key:self.entity_id});
        let mut frames = Vec::new();
        match &self.state {
            UnitState::Live { heads } => {
                state["state"] = json!("live");
                state["current_heads"] =
                    json!(heads.iter().map(|h| &h.metadata).collect::<Vec<_>>());
                for h in heads {
                    frames.push(codec::Frame {
                        kind: if self.entity_type == "memory" {
                            "body"
                        } else {
                            "instructions"
                        }
                        .into(),
                        key: change_id(h)?.into(),
                        payload: h.body.as_bytes().to_vec(),
                    });
                }
            }
            UnitState::Redirect {
                redirect_to,
                consolidation_change_id,
            } => {
                state["state"] = json!("redirect");
                state["redirect_to"] = json!(redirect_to);
                state["consolidation_change_id"] = json!(consolidation_change_id);
            }
        }
        frames.insert(
            0,
            codec::Frame {
                kind: "state".into(),
                key: self.entity_id.clone(),
                payload: serde_json::to_vec(&state)?,
            },
        );
        let mut out = Vec::new();
        codec::write("current", &frames, &mut out)?;
        Ok(out)
    }
    pub fn decode(r: &mut impl std::io::BufRead) -> Result<Self> {
        let frames = codec::read("current", r)?;
        let states: Vec<_> = frames.iter().filter(|f| f.kind == "state").collect();
        if states.len() != 1 {
            return Err(Error::Corrupt("state cardinality".into()));
        }
        let v: Value = codec::parse_json(&states[0].payload)?;
        let entity_type = v["entity_type"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("entity type".into()))?
            .to_owned();
        let entity_id = v[if entity_type == "memory" {
            "memory_id"
        } else {
            "procedure_id"
        }]
        .as_str()
        .ok_or_else(|| Error::Corrupt("entity id".into()))?
        .to_owned();
        if states[0].key != entity_id {
            return Err(Error::Corrupt("state key".into()));
        }
        let state = match v["state"].as_str() {
            Some("live") => {
                if v.get("redirect_to").is_some() || v.get("consolidation_change_id").is_some() {
                    return Err(Error::Corrupt("live/redirect declarations mixed".into()));
                }
                let metadata = v["current_heads"]
                    .as_array()
                    .ok_or_else(|| Error::Corrupt("heads".into()))?;
                if frames.len() != metadata.len() + 1 {
                    return Err(Error::Corrupt("body cardinality".into()));
                }
                let mut heads = Vec::new();
                for m in metadata {
                    let id = m["change_id"]
                        .as_str()
                        .ok_or_else(|| Error::Corrupt("change id".into()))?;
                    let f = frames
                        .iter()
                        .find(|f| {
                            f.key == id
                                && f.kind
                                    == if entity_type == "memory" {
                                        "body"
                                    } else {
                                        "instructions"
                                    }
                        })
                        .ok_or_else(|| Error::Corrupt("missing body".into()))?;
                    heads.push(Head {
                        metadata: m.clone(),
                        body: String::from_utf8(f.payload.clone())
                            .map_err(|_| Error::Corrupt("body UTF8".into()))?,
                    });
                }
                UnitState::Live { heads }
            }
            Some("redirect") => {
                if v.get("current_heads").is_some() {
                    return Err(Error::Corrupt("redirect/live declarations mixed".into()));
                }
                if frames.len() != 1 {
                    return Err(Error::Corrupt("redirect body".into()));
                }
                UnitState::Redirect {
                    redirect_to: v["redirect_to"].as_str().unwrap_or("").into(),
                    consolidation_change_id: v["consolidation_change_id"]
                        .as_str()
                        .unwrap_or("")
                        .into(),
                }
            }
            _ => return Err(Error::Corrupt("state discriminator".into())),
        };
        let unit = Self {
            entity_id,
            entity_type,
            state,
        };
        unit.validate()?;
        Ok(unit)
    }
}
fn change_id(h: &Head) -> Result<&str> {
    h.metadata["change_id"]
        .as_str()
        .ok_or_else(|| Error::Corrupt("change id".into()))
}
pub fn version(unit: &CurrentUnit, head: &Head) -> Result<String> {
    version_stream(
        unit,
        head,
        head.body.len() as u64,
        &mut std::io::Cursor::new(head.body.as_bytes()),
    )
}
fn version_stream(
    unit: &CurrentUnit,
    head: &Head,
    body_bytes: u64,
    body: &mut impl Read,
) -> Result<String> {
    let mut metadata = head.metadata.clone();
    for key in ["relations", "work_targets", "evidence_memory_ids"] {
        if let Some(items) = metadata.get_mut(key).and_then(Value::as_array_mut) {
            let mut keyed = items
                .iter()
                .map(|v| Ok((codec::canonical_json(v)?, v.clone())))
                .collect::<Result<Vec<_>>>()?;
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            *items = keyed.into_iter().map(|(_, v)| v).collect();
        }
    }
    let meta = codec::canonical_json(
        &json!({"entity_id":unit.entity_id,"entity_type":unit.entity_type,"head":metadata}),
    )?;
    let mut hash = Sha256::new();
    for bytes in [b"agentlaw-state-v1".as_slice(), meta.as_slice()] {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    hash.update(body_bytes.to_be_bytes());
    let copied = std::io::copy(body, &mut hash)?;
    if copied != body_bytes {
        return Err(Error::Corrupt(
            "body length changed during version calculation".into(),
        ));
    }
    let mut bytes = uuid::Uuid::parse_str(change_id(head)?)
        .map_err(|_| Error::Corrupt("change UUID".into()))?
        .as_bytes()
        .to_vec();
    bytes.extend_from_slice(&hash.finalize());
    Ok(format!("av1.{}", URL_SAFE_NO_PAD.encode(bytes)))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Target {
    path: String,
    expected: Option<String>,
    image: String,
    digest: String,
    append: Option<AppendPlan>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppendPlan {
    offset: u64,
    prefix_digest: String,
    old_trailer: Vec<u8>,
    target_length: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    #[serde(default)]
    imported: bool,
    root: String,
    operation_id: String,
    request_digest: String,
    prior_generation: u64,
    receipt: PublishReceipt,
    targets: Vec<Target>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Fence {
    generation: u64,
    operation_id: Option<String>,
    manifest_digest: Option<String>,
}
pub struct Store {
    root: PathBuf,
    local: PathBuf,
    coordination: PathBuf,
}
fn local_coordination(local: &Path) -> PathBuf {
    local.parent().unwrap_or(local).join("source-coordination")
}
impl Store {
    /// Existing source/control state only: never bootstrap, migrate, recover, or publish.
    /// Diagnostic audits may create disposable local streaming spools.
    pub fn open_read_only(root: impl AsRef<Path>, local: impl AsRef<Path>) -> Result<Self> {
        let coordination = local_coordination(local.as_ref());
        Self::open_read_only_with_coordination(root, local, coordination)
    }
    pub fn open_read_only_with_coordination(
        root: impl AsRef<Path>,
        local: impl AsRef<Path>,
        coordination: impl AsRef<Path>,
    ) -> Result<Self> {
        let s = Self {
            root: fs::canonicalize(root)?,
            local: fs::canonicalize(local)?,
            coordination: coordination.as_ref().to_path_buf(),
        };
        s.check_local_binding(false)?;
        let _gate = s.lock()?;
        s.validate_format()?;
        s.ensure_clean()?;
        let epoch: String = s.load(&s.local.join("source-epoch"))?;
        validate_id(&epoch)?;
        Ok(s)
    }
    pub fn local_root(&self) -> &Path {
        &self.local
    }
    pub fn coordination_root(&self) -> &Path {
        &self.coordination
    }
    pub fn open(root: impl AsRef<Path>, local: impl AsRef<Path>) -> Result<Self> {
        let coordination = local_coordination(local.as_ref());
        Self::open_with_coordination(root, local, coordination)
    }
    pub fn open_with_coordination(
        root: impl AsRef<Path>,
        local: impl AsRef<Path>,
        coordination: impl AsRef<Path>,
    ) -> Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        fs::create_dir_all(local.as_ref())?;
        let s = Self {
            root: fs::canonicalize(root)?,
            local: fs::canonicalize(local)?,
            coordination: coordination.as_ref().to_path_buf(),
        };
        s.register_local_binding()?;
        let _lock = s.lock()?;
        if !s.local.join("source-fence").exists() {
            if s.root.join("current").exists() || s.root.join("history").exists() {
                return Err(Error::RecoveryRequired(
                    "existing source without fence; explicit validation/import needed".into(),
                ));
            }
            if !s.root.join("format.md").exists() {
                let mut bytes = Vec::new();
                codec::write(
                    "format",
                    &[codec::Frame {
                        kind: "format".into(),
                        key: "root".into(),
                        payload: br#"{"format_version":1}"#.to_vec(),
                    }],
                    &mut bytes,
                )?;
                install(&s.root.join("format.md"), &bytes)?;
            }
            if !s.root.join(".gitattributes").exists() {
                install(
                    &s.root.join(".gitattributes"),
                    b"/format.md -text\n/current/** -text\n/history/** -text\n/catalog/** -text\n",
                )?;
            }
            s.record(
                &s.local.join("source-fence"),
                &Fence {
                    generation: 0,
                    operation_id: None,
                    manifest_digest: None,
                },
            )?;
        }
        s.validate_format()?;
        s.ensure_epoch()?;
        s.resume_journal_repair_locked()?;
        // Current reads do not depend on a surviving ledger database. The
        // read-only port reports CoverageLost if the ledger cannot be opened.
        if s.local.join("journal.sqlite").exists() {
            let _ = journal::open(&s.local.join("journal.sqlite"));
        }
        s.recover_locked()?;
        drop(_lock);
        s.automatic_maintenance();
        Ok(s)
    }
    /// Explicitly attach portable source after scanning its framing and current state.
    /// This is not a clone-identity decision and does not transfer pending local state.
    pub fn attach_existing(root: impl AsRef<Path>, local: impl AsRef<Path>) -> Result<Self> {
        let coordination = local_coordination(local.as_ref());
        Self::attach_existing_with_coordination(root, local, coordination)
    }
    pub fn attach_existing_with_coordination(
        root: impl AsRef<Path>,
        local: impl AsRef<Path>,
        coordination: impl AsRef<Path>,
    ) -> Result<Self> {
        fs::create_dir_all(local.as_ref())?;
        let s = Self {
            root: fs::canonicalize(root)?,
            local: fs::canonicalize(local)?,
            coordination: coordination.as_ref().to_path_buf(),
        };
        s.register_local_binding()?;
        let gate = s.lock()?;
        s.validate_format()?;
        s.ensure_epoch()?;
        if s.local.join("source-fence").exists() && !s.local.join("validation-required").exists() {
            s.recover_locked()?;
            return Ok(s);
        }
        s.record(&s.local.join("validation-required"), &true)?;
        if !s.local.join("source-fence").exists() {
            s.record(
                &s.local.join("source-fence"),
                &Fence {
                    generation: 0,
                    operation_id: None,
                    manifest_digest: None,
                },
            )?;
        }
        drop(gate);
        let report = s.audit_source()?;
        let _gate = s.lock()?;
        if s.generation_for_audit()? != report.source_position.sequence {
            return Err(Error::Stale(
                "source changed during attachment audit".into(),
            ));
        }
        fs::remove_file(s.local.join("validation-required"))?;
        use persistence::Persistence;
        persistence::PlatformPersistence.sync_namespace(&s.local)?;
        Ok(s)
    }
    fn ensure_epoch(&self) -> Result<()> {
        let path = self.local.join("source-epoch");
        if path.exists() {
            let id: String = self.load(&path)?;
            validate_id(&id)
        } else {
            self.record(&path, &uuid::Uuid::new_v4().to_string())
        }
    }
    pub fn published_reader(&self) -> published::PublishedReader<'_> {
        published::PublishedReader { store: self }
    }
    pub fn owned_published_reader(&self) -> published::OwnedPublishedReader {
        published::OwnedPublishedReader {
            store: Store {
                root: self.root.clone(),
                local: self.local.clone(),
                coordination: self.coordination.clone(),
            },
        }
    }
    fn validate_format(&self) -> Result<()> {
        let file = File::open(self.root.join("format.md"))
            .map_err(|_| Error::Corrupt("not an Agentlaw source: format.md required".into()))?;
        let frames = codec::read("format", &mut BufReader::new(file))?;
        if frames.len() != 1
            || frames[0].kind != "format"
            || frames[0].key != "root"
            || codec::parse_json(&frames[0].payload)? != json!({"format_version":1})
        {
            return Err(Error::Corrupt("unsupported source format".into()));
        }
        Ok(())
    }
    fn register_local_binding(&self) -> Result<()> {
        self.check_local_binding(true)
    }
    fn check_local_binding(&self, create: bool) -> Result<()> {
        if !self.coordination.is_absolute() {
            return Err(Error::RecoveryRequired(
                "source coordination directory must be absolute".into(),
            ));
        }
        let identity = self.root.to_string_lossy().to_string();
        #[cfg(windows)]
        let identity = identity.to_lowercase();
        let source_key = codec::digest(identity.as_bytes());
        let dir = self.coordination.join(&source_key);
        if create {
            fs::create_dir_all(&dir)?;
        }
        let guard = OpenOptions::new()
            .create(create)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("binding.lock"))?;
        guard.lock_exclusive()?;
        let record = dir.join("binding.json");
        let binding = self.local.to_string_lossy().to_string();
        #[cfg(windows)]
        let binding = binding.to_lowercase();
        if record.exists() {
            let old: String = self.load(&record)?;
            if old != binding {
                return Err(Error::LocalBindingMismatch);
            }
        } else if create {
            self.record(&record, &binding)?;
        } else {
            return Err(Error::RecoveryRequired("local binding missing".into()));
        }
        Ok(())
    }
    pub fn publish_catalog(
        &self,
        operation_id: &str,
        project_id: &str,
        payload: Value,
    ) -> Result<PublishReceipt> {
        validate_id(operation_id)?;
        validate_id(project_id)?;
        if payload["project_id"] != project_id {
            return Err(Error::Corrupt("catalog identity".into()));
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
        let _l = self.lock()?;
        self.recover_locked()?;
        let prior = self.ensure_clean()?;
        let request_digest = codec::digest(&codec::canonical_json(&payload)?);
        let dir = self.local.join("recovery").join(operation_id);
        if dir.join("manifest").exists() {
            let m: Manifest = self.load(&dir.join("manifest"))?;
            if m.request_digest != request_digest {
                return Err(Error::Corrupt("catalog operation reused".into()));
            }
            if dir.join("published").exists() {
                return Ok(m.receipt);
            }
            if m.prior_generation != prior {
                return Err(Error::Stale("catalog prepared source changed".into()));
            }
            for target in &m.targets {
                if hash_file(&self.safe_path(&target.path)?)? != target.expected {
                    return Err(Error::Stale(target.path.clone()));
                }
            }
            let digest = codec::digest(&fs::read(dir.join("manifest"))?);
            self.check_decision_capacity(&dir, &m.targets, None)?;
            self.record(
                &self.local.join("source-fence"),
                &Fence {
                    generation: prior,
                    operation_id: Some(operation_id.into()),
                    manifest_digest: Some(digest.clone()),
                },
            )?;
            self.record(&dir.join("decision"), &digest)?;
            self.redo(&dir, &m, None)?;
            return Ok(m.receipt);
        }
        self.admit_bytes(operation_id, resource::serialized_bytes(&payload)?, None)?;
        let path = format!("catalog/projects/{}/{}.md", &project_id[..2], project_id);
        if self.root.join(&path).exists() {
            return Err(Error::Stale(
                "catalog exists; explicit update required".into(),
            ));
        }
        fs::create_dir_all(&dir)?;
        let mut bytes = Vec::new();
        codec::write(
            "catalog",
            &[codec::Frame {
                kind: "project".into(),
                key: project_id.into(),
                payload: serde_json::to_vec(&payload)?,
            }],
            &mut bytes,
        )?;
        let mut targets = Vec::new();
        self.prepare_target(&dir, &mut targets, path, bytes)?;
        let receipt = PublishReceipt {
            operation_id: operation_id.into(),
            generation: prior.checked_add(1).ok_or(Error::Capacity)?,
            references: vec![],
            durability: Durability::FileSyncedProcessCrashProtocolPowerLossUnverified,
        };
        let m = Manifest {
            imported: false,
            root: self.root.to_string_lossy().into(),
            operation_id: operation_id.into(),
            request_digest,
            prior_generation: prior,
            receipt: receipt.clone(),
            targets,
        };
        self.record(&dir.join("manifest"), &m)?;
        self.check_decision_capacity(&dir, &m.targets, None)?;
        let md = codec::digest(&fs::read(dir.join("manifest"))?);
        self.record(
            &self.local.join("source-fence"),
            &Fence {
                generation: prior,
                operation_id: Some(operation_id.into()),
                manifest_digest: Some(md.clone()),
            },
        )?;
        self.record(&dir.join("decision"), &md)?;
        self.redo(&dir, &m, None)?;
        Ok(receipt)
    }
    pub fn read_catalog(&self, project_id: &str) -> Result<Value> {
        validate_id(project_id)?;
        let _l = self.lock()?;
        self.ensure_clean()?;
        let p = self.safe_path(&format!(
            "catalog/projects/{}/{}.md",
            &project_id[..2],
            project_id
        ))?;
        let frames = codec::read("catalog", &mut BufReader::new(File::open(p)?))?;
        if frames.len() != 1 || frames[0].key != project_id {
            return Err(Error::Corrupt("catalog cardinality/identity".into()));
        }
        codec::parse_json(&frames[0].payload)
    }
    pub fn list_catalog(&self) -> Result<Vec<Value>> {
        let _l = self.lock()?;
        self.ensure_clean()?;
        let base = self.root.join("catalog/projects");
        let mut result = Vec::new();
        if !base.exists() {
            return Ok(result);
        }
        for shard in fs::read_dir(base)? {
            for entry in fs::read_dir(shard?.path())? {
                let entry = entry?;
                if entry.path().extension().is_some_and(|e| e == "tmp") {
                    continue;
                }
                let frames =
                    codec::read("catalog", &mut BufReader::new(File::open(entry.path())?))?;
                if frames.len() != 1 {
                    return Err(Error::Corrupt("catalog cardinality".into()));
                }
                let value = codec::parse_json(&frames[0].payload)?;
                if value["project_id"] != frames[0].key {
                    return Err(Error::Corrupt("catalog identity".into()));
                }
                result.push(value);
                if result.len() > 100_000 {
                    return Err(Error::Capacity);
                }
            }
        }
        Ok(result)
    }
    fn lock(&self) -> Result<File> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.local.join("source.lock"))?;
        f.lock_exclusive()?;
        Ok(f)
    }
    fn writer_id(&self) -> Result<String> {
        let path = self.local.join("writer-id");
        if path.exists() {
            let id: String = self.load(&path)?;
            validate_id(&id)?;
            Ok(id)
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            self.record(&path, &id)?;
            Ok(id)
        }
    }
    fn record(&self, path: &Path, value: &impl Serialize) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        let envelope =
            json!({"digest":codec::digest(&bytes),"payload":String::from_utf8(bytes).unwrap()});
        install(path, &serde_json::to_vec(&envelope)?)
    }
    fn load<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<T> {
        let envelope = codec::parse_json(&fs::read(path)?)?;
        let bytes = envelope["payload"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("record payload".into()))?
            .as_bytes();
        if envelope["digest"] != codec::digest(bytes) {
            return Err(Error::Corrupt("record digest".into()));
        }
        Ok(serde_json::from_value(codec::parse_json(bytes)?)?)
    }
    fn fence(&self) -> Result<Fence> {
        self.load(&self.local.join("source-fence"))
    }
    fn ensure_clean(&self) -> Result<u64> {
        if self.local.join("validation-required").exists() {
            return Err(Error::RecoveryRequired(
                "initial source integrity audit has not completed".into(),
            ));
        }
        self.generation_for_audit()
    }
    fn generation_for_audit(&self) -> Result<u64> {
        let f = self.fence()?;
        if f.operation_id.is_some() {
            return Err(Error::RecoveryRequired("unfinished publication".into()));
        }
        Ok(f.generation)
    }
    fn relative(unit: &CurrentUnit) -> String {
        format!(
            "current/{}/{}/{}.md",
            if unit.entity_type == "memory" {
                "memory"
            } else {
                "procedure"
            },
            &unit.entity_id[..2],
            unit.entity_id
        )
    }
    pub fn recover(&self) -> Result<()> {
        let _l = self.lock()?;
        self.recover_locked()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoricalState {
    pub metadata: Value,
    pub body: String,
    pub evidence: String,
    pub delta: Value,
}
fn history_frames(
    unit: &CurrentUnit,
    head: &Head,
    old: &[&CurrentUnit],
    evidence: &str,
) -> Result<Vec<codec::Frame>> {
    let id = change_id(head)?;
    let parents: Vec<_> = old
        .iter()
        .flat_map(|u| u.heads().iter().map(move |h| (*u, h)))
        .collect();
    let mut candidates = parents
        .iter()
        .map(|(_, p)| {
            Ok((
                serde_json::to_vec(&splice(&p.body, &head.body))?,
                change_id(p)?.to_owned(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    candidates.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.1.cmp(&b.1)));
    let delta = candidates
        .first()
        .map(|c| c.0.clone())
        .unwrap_or(serde_json::to_vec(&splice("", &head.body))?);
    let mut metadata = json!({"entity_id":unit.entity_id,"entity_type":unit.entity_type,"change_id":id,"operation_kind":if old.len()>1{"consolidate"}else if old.is_empty(){"create"}else{"evolve"},"parent_change_ids":parents.iter().map(|(_,h)|change_id(h)).collect::<Result<Vec<_>>>()?,"parent_owners":parents.iter().map(|(u,p)|json!({"change_id":change_id(p).unwrap(),"entity_id":u.entity_id})).collect::<Vec<_>>(),"metadata_after":head.metadata,"result_body_bytes":head.body.len().to_string(),"result_body_sha256":codec::digest(head.body.as_bytes()),"observed_version":version(unit,head)?});
    if parents.len() > 1 {
        metadata["delta_base_id"] = json!(candidates[0].1);
    }
    if old.len() > 1 {
        let mut ids = old.iter().map(|u| u.entity_id.clone()).collect::<Vec<_>>();
        ids.sort();
        metadata["source_identity_ids"] = json!(ids);
    }
    let mb = serde_json::to_vec(&metadata)?;
    let components = [
        ("metadata", "change_metadata_chunk", mb),
        ("delta", "delta_chunk", delta),
        ("evidence", "evidence_chunk", evidence.as_bytes().to_vec()),
    ];
    let mut desc = json!({"change_id":id,"entity_id":unit.entity_id});
    let mut frames = Vec::new();
    for (name, ty, bytes) in components {
        let chunks = chunks(&bytes)?;
        desc[name] = component(&bytes, chunks.len())?;
        for (i, payload) in chunks.into_iter().enumerate() {
            frames.push(codec::Frame {
                kind: ty.into(),
                key: format!("{id}.{i}"),
                payload,
            });
        }
    }
    frames.insert(
        0,
        codec::Frame {
            kind: "change_descriptor".into(),
            key: id.into(),
            payload: serde_json::to_vec(&desc)?,
        },
    );
    let cb = head.body.as_bytes();
    let cm = serde_json::to_vec(&head.metadata)?;
    let bc = chunks(cb)?;
    let mc = chunks(&cm)?;
    frames.push(codec::Frame{kind:"checkpoint_descriptor".into(),key:id.into(),payload:serde_json::to_vec(&json!({"change_id":id,"observed_version":version(unit,head)?,"metadata":component(&cm,mc.len())?,"body":component(cb,bc.len())?}))?});
    for (ty, parts) in [
        ("checkpoint_metadata_chunk", mc),
        ("checkpoint_body_chunk", bc),
    ] {
        for (i, payload) in parts.into_iter().enumerate() {
            frames.push(codec::Frame {
                kind: ty.into(),
                key: format!("{id}.{i}"),
                payload,
            });
        }
    }
    Ok(frames)
}
fn component(bytes: &[u8], count: usize) -> Result<Value> {
    Ok(
        json!({"bytes":bytes.len().to_string(),"sha256":codec::digest(bytes),"chunks":u32::try_from(count).map_err(|_|Error::Capacity)?}),
    )
}
fn chunks(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Corrupt("chunk UTF8".into()))?;
    if bytes.is_empty() {
        return Ok(vec![vec![]]);
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = (start + 1024 * 1024).min(bytes.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(bytes[start..end].to_vec());
        start = end;
    }
    Ok(chunks)
}
/// Deterministic single-splice encoder: maximal unchanged UTF-8 prefix/suffix.
fn splice(base: &str, result: &str) -> Value {
    if base == result {
        return json!({"codec":"utf8-splice-v1","edits":[]});
    }
    let mut prefix = base
        .bytes()
        .zip(result.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !base.is_char_boundary(prefix) || !result.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = base.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(result.as_bytes()[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !base.is_char_boundary(base.len() - suffix)
        || !result.is_char_boundary(result.len() - suffix)
    {
        suffix -= 1;
    }
    json!({"codec":"utf8-splice-v1","edits":[{"start_byte":prefix.to_string(),"delete_bytes":(base.len()-prefix-suffix).to_string(),"insert_utf8":&result[prefix..result.len()-suffix]}]})
}
pub fn apply_splice(base: &str, delta: &Value) -> Result<String> {
    if delta["codec"] != "utf8-splice-v1" {
        return Err(Error::Corrupt("delta codec".into()));
    }
    let edits = delta["edits"]
        .as_array()
        .ok_or_else(|| Error::Corrupt("delta edits".into()))?;
    let mut out = String::new();
    let mut cursor = 0usize;
    let mut previous = None;
    for edit in edits {
        let parse = |key: &str| -> Result<usize> {
            let s = edit[key]
                .as_str()
                .ok_or_else(|| Error::Corrupt("delta offset".into()))?;
            if s.len() > 1 && s.starts_with('0') {
                return Err(Error::Corrupt("noncanonical offset".into()));
            }
            let n: u64 = s
                .parse()
                .map_err(|_| Error::Corrupt("offset overflow".into()))?;
            usize::try_from(n).map_err(|_| Error::Capacity)
        };
        let start = parse("start_byte")?;
        let end = start
            .checked_add(parse("delete_bytes")?)
            .ok_or(Error::Capacity)?;
        if start < cursor
            || previous == Some(start)
            || end > base.len()
            || !base.is_char_boundary(start)
            || !base.is_char_boundary(end)
        {
            return Err(Error::Corrupt(
                "overlapping/out-of-range/non-UTF8 splice".into(),
            ));
        }
        out.push_str(&base[cursor..start]);
        out.push_str(
            edit["insert_utf8"]
                .as_str()
                .ok_or_else(|| Error::Corrupt("splice insertion".into()))?,
        );
        cursor = end;
        previous = Some(start);
    }
    out.push_str(&base[cursor..]);
    Ok(out)
}
fn hit(f: Option<FaultPoint>, at: FaultPoint) -> Result<()> {
    if f == Some(at) {
        Err(Error::Interrupted(at))
    } else {
        Ok(())
    }
}
fn hash_file(p: &Path) -> Result<Option<String>> {
    match File::open(p) {
        Ok(mut f) => {
            let mut h = Sha256::new();
            std::io::copy(&mut f, &mut h)?;
            Ok(Some(format!("{:x}", h.finalize())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn install(path: &Path, bytes: &[u8]) -> Result<()> {
    install_reader(path, &mut std::io::Cursor::new(bytes))
}
fn install_reader(path: &Path, input: &mut impl Read) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Corrupt("parent".into()))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".agentlaw-{}.tmp", uuid::Uuid::new_v4()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    std::io::copy(input, &mut f)?;
    f.sync_all()?;
    drop(f);
    // On Windows Rust rename replaces existing regular files, using MoveFileExW.
    use persistence::Persistence;
    persistence::PlatformPersistence.install_replace(&temp, path)?;
    persistence::PlatformPersistence.sync_namespace(parent)?;
    Ok(())
}
