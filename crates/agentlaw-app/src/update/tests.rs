use super::*;
use std::fs;

#[test]
fn final_transport_result_preserves_memory_and_repeats_visible_advice() {
    use std::io::{self, Write};
    use std::sync::mpsc;
    struct Output {
        tx: mpsc::Sender<Value>,
        buf: Vec<u8>,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            for byte in bytes {
                if *byte == b'\n' {
                    self.tx
                        .send(serde_json::from_slice(&self.buf).unwrap())
                        .unwrap();
                    self.buf.clear();
                } else {
                    self.buf.push(*byte);
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    struct Immediate;
    impl crate::Backend for Immediate {
        fn call(&mut self, _: agentlaw_contracts::Request) -> agentlaw_contracts::Result<Value> {
            Ok(json!({"status":"remembered","results":[]}))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let advisor = Advisor {
        state: root.path().to_path_buf(),
        current: Arc::new(Mutex::new(Cache {
            last_success: Some(now()),
            retry_after: None,
            latest_tag: Some("v99.0.0".into()),
        })),
        running: Arc::new(Mutex::new(false)),
    };
    let lines = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall":{"recall_for":"test"}}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall":{"recall_for":"test"}}}}),
    ];
    let input = lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let (tx, rx) = mpsc::channel();
    crate::transport::serve_with_advisor(
        std::io::Cursor::new(input),
        Output { tx, buf: vec![] },
        Immediate,
        Some(advisor),
    )
    .unwrap();
    let responses: Vec<Value> = rx.try_iter().collect();
    assert_eq!(responses.len(), 3);
    for response in &responses[1..] {
        let result = &response["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["status"], "remembered");
        assert_eq!(
            result["structuredContent"]["update_notice"]["latest_version"],
            "v99.0.0"
        );
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
    }
}

#[test]
fn only_full_published_versions_are_admitted() {
    assert_eq!(version("v0.2.7"), Some([0, 2, 7]));
    assert_eq!(version("v0.2.7-rc1"), None);
    assert_eq!(version("v0.2.7.1"), None);
    assert_eq!(
        parse_release(br#"{"tag_name":"v0.2.7","draft":false,"prerelease":false}"#),
        Some("v0.2.7".into())
    );
    assert_eq!(
        parse_release(br#"{"tag_name":"v0.2.7-rc1","draft":false,"prerelease":false}"#),
        None
    );
    assert_eq!(
        parse_release(br#"{"tag_name":"v0.2.8","draft":true,"prerelease":false}"#),
        None
    );
}

#[test]
fn invalid_cache_is_unknown_and_does_not_block_advice() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("update-check.json"), b"not json").unwrap();
    let cache = Cache {
        last_success: Some(now()),
        retry_after: None,
        latest_tag: Some("v99.0.0".into()),
    };
    let advisor = Advisor {
        state: root.path().to_path_buf(),
        current: Arc::new(Mutex::new(cache)),
        running: Arc::new(Mutex::new(false)),
    };
    let notice = advisor.notice().unwrap();
    assert_eq!(notice["latest_version"], "v99.0.0");
    let next_action = notice["next_action"].as_str().unwrap();
    assert!(next_action.contains("After explicit user approval"));
    assert!(next_action.contains("installed public agentlaw update"));
    assert!(!next_action.contains("preview"));
}

#[test]
fn conditional_guidance_matches_build_consumed_contract() {
    let guidance =
        include_str!("../../../../docs/contracts/agentlaw-llm-guidance.md").replace("\r\n", "\n");
    let section = guidance
        .split_once("## Conditional release advisory in a tool result")
        .unwrap()
        .1;
    let body = section
        .split_once("```text\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    // Git checkout line endings may occur inside this multiline fence.
    // Compare the complete wording with the same normalization on both sides.
    assert_eq!(body, crate::update_notice_guidance().replace("\r\n", "\n"));
}

#[test]
fn ordinary_notice_ignores_update_plan_history() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    fs::create_dir_all(state.join("update-plans")).unwrap();
    for index in 0..70 {
        fs::write(
            state.join("update-plans").join(format!("{index}.json")),
            b"corrupt historical plan",
        )
        .unwrap();
    }
    let advisor = Advisor {
        state,
        current: Arc::new(Mutex::new(Cache {
            last_success: Some(now()),
            retry_after: None,
            latest_tag: Some(format!("v{}", env!("CARGO_PKG_VERSION"))),
        })),
        running: Arc::new(Mutex::new(false)),
    };
    assert!(advisor.notice().is_none());
}
