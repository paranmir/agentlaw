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
    let body=format!("# Objective\r\n{}\r\n# Current position\n```\n# Resume point\n```\n현재 위치\n# Resume point\nnext step\n# References\nref", "x".repeat(100000));
    let (mut selection, token) = selection_with_spooled_body(tmp.path(), &body);
    let parts = selection.task_sections(&token).unwrap();
    let envelope = selection
        .finish(
            json!({"objective":parts[0],"current_position":parts[1],"resume_point":parts[2]}),
            tmp.path(),
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

#[test]
fn lost_or_changed_local_spool_requires_reacquisition() {
    use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&source, local.join("canonical")).unwrap();
    let id = || uuid::Uuid::new_v4().to_string();
    let memory = id();
    let body = "Canonical body remains intact.";
    store.publish(&id(), vec![Mutation {
        unit: CurrentUnit {
            entity_id: memory.clone(),
            entity_type: "memory".into(),
            state: UnitState::Live { heads: vec![Head {
                metadata:json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":id(),"user_id":"user"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: body.into(),
            }] },
        },
        expected_versions:vec![],
        evidence:"Published fixture".into(),
    }]).unwrap();
    let canonical = source
        .join("current/memory")
        .join(&memory[..2])
        .join(format!("{memory}.md"));
    let original = std::fs::read(&canonical).unwrap();
    let acquire = || {
        let (_, mut records, missing) = store
            .acquire_closure(std::slice::from_ref(&memory))
            .unwrap();
        assert!(missing.is_empty());
        let spool = records
            .pop()
            .unwrap()
            .current
            .bodies
            .into_values()
            .next()
            .unwrap();
        let path = spool.path.clone();
        let mut selection = StreamedSelection::default();
        let token = selection.insert(spool, 0, body.len() as u64);
        (selection, path, json!({"body":token}))
    };
    for lost in [true, false] {
        let (selection, spool, response) = acquire();
        if lost {
            std::fs::rename(spool, temp.path().join("displaced-spool")).unwrap();
        } else {
            std::fs::write(spool, body.replacen('C', "X", 1)).unwrap();
        }
        let error = selection
            .finish_read(&response, &local, &RequestControl::default())
            .unwrap_err();
        assert_eq!(
            error.code, "recall_spool_unavailable",
            "FR-06.4: lost={lost}, {error:?}"
        );
        assert!(error.retryable);
        assert!(error.details.is_none());
        let action = error.next_action.unwrap().to_lowercase();
        assert!(
            action.contains("recall") && (action.contains("spool") || action.contains("copy")),
            "{action}"
        );
        assert!(!action.contains("doctor --audit"), "{action}");
        assert!(std::fs::read_dir(local.join("response-artifacts"))
            .map(|mut paths| paths.next().is_none())
            .unwrap_or_else(|error| error.kind() == std::io::ErrorKind::NotFound));
        assert_eq!(std::fs::read(&canonical).unwrap(), original);
        let (fresh, _, result) = acquire();
        assert_eq!(
            fresh.inline(&result, &RequestControl::default()).unwrap()["body"],
            body
        );
    }
}
