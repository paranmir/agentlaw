//! C6 history responsibilities; shares the facade's source gate and persistence protocol.
use super::*;

impl Store {
    /// Full exact history scan, one bounded pack at a time; current reads never invoke it.
    pub fn history(&self, entity_id: &str) -> Result<Vec<HistoricalState>> {
        validate_id(entity_id)?;
        let _l = self.lock()?;
        self.ensure_clean()?;
        let mut descriptors = BTreeMap::new();
        self.scan_history(|frame| {
            if frame.kind == "change_descriptor" {
                let d = codec::parse_json(&frame.payload)?;
                if d["entity_id"] == entity_id {
                    if let Some(old) = descriptors.insert(frame.key.clone(), d.clone()) {
                        if old != d {
                            return Err(Error::Corrupt("conflicting immutable change".into()));
                        }
                    }
                }
            }
            Ok(())
        })?;
        let mut selected: BTreeMap<(String, String), BTreeMap<u32, Vec<u8>>> = BTreeMap::new();
        let mut checkpoints = BTreeMap::new();
        let mut total = 0u64;
        self.scan_history(|frame| {
            let id = frame.key.split('.').next().unwrap();
            if !descriptors.contains_key(id) {
                return Ok(());
            }
            if frame.kind == "checkpoint_descriptor" {
                let d = codec::parse_json(&frame.payload)?;
                if let Some(old) = checkpoints.insert(id.to_string(), d.clone()) {
                    if old != d {
                        return Err(Error::Corrupt("checkpoint collision".into()));
                    }
                }
            } else if let Some(n) = frame.key.split('.').nth(1) {
                let n: u32 = n
                    .parse()
                    .map_err(|_| Error::Corrupt("chunk number".into()))?;
                total = total
                    .checked_add(frame.payload.len() as u64)
                    .ok_or(Error::Capacity)?;
                if total > codec::MAX_FRAME_BYTES {
                    return Err(Error::Capacity);
                }
                let map = selected.entry((id.into(), frame.kind.clone())).or_default();
                if let Some(old) = map.insert(n, frame.payload.clone()) {
                    if old != frame.payload {
                        return Err(Error::Corrupt("conflicting chunk".into()));
                    }
                }
            }
            Ok(())
        })?;
        let assemble = |id: &str, ty: &str, desc: &Value| -> Result<Vec<u8>> {
            let map = selected
                .get(&(id.to_owned(), ty.into()))
                .ok_or_else(|| Error::Corrupt("missing history component".into()))?;
            let count = desc["chunks"]
                .as_u64()
                .ok_or_else(|| Error::Corrupt("chunk count".into()))?;
            if count != map.len() as u64 {
                return Err(Error::Corrupt("chunk cardinality".into()));
            }
            let mut bytes = Vec::new();
            for i in 0..count {
                bytes.extend_from_slice(
                    map.get(&(u32::try_from(i).map_err(|_| Error::Capacity)?))
                        .ok_or_else(|| Error::Corrupt("chunk gap".into()))?,
                );
            }
            if desc["sha256"] != codec::digest(&bytes) || desc["bytes"] != bytes.len().to_string() {
                return Err(Error::Corrupt("component digest/length".into()));
            }
            Ok(bytes)
        };
        let mut result = Vec::new();
        let mut deltas = BTreeMap::new();
        for (id, desc) in &descriptors {
            let metadata =
                codec::parse_json(&assemble(id, "change_metadata_chunk", &desc["metadata"])?)?;
            let delta = codec::parse_json(&assemble(id, "delta_chunk", &desc["delta"])?)?;
            deltas.insert(id.clone(), delta);
            let evidence = String::from_utf8(assemble(id, "evidence_chunk", &desc["evidence"])?)
                .map_err(|_| Error::Corrupt("evidence UTF8".into()))?;
            let cp = checkpoints
                .get(id)
                .ok_or_else(|| Error::Corrupt("missing checkpoint".into()))?;
            let body = String::from_utf8(assemble(id, "checkpoint_body_chunk", &cp["body"])?)
                .map_err(|_| Error::Corrupt("body UTF8".into()))?;
            let cm =
                codec::parse_json(&assemble(id, "checkpoint_metadata_chunk", &cp["metadata"])?)?;
            if metadata["metadata_after"] != cm
                || metadata["result_body_sha256"] != codec::digest(body.as_bytes())
                || metadata["result_body_bytes"] != body.len().to_string()
            {
                return Err(Error::Corrupt("history result mismatch".into()));
            }
            let unit = CurrentUnit {
                entity_id: entity_id.into(),
                entity_type: metadata["entity_type"].as_str().unwrap_or("").into(),
                state: UnitState::Live {
                    heads: vec![Head {
                        metadata: cm,
                        body: body.clone(),
                    }],
                },
            };
            if metadata["observed_version"] != version(&unit, &unit.heads()[0])? {
                return Err(Error::Corrupt("history state digest".into()));
            }
            result.push(HistoricalState {
                metadata,
                body,
                evidence,
                delta: deltas[id].clone(),
            });
        }
        for record in &result {
            let id = record.metadata["change_id"]
                .as_str()
                .ok_or_else(|| Error::Corrupt("history identity".into()))?;
            let parents = record.metadata["parent_change_ids"]
                .as_array()
                .ok_or_else(|| Error::Corrupt("parent list".into()))?;
            let mut parent_ids = BTreeSet::new();
            for parent in parents {
                let p = parent
                    .as_str()
                    .ok_or_else(|| Error::Corrupt("parent identity".into()))?;
                validate_id(p)?;
                if p == id || !parent_ids.insert(p) {
                    return Err(Error::Corrupt("duplicate/self parent".into()));
                }
                if !descriptors.contains_key(p) {
                    self.checkpoint_body(p)?;
                }
            }
            let base_id = if parents.len() > 1 {
                let base = record.metadata["delta_base_id"]
                    .as_str()
                    .ok_or_else(|| Error::Corrupt("missing multi-parent base".into()))?;
                if !parent_ids.contains(base) {
                    return Err(Error::Corrupt("base is not parent".into()));
                }
                Some(base)
            } else {
                if record.metadata.get("delta_base_id").is_some() {
                    return Err(Error::Corrupt("unexpected delta base".into()));
                }
                parents.first().and_then(Value::as_str)
            };
            let base = match base_id {
                None => String::new(),
                Some(id) => match result.iter().find(|r| r.metadata["change_id"] == id) {
                    Some(r) => r.body.clone(),
                    None => self.checkpoint_body(id)?,
                },
            };
            if apply_splice(&base, &deltas[id])? != record.body {
                return Err(Error::Corrupt("delta/checkpoint disagreement".into()));
            }
        }
        // Kahn-style causal order, never timestamp order. Cross-identity parents are retained.
        let mut ordered = Vec::new();
        let mut done = BTreeSet::new();
        while !result.is_empty() {
            let i = result
                .iter()
                .position(|r| {
                    r.metadata["parent_change_ids"]
                        .as_array()
                        .is_some_and(|ps| {
                            ps.iter().all(|p| {
                                p.as_str().is_some_and(|p| {
                                    !descriptors.contains_key(p) || done.contains(p)
                                })
                            })
                        })
                })
                .ok_or_else(|| Error::Corrupt("history DAG cycle".into()))?;
            let record = result.remove(i);
            done.insert(record.metadata["change_id"].as_str().unwrap().to_owned());
            ordered.push(record);
        }
        Ok(ordered)
    }
    /// Include only cross-identity causal ancestors, not unrelated later source revisions.
    pub fn history_closure(&self, entity_id: &str) -> Result<Vec<HistoricalState>> {
        let mut records: BTreeMap<String, HistoricalState> = self
            .history(entity_id)?
            .into_iter()
            .map(|r| (r.metadata["change_id"].as_str().unwrap().to_owned(), r))
            .collect();
        let mut requested = BTreeSet::new();
        loop {
            let missing = records
                .values()
                .flat_map(|r| r.metadata["parent_owners"].as_array().into_iter().flatten())
                .filter_map(|p| {
                    Some((
                        p["entity_id"].as_str()?.to_owned(),
                        p["change_id"].as_str()?.to_owned(),
                    ))
                })
                .filter(|(_, id)| !records.contains_key(id))
                .collect::<BTreeSet<_>>();
            if missing.is_empty() {
                break;
            }
            for (owner, id) in missing {
                if !requested.insert((owner.clone(), id.clone())) {
                    return Err(Error::Corrupt("unresolved historical parent".into()));
                }
                let record = self
                    .history(&owner)?
                    .into_iter()
                    .find(|r| r.metadata["change_id"] == id)
                    .ok_or_else(|| Error::Corrupt("missing cross-identity parent".into()))?;
                records.insert(id, record);
            }
        }
        let mut total = 0u64;
        for r in records.values() {
            total = total
                .checked_add((r.body.len() + r.evidence.len()) as u64)
                .ok_or(Error::Capacity)?;
        }
        if total > codec::MAX_FRAME_BYTES {
            return Err(Error::Capacity);
        }
        let mut result = Vec::new();
        let mut done = BTreeSet::new();
        while !records.is_empty() {
            let id = records
                .iter()
                .find(|(_, r)| {
                    r.metadata["parent_change_ids"].as_array().is_some_and(|p| {
                        p.iter()
                            .all(|id| id.as_str().is_some_and(|id| done.contains(id)))
                    })
                })
                .map(|(id, _)| id.clone())
                .ok_or_else(|| {
                    Error::Corrupt("cross-identity causal cycle/missing parent".into())
                })?;
            done.insert(id.clone());
            result.push(records.remove(&id).unwrap());
        }
        Ok(result)
    }
    pub(super) fn scan_history(
        &self,
        mut visit: impl FnMut(codec::Frame) -> Result<()>,
    ) -> Result<()> {
        let base = self.root.join("history");
        if !base.exists() {
            return Ok(());
        }
        for writer in fs::read_dir(base)? {
            let writer = writer?;
            if !writer.file_type()?.is_dir() {
                return Err(Error::Corrupt("history writer directory".into()));
            }
            for pack in fs::read_dir(writer.path())? {
                let pack = pack?;
                if pack.path().extension().is_some_and(|e| e == "tmp") {
                    continue;
                }
                for frame in codec::read("history", &mut BufReader::new(File::open(pack.path())?))?
                {
                    visit(frame)?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn checkpoint_body(&self, id: &str) -> Result<String> {
        let mut desc = None;
        let mut chunks = BTreeMap::new();
        let mut bytes = 0u64;
        self.scan_history(|f| {
            if f.key == id && f.kind == "checkpoint_descriptor" {
                let d = codec::parse_json(&f.payload)?;
                if desc.as_ref().is_some_and(|old| old != &d) {
                    return Err(Error::Corrupt("checkpoint identity collision".into()));
                }
                desc = Some(d);
            }
            if f.kind == "checkpoint_body_chunk" && f.key.starts_with(&format!("{id}.")) {
                let n: u32 = f
                    .key
                    .split('.')
                    .nth(1)
                    .unwrap()
                    .parse()
                    .map_err(|_| Error::Corrupt("chunk number".into()))?;
                bytes = bytes
                    .checked_add(f.payload.len() as u64)
                    .ok_or(Error::Capacity)?;
                if bytes > codec::MAX_FRAME_BYTES {
                    return Err(Error::Capacity);
                }
                if let Some(old) = chunks.insert(n, f.payload.clone()) {
                    if old != f.payload {
                        return Err(Error::Corrupt("checkpoint chunk collision".into()));
                    }
                }
            }
            Ok(())
        })?;
        let d = desc.ok_or_else(|| Error::Corrupt("missing parent checkpoint".into()))?;
        let count = d["body"]["chunks"]
            .as_u64()
            .ok_or_else(|| Error::Corrupt("checkpoint count".into()))?;
        if count != chunks.len() as u64 {
            return Err(Error::Corrupt("parent checkpoint incomplete".into()));
        }
        let mut bytes = Vec::new();
        for i in 0..count {
            bytes.extend_from_slice(
                chunks
                    .get(&(u32::try_from(i).map_err(|_| Error::Capacity)?))
                    .ok_or_else(|| Error::Corrupt("checkpoint gap".into()))?,
            );
        }
        if d["body"]["sha256"] != codec::digest(&bytes)
            || d["body"]["bytes"] != bytes.len().to_string()
        {
            return Err(Error::Corrupt("parent checkpoint digest".into()));
        }
        String::from_utf8(bytes).map_err(|_| Error::Corrupt("checkpoint UTF8".into()))
    }
}
