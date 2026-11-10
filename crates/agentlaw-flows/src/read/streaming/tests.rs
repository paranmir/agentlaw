use super::*;

fn selection_with_spooled_body(directory: &Path, body: &str) -> (StreamedSelection, String) {
    let path = directory.join("selected-body.payload");
    std::fs::write(&path, body).unwrap();
    let spool = serde_json::from_value::<agentlaw_storage::acquire::SpoolBody>(json!({
        "path": path,
        "bytes": body.len(),
        "sha256": format!("{:x}", Sha256::digest(body.as_bytes())),
    }))
    .unwrap();
    let mut selection = StreamedSelection::default();
    let token = selection.insert(spool, 0, body.len() as u64);
    (selection, token)
}

fn read_streamed_artifact(envelope: &Value) -> Value {
    assert_eq!(envelope["code"], "complete_content_in_file");
    assert_eq!(envelope["content_read"], false);
    assert_eq!(envelope["artifact"]["complete"], true);
    assert_eq!(envelope["artifact"]["format"], "json");
    assert_eq!(envelope["artifact"]["access"], "runtime_host_filesystem");
    let next_action = envelope["next_action"].as_str().unwrap();
    for guidance in [
        "has NOT been read",
        "Read the complete file with your harness file tool before relying on it.",
        "If that filesystem is inaccessible, explain the limitation and request file transfer.",
        "Do not infer an empty result or summarize unread content.",
        "After the indicated expiry, repeat the request",
    ] {
        assert!(next_action.contains(guidance), "{next_action}");
    }
    let bytes = std::fs::read(envelope["artifact"]["path"].as_str().unwrap()).unwrap();
    assert_eq!(envelope["artifact"]["bytes"], bytes.len());
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn streamed_selection_preserves_composed_turn_instruction_and_full_content() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "한글\r\n\"quote\"\\backslash\tfull body without a final newline";
    let (selection, token) = selection_with_spooled_body(tmp.path(), body);
    let instruction = concat!(
        "Required context is unavailable. Explain this limitation in the user's language; ",
        "do not treat absent required context as reviewed. ",
        "Briefly disclose this recall's incomplete semantic search in the user's language; ",
        "combine any required-context warning into the same sentence. ",
        "Omit only an unchanged semantic notice already visible for this task. ",
        "Do not auto-retry or repair."
    );
    let original = json!({
        "status": "needs_user_input",
        "turn_instruction": instruction,
        "memories": [{
            "memory_id": "selected-memory",
            "current_heads": [{"what_to_remember": token}],
        }],
        "undelivered_required": [{"memory_id": "missing-required-memory"}],
        "diagnostics": [{
            "code": "semantic_channel_incomplete",
            "message": "Semantic retrieval failed; exact and indexed lexical retrieval remain available.",
            "retryable": false,
        }],
    });
    let envelope = selection
        .finish(original.clone(), tmp.path(), &RequestControl::default())
        .unwrap();
    assert_eq!(envelope["turn_instruction"], instruction);
    assert_eq!(envelope["operation_status"], original["status"]);
    let complete = read_streamed_artifact(&envelope);
    let mut expected = original;
    expected["memories"][0]["current_heads"][0]["what_to_remember"] = json!(body);
    assert_eq!(complete, expected);
}

#[test]
fn streamed_selection_without_turn_instruction_keeps_field_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "Complete selected body with no turn instruction.";
    let (selection, token) = selection_with_spooled_body(tmp.path(), body);
    let original = json!({
        "memories": [{
            "memory_id": "selected-memory",
            "current_heads": [{"what_to_remember": token}],
        }],
        "candidates": [],
    });
    let envelope = selection
        .finish(original.clone(), tmp.path(), &RequestControl::default())
        .unwrap();
    assert!(envelope.get("turn_instruction").is_none());
    assert!(envelope.get("operation_status").is_none());
    let complete = read_streamed_artifact(&envelope);
    assert!(complete.get("turn_instruction").is_none());
    let mut expected = original;
    expected["memories"][0]["current_heads"][0]["what_to_remember"] = json!(body);
    assert_eq!(complete, expected);
}

