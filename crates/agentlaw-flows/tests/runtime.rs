use agentlaw_flows::{parse_request as parse_public_request, Runtime};
use serde_json::{json, Value};
fn parse_request(input: &str) -> agentlaw_flows::Result<agentlaw_flows::Request> {
    let mut fields = serde_json::from_str::<Value>(input)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    let action = fields
        .remove("action")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let mut grouped = serde_json::Map::new();
    grouped.insert("action".into(), Value::String(action.clone()));
    grouped.insert(action, Value::Object(fields));
    parse_public_request(&Value::Object(grouped).to_string())
}
fn call(runtime: &mut Runtime, input: Value) -> Value {
    runtime
        .call(parse_request(&input.to_string()).unwrap())
        .unwrap()
}
fn create(body: &str) -> Value {
    json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed by test","applies_to":["user"]}]})
}

#[test]
fn same_words_in_different_applicability_are_not_a_duplicate_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    call(
        &mut runtime,
        json!({"action":"connect_project_memory","intent":"create","project_path":"C:/work/scoped","project_name":"Scoped"}),
    );
    let body = "Use argument arrays when a path contains spaces.";
    let user = call(&mut runtime, create(body));
    let project = call(
        &mut runtime,
        json!({"action":"remember_this","project_path":"C:/work/scoped","memories":[{"operation":"create","what_to_remember":body,"evidence":"Confirmed separately for this project.","applies_to":["project"]}]}),
    );
    assert_eq!(user["status"], "remembered");
    assert_eq!(
        project["status"], "remembered",
        "distinct applicability must not force a duplicate review: {project}"
    );
    let user_id = &user["results"][0]["memory_ref"]["memory_id"];
    let project_id = &project["results"][0]["memory_ref"]["memory_id"];
    assert_ne!(user_id, project_id);
    let result = call(
        &mut runtime,
        json!({"action":"recall","memory_ids":[user_id,project_id]}),
    );
    assert_eq!(
        result["memories"].as_array().unwrap().len(),
        2,
        "exact ID recall is allowed across scope: {result}"
    );
}

