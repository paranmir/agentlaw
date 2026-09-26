//! Transport adapters. Business meaning stays in the shared runtime.
use agentlaw_contracts::{parse_request, DomainError, Request, Result, INPUT_SCHEMA};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub mod config;
pub mod delivery;
pub mod diagnostics;
pub mod git_ops;
pub mod history_export;
pub mod install;
pub mod installed;
pub mod inventory;
pub mod machine;
pub mod setup;
use agentlaw_flows::history_diff as stream_diff;
pub mod transport;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const GUIDANCE: &str = include_str!("../../../docs/contracts/agentlaw-llm-guidance.md");

pub fn tool_description() -> &'static str {
    description_from_guidance(GUIDANCE)
        .expect("accepted tool description must exist in the source contract")
}

fn description_from_guidance(guidance: &str) -> Option<&str> {
    let (_, after_fence) = guidance.split_once("```text")?;
    // Git may check the Markdown contract out with Windows line endings.
    let body = after_fence
        .strip_prefix("\r\n")
        .or_else(|| after_fence.strip_prefix('\n'))?;
    let (body, _) = body.split_once("\n```")?;
    Some(body.strip_suffix('\r').unwrap_or(body))
}

pub fn schema() -> Value {
    json!({"name":"agentlaw", "description":tool_description(),
        "inputSchema":serde_json::from_str::<Value>(INPUT_SCHEMA).expect("checked schema")})
}

pub trait Backend {
    fn call(&mut self, request: Request) -> Result<Value>;
    fn call_with_control(
        &mut self,
        request: Request,
        control: agentlaw_flows::RequestControl,
    ) -> Result<Value> {
        control.check()?;
        self.call(request)
    }
}

pub fn call_json(backend: &mut impl Backend, input: &str) -> Result<Value> {
    backend.call(parse_request(input)?)
}

#[derive(Default)]
pub struct McpSession {
    initialized: bool,
    ready: bool,
}

