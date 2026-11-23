//! Exceptional exact reads serialize owned body spools without building a giant Value.
use super::*;
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
fn failed() -> DomainError {
    DomainError::new(
        "result_delivery_failed",
        "Cannot preserve the complete acquired result; no partial content is represented as read.",
    )
}
fn write_json(out: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(out, value).map_err(|_| failed())
}
#[cfg(test)]
mod tests;