#[test]
fn fabricated_resolution_does_not_publish_or_destroy_retained_proposals() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    call(&mut runtime, create("Verified shell command convention."));
    let pending = call(&mut runtime, create("Verified shell command convention."));
    assert_eq!(pending["status"], "resolution_required");
    let request = parse_request(&json!({"action":"remember_this","pending_action":"continue","pending_batch_ref":pending["pending_batch_ref"],"resolution_ref":uuid::Uuid::new_v4().to_string()}).to_string()).unwrap();
    assert_eq!(
        runtime.call(request).unwrap_err().code,
        "invalid_resolution_ref"
    );
    drop(runtime);
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let inspect = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"inspect","pending_batch_ref":pending["pending_batch_ref"]}),
    );
    assert_eq!(inspect["status"], "pending");
    assert_eq!(
        inspect["proposals"][0]["what_to_remember"],
        "Verified shell command convention."
    );
    let store = agentlaw_storage::Store::open(&root, local.join("canonical")).unwrap();
    assert_eq!(
        store.snapshot().unwrap().1.len(),
        1,
        "invalid approval silently published a duplicate"
    );
}
#[test]
fn unrelated_publication_does_not_invalidate_same_scope_evolution() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let first = call(&mut runtime, create("alpha subject"))["results"][0]["memory_ref"].clone();
    let second = call(&mut runtime, create("zebra unrelated"))["results"][0]["memory_ref"].clone();
    let store = agentlaw_storage::Store::open(&root, local.join("canonical")).unwrap();
    let changed = AtomicBool::new(false);
    let control =
        agentlaw_flows::RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
            if phase == "publishing" && !changed.swap(true, Ordering::SeqCst) {
                let mut unit = store
                    .read_current(second["memory_id"].as_str().unwrap())
                    .unwrap();
                let refs = unit.references().unwrap();
                if let agentlaw_storage::UnitState::Live { heads } = &mut unit.state {
                    heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
                    heads[0].body = "zebra independently revised".into();
                }
                store
                    .publish(
                        &uuid::Uuid::new_v4().to_string(),
                        vec![agentlaw_storage::Mutation {
                            unit,
                            expected_versions: refs
                                .into_iter()
                                .map(|r| r.observed_version)
                                .collect(),
                            evidence: "Independent change".into(),
                        }],
                    )
                    .unwrap();
            }
        });
    let request=parse_request(&json!({"action":"remember_this","memories":[{"operation":"evolve","parent_refs":[first],"what_to_remember":"alpha revised","evidence":"Requested correction"}]}).to_string()).unwrap();
    assert_eq!(
        runtime.call_with_control(request, control).unwrap()["status"],
        "remembered"
    );
}
#[test]
fn active_task_candidates_copy_only_complete_handoff_sections() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    call(
        &mut runtime,
        json!({"action":"connect_project_memory","intent":"create","project_path":"C:/work/task","project_name":"Task"}),
    );
    let body="# Objective\nComplete the migration.\n# Current position\nFixtures verified.\n# Resume point\nRun the final checks.\n# References\nPrivate full reference section.\n";
    call(
        &mut runtime,
        json!({"action":"remember_this","project_path":"C:/work/task","memories":[{"operation":"create","what_to_remember":body,"evidence":"Current work","applies_to":["project"],"in_working_set":true}]}),
    );
    let result = call(
        &mut runtime,
        json!({"action":"recall","project_path":"C:/work/task","recall_for":"resume","include_active_tasks":true}),
    );
    let c = &result["candidates"][0];
    assert_eq!(c["objective"], "Complete the migration.\n");
    assert_eq!(c["resume_point"], "Run the final checks.\n");
    assert!(c.get("excerpt").is_none());
    assert!(c.get("applicability").is_none());
    assert_eq!(result["active_task_count"], 1);
    assert!(result.get("task_instruction").is_none());
}
#[test]
fn repair_rebuilds_a_private_generation_and_preserves_pending() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    call(&mut runtime, create("repair needle"));
    let pending = call(&mut runtime, create("repair needle"));
    drop(runtime);
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let result = runtime
        .repair_derived(agentlaw_flows::RequestControl::default())
        .unwrap();
    assert_eq!(result["lexical_complete"], true);
    let recall = call(
        &mut runtime,
        json!({"action":"recall","recall_for":"repair needle"}),
    );
    assert_eq!(recall["candidates"].as_array().unwrap().len(), 1);
    let inspect = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"inspect","pending_batch_ref":pending["pending_batch_ref"]}),
    );
    assert_eq!(inspect["proposals"][0]["what_to_remember"], "repair needle");
}
#[test]
fn cancelled_publication_is_retained_but_never_autopublished() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let control = agentlaw_flows::RequestControl::new(cancel, move |phase| {
        if phase == "publishing" {
            flag.store(true, Ordering::Relaxed)
        }
    });
    let result = runtime
        .call_with_control(
            parse_request(&create("cancel before decision").to_string()).unwrap(),
            control,
        )
        .unwrap();
    assert_eq!(result["status"], "pending");
    drop(runtime);
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let recall = call(
        &mut runtime,
        json!({"action":"recall","recall_for":"cancel before decision"}),
    );
    assert!(recall["candidates"].as_array().unwrap().is_empty());
    let resumed = call(
        &mut runtime,
        json!({"action":"remember_this","pending_batch_ref":result["pending_batch_ref"],"pending_action":"continue","resolution_ref":result["resolution_ref"]}),
    );
    assert_eq!(resumed["status"], "remembered");
}
#[test]
fn interrupted_authoring_exposes_submission_and_fresh_context() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let id = call(&mut runtime, create("Evidence for safe migration"))["results"][0]["memory_ref"]
        ["memory_id"]
        .clone();
    let preparation = json!({"action":"remember_this","procedure":{"operation":"create","evidence_memory_ids":[id]}});
    let context = call(&mut runtime, preparation.clone());
    let submission = json!({"action":"remember_this","authoring_ref":context["authoring_ref"],"procedure":{"operation":"create","evidence_memory_ids":[id],"applies_to":["user"],"name":"Safe migration","use_when":"Migration","instructions":"Run every complete check.","evidence":"Observed migration checks."}});
    let flag = Arc::new(AtomicBool::new(false));
    let cancellation = flag.clone();
    let control = agentlaw_flows::RequestControl::new(flag, move |p| {
        if p == "publishing" {
            cancellation.store(true, Ordering::Relaxed)
        }
    });
    let cancelled = runtime
        .call_with_control(parse_request(&submission.to_string()).unwrap(), control)
        .unwrap();
    assert_eq!(cancelled["code"], "cancelled");
    drop(runtime);
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let inspect = submission.clone();
    let fresh = call(&mut runtime, inspect);
    assert_ne!(fresh["authoring_ref"], context["authoring_ref"]);
    assert_eq!(
        fresh["retained_submission"]["procedure"]["instructions"],
        "Run every complete check."
    );
    let mut final_submission = submission;
    final_submission["authoring_ref"] = fresh["authoring_ref"].clone();
    let written = call(&mut runtime, final_submission.clone());
    assert_eq!(written["status"], "remembered");
    assert_eq!(
        call(&mut runtime, final_submission),
        written,
        "same authorized execution retry must return its receipt"
    );
}
#[test]
fn punctuation_only_body_equality_is_not_lost_by_lexical_analyzer() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    call(&mut runtime, create("!!! ..."));
    assert_eq!(
        call(&mut runtime, create("!!! ..."))["status"],
        "resolution_required"
    );
}
#[test]
fn installation_machine_identity_is_validated_across_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let machine = uuid::Uuid::new_v4().to_string();
    let runtime = Runtime::open_with_machine(
        dir.path().join("a"),
        dir.path().join("la"),
        "user",
        &machine,
    )
    .unwrap();
    assert_eq!(runtime.machine_id(), machine);
    drop(runtime);
    assert!(Runtime::open_with_machine(
        dir.path().join("a"),
        dir.path().join("la"),
        "user",
        &uuid::Uuid::new_v4().to_string()
    )
    .is_err());
    let other = Runtime::open_with_machine(
        dir.path().join("b"),
        dir.path().join("lb"),
        "user",
        &machine,
    )
    .unwrap();
    assert_eq!(other.machine_id(), machine);
}
#[test]
fn durable_create_recall_evolve_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let machine = runtime.machine_id().to_owned();
    let written = call(&mut runtime, create("first body"));
    assert_eq!(written["status"], "remembered");
    let reference = written["results"][0]["memory_ref"].clone();
    let recall = call(
        &mut runtime,
        json!({"action":"recall","memory_ids":[reference["memory_id"]]}),
    );
    assert_eq!(
        recall["memories"][0]["current_heads"][0]["what_to_remember"],
        "first body"
    );
    assert!(recall.get("candidate_counts").is_none());
    let evolved = call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"evolve","parent_refs":[reference],"what_to_remember":"second body","evidence":"New observation"}]}),
    );
    assert_eq!(evolved["status"], "remembered");
    drop(runtime);
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    assert_eq!(runtime.machine_id(), machine);
    let recall = call(
        &mut runtime,
        json!({"action":"recall","memory_ids":[reference["memory_id"]]}),
    );
    assert_eq!(
        recall["memories"][0]["current_heads"][0]["what_to_remember"],
        "second body"
    );
}
#[test]
fn pending_survives_restart_and_confirmation_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    call(&mut runtime, create("one"));
    let pending = call(&mut runtime, create("one"));
    assert_eq!(pending["status"], "resolution_required");
    drop(runtime);
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let inspected = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"inspect","pending_batch_ref":pending["pending_batch_ref"]}),
    );
    assert_eq!(inspected["status"], "pending");
    let result = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"continue","pending_batch_ref":inspected["pending_batch_ref"],"resolution_ref":inspected["resolution_ref"]}),
    );
    assert_eq!(result["status"], "remembered");
}
#[test]
fn project_discovery_does_not_create_identity() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let discover = call(
        &mut runtime,
        json!({"action":"connect_project_memory","project_path":"C:/work/example"}),
    );
    assert_eq!(discover["code"], "project_connection_required");
    assert!(discover["next_action"]
        .as_str()
        .unwrap()
        .contains("even for one candidate"));
    assert!(discover.get("project_connection").is_none());
    let created = call(
        &mut runtime,
        json!({"action":"connect_project_memory","project_path":"C:/work/example","intent":"create","project_name":"Example"}),
    );
    let again = call(
        &mut runtime,
        json!({"action":"connect_project_memory","project_path":"C:/work/example"}),
    );
    assert_eq!(created["project_connection"], again["project_connection"]);
}
#[test]
fn first_recall_discovers_without_binding_even_one_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let created = call(
        &mut runtime,
        json!({"action":"connect_project_memory","project_path":"C:/work/original","intent":"create","project_name":"Example"}),
    );
    let first = call(
        &mut runtime,
        json!({"action":"recall","recall_for":"Continue work","project_path":"C:/work/copy"}),
    );
    assert_eq!(first["status"], "needs_user_input");
    assert_eq!(first["code"], "project_connection_required");
    assert_eq!(
        first["candidates"][0]["project_id"],
        created["project_connection"]["project_id"]
    );
    assert!(first["turn_instruction"]
        .as_str()
        .unwrap()
        .contains("Project rules, active Tasks and work targets have not been checked"));
    assert!(first.get("partial_recall").is_some());
    let second = call(
        &mut runtime,
        json!({"action":"recall","recall_for":"Continue work","project_path":"C:/work/copy"}),
    );
    assert_eq!(second["status"], "needs_user_input");
}