#[test]
fn empty_streamed_selection_preserves_empty_and_inline_responses_without_artifact() {
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("local");
    let selection = StreamedSelection::default();
    for original in [
        json!({}),
        json!({
            "status": "ready",
            "turn_instruction": "Continue using this complete inline response.",
            "memories": [{"what_to_remember": "Complete inline body."}],
            "next_action": "Use the complete inline result.",
        }),
    ] {
        let result = selection
            .finish(original.clone(), &local, &RequestControl::default())
            .unwrap();
        assert_eq!(result, original);
        assert!(result.get("artifact").is_none());
        assert!(!local.exists());
    }
}

#[test]
fn streamed_task_sections_preserve_complete_text_and_ignore_fenced_headings() {
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(tmp.path().join("source"), tmp.path().join("local"), "user").unwrap();
    let body=format!("# Objective\r\n{}\r\n# Current position\n```\n# Resume point\n```\n현재 위치\n# Resume point\nnext step\n# References\nref", "x".repeat(100000));
    let written=runtime.call(parse_request(&json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"fixture","applies_to":["user"]}]}}).to_string()).unwrap()).unwrap();
    let acquired = runtime
        .store
        .acquire_current(
            written["results"][0]["memory_ref"]["memory_id"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let spool = acquired.bodies.values().next().unwrap().clone();
    let mut selection = StreamedSelection::default();
    let bytes = spool.bytes;
    let token = selection.insert(spool, 0, bytes);
    let parts = selection.task_sections(&token).unwrap();
    let envelope = selection
        .finish(
            json!({"objective":parts[0],"current_position":parts[1],"resume_point":parts[2]}),
            &runtime.local,
            &RequestControl::default(),
        )
        .unwrap();
    let value: Value = serde_json::from_reader(
        std::fs::File::open(envelope["artifact"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let expected = crate::write::task_sections(&body).unwrap();
    assert_eq!(value["objective"], expected[0]);
    assert_eq!(value["current_position"], expected[1]);
    assert_eq!(value["resume_point"], expected[2]);
}
#[test]
fn exact_current_larger_than_owned_budget_is_delivered_as_whole_file() {
    use agentlaw_storage::codec;
    use std::io::Cursor;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("source");
    let mut runtime = Runtime::open(&root, tmp.path().join("local"), "user").unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let change = uuid::Uuid::new_v4().to_string();
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":change,"applicability":{"scope":"user"},"origin":{"machine_id":runtime.machine_id,"user_id":"user"},"recorded_at_ms":0,"is_rule":true,"relations":[],"work_targets":[]}),
                body: String::new(),
            }],
        },
    };
    let frames = codec::read("current", &mut Cursor::new(unit.encode().unwrap())).unwrap();
    let state = &frames[0];
    let block = vec![b'x'; 65536];
    let repetitions = 1025u64;
    let bytes = block.len() as u64 * repetitions;
    let mut hash = Sha256::new();
    for _ in 0..repetitions {
        hash.update(&block)
    }
    let digest = format!("{:x}", hash.finalize());
    let parent = root.join("current/memory").join(&id[..2]);
    std::fs::create_dir_all(&parent).unwrap();
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(parent.join(format!("{id}.md"))).unwrap());
    writeln!(file, "<!-- agentlaw-file-v1 kind=current -->").unwrap();
    writeln!(
        file,
        "<!-- agentlaw-record-v1 type=state key={id} bytes={} sha256={} -->",
        state.payload.len(),
        codec::digest(&state.payload)
    )
    .unwrap();
    file.write_all(&state.payload).unwrap();
    file.write_all(b"\n<!-- /agentlaw-record-v1 -->\n").unwrap();
    writeln!(
        file,
        "<!-- agentlaw-record-v1 type=body key={change} bytes={bytes} sha256={digest} -->"
    )
    .unwrap();
    for _ in 0..repetitions {
        file.write_all(&block).unwrap()
    }
    file.write_all(b"\n<!-- /agentlaw-record-v1 -->\n<!-- /agentlaw-file-v1 records=2 -->\n")
        .unwrap();
    file.flush().unwrap();
    drop(file);
    let result = runtime
        .call(
            parse_request(&json!({"action":"recall","recall":{"memory_ids":[id]}}).to_string())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    assert_eq!(result["content_read"], false);
    assert!(result["artifact"]["bytes"].as_u64().unwrap() > bytes);
    let length = std::fs::metadata(result["artifact"]["path"].as_str().unwrap())
        .unwrap()
        .len();
    assert_eq!(length, result["artifact"]["bytes"].as_u64().unwrap());
    let restored = runtime
        .call(
            parse_request(
                &json!({"action":"recall","recall":{"recall_for":"unrelated ordinary work"}})
                    .to_string(),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(restored["code"], "complete_content_in_file");
    assert!(restored["artifact"]["bytes"].as_u64().unwrap() > bytes);
    let mut file = std::fs::File::open(restored["artifact"]["path"].as_str().unwrap()).unwrap();
    let mut prefix = vec![0; 4096];
    let n = file.read(&mut prefix).unwrap();
    let prefix = String::from_utf8_lossy(&prefix[..n]);
    assert!(prefix.contains("candidate_counts"));
    assert!(prefix.contains("\"is_rule\":true"));
    assert!(!prefix.contains("agentlaw-owned-body"));
}
#[test]
fn streamed_exact_result_preserves_body_and_observed_version() {
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(tmp.path().join("source"), tmp.path().join("local"), "user").unwrap();
    let body = "한글\r\n\"quote\"\\backslash\twithout final newline";
    let written=runtime.call(parse_request(&json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":body,"evidence":"fixture","applies_to":["user"]}]}}).to_string()).unwrap()).unwrap();
    let reference = written["results"][0]["memory_ref"].clone();
    let Request::Recall(request) = parse_request(
        &json!({"action":"recall","recall":{"memory_ids":[reference["memory_id"]]}}).to_string(),
    )
    .unwrap() else {
        panic!()
    };
    let envelope = runtime.stream_exact_recall(&request).unwrap();
    assert_eq!(envelope["content_read"], false);
    let result: Value = serde_json::from_slice(
        &std::fs::read(envelope["artifact"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        result["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
    assert_eq!(
        result["memories"][0]["current_heads"][0]["memory_ref"],
        reference
    );
}

#[test]
fn cancelled_file_delivery_removes_its_incomplete_artifact() {
    let tmp = tempfile::tempdir().unwrap();
    let (selection, token) = selection_with_spooled_body(tmp.path(), "selected body");
    let control = RequestControl::default();
    control
        .cancel
        .store(true, std::sync::atomic::Ordering::Release);
    let error = selection
        .finish_read(&json!({"body":token}), tmp.path(), &control)
        .unwrap_err();
    assert_eq!(error.code, "cancelled");
    let directory = tmp.path().join("response-artifacts");
    assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0);
}

#[test]
fn unicode_indented_fences_match_owned_task_parsing() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "# Objective\nGoal\n# Current position\n\u{2003}```\n# Resume point\n\u{2003}```\nCurrent\n# Resume point\nNext\n# References\nNone\n";
    let expected = crate::write::task_sections(body).unwrap();
    let (mut selection, token) = selection_with_spooled_body(tmp.path(), body);
    let sections = selection.task_sections(&token).unwrap();
    let actual = selection
        .inline(&json!(sections), &RequestControl::default())
        .unwrap();
    assert_eq!(actual, json!(expected));
    assert!(actual[1].as_str().unwrap().contains("# Resume point"));
}