impl McpSession {
    pub fn handle(&mut self, raw: &str, backend: &mut impl Backend) -> Option<Value> {
        let value = match agentlaw_contracts::validation::decode_unique(raw) {
            Ok(v) => v,
            Err(_) => return Some(rpc_error(Value::Null, -32700, "Invalid JSON message.")),
        };
        let id = value.get("id").cloned();
        if !value.is_object()
            || value.get("jsonrpc") != Some(&json!("2.0"))
            || id
                .as_ref()
                .is_some_and(|id| !id.is_string() && id.as_i64().is_none() && id.as_u64().is_none())
        {
            return Some(rpc_error(Value::Null, -32600, "Invalid JSON-RPC request."));
        }
        let method = match value.get("method").and_then(Value::as_str) {
            Some(m) => m,
            None => {
                return Some(rpc_error(
                    id.unwrap_or(Value::Null),
                    -32600,
                    "Missing method.",
                ))
            }
        };
        if id.is_none() {
            if method == "notifications/initialized" && self.initialized {
                self.ready = true;
            }
            // Notifications never execute a tool or receive a JSON-RPC response.
            return None;
        }
        let id = id.unwrap();
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Some(rpc_error(id, -32602, "Expected object params."));
        }
        let result = match method {
            "initialize" => {
                if self.initialized {
                    return Some(rpc_error(id, -32600, "Already initialized."));
                }
                if params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .is_none()
                    || !params.get("capabilities").is_some_and(Value::is_object)
                    || !params.get("clientInfo").is_some_and(Value::is_object)
                {
                    return Some(rpc_error(id, -32602, "Missing initialization fields."));
                }
                self.initialized = true;
                json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{"listChanged":false}},
                    "serverInfo":{"name":"agentlaw","version":env!("CARGO_PKG_VERSION")}})
            }
            "ping" => json!({}),
            _ if !self.ready => {
                return Some(rpc_error(id, -32000, "Initialize the MCP session first."))
            }
            "tools/list" => {
                if params.get("cursor").is_some() {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "This tool list has no continuation cursor.",
                    ));
                }
                json!({"tools":[schema()]})
            }
            "tools/call" => {
                if params.get("name") != Some(&json!("agentlaw")) {
                    return Some(rpc_error(id, -32602, "Unknown tool."));
                }
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let (body, failed) = match call_json(backend, &arguments.to_string()) {
                    Ok(body) => (body, false),
                    Err(error) => (error_payload(&error), true),
                };
                // The text copy is required for clients that do not surface structuredContent.
                json!({"content":[{"type":"text","text":body.to_string()}],"structuredContent":body,"isError":failed})
            }
            _ => return Some(rpc_error(id, -32601, "Unknown method.")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

pub fn error_payload(error: &DomainError) -> Value {
    json!({"code":error.code,"message":error.message,"retryable":error.retryable,
        "next_action":"Explain the issue in the user's language. Do not treat it as an empty or successful result."})
}

pub fn exit_code(error: &DomainError) -> i32 {
    if error.code.starts_with("invalid_")
        || error.code.ends_with("_required")
        || matches!(
            error.code.as_str(),
            "needs_decision" | "unsupported" | "project_not_found"
        )
    {
        2
    } else {
        1
    }
}

/// Read at most one bounded UTF-8 JSON-RPC line; never truncate a message into a valid request.
pub fn read_message(reader: &mut impl BufRead) -> Result<Option<String>> {
    let mut message = Vec::new();
    loop {
        let buf = reader
            .fill_buf()
            .map_err(|_| DomainError::new("transport_io", "Could not read transport input."))?;
        if buf.is_empty() {
            break;
        }
        let count = buf
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(buf.len());
        if message
            .len()
            .checked_add(count)
            .is_none_or(|n| n > MAX_REQUEST_BYTES)
        {
            return Err(DomainError::new("transport_capacity", "Request exceeds the supported 16 MiB transport frame. No part of this request was executed."));
        }
        message.extend_from_slice(&buf[..count]);
        let ended = buf[count - 1] == b'\n';
        reader.consume(count);
        if ended {
            break;
        }
    }
    if message.is_empty() {
        return Ok(None);
    }
    String::from_utf8(message)
        .map(Some)
        .map_err(|_| DomainError::new("invalid_encoding", "Expected UTF-8 JSON input."))
}

pub fn serve_stdio(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    backend: &mut impl Backend,
) -> Result<()> {
    let mut session = McpSession::default();
    while let Some(line) = read_message(reader)? {
        if let Some(response) = session.handle(&line, backend) {
            serde_json::to_writer(&mut *writer, &response)
                .map_err(|_| DomainError::new("transport_io", "Could not write the response."))?;
            writer
                .write_all(b"\n")
                .and_then(|_| writer.flush())
                .map_err(|_| DomainError::new("transport_io", "Could not flush the response."))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestBackend(usize);
    impl Backend for TestBackend {
        fn call(&mut self, _: Request) -> Result<Value> {
            self.0 += 1;
            Ok(json!({"memories":[]}))
        }
    }
    fn ready(s: &mut McpSession, b: &mut TestBackend) {
        let response=s.handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,b).unwrap();
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(s
            .handle(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                b
            )
            .is_none());
    }
    #[test]
    fn schema_has_one_tool_and_exact_description() {
        let mut s = McpSession::default();
        let mut b = TestBackend(0);
        ready(&mut s, &mut b);
        let r = s
            .handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &mut b)
            .unwrap();
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 1);
        assert_eq!(r["result"]["tools"][0]["name"], "agentlaw");
        assert!(tool_description().starts_with("Use recall"));
    }
    #[test]
    fn description_accepts_git_line_endings_and_requires_complete_fences() {
        let body = "Use recall before work.\nPreserve the next line verbatim.";
        let guidance = format!("# Contract\n\n```text\n{body}\n```\nNot part of the description.");
        assert_eq!(description_from_guidance(&guidance), Some(body));
        let windows_guidance = guidance.replace('\n', "\r\n");
        let windows_body = body.replace('\n', "\r\n");
        assert_eq!(
            description_from_guidance(&windows_guidance),
            Some(windows_body.as_str())
        );
        assert_eq!(description_from_guidance("No accepted description."), None);
        assert_eq!(description_from_guidance("```text\nUnclosed."), None);
        assert_eq!(
            description_from_guidance("```text-inline\nWrong fence.\n```"),
            None
        );
    }
    #[test]
    fn tool_call_has_equal_text_and_structured_output() {
        let mut s = McpSession::default();
        let mut b = TestBackend(0);
        ready(&mut s, &mut b);
        let r=s.handle(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","memory_ids":["m"]}}}"#,&mut b).unwrap();
        assert_eq!(b.0, 1);
        assert_eq!(r["result"]["isError"], false);
        let text: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, r["result"]["structuredContent"]);
    }
    #[test]
    fn invalid_and_notifications_never_execute() {
        let mut s = McpSession::default();
        let mut b = TestBackend(0);
        ready(&mut s, &mut b);
        let r=s.handle(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"erase"}}}"#,&mut b).unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(b.0, 0);
        assert!(s.handle(r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall_for":"x"}}}"#,&mut b).is_none());
        assert_eq!(b.0, 0);
    }
    #[test]
    fn rejects_duplicate_keys_without_echoing_data() {
        let mut s = McpSession::default();
        let mut b = TestBackend(0);
        let r = s
            .handle(r#"{"jsonrpc":"2.0","id":1,"id":2,"method":"ping"}"#, &mut b)
            .unwrap();
        assert_eq!(r["error"]["code"], -32700);
    }
}