#[test]
fn ordinary_contextual_recall_delivers_rules_outside_search_rank_and_id_only_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    call(
        &mut runtime,
        json!({"action":"connect_project_memory","intent":"create","project_path":"C:/work/rules","project_name":"Rules"}),
    );
    let user = call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":"Always inspect durable constraints.","evidence":"Test rule","applies_to":["user"],"is_rule":true}]}),
    );
    let machine_id = runtime.machine_id().to_owned();
    let machine = call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":"Use local paths carefully.","evidence":"Test rule","applies_to":["machine"],"is_rule":true}]}),
    );
    let project = call(
        &mut runtime,
        json!({"action":"remember_this","project_path":"C:/work/rules","memories":[{"operation":"create","what_to_remember":"Keep the project contract aligned.","evidence":"Test rule","applies_to":["project"],"is_rule":true}]}),
    );
    for written in [&user, &machine, &project] {
        assert_eq!(written["status"], "remembered", "{written}");
    }
    let ids: Vec<_> = [&user, &machine, &project]
        .iter()
        .map(|v| v["results"][0]["memory_ref"]["memory_id"].clone())
        .collect();
    let result = call(
        &mut runtime,
        json!({"action":"recall","project_path":"C:/work/rules","recall_for":"completely unrelated subject","memory_candidate_limit":1}),
    );
    for id in &ids {
        assert!(
            result["memories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["memory_id"] == *id),
            "missing rule {id}: {result}"
        );
    }
    assert_eq!(result["memories"].as_array().unwrap().len(), 3);
    let id_only = call(
        &mut runtime,
        json!({"action":"recall","memory_ids":[ids[0].clone()]}),
    );
    assert_eq!(id_only["memories"].as_array().unwrap().len(), 1);
    assert_eq!(id_only["memories"][0]["memory_id"], ids[0]);
    let mixed = call(
        &mut runtime,
        json!({"action":"recall","recall_for":"unrelated","memory_ids":[ids[0].clone()],"machine_id":machine_id,"project_path":"C:/work/rules"}),
    );
    for id in &ids {
        assert!(mixed["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["memory_id"] == *id));
    }
}

#[test]
fn unbound_project_path_delivers_user_and_machine_rules_as_partial_recall() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let user = call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":"User-wide prerequisite.","evidence":"Test rule","applies_to":["user"],"is_rule":true}]}),
    );
    let machine = call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":"Machine prerequisite.","evidence":"Test rule","applies_to":["machine"],"is_rule":true}]}),
    );
    let no_project = call(
        &mut runtime,
        json!({"action":"recall","recall_for":"unrelated request"}),
    );
    assert!(no_project.get("status").is_none(), "{no_project}");
    assert_eq!(no_project["memories"].as_array().unwrap().len(), 2);
    let result = call(
        &mut runtime,
        json!({"action":"recall","project_path":"C:/work/unbound","recall_for":"unrelated request","include_active_tasks":true,"memory_candidate_limit":1}),
    );
    assert_eq!(result["status"], "needs_user_input", "{result}");
    assert_eq!(result["code"], "project_connection_required");
    let partial = &result["partial_recall"];
    assert!(partial.is_object(), "{result}");
    assert!(partial.get("active_task_count").is_none());
    for written in [&user, &machine] {
        let id = &written["results"][0]["memory_ref"]["memory_id"];
        assert!(
            partial["memories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["memory_id"] == *id),
            "missing partial rule {id}: {result}"
        );
    }
    assert!(result["turn_instruction"]
        .as_str()
        .unwrap()
        .contains("Project rules, active Tasks and work targets have not been checked"));
}

