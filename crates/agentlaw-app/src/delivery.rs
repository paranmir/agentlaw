//! Oversized complete results are version-frozen artifacts, not truncated memories.
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};
use std::{
    io::{self, Write},
    path::Path,
};

struct Count(u64);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("Size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn adapt(mut value: Value, state: &Path, limit: usize) -> Result<Value> {
    let config_path = std::path::absolute(state.join("config.json")).map_err(|_| failure())?;
    if let Some(notice) = value
        .get_mut("response_limit")
        .and_then(Value::as_object_mut)
    {
        notice.insert("config_file".into(), json!(config_path));
    }
    let mut size = Count(0);
    serde_json::to_writer(&mut size, &value).map_err(|_| failure())?;
    if size.0 <= limit as u64 {
        return Ok(value);
    }
    let directory = state.join("response-artifacts");
    let mut writer = agentlaw_flows::artifacts::ArtifactWriter::create(&directory)?;
    serde_json::to_writer(&mut writer, &value).map_err(|_| failure())?;
    let artifact = writer.finish()?;
    let mut envelope = json!({
        "code":"complete_content_in_file",
        "content_read":false,
        "artifact":artifact,
        "response_limit_bytes":limit,
        "config_path":config_path,
        "next_action":"The full acquired result, including every selected memory and required reference, is preserved in this file. Its contents have NOT been delivered into your context. Use your harness file-reading tool to read the complete result and follow any required-memory instructions before relying on it. This is a path on the Agentlaw Runtime host; if your harness cannot access that filesystem, explain the access limitation in the user's language and request file transfer. Do not infer an empty result, summarize unread content, or treat file creation as completed reading. The file is retained until the indicated expiry (at least seven days); after expiry, repeat the read to acquire current state, which may be a different version. The response limit can be changed in the indicated config file."
    });
    if let Some(status) = value.get("status") {
        envelope["operation_status"] = status.clone();
    }
    Ok(envelope)
}
fn failure() -> DomainError {
    DomainError::new("result_delivery_failed", "The complete result could not be delivered. Do not assume a mutating operation was rolled back; inspect current state before submitting a new write.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_file_is_immutable_and_small_responses_make_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let small = json!({"memories":[]});
        assert_eq!(adapt(small.clone(), &state, 4096).unwrap(), small);
        assert!(!state.exists());
        let large = json!({"memories":[{"what_to_remember":"한글\r\n".repeat(2000)}],"undelivered_required":[{"memory_id":"actual-ref"}]});
        let result = adapt(large.clone(), &state, 4096).unwrap();
        assert_eq!(result["content_read"], false);
        assert!(result.get("memories").is_none());
        let body = std::fs::read(result["artifact"]["path"].as_str().unwrap()).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), large);
        assert_eq!(
            body.len() as u64,
            result["artifact"]["bytes"].as_u64().unwrap()
        );
    }
}
