//! Exceptional exact reads serialize owned body spools without building a giant Value.
use super::*;
use agentlaw_storage::published::PublishedChangeSource;
use std::io::{Read, Write};
#[derive(Clone)]
struct BodySlice {
    body: agentlaw_storage::acquire::SpoolBody,
    start: u64,
    bytes: u64,
}
#[derive(Clone, Default)]
pub(crate) struct StreamedSelection {
    bodies: BTreeMap<String, BodySlice>,
    versions: BTreeMap<String, String>,
}
impl StreamedSelection {
    pub(super) fn register_version(&mut self, original: String, actual: String) {
        self.versions.insert(original, actual);
    }
    pub(super) fn uses_spools(&self, value: &Value) -> bool {
        match value {
            Value::String(value) => self.bodies.contains_key(value),
            Value::Array(values) => values.iter().any(|value| self.uses_spools(value)),
            Value::Object(values) => values.values().any(|value| self.uses_spools(value)),
            _ => false,
        }
    }
    pub(super) fn inline(&self, value: &Value, control: &RequestControl) -> Result<Value> {
        if !self.uses_spools(value) {
            return Ok(value.clone());
        }
        let mut bytes = Vec::new();
        self.emit(&mut bytes, value, control)?;
        serde_json::from_slice(&bytes).map_err(|_| failed())
    }
    pub(super) fn serialized_size(&self, value: &Value, control: &RequestControl) -> Result<usize> {
        struct Count(usize);
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .ok_or_else(|| std::io::Error::other("response size overflow"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count(0);
        self.emit(&mut count, value, control)?;
        Ok(count.0)
    }
    pub(crate) fn actual_version(&self, version: String) -> String {
        self.versions.get(&version).cloned().unwrap_or(version)
    }
    pub(crate) fn excerpt(&self, body: &str, chars: usize) -> Result<String> {
        let Some(slice) = self.bodies.get(body) else {
            return Ok(body.chars().take(chars).collect());
        };
        let mut bytes = Vec::new();
        slice
            .body
            .open()
            .map_err(|_| failed())?
            .take((chars as u64).saturating_mul(4))
            .read_to_end(&mut bytes)
            .map_err(|_| failed())?;
        let valid = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(e) if e.error_len().is_none() => {
                std::str::from_utf8(&bytes[..e.valid_up_to()]).map_err(|_| failed())?
            }
            Err(_) => return Err(failed()),
        };
        Ok(valid.chars().take(chars).collect())
    }
    pub(super) fn insert(
        &mut self,
        body: agentlaw_storage::acquire::SpoolBody,
        start: u64,
        bytes: u64,
    ) -> String {
        let token = format!("agentlaw-owned-body:{}", uuid::Uuid::new_v4());
        self.bodies
            .insert(token.clone(), BodySlice { body, start, bytes });
        token
    }
    pub(super) fn task_sections(&mut self, body: &str) -> Result<[String; 3]> {
        let Some(slice) = self.bodies.get(body).cloned() else {
            return task_sections(body);
        };
        let ranges = task_ranges(&slice.body)?;
        Ok(std::array::from_fn(|i| {
            self.insert(
                slice.body.clone(),
                ranges[i].1,
                ranges[i + 1].0 - ranges[i].1,
            )
        }))
    }
    pub(crate) fn finish(
        &self,
        value: Value,
        local: &Path,
        control: &RequestControl,
    ) -> Result<Value> {
        if self.bodies.is_empty() {
            return Ok(value);
        }
        let mut out = crate::artifacts::ArtifactWriter::create(local.join("response-artifacts"))?;
        self.emit(&mut out, &value, control)?;
        let artifact = out.finish()?;
        let mut envelope = json!({"code":"complete_content_in_file","content_read":false,"artifact":artifact,"next_action":"The complete version-frozen response, including selected full heads and all recall or authoring instructions, is in this Runtime-host file and has NOT been read. Read the complete file with your harness file tool before relying on it. If that filesystem is inaccessible, explain the limitation and request file transfer. Do not infer an empty result or summarize unread content. After the indicated expiry, repeat the request to acquire current state, which may differ."});
        if let Some(status) = value.get("status") {
            envelope["operation_status"] = status.clone();
        }
        if let Some(instruction) = value.get("turn_instruction") {
            envelope["turn_instruction"] = instruction.clone();
        }
        Ok(envelope)
    }
    pub(super) fn finish_read(
        &self,
        value: &Value,
        local: &Path,
        control: &RequestControl,
    ) -> Result<Value> {
        let mut out =
            crate::artifacts::ReadArtifactWriter::create(local.join("response-artifacts"))?;
        self.emit(&mut out, value, control)?;
        let artifact = out.finish()?;
        let mut envelope = json!({"code":"complete_content_in_file", "content_read":false, "artifact":artifact, "next_action":"The complete result has NOT been read. Read the result file on the Runtime host before relying on its contents. If it is inaccessible, request file transfer. The file is retained for at least seven days."});
        if let Some(instruction) = value.get("turn_instruction") {
            envelope["turn_instruction"] = instruction.clone();
        }
        Ok(envelope)
    }
    fn emit(&self, out: &mut impl Write, value: &Value, control: &RequestControl) -> Result<()> {
        control.check()?;
        match value {
            Value::String(text) if self.bodies.contains_key(text) => {
                escaped_slice(out, &self.bodies[text], control)
            }
            Value::String(text) if self.versions.contains_key(text) => {
                write_json(out, &json!(self.versions[text]))
            }
            Value::Array(items) => {
                out.write_all(b"[").map_err(|_| failed())?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.write_all(b",").map_err(|_| failed())?
                    }
                    self.emit(out, item, control)?;
                }
                out.write_all(b"]").map_err(|_| failed())
            }
            Value::Object(object) => {
                out.write_all(b"{").map_err(|_| failed())?;
                for (i, (key, item)) in object.iter().enumerate() {
                    if i > 0 {
                        out.write_all(b",").map_err(|_| failed())?
                    }
                    write_json(out, &json!(key))?;
                    out.write_all(b":").map_err(|_| failed())?;
                    self.emit(out, item, control)?;
                }
                out.write_all(b"}").map_err(|_| failed())
            }
            _ => write_json(out, value),
        }
    }
}
fn escaped_slice(out: &mut impl Write, slice: &BodySlice, control: &RequestControl) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = slice.body.open().map_err(|_| failed())?;
    file.seek(SeekFrom::Start(slice.start))
        .map_err(|_| failed())?;
    let mut reader = file.take(slice.bytes);
    let mut buffer = [0u8; 65536];
    let mut remaining = slice.bytes;
    let mut tail = Vec::new();
    let mut digest = Sha256::new();
    out.write_all(b"\"").map_err(|_| failed())?;
    loop {
        control.check()?;
        let n = reader.read(&mut buffer).map_err(|_| failed())?;
        if n == 0 {
            break;
        }
        remaining -= n as u64;
        digest.update(&buffer[..n]);
        tail.extend_from_slice(&buffer[..n]);
        let valid = match std::str::from_utf8(&tail) {
            Ok(_) => tail.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => return Err(failed()),
        };
        let text = std::str::from_utf8(&tail[..valid]).map_err(|_| failed())?;
        let encoded = serde_json::to_vec(text).map_err(|_| failed())?;
        out.write_all(&encoded[1..encoded.len() - 1])
            .map_err(|_| failed())?;
        tail.drain(..valid);
    }
    if remaining != 0
        || !tail.is_empty()
        || (slice.start == 0
            && slice.bytes == slice.body.bytes
            && format!("{:x}", digest.finalize()) != slice.body.sha256)
    {
        return Err(failed());
    }
    out.write_all(b"\"").map_err(|_| failed())
}
/// Bounded line signatures: long content lines never allocate in proportion to
/// their length. Only the four small heading names and fence prefixes matter.
fn task_ranges(body: &agentlaw_storage::acquire::SpoolBody) -> Result<Vec<(u64, u64)>> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(body.open().map_err(|_| failed())?);
    let mut offset = 0u64;
    let mut ranges = Vec::new();
    let mut fence: Option<(char, u64)> = None;
    let mut digest = Sha256::new();
    loop {
        let start = offset;
        let mut signature = Vec::new();
        let mut pending = Vec::new();
        let mut excessive = false;
        let mut begun = false;
        let mut fence_probe = crate::write::TaskFenceProbe::default();
        let mut fence_utf8 = Vec::with_capacity(4);
        loop {
            let bytes = reader.fill_buf().map_err(|_| failed())?;
            if bytes.is_empty() {
                break;
            }
            let count = bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |i| i + 1);
            let ended = bytes[count - 1] == b'\n';
            digest.update(&bytes[..count]);
            for &byte in &bytes[..count] {
                if !fence_probe.done() {
                    fence_utf8.push(byte);
                    match std::str::from_utf8(&fence_utf8) {
                        Ok(text) => {
                            for ch in text.chars() {
                                fence_probe.push(ch);
                            }
                            fence_utf8.clear();
                        }
                        Err(error) if error.error_len().is_none() && fence_utf8.len() < 4 => {}
                        Err(_) => return Err(failed()),
                    }
                }
                if !begun && byte.is_ascii_whitespace() {
                    continue;
                }
                begun = true;
                if byte.is_ascii_whitespace() || byte == b'#' {
                    if pending.len() < 1024 {
                        pending.push(byte)
                    }
                } else {
                    if signature.len() + pending.len() + 1 <= 1024 {
                        signature.append(&mut pending);
                        signature.push(byte)
                    } else {
                        excessive = true;
                        pending.clear();
                    }
                }
            }
            offset += count as u64;
            reader.consume(count);
            if ended {
                break;
            }
        }
        if offset == start {
            break;
        }
        if !fence_utf8.is_empty() {
            return Err(failed());
        }
        if let Some((c, count)) = fence_probe.finish() {
            match fence {
                None => fence = Some((c, count)),
                Some((old, n)) if old == c && count >= n => fence = None,
                _ => {}
            }
            continue;
        }
        if fence.is_none() && !excessive {
            let text = std::str::from_utf8(&signature).unwrap_or("");
            if let Some(name) = crate::write::task_heading(text) {
                let names = [
                    "Objective",
                    "Current position",
                    "Resume point",
                    "References",
                ];
                if names.contains(&name) {
                    if names.get(ranges.len()) != Some(&name) {
                        return Err(DomainError::new(
                            "invalid_task_headings",
                            "Task headings are invalid.",
                        ));
                    }
                    ranges.push((start, offset));
                }
            }
        }
    }
    if offset != body.bytes || format!("{:x}", digest.finalize()) != body.sha256 {
        return Err(failed());
    }
    if ranges.len() != 4 {
        return Err(DomainError::new(
            "invalid_task_headings",
            "Task headings are invalid.",
        ));
    }
    Ok(ranges)
}
impl Runtime {
    pub(crate) fn snapshot_for_recall(
        &self,
        ids: &[String],
        procedures: &[String],
    ) -> Result<Snapshot> {
        match self.snapshot_selected(ids, procedures) {
            Ok(snapshot) => return Ok(snapshot),
            Err(e) if e.code == "source_payload_requires_spool" => {}
            Err(e) => return Err(e),
        }
        let (position, resolved, missing) = source(self.store.acquire_closure(ids))?;
        let mut streaming = StreamedSelection::default();
        let mut states = BTreeMap::new();
        let mut units = BTreeMap::new();
        let mut refs = Vec::new();
        for value in resolved {
            let current = value.current;
            let id = current.state["memory_id"]
                .as_str()
                .ok_or_else(failed)?
                .to_owned();
            let mut heads = Vec::new();
            for metadata in current.state["current_heads"]
                .as_array()
                .ok_or_else(failed)?
            {
                let body = current
                    .bodies
                    .get(metadata["change_id"].as_str().ok_or_else(failed)?)
                    .ok_or_else(failed)?
                    .clone();
                let bytes = body.bytes;
                heads.push(Head {
                    metadata: metadata.clone(),
                    body: streaming.insert(body, 0, bytes),
                });
            }
            let unit = CurrentUnit {
                entity_id: id.clone(),
                entity_type: "memory".into(),
                state: UnitState::Live { heads },
            };
            let mut memories = Vec::new();
            for (head, reference) in unit.heads().iter().zip(&current.references) {
                let mut memory = memory_from(&unit, head)?;
                memory.memory_ref.observed_version = reference.observed_version.clone();
                refs.push(memory.memory_ref.clone());
                memories.push(memory)
            }
            let state = CurrentState {
                requested_id: value.requested_id.clone(),
                resolved_id: id.clone(),
                heads: memories,
                redirect_path: value.redirect_path,
            };
            states.insert(
                id.clone(),
                CurrentState {
                    requested_id: id.clone(),
                    redirect_path: vec![],
                    ..state.clone()
                },
            );
            states.insert(value.requested_id, state);
            units.insert(id, unit);
        }
        for id in procedures {
            match self.store.acquire_procedure(id) {
                Ok(current) => {
                    if current.source_position != position {
                        return Err(DomainError::new(
                            "source_changed",
                            "Source changed during acquired selection.",
                        ));
                    }
                    let mut heads = Vec::new();
                    for metadata in current.state["current_heads"]
                        .as_array()
                        .ok_or_else(failed)?
                    {
                        let body = current
                            .bodies
                            .get(metadata["change_id"].as_str().ok_or_else(failed)?)
                            .ok_or_else(failed)?
                            .clone();
                        let bytes = body.bytes;
                        heads.push(Head {
                            metadata: metadata.clone(),
                            body: streaming.insert(body, 0, bytes),
                        });
                    }
                    let unit = CurrentUnit {
                        entity_id: id.clone(),
                        entity_type: "learned_procedure".into(),
                        state: UnitState::Live { heads },
                    };
                    for (head, reference) in unit.heads().iter().zip(&current.references) {
                        streaming.versions.insert(
                            source(agentlaw_storage::version(&unit, head))?,
                            reference.observed_version.clone(),
                        );
                    }
                    units.insert(id.clone(), unit);
                }
                Err(agentlaw_storage::Error::NotFound(_)) => {}
                Err(e) => return source(Err(e)),
            }
        }
        if source(self.store.published_reader().position())? != position {
            return Err(DomainError::new(
                "source_changed",
                "Source changed during acquired selection.",
            ));
        }
        Ok(Snapshot {
            generation: position.sequence,
            states,
            units: units.into_values().collect(),
            basis: ReadSet {
                memory_refs: refs,
                fingerprint: String::new(),
            },
            publication_basis: agentlaw_storage::ReadSet {
                source_position: position,
                observed: BTreeMap::new(),
                absent: missing,
                reverse_required: BTreeMap::new(),
                watched_scopes: vec![],
            },
            streaming,
        })
    }
}
fn failed() -> DomainError {
    DomainError::new(
        "result_delivery_failed",
        "Cannot preserve the complete acquired result; no partial content is represented as read.",
    )
}
fn write_json(out: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(out, value).map_err(|_| failed())
}
fn escaped_body(out: &mut impl Write, body: &agentlaw_storage::acquire::SpoolBody) -> Result<()> {
    let mut reader = body.open().map_err(|_| failed())?;
    out.write_all(b"\"").map_err(|_| failed())?;
    let mut buf = [0u8; 65536];
    let mut count = 0u64;
    let mut digest = Sha256::new();
    let mut utf8_tail = Vec::new();
    loop {
        let n = reader.read(&mut buf).map_err(|_| failed())?;
        if n == 0 {
            break;
        }
        count += n as u64;
        digest.update(&buf[..n]);
        utf8_tail.extend_from_slice(&buf[..n]);
        match std::str::from_utf8(&utf8_tail) {
            Ok(_) => utf8_tail.clear(),
            Err(error) if error.error_len().is_none() => {
                utf8_tail.drain(..error.valid_up_to());
            }
            Err(_) => return Err(failed()),
        }
        let mut start = 0;
        for (i, b) in buf[..n].iter().enumerate() {
            if *b < 32 || *b == b'"' || *b == b'\\' {
                out.write_all(&buf[start..i]).map_err(|_| failed())?;
                match *b {
                    b'"' => out.write_all(b"\\\""),
                    b'\\' => out.write_all(b"\\\\"),
                    n => write!(out, "\\u{n:04x}"),
                }
                .map_err(|_| failed())?;
                start = i + 1
            }
        }
        out.write_all(&buf[start..n]).map_err(|_| failed())?;
    }
    if !utf8_tail.is_empty()
        || count != body.bytes
        || format!("{:x}", digest.finalize()) != body.sha256
    {
        return Err(failed());
    }
    out.write_all(b"\"").map_err(|_| failed())
}
fn head(
    out: &mut impl Write,
    current: &agentlaw_storage::acquire::AcquiredCurrent,
    metadata: &Value,
    procedure: bool,
) -> Result<()> {
    let id = current.state[if procedure {
        "procedure_id"
    } else {
        "memory_id"
    }]
    .as_str()
    .ok_or_else(failed)?;
    let change = metadata["change_id"].as_str().ok_or_else(failed)?;
    let body = current.bodies.get(change).ok_or_else(failed)?;
    let index = current.state["current_heads"]
        .as_array()
        .ok_or_else(failed)?
        .iter()
        .position(|h| h["change_id"] == change)
        .ok_or_else(failed)?;
    let version = current.references.get(index).ok_or_else(failed)?;
    let mut value = if procedure {
        json!({"procedure_id":id,"name":metadata["name"],"use_when":metadata["use_when"],"applicability":scope_value(&metadata["applicability"])? ,"procedure_ref":{"procedure_id":id,"observed_version":version.observed_version},"evidence_memory_ids":metadata["evidence_memory_ids"]})
    } else {
        let unit = CurrentUnit {
            entity_id: id.into(),
            entity_type: "memory".into(),
            state: UnitState::Live { heads: vec![] },
        };
        let mut memory = memory_from(
            &unit,
            &Head {
                metadata: metadata.clone(),
                body: String::new(),
            },
        )?;
        memory.memory_ref.observed_version = version.observed_version.clone();
        to_value(RecallHead::from(memory))?
    };
    let key = if procedure {
        "instructions"
    } else {
        "what_to_remember"
    };
    value.as_object_mut().ok_or_else(failed)?.remove(key);
    let encoded = serde_json::to_vec(&value).map_err(|_| failed())?;
    out.write_all(&encoded[..encoded.len() - 1])
        .map_err(|_| failed())?;
    write!(out, ",\"{key}\":").map_err(|_| failed())?;
    escaped_body(out, body)?;
    out.write_all(b"}").map_err(|_| failed())
}
impl Runtime {
    pub(crate) fn stream_exact_recall(&self, r: &RecallRequest) -> Result<Value> {
        self.request_control.check()?;
        let (position, memories, missing) = source(
            self.store
                .acquire_closure(r.memory_ids.as_deref().unwrap_or(&[])),
        )?;
        let mut procedures = Vec::new();
        let mut absent_procedures = Vec::new();
        for id in r.procedure_ids.iter().flatten() {
            match self.store.acquire_procedure(id) {
                Ok(current) => {
                    if current.source_position != position {
                        return Err(DomainError::new(
                            "source_changed",
                            "Source changed during acquired reads; retry.",
                        ));
                    }
                    procedures.push(current)
                }
                Err(agentlaw_storage::Error::NotFound(_)) => absent_procedures.push(id.clone()),
                Err(e) => return source(Err(e)),
            }
        }
        let mut out =
            crate::artifacts::ArtifactWriter::create(self.local.join("response-artifacts"))?;
        out.write_all(b"{\"current_time\":").map_err(|_| failed())?;
        write_json(
            &mut out,
            &json!(time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| failed())?),
        )?;
        out.write_all(b",\"memories\":[").map_err(|_| failed())?;
        for (i, memory) in memories.iter().enumerate() {
            self.request_control.check()?;
            if i > 0 {
                out.write_all(b",").map_err(|_| failed())?
            }
            out.write_all(b"{\"memory_id\":").map_err(|_| failed())?;
            write_json(&mut out, &memory.current.state["memory_id"])?;
            out.write_all(b",\"current_heads\":[")
                .map_err(|_| failed())?;
            let heads = memory.current.state["current_heads"]
                .as_array()
                .ok_or_else(failed)?;
            for (j, metadata) in heads.iter().enumerate() {
                if j > 0 {
                    out.write_all(b",").map_err(|_| failed())?
                }
                head(&mut out, &memory.current, metadata, false)?
            }
            out.write_all(b"]").map_err(|_| failed())?;
            if heads.len() > 1 {
                out.write_all(b",\"head_reconciliation_required\":true,\"head_reconciliation_instruction\":\"Review every competing head before evolving this memory.\"").map_err(|_|failed())?
            }
            out.write_all(b"}").map_err(|_| failed())?;
        }
        out.write_all(b"],\"candidates\":[],\"learned_procedures\":[")
            .map_err(|_| failed())?;
        for (i, current) in procedures.iter().enumerate() {
            if i > 0 {
                out.write_all(b",").map_err(|_| failed())?
            }
            let heads = current.state["current_heads"]
                .as_array()
                .ok_or_else(failed)?;
            if heads.len() == 1 {
                head(&mut out, current, &heads[0], true)?
            } else {
                out.write_all(b"{\"procedure_id\":").map_err(|_| failed())?;
                write_json(&mut out, &current.state["procedure_id"])?;
                out.write_all(b",\"current_heads\":[")
                    .map_err(|_| failed())?;
                for (j, h) in heads.iter().enumerate() {
                    if j > 0 {
                        out.write_all(b",").map_err(|_| failed())?
                    }
                    head(&mut out, current, h, true)?
                }
                out.write_all(b"],\"head_reconciliation_required\":true,\"head_reconciliation_instruction\":\"Review every competing procedure head.\"}").map_err(|_|failed())?
            }
        }
        out.write_all(b"]").map_err(|_| failed())?;
        if !missing.is_empty() || !absent_procedures.is_empty() {
            let explicit: BTreeSet<_> = r.memory_ids.iter().flatten().collect();
            let absent: Vec<_> = missing.iter().filter(|id| explicit.contains(id)).collect();
            let required: Vec<_> = missing
                .iter()
                .filter(|id| !explicit.contains(id))
                .map(|id| json!({"memory_id":id,"reason":"Required current identity is absent."}))
                .collect();
            out.write_all(b",\"missing_ids\":").map_err(|_| failed())?;
            write_json(
                &mut out,
                &json!({"memory_ids":absent,"procedure_ids":absent_procedures}),
            )?;
            out.write_all(b",\"undelivered_required\":")
                .map_err(|_| failed())?;
            write_json(&mut out, &json!(required))?;
        }
        out.write_all(b"}").map_err(|_| failed())?;
        let artifact = out.finish()?;
        Ok(
            json!({"code":"complete_content_in_file","content_read":false,"artifact":artifact,"next_action":"The full acquired memory/procedure result is preserved in this Runtime-host file and has NOT been read into your context. Read the whole file with your harness file tool, including required references and competing heads, before relying on it. If that filesystem is inaccessible, explain the limitation and request file transfer. Do not summarize unread content or infer an empty result. The file is retained until its indicated expiry (at least seven days). After expiry repeat the read to acquire current state, which may be a different version."}),
        )
    }
}

#[cfg(test)]
mod tests;