#[test]
fn contextual_rule_recall_synchronizes_index_stale_before_call() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut writer = Runtime::open(&root, &local, "user").unwrap();
    let mut reader = Runtime::open(&root, &local, "user").unwrap();
    let before = call(
        &mut reader,
        json!({"action":"recall","recall_for":"unrelated work"}),
    );
    assert!(before["memories"].as_array().unwrap().is_empty());
    let saved = call(
        &mut writer,
        json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":"New user rule after reader index creation.","evidence":"Test rule","applies_to":["user"],"is_rule":true}]}),
    );
    assert_eq!(saved["status"], "remembered", "{saved}");
    let after = call(
        &mut reader,
        json!({"action":"recall","recall_for":"unrelated work"}),
    );
    let expected = &saved["results"][0]["memory_ref"];
    assert!(
        after["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["memory_id"] == expected["memory_id"]
                && m["current_heads"][0]["memory_ref"] == *expected),
        "{after}"
    );
    let changed = call(
        &mut writer,
        json!({"action":"remember_this","memories":[{"operation":"evolve","parent_refs":[expected],"what_to_remember":"Now an ordinary observation.","evidence":"Rule withdrawn by test","is_rule":false}]}),
    );
    assert_eq!(changed["status"], "remembered", "{changed}");
    let withdrawn = call(
        &mut reader,
        json!({"action":"recall","recall_for":"unrelated work"}),
    );
    assert!(
        !withdrawn["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["memory_id"] == expected["memory_id"]),
        "{withdrawn}"
    );
}

