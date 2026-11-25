//! Exceptional exact reads serialize owned body spools without building a giant Value.
use super::*;
use std::io::{Read, Write};
#[derive(Clone)]
struct BodySlice {
    body: agentlaw_storage::acquire::SpoolBody,
    start: u64,
    bytes: u64,
    sha256: Option<String>,
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
    pub(super) fn inline(&self, value: &Value, control: &RequestControl) -> RecallResult<Value> {
        if !self.uses_spools(value) {
            return Ok(value.clone());
        }
        let mut bytes = Vec::new();
        self.emit(&mut bytes, value, control, true)?;
        serde_json::from_slice(&bytes).map_err(|_| serialization_failure().into())
    }
    pub(super) fn serialized_size(
        &self,
        value: &Value,
        control: &RequestControl,
    ) -> RecallResult<usize> {
        struct Count(usize);
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count(0);
        self.emit(&mut count, value, control, true)?;
        Ok(count.0)
    }
    pub(crate) fn actual_version(&self, version: String) -> String {
        self.versions.get(&version).cloned().unwrap_or(version)
    }
    pub(crate) fn excerpt(&self, body: &str, chars: usize) -> Result<String> {
        self.excerpt_with_policy(body, chars, false)
    }
    pub(super) fn recall_excerpt(&self, body: &str, chars: usize) -> Result<String> {
        self.excerpt_with_policy(body, chars, true)
    }
    fn excerpt_with_policy(&self, body: &str, chars: usize, recall: bool) -> Result<String> {
        let Some(slice) = self.bodies.get(body) else {
            return Ok(body.chars().take(chars).collect());
        };
        let mut bytes = Vec::new();
        open_spool(&slice.body, recall)?
            .take((chars as u64).saturating_mul(4))
            .read_to_end(&mut bytes)
            .map_err(|error| spool_read_failure(error, recall))?;
        let valid = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(e) if e.error_len().is_none() => {
                std::str::from_utf8(&bytes[..e.valid_up_to()]).map_err(|_| invalid_spool(recall))?
            }
            Err(_) => return Err(invalid_spool(recall)),
        };
        Ok(valid.chars().take(chars).collect())
    }
    pub(super) fn insert(
        &mut self,
        body: agentlaw_storage::acquire::SpoolBody,
        start: u64,
        bytes: u64,
    ) -> String {
        let sha256 = (start == 0 && bytes == body.bytes).then(|| body.sha256.clone());
        self.insert_slice(BodySlice {
            body,
            start,
            bytes,
            sha256,
        })
    }
    fn insert_slice(&mut self, slice: BodySlice) -> String {
        let token = format!("agentlaw-owned-body:{}", uuid::Uuid::new_v4());
        self.bodies.insert(token.clone(), slice);
        token
    }
    pub(super) fn task_sections(&mut self, body: &str) -> Result<[String; 3]> {
        let Some(slice) = self.bodies.get(body).cloned() else {
            return task_sections(body);
        };
        match self.parse_spooled_task_sections(&slice.body, None)? {
            TaskSectionParse::Complete(sections) => Ok(sections),
            TaskSectionParse::InvalidHeadings => Err(invalid_spooled_task_headings()),
        }
    }
    pub(super) fn parse_spooled_task_sections(
        &mut self,
        body: &agentlaw_storage::acquire::SpoolBody,
        request: Option<&RequestControl>,
    ) -> RecallResult<TaskSectionParse<[String; 3]>> {
        Ok(match task_ranges(body, request)? {
            TaskSectionParse::Complete(ranges) => {
                TaskSectionParse::Complete(ranges.map(|range| self.insert_slice(range)))
            }
            TaskSectionParse::InvalidHeadings => TaskSectionParse::InvalidHeadings,
        })
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
        self.emit(&mut out, &value, control, false)?;
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
    #[cfg(test)]
    pub(super) fn finish_read(
        &self,
        value: &Value,
        local: &Path,
        control: &RequestControl,
    ) -> Result<Value> {
        self.finish_recall(value, local, control)
            .map_err(Into::into)
    }
    pub(super) fn finish_recall(
        &self,
        value: &Value,
        local: &Path,
        control: &RequestControl,
    ) -> RecallResult<Value> {
        let mut out =
            crate::artifacts::ReadArtifactWriter::create(local.join("response-artifacts"))
                .map_err(RecallFailure::DeliveryFailed)?;
        self.emit(&mut out, value, control, true)?;
        let artifact = out.finish().map_err(RecallFailure::DeliveryFailed)?;
        let mut envelope = json!({"code":"complete_content_in_file", "content_read":false, "artifact":artifact, "next_action":"The complete result has NOT been read. Read the result file on the Runtime host before relying on its contents. If it is inaccessible, request file transfer. The file is retained for at least seven days."});
        if let Some(instruction) = value.get("turn_instruction") {
            envelope["turn_instruction"] = instruction.clone();
        }
        Ok(envelope)
    }
    fn emit(
        &self,
        out: &mut impl Write,
        value: &Value,
        control: &RequestControl,
        recall: bool,
    ) -> RecallResult<()> {
        check_recall(control)?;
        match value {
            Value::String(text) if self.bodies.contains_key(text) => {
                escaped_slice(out, &self.bodies[text], control, recall)
            }
            Value::String(text) if self.versions.contains_key(text) => {
                write_json(out, &json!(self.versions[text]))
            }
            Value::Array(items) => {
                out.write_all(b"[").map_err(response_write_failure)?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.write_all(b",").map_err(response_write_failure)?
                    }
                    self.emit(out, item, control, recall)?;
                }
                out.write_all(b"]").map_err(response_write_failure)
            }
            Value::Object(object) => {
                out.write_all(b"{").map_err(response_write_failure)?;
                for (i, (key, item)) in object.iter().enumerate() {
                    if i > 0 {
                        out.write_all(b",").map_err(response_write_failure)?
                    }
                    write_json(out, &json!(key))?;
                    out.write_all(b":").map_err(response_write_failure)?;
                    self.emit(out, item, control, recall)?;
                }
                out.write_all(b"}").map_err(response_write_failure)
            }
            _ => write_json(out, value),
        }
    }
}
fn escaped_slice(
    out: &mut impl Write,
    slice: &BodySlice,
    control: &RequestControl,
    recall: bool,
) -> RecallResult<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = open_spool(&slice.body, recall)?;
    file.seek(SeekFrom::Start(slice.start))
        .map_err(|error| spool_read_failure(error, recall))?;
    let mut reader = file.take(slice.bytes);
    let mut buffer = [0u8; 65536];
    let mut remaining = slice.bytes;
    let mut tail = Vec::new();
    let mut digest = Sha256::new();
    out.write_all(b"\"").map_err(response_write_failure)?;
    loop {
        check_recall(control)?;
        let n = reader
            .read(&mut buffer)
            .map_err(|error| spool_read_failure(error, recall))?;
        if n == 0 {
            break;
        }
        remaining -= n as u64;
        digest.update(&buffer[..n]);
        tail.extend_from_slice(&buffer[..n]);
        let valid = match std::str::from_utf8(&tail) {
            Ok(_) => tail.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => return Err(invalid_spool(recall).into()),
        };
        let text = std::str::from_utf8(&tail[..valid]).map_err(|_| invalid_spool(recall))?;
        let encoded = serde_json::to_vec(text).map_err(|_| serialization_failure())?;
        out.write_all(&encoded[1..encoded.len() - 1])
            .map_err(response_write_failure)?;
        tail.drain(..valid);
    }
    if remaining != 0
        || !tail.is_empty()
        || slice
            .sha256
            .as_ref()
            .is_some_and(|expected| format!("{:x}", digest.finalize()) != *expected)
    {
        return Err(invalid_spool(recall).into());
    }
    out.write_all(b"\"").map_err(response_write_failure)
}
/// Recognize heading and fence prefixes without buffering whole lines.
fn task_ranges(
    body: &agentlaw_storage::acquire::SpoolBody,
    request: Option<&RequestControl>,
) -> RecallResult<TaskSectionParse<[BodySlice; 3]>> {
    use std::io::BufRead;
    if let Some(request) = request {
        check_recall(request)?;
    }
    let mut reader = std::io::BufReader::new(open_spool(body, true)?);
    let mut offset = 0u64;
    let mut sections = Vec::new();
    let mut heading_count = 0;
    let mut invalid_headings = false;
    let mut section_start = None;
    let mut section_digest = Sha256::new();
    let mut fence: Option<(char, u64)> = None;
    let mut digest = Sha256::new();
    loop {
        let start = offset;
        let before_heading = section_start.map(|_| section_digest.clone());
        let mut heading_probe = TaskHeadingProbe::default();
        let mut fence_probe = crate::write::TaskFenceProbe::default();
        let mut prefix_utf8 = Vec::with_capacity(4);
        loop {
            if let Some(request) = request {
                check_recall(request)?;
            }
            let bytes = reader
                .fill_buf()
                .map_err(|error| spool_read_failure(error, true))?;
            if bytes.is_empty() {
                break;
            }
            let count = bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |i| i + 1);
            let ended = bytes[count - 1] == b'\n';
            digest.update(&bytes[..count]);
            if section_start.is_some() {
                section_digest.update(&bytes[..count]);
            }
            for &byte in &bytes[..count] {
                if fence_probe.done() && heading_probe.done() {
                    break;
                }
                prefix_utf8.push(byte);
                match std::str::from_utf8(&prefix_utf8) {
                    Ok(text) => {
                        for ch in text.chars() {
                            fence_probe.push(ch);
                            heading_probe.push(ch);
                        }
                        prefix_utf8.clear();
                    }
                    Err(error) if error.error_len().is_none() && prefix_utf8.len() < 4 => {}
                    Err(_) => return Err(invalid_spool(true).into()),
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
        if !prefix_utf8.is_empty() {
            return Err(invalid_spool(true).into());
        }
        if let Some((c, count)) = fence_probe.finish() {
            match fence {
                None => fence = Some((c, count)),
                Some((old, n)) if old == c && count >= n => fence = None,
                _ => {}
            }
            continue;
        }
        if fence.is_none() {
            if let Some(name) = heading_probe.finish() {
                if TASK_HEADINGS.get(heading_count) != Some(&name) {
                    invalid_headings = true;
                    continue;
                }
                if let (Some(section_start), Some(hash)) = (section_start, before_heading) {
                    sections.push(BodySlice {
                        body: body.clone(),
                        start: section_start,
                        bytes: start - section_start,
                        sha256: Some(format!("{:x}", hash.finalize())),
                    });
                }
                heading_count += 1;
                section_start = (heading_count < TASK_HEADINGS.len()).then_some(offset);
                section_digest = Sha256::new();
            }
        }
    }
    if offset != body.bytes || format!("{:x}", digest.finalize()) != body.sha256 {
        return Err(invalid_spool(true).into());
    }
    if invalid_headings || heading_count != TASK_HEADINGS.len() {
        return Ok(TaskSectionParse::InvalidHeadings);
    }
    let sections = sections
        .try_into()
        .unwrap_or_else(|_| unreachable!("four headings delimit three sections"));
    Ok(TaskSectionParse::Complete(sections))
}

pub(super) fn invalid_spooled_task_headings() -> DomainError {
    DomainError::new("invalid_task_headings", "Task headings are invalid.")
}

fn open_spool(body: &agentlaw_storage::acquire::SpoolBody, recall: bool) -> Result<std::fs::File> {
    let file = body
        .open()
        .map_err(|error| spool_open_failure(error, recall))?;
    if recall
        && file
            .metadata()
            .map_err(|error| spool_read_failure(error, recall))?
            .len()
            != body.bytes
    {
        return Err(invalid_spool(recall));
    }
    Ok(file)
}

fn spool_open_failure(error: agentlaw_storage::Error, recall: bool) -> DomainError {
    if recall {
        recall_spool_error()
    } else {
        source::<()>(Err(error)).unwrap_err()
    }
}

fn spool_read_failure(error: std::io::Error, recall: bool) -> DomainError {
    spool_open_failure(error.into(), recall)
}

fn invalid_spool(recall: bool) -> DomainError {
    if recall {
        recall_spool_error()
    } else {
        DomainError::new(
            "source_corrupt",
            "The acquired source body is incomplete or invalid.",
        )
    }
}

fn serialization_failure() -> DomainError {
    DomainError::new(
        "serialization_failed",
        "The acquired response could not be serialized.",
    )
}

fn response_write_failure(error: std::io::Error) -> RecallFailure {
    output_write_failure(error.kind())
}

fn output_write_failure(kind: std::io::ErrorKind) -> RecallFailure {
    RecallFailure::DeliveryFailed(crate::artifacts::read_delivery_failure(
        "The response file could not be written",
        kind,
    ))
}

fn write_json(out: &mut impl Write, value: &Value) -> RecallResult<()> {
    serde_json::to_writer(out, value).map_err(|error| {
        if let Some(kind) = error.io_error_kind() {
            output_write_failure(kind)
        } else {
            serialization_failure().into()
        }
    })
}
#[cfg(test)]
mod tests;
