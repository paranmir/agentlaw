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
    if let Some(instruction) = value.get("turn_instruction") {
        envelope["turn_instruction"] = instruction.clone();
    }
    Ok(envelope)
}
fn failure() -> DomainError {
    DomainError::new("result_delivery_failed", "The complete result could not be delivered. Do not assume a mutating operation was rolled back; inspect current state before submitting a new write.")
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSED_INSTRUCTION: &str = concat!(
        "Required context is unavailable. Explain this limitation in the user's language; do not treat absent required context as reviewed. ",
        "Briefly disclose this recall's incomplete semantic search in the user's language; combine any required-context warning into the same sentence. Omit only an unchanged semantic notice already visible for this task. Do not auto-retry or repair."
    );

    fn complete_result() -> Value {
        json!({
            "status":"complete",
            "memories":[{"what_to_remember":"한글\r\n".repeat(64)}],
            "undelivered_required":[{"memory_id":"actual-ref"}],
            "diagnostics":[{
                "code":"semantic_channel_incomplete",
                "message":"Semantic retrieval failed; exact and indexed lexical retrieval remain available.",
                "retryable":false
            }]
        })
    }

    fn assert_complete_artifact(result: &Value, original: &Value) {
        let body = std::fs::read(result["artifact"]["path"].as_str().unwrap()).unwrap();
        assert_eq!(body, serde_json::to_vec(original).unwrap());
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), *original);
        assert_eq!(
            body.len() as u64,
            result["artifact"]["bytes"].as_u64().unwrap()
        );
    }

    #[test]
    fn complete_file_is_immutable_and_small_responses_make_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        for small in [
            json!({"memories":[]}),
            json!({"memories":[],"turn_instruction":COMPOSED_INSTRUCTION}),
        ] {
            assert_eq!(adapt(small.clone(), &state, 4096).unwrap(), small);
            assert!(!state.exists());
        }
        let large = complete_result();
        let result = adapt(large.clone(), &state, 512).unwrap();
        assert_eq!(result["code"], "complete_content_in_file");
        assert_eq!(result["content_read"], false);
        assert_eq!(result["operation_status"], large["status"]);
        assert_eq!(result["response_limit_bytes"], 512);
        assert_eq!(
            result["config_path"],
            json!(std::path::absolute(state.join("config.json")).unwrap())
        );
        assert!(result.get("memories").is_none());
        assert!(result.get("turn_instruction").is_none());
        assert!(result["next_action"]
            .as_str()
            .unwrap()
            .contains("Its contents have NOT been delivered into your context."));
        assert_complete_artifact(&result, &large);
    }

    #[test]
    fn oversized_composed_instruction_preserves_envelope_and_complete_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let mut original = complete_result();
        let without_instruction = adapt(original.clone(), &state, 512).unwrap();
        original["turn_instruction"] = json!(COMPOSED_INSTRUCTION);
        let result = adapt(original.clone(), &state, 512).unwrap();

        assert_eq!(result["turn_instruction"], COMPOSED_INSTRUCTION);
        for field in [
            "code",
            "operation_status",
            "next_action",
            "content_read",
            "response_limit_bytes",
            "config_path",
        ] {
            assert_eq!(result[field], without_instruction[field], "{field}");
        }
        assert_complete_artifact(&result, &original);
    }
}