#[test]
fn joint_project_machine_rule_requires_both_scopes_in_runtime_index() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    call(
        &mut runtime,
        json!({"action":"connect_project_memory","intent":"create","project_path":"C:/work/joint","project_name":"Joint"}),
    );
    let saved = call(
        &mut runtime,
        json!({"action":"remember_this","project_path":"C:/work/joint","memories":[{"operation":"create","what_to_remember":"Joint scope rule irrelevant to the query.","evidence":"Test rule","applies_to":["project","machine"],"is_rule":true}]}),
    );
    assert_eq!(saved["status"], "remembered", "{saved}");
    let id = &saved["results"][0]["memory_ref"]["memory_id"];
    let same = call(
        &mut runtime,
        json!({"action":"recall","project_path":"C:/work/joint","recall_for":"unrelated"}),
    );
    assert!(
        same["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["memory_id"] == *id),
        "{same}"
    );
    let other_machine = uuid::Uuid::new_v4().to_string();
    let different = call(
        &mut runtime,
        json!({"action":"recall","project_path":"C:/work/joint","machine_id":other_machine,"recall_for":"unrelated"}),
    );
    assert!(
        !different["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["memory_id"] == *id),
        "{different}"
    );
}
#[test]
fn procedure_authoring_publishes_and_exact_recall_returns_full_instructions() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let evidence = call(&mut runtime, create("Use a fixture to verify migration."))["results"][0]
        ["memory_ref"]["memory_id"]
        .clone();
    let prepared = call(
        &mut runtime,
        json!({"action":"remember_this","procedure":{"operation":"create","evidence_memory_ids":[evidence]}}),
    );
    assert_eq!(prepared["status"], "authoring_required");
    let written = call(
        &mut runtime,
        json!({"action":"remember_this","authoring_ref":prepared["authoring_ref"],"procedure":{"operation":"create","evidence_memory_ids":[evidence],"applies_to":["user"],"name":"Verify a migration","use_when":"When changing a schema.","instructions":"Run fixture checks before applying changes.","evidence":"The recorded observation supports this procedure."}}),
    );
    assert_eq!(written["status"], "remembered");
    let recall = call(
        &mut runtime,
        json!({"action":"recall","procedure_ids":[written["procedure_ref"]["procedure_id"]]}),
    );
    assert_eq!(
        recall["learned_procedures"][0]["instructions"],
        "Run fixture checks before applying changes."
    );
    drop(runtime);
    let control = rusqlite::Connection::open(dir.path().join("local/control.sqlite")).unwrap();
    control
        .execute(
            "UPDATE authoring_executions SET status='pending',result=NULL",
            [],
        )
        .unwrap();
    drop(control);
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let history = call(
        &mut runtime,
        json!({"action":"history","procedure_id":written["procedure_ref"]["procedure_id"],"context_layers":20}),
    );
    assert_eq!(
        history["latest_changes"].as_array().unwrap().len(),
        1,
        "Restart must reuse the exact published authoring execution, not create another change."
    );
}
#[test]
fn history_exposes_causal_diff_and_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let written = call(&mut runtime, create("before"));
    let reference = written["results"][0]["memory_ref"].clone();
    call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"evolve","parent_refs":[reference],"what_to_remember":"after","evidence":"Correction"}]}),
    );
    let history = call(
        &mut runtime,
        json!({"action":"history","memory_id":reference["memory_id"],"context_layers":1}),
    );
    assert_eq!(history["latest_changes"].as_array().unwrap().len(), 2);
    assert_eq!(history["latest_changes"][1]["evidence"], "Correction");
    assert!(history["latest_changes"][1]["markdown_diff"]
        .as_str()
        .unwrap()
        .contains("+after"));
}
#[test]
fn consolidation_redirect_preserves_exact_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let first = call(&mut runtime, create("first"))["results"][0]["memory_ref"].clone();
    let pending = call(&mut runtime, create("first"));
    let second=call(&mut runtime,json!({"action":"remember_this","pending_action":"continue","pending_batch_ref":pending["pending_batch_ref"],"resolution_ref":pending["resolution_ref"]}))["results"][0]["memory_ref"].clone();
    let merged = call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"consolidate","consolidation_refs":[first,second],"what_to_remember":"combined","evidence":"Confirmed same subject"}]}),
    );
    assert_eq!(merged["status"], "remembered");
    assert_eq!(
        merged["results"].as_array().unwrap().len(),
        1,
        "one consolidate proposal has one result; redirects are not additional proposals"
    );
    let recalled = call(
        &mut runtime,
        json!({"action":"recall","memory_ids":[first["memory_id"],second["memory_id"]]}),
    );
    assert_eq!(recalled["memories"].as_array().unwrap().len(), 1);
    assert_eq!(
        recalled["memories"][0]["current_heads"][0]["what_to_remember"],
        "combined"
    );
}

