//! Owned, immutable body spools for current units larger than the in-memory API.
use super::*;
#[derive(Debug)]
struct AcquisitionLease(PathBuf);
impl Drop for AcquisitionLease {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|s| s == "payload") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpoolBody {
    #[serde(skip)]
    _lease: Option<std::sync::Arc<AcquisitionLease>>,
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}
impl SpoolBody {
    pub fn open(&self) -> Result<File> {
        Ok(File::open(&self.path)?)
    }
    pub fn copy_to(&self, out: &mut impl Write) -> Result<u64> {
        let mut reader = self.open()?;
        let mut copied = 0u64;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            out.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
            copied = copied.checked_add(n as u64).ok_or(Error::Capacity)?;
        }
        if copied != self.bytes || format!("{:x}", hash.finalize()) != self.sha256 {
            return Err(Error::Corrupt("acquired body length changed".into()));
        }
        Ok(copied)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquiredCurrent {
    pub state: Value,
    pub bodies: BTreeMap<String, SpoolBody>,
    pub references: Vec<VersionRef>,
    pub source_position: published::SourcePosition,
}
impl AcquiredCurrent {
    /// Only publication may materialize this after its measured memory admission.
    pub(super) fn into_owned_admitted(self) -> Result<CurrentUnit> {
        let kind = self.state["entity_type"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("entity type".into()))?
            .to_owned();
        let id = self.state[if kind == "memory" {
            "memory_id"
        } else {
            "procedure_id"
        }]
        .as_str()
        .ok_or_else(|| Error::Corrupt("entity identity".into()))?
        .to_owned();
        let state = if self.state["state"] == "redirect" {
            UnitState::Redirect {
                redirect_to: self.state["redirect_to"].as_str().unwrap().into(),
                consolidation_change_id: self.state["consolidation_change_id"]
                    .as_str()
                    .unwrap()
                    .into(),
            }
        } else {
            let mut heads = Vec::new();
            for metadata in self.state["current_heads"].as_array().into_iter().flatten() {
                let id = metadata["change_id"]
                    .as_str()
                    .ok_or_else(|| Error::Corrupt("change identity".into()))?;
                let mut body = String::new();
                self.bodies
                    .get(id)
                    .ok_or_else(|| Error::Corrupt("missing body".into()))?
                    .open()?
                    .read_to_string(&mut body)?;
                heads.push(Head {
                    metadata: metadata.clone(),
                    body,
                });
            }
            UnitState::Live { heads }
        };
        let unit = CurrentUnit {
            entity_id: id,
            entity_type: kind,
            state,
        };
        unit.validate()?;
        Ok(unit)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquiredResolvedCurrent {
    pub requested_id: String,
    pub redirect_path: Vec<String>,
    pub current: AcquiredCurrent,
}
impl Store {
    /// Complete selected/required closure; body bytes are owned file spools, not RAM strings.
    pub fn acquire_closure(
        &self,
        ids: &[String],
    ) -> Result<(
        published::SourcePosition,
        Vec<AcquiredResolvedCurrent>,
        Vec<String>,
    )> {
        let _gate = self.lock()?;
        let position = published::SourcePosition {
            epoch: self.load(&self.local.join("source-epoch"))?,
            sequence: self.ensure_clean()?,
        };
        let mut queue = std::collections::VecDeque::from(ids.to_vec());
        let mut visited = BTreeSet::new();
        let mut resolved = BTreeSet::new();
        let mut result = Vec::new();
        let mut missing = BTreeSet::new();
        while let Some(requested_id) = queue.pop_front() {
            if !visited.insert(requested_id.clone()) {
                continue;
            }
            let mut next = requested_id.clone();
            let mut chain = BTreeSet::new();
            let mut redirect_path = Vec::new();
            loop {
                if !chain.insert(next.clone()) {
                    return Err(Error::Corrupt("redirect cycle".into()));
                }
                let current = match self.acquire_kind_locked(&next, "memory", position.clone()) {
                    Ok(v) => v,
                    Err(Error::NotFound(_)) => {
                        missing.insert(next);
                        break;
                    }
                    Err(e) => return Err(e),
                };
                if current.state["state"] == "redirect" {
                    redirect_path.push(next);
                    next = current.state["redirect_to"]
                        .as_str()
                        .ok_or_else(|| Error::Corrupt("redirect target".into()))?
                        .to_owned();
                    continue;
                }
                if resolved.insert(next) {
                    for head in current.state["current_heads"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        for relation in head["relations"].as_array().into_iter().flatten() {
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
                    result.push(AcquiredResolvedCurrent {
                        requested_id,
                        redirect_path,
                        current,
                    });
                }
                break;
            }
        }
        Ok((position, result, missing.into_iter().collect()))
    }
    pub fn acquire_current(&self, id: &str) -> Result<AcquiredCurrent> {
        let _gate = self.lock()?;
        let position = published::SourcePosition {
            epoch: self.load(&self.local.join("source-epoch"))?,
            sequence: self.ensure_clean()?,
        };
        self.acquire_kind_locked(id, "memory", position)
    }
    pub fn acquire_procedure(&self, id: &str) -> Result<AcquiredCurrent> {
        let _gate = self.lock()?;
        let position = published::SourcePosition {
            epoch: self.load(&self.local.join("source-epoch"))?,
            sequence: self.ensure_clean()?,
        };
        self.acquire_kind_locked(id, "procedure", position)
    }
    pub(super) fn acquire_kind_locked(
        &self,
        id: &str,
        kind: &str,
        position: published::SourcePosition,
    ) -> Result<AcquiredCurrent> {
        validate_id(id)?;
        let path = self.safe_path(&format!("current/{kind}/{}/{}.md", &id[..2], id))?;
        let file = File::open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound(id.into())
            } else {
                e.into()
            }
        })?;
        let dir = self
            .local
            .join("acquisitions")
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&dir)?;
        let lease = std::sync::Arc::new(AcquisitionLease(dir.clone()));
        let mut state = None;
        let mut bodies = BTreeMap::new();
        let count = codec::scan("current", &mut BufReader::new(file), |info, payload| {
            if info.kind == "state" {
                if state.is_some() || info.key != id {
                    return Err(Error::Corrupt("current state identity/cardinality".into()));
                }
                let mut bytes = Vec::new();
                payload.read_to_end(&mut bytes)?;
                state = Some(codec::parse_json(&bytes)?);
            } else {
                if info.kind
                    != if kind == "memory" {
                        "body"
                    } else {
                        "instructions"
                    }
                {
                    return Err(Error::Corrupt("current body type".into()));
                }
                let path = dir.join(format!("{}.payload", info.key));
                let mut out = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)?;
                std::io::copy(payload, &mut out)?;
                out.sync_all()?;
                bodies.insert(
                    info.key.clone(),
                    SpoolBody {
                        _lease: Some(lease.clone()),
                        path,
                        bytes: info.bytes,
                        sha256: info.sha256.clone(),
                    },
                );
            }
            Ok(())
        })?;
        let state = state.ok_or_else(|| Error::Corrupt("missing state".into()))?;
        let entity_type = if kind == "memory" {
            "memory"
        } else {
            "learned_procedure"
        };
        let idkey = if kind == "memory" {
            "memory_id"
        } else {
            "procedure_id"
        };
        if state[idkey] != id || state["entity_type"] != entity_type {
            return Err(Error::Corrupt("current identity/type".into()));
        }
        let unit_state = match state["state"].as_str() {
            Some("live") => {
                if state.get("redirect_to").is_some()
                    || state.get("consolidation_change_id").is_some()
                {
                    return Err(Error::Corrupt("mixed live/redirect".into()));
                }
                let heads = state["current_heads"]
                    .as_array()
                    .ok_or_else(|| Error::Corrupt("head list".into()))?
                    .iter()
                    .map(|m| Head {
                        metadata: m.clone(),
                        body: String::new(),
                    })
                    .collect::<Vec<_>>();
                if heads.len() != bodies.len() || count != heads.len() as u64 + 1 {
                    return Err(Error::Corrupt("body cardinality".into()));
                }
                UnitState::Live { heads }
            }
            Some("redirect") => {
                if !bodies.is_empty() || state.get("current_heads").is_some() {
                    return Err(Error::Corrupt("redirect body/heads".into()));
                }
                UnitState::Redirect {
                    redirect_to: state["redirect_to"].as_str().unwrap_or("").into(),
                    consolidation_change_id: state["consolidation_change_id"]
                        .as_str()
                        .unwrap_or("")
                        .into(),
                }
            }
            _ => return Err(Error::Corrupt("state discriminator".into())),
        };
        let unit = CurrentUnit {
            entity_id: id.into(),
            entity_type: entity_type.into(),
            state: unit_state,
        };
        unit.validate()?;
        let mut references = Vec::new();
        for head in unit.heads() {
            let body = bodies
                .get(change_id(head)?)
                .ok_or_else(|| Error::Corrupt("missing head body".into()))?;
            references.push(VersionRef {
                memory_id: id.into(),
                observed_version: version_stream(&unit, head, body.bytes, &mut body.open()?)?,
            });
        }
        Ok(AcquiredCurrent {
            state,
            bodies,
            references,
            source_position: position,
        })
    }
}