#[test]
fn dependent_review_is_durable_and_explicit() {
    fn head(runtime: &mut Runtime, id: &Value) -> Value {
        let recalled = call(runtime, json!({"action":"recall","memory_ids":[id]}));
        let memory = recalled["memories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|memory| memory["memory_id"] == *id)
            .unwrap();
        assert_eq!(memory["current_heads"].as_array().unwrap().len(), 1);
        memory["current_heads"][0].clone()
    }
    fn dependent_issue(response: &Value) -> Value {
        let issues: Vec<_> = response["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|issue| issue["code"] == "required_dependent_review")
            .collect();
        assert_eq!(issues.len(), 1, "expected one dependent review: {response}");
        issues[0].clone()
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let target = call(&mut runtime, create("target original"))["results"][0]["memory_ref"].clone();
    let dependent = call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"create","what_to_remember":"dependent","evidence":"Depends on target","applies_to":["user"],"required_memory_ids":[target["memory_id"]]}]}),
    )["results"][0]["memory_ref"].clone();
    let pending = call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"evolve","parent_refs":[target],"what_to_remember":"target corrected","evidence":"User correction"}]}),
    );
    assert_eq!(pending["status"], "resolution_required");
    let old_issue = dependent_issue(&pending);
    let review = old_issue["review_ref"].clone();
    assert_eq!(
        old_issue["current_memory"]["memory_id"],
        dependent["memory_id"]
    );
    assert_eq!(
        old_issue["current_memory"]["current_heads"][0]["memory_ref"],
        dependent
    );
    assert_eq!(
        head(&mut runtime, &target["memory_id"])["memory_ref"],
        target
    );

    // A separate frontend changes B while A's proposed correction is retained.
    let mut other = Runtime::open(&root, &local, "user").unwrap();
    let updated = call(
        &mut other,
        json!({"action":"remember_this","memories":[{
            "operation":"evolve","parent_refs":[dependent],
            "what_to_remember":"dependent revised by another runtime",
            "evidence":"New dependent constraints, still requiring the same target"
        }]}),
    );
    assert_eq!(updated["status"], "remembered");
    let updated_ref = updated["results"][0]["memory_ref"].clone();
    assert_ne!(
        updated_ref["observed_version"],
        dependent["observed_version"]
    );
    let updated_head = head(&mut other, &dependent["memory_id"]);
    assert_eq!(
        updated_head["required_memory_ids"],
        json!([target["memory_id"]])
    );
    drop(other);
    drop(runtime);

    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let inspected = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"inspect",
        "pending_batch_ref":pending["pending_batch_ref"]}),
    );
    assert_eq!(inspected["status"], "pending");
    assert_eq!(inspected["pending_batch_ref"], pending["pending_batch_ref"]);
    assert_eq!(dependent_issue(&inspected), old_issue);
    assert_eq!(
        inspected["proposals"][0]["what_to_remember"],
        "target corrected"
    );
    assert_eq!(
        head(&mut runtime, &target["memory_id"])["memory_ref"],
        target
    );

    // The batch ref is current, but its old review applies only to the old B head.
    let refreshed = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"continue",
        "pending_batch_ref":inspected["pending_batch_ref"],
        "review_judgments":[{"review_ref":review,"decision":"unchanged"}]}),
    );
    assert_eq!(refreshed["status"], "resolution_required");
    assert_eq!(
        refreshed["pending_batch_ref"]["pending_batch_id"],
        pending["pending_batch_ref"]["pending_batch_id"]
    );
    assert_ne!(
        refreshed["pending_batch_ref"]["observed_version"],
        pending["pending_batch_ref"]["observed_version"]
    );
    let fresh_issue = dependent_issue(&refreshed);
    assert_ne!(fresh_issue["review_ref"], review);
    assert_eq!(
        fresh_issue["current_memory"]["current_heads"][0],
        updated_head
    );
    assert_eq!(refreshed["proposals"], inspected["proposals"]);
    assert_eq!(
        head(&mut runtime, &target["memory_id"])["what_to_remember"],
        "target original"
    );
    assert_eq!(
        head(&mut runtime, &target["memory_id"])["memory_ref"],
        target
    );

    let stale_batch = runtime
        .call(
            parse_request(
                &json!({"action":"remember_this","pending_action":"continue",
        "pending_batch_ref":pending["pending_batch_ref"],
        "review_judgments":[{"review_ref":fresh_issue["review_ref"],"decision":"unchanged"}]})
                .to_string(),
            )
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(stale_batch.code, "stale_pending_ref");
    let stale_review = runtime
        .call(
            parse_request(
                &json!({"action":"remember_this","pending_action":"continue",
        "pending_batch_ref":refreshed["pending_batch_ref"],
        "review_judgments":[{"review_ref":review,"decision":"unchanged"}]})
                .to_string(),
            )
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(stale_review.code, "invalid_review_ref");

    // Confirm the new review and unapproved proposal survived in the actual DB,
    // rather than merely being present in the returned JSON or an in-memory cache.
    drop(runtime);
    let control = rusqlite::Connection::open_with_flags(
        local.join("control.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (status, payload, result): (String, String, Option<String>) = control
        .query_row(
            "SELECT status,payload,result FROM pending WHERE id=?1",
            [refreshed["pending_batch_ref"]["pending_batch_id"]
                .as_str()
                .unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "pending");
    assert!(result.is_none());
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["issues"], refreshed["issues"]);
    assert_eq!(
        payload["batch"]["reference"],
        refreshed["pending_batch_ref"]
    );
    assert!(payload["approved_review_refs"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(payload["operation_id"].is_null());
    drop(control);

    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let inspected = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"inspect",
        "pending_batch_ref":refreshed["pending_batch_ref"]}),
    );
    assert_eq!(
        inspected["pending_batch_ref"],
        refreshed["pending_batch_ref"]
    );
    assert_eq!(dependent_issue(&inspected), fresh_issue);
    assert_eq!(
        head(&mut runtime, &target["memory_id"])["memory_ref"],
        target
    );
    let result = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"continue","pending_batch_ref":inspected["pending_batch_ref"],"review_judgments":[{"review_ref":fresh_issue["review_ref"],"decision":"unchanged"}]}),
    );
    assert_eq!(result["status"], "remembered");
    assert_eq!(result["results"].as_array().unwrap().len(), 1);
    let final_target = head(&mut runtime, &target["memory_id"]);
    assert_eq!(final_target["what_to_remember"], "target corrected");
    assert_eq!(
        final_target["memory_ref"],
        result["results"][0]["memory_ref"]
    );
    assert_ne!(
        final_target["memory_ref"]["observed_version"],
        target["observed_version"]
    );
    assert_eq!(head(&mut runtime, &dependent["memory_id"]), updated_head);
    let published = call(
        &mut runtime,
        json!({"action":"remember_this","pending_action":"inspect",
        "pending_batch_ref":inspected["pending_batch_ref"]}),
    );
    assert_eq!(published, result);
}

#[test]
fn c7_reads_publication_ledger_through_readonly_port() {
    use agentlaw_worker::derived::*;
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    call(&mut runtime, create("index me"));
    let reader = runtime.published_source();
    let position = reader.position().unwrap();
    let context = DerivedContext {
        repository_id: runtime.binding_id().into(),
        initial_basis: SourcePosition {
            epoch: position.epoch,
            sequence: 0,
        },
        model_digest: "test-model".into(),
        config_digest: "test-config".into(),
    };
    let mut broker = agentlaw_worker::Broker::open(dir.path().join("jobs.sqlite"), 10).unwrap();
    let mut coordinator = DerivedWorkCoordinator::new(&mut broker, &reader).unwrap();
    assert_eq!(coordinator.advance(&context, 10).unwrap().sequence, 1);
}

#[test]
fn restart_reuses_published_execution_after_local_ack_loss() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let written = call(&mut runtime, create("exactly once"));
    let id = written["results"][0]["memory_ref"]["memory_id"].clone();
    drop(runtime);
    let control = rusqlite::Connection::open(local.join("control.sqlite")).unwrap();
    control
        .execute("UPDATE pending SET status='pending',result=NULL", [])
        .unwrap();
    drop(control);
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let history = call(
        &mut runtime,
        json!({"action":"history","memory_id":id,"context_layers":20}),
    );
    assert_eq!(history["latest_changes"].as_array().unwrap().len(), 1);
}

#[test]
fn omitted_relation_preserves_canonical_description() {
    use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let mut runtime = Runtime::open(&root, &local, "user").unwrap();
    let target =
        call(&mut runtime, create("target"))["results"][0]["memory_ref"]["memory_id"].clone();
    let id = uuid::Uuid::new_v4().to_string();
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":runtime.machine_id()},"recorded_at_ms":0,"is_rule":false,"relations":[{"kind":"related","target_memory_id":target,"description":"Keep this relationship rationale."}],"work_targets":[]}),
                body: "parent body".into(),
            }],
        },
    };
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "Seed canonical optional metadata".into(),
            }],
        )
        .unwrap();
    let r = &receipt.references[0];
    call(
        &mut runtime,
        json!({"action":"remember_this","memories":[{"operation":"evolve","parent_refs":[{"memory_id":r.memory_id,"observed_version":r.observed_version}],"what_to_remember":"updated parent body","evidence":"Changed only body"}]}),
    );
    assert_eq!(
        store.read_current(&id).unwrap().heads()[0].metadata["relations"][0]["description"],
        "Keep this relationship rationale."
    );
}

#[test]
fn failed_store_switch_does_not_initialize_target() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    let local = dir.path().join("local");
    drop(Runtime::open(&a, &local, "user").unwrap());
    assert!(Runtime::open(&b, &local, "user").is_err());
    assert!(!b.exists());
    assert!(Runtime::open(&a, &local, "user").is_ok());
}

#[test]
fn cross_project_lookup_does_not_rebind_current_folder() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime =
        Runtime::open(dir.path().join("source"), dir.path().join("local"), "user").unwrap();
    let a = call(
        &mut runtime,
        json!({"action":"connect_project_memory","intent":"create","project_path":"C:/work/alpha","project_name":"Alpha"}),
    );
    let b = call(
        &mut runtime,
        json!({"action":"connect_project_memory","intent":"create","project_path":"C:/work/beta","project_name":"Beta"}),
    );
    let written = call(
        &mut runtime,
        json!({"action":"remember_this","project_path":"C:/work/alpha","memories":[{"operation":"create","what_to_remember":"unique_alpha_needle","evidence":"Observed","applies_to":["project"]}]}),
    );
    let id = written["results"][0]["memory_ref"]["memory_id"].clone();
    let exact = call(
        &mut runtime,
        json!({"action":"recall","project_path":"C:/work/beta","memory_ids":[id]}),
    );
    assert_eq!(
        exact["memories"][0]["current_heads"][0]["applicability"]["project_id"],
        a["project_connection"]["project_id"]
    );
    let queried = call(
        &mut runtime,
        json!({"action":"recall","project_path":"C:/work/beta","project_hint":"Alpha","selected_project_id":a["project_connection"]["project_id"],"recall_for":"unique_alpha_needle"}),
    );
    assert_eq!(queried["candidates"][0]["memory_id"], id);
    let current = call(
        &mut runtime,
        json!({"action":"connect_project_memory","project_path":"C:/work/beta"}),
    );
    assert_eq!(current["project_connection"], b["project_connection"]);
}
