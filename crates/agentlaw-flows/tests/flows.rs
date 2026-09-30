use agentlaw_flows::*;
use agentlaw_flows::{context::*, recall::*, write::*};
use std::collections::BTreeMap;
const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";
fn context() -> RequestContext {
    RequestContext {
        store_binding_id: "store".into(),
        user_id: "user".into(),
        machine_id: A.into(),
        project_id: Some(B.into()),
        connection_version: "1".into(),
    }
}
fn memory(id: &str) -> Memory {
    Memory {
        memory_ref: MemoryRef {
            memory_id: id.into(),
            observed_version: "v1".into(),
        },
        what_to_remember: "Exact content".into(),
        evidence: "Observed".into(),
        applies_to: vec![ScopeKind::Project],
        applicability: Applicability {
            scope: vec![ScopeKind::Project],
            project_id: Some(B.into()),
            machine_id: None,
        },
        origin: Origin {
            machine_id: A.into(),
            ..Default::default()
        },
        in_working_set: None,
        is_rule: false,
        related_memory_ids: vec![],
        required_memory_ids: vec![],
        work_targets: vec![],
    }
}
#[derive(Default)]
struct Snapshot(BTreeMap<String, CurrentState>);
impl Snapshot {
    fn insert(&mut self, m: Memory) {
        let id = m.memory_ref.memory_id.clone();
        self.0.insert(
            id.clone(),
            CurrentState {
                requested_id: id.clone(),
                resolved_id: id,
                heads: vec![m],
                redirect_path: vec![],
            },
        );
    }
}
impl SourceSnapshot for Snapshot {
    fn current(&self, id: &str) -> Result<Option<CurrentState>> {
        Ok(self.0.get(id).cloned())
    }
    fn inventory(&self) -> Result<Vec<CurrentState>> {
        Ok(self.0.values().cloned().collect())
    }
    fn read_set(&self) -> ReadSet {
        ReadSet::default()
    }
}
struct Search;
impl RecallSearch for Search {
    fn search(&self, _: &RequestContext, _: &str, _: &[String]) -> Result<Discovery> {
        panic!("exact recall must not search")
    }
}
struct EmptySearch;
impl RecallSearch for EmptySearch {
    fn search(&self, _: &RequestContext, _: &str, _: &[String]) -> Result<Discovery> {
        Ok(Discovery {
            candidates: vec![],
            full_ids: vec![],
            diagnostics: vec![],
        })
    }
}

#[test]
fn contextual_recall_selects_same_head_applicable_rules_outside_search_limits() {
    const USER: &str = "33333333-3333-4333-8333-333333333333";
    const MACHINE: &str = "44444444-4444-4444-8444-444444444444";
    const OTHER: &str = "55555555-5555-4555-8555-555555555555";
    const SPLIT: &str = "66666666-6666-4666-8666-666666666666";
    const EXTRA: &str = "77777777-7777-4777-8777-777777777777";
    const BOTH: &str = "88888888-8888-4888-8888-888888888888";
    const WRONG_MACHINE: &str = "99999999-9999-4999-8999-999999999999";
    let mut source = Snapshot::default();
    let mut user = memory(USER);
    user.is_rule = true;
    user.applies_to = vec![ScopeKind::User];
    user.applicability = Applicability {
        scope: vec![ScopeKind::User],
        project_id: None,
        machine_id: None,
    };
    user.required_memory_ids.push(EXTRA.into());
    source.insert(user);
    let mut project = memory(B);
    project.is_rule = true;
    source.insert(project);
    let mut machine = memory(MACHINE);
    machine.is_rule = true;
    machine.applies_to = vec![ScopeKind::Machine];
    machine.applicability = Applicability {
        scope: vec![ScopeKind::Machine],
        project_id: None,
        machine_id: Some(A.into()),
    };
    source.insert(machine);
    let mut other = memory(OTHER);
    other.is_rule = true;
    other.applicability.project_id = Some(OTHER.into());
    source.insert(other);
    let mut split_rule = memory(SPLIT);
    split_rule.is_rule = true;
    split_rule.applicability.project_id = Some(OTHER.into());
    let mut split_scope = split_rule.clone();
    split_scope.is_rule = false;
    split_scope.applicability.project_id = Some(B.into());
    split_scope.memory_ref.observed_version = "v2".into();
    source.insert(split_rule);
    source.0.get_mut(SPLIT).unwrap().heads.push(split_scope);
    source.insert(memory(EXTRA));
    let mut both = memory(BOTH);
    both.is_rule = true;
    both.applies_to = vec![ScopeKind::Project, ScopeKind::Machine];
    both.applicability.scope = both.applies_to.clone();
    both.applicability.machine_id = Some(A.into());
    source.insert(both.clone());
    let mut alternate = both.clone();
    alternate.is_rule = false;
    alternate.memory_ref.observed_version = "v2".into();
    source.0.get_mut(BOTH).unwrap().heads.push(alternate);
    let mut wrong_machine = both;
    wrong_machine.memory_ref.memory_id = WRONG_MACHINE.into();
    wrong_machine.applicability.machine_id = Some(OTHER.into());
    source.insert(wrong_machine);
    let contextual = RecallRequest {
        recall_for: Some("unrelated work".into()),
        memory_candidate_limit: Some(1),
        ..Default::default()
    };
    let result = recall(&source, &EmptySearch, &context(), &contextual, "now".into()).unwrap();
    let ids: BTreeMap<_, _> = result
        .memories
        .iter()
        .map(|m| (m.memory_id.clone(), m))
        .collect();
    for expected in [USER, B, MACHINE, EXTRA, BOTH] {
        assert!(ids.contains_key(&expected.to_owned()), "missing {expected}");
    }
    assert_eq!(ids[BOTH].current_heads.len(), 2);
    assert_eq!(ids[BOTH].head_reconciliation_required, Some(true));
    assert!(!ids.contains_key(&OTHER.to_owned()));
    assert!(!ids.contains_key(&SPLIT.to_owned()));
    assert!(!ids.contains_key(&WRONG_MACHINE.to_owned()));
    assert_eq!(result.candidate_counts.unwrap().memories.shown, 0);
    let restored = RecallRequest {
        restore_context: Some(true),
        ..contextual.clone()
    };
    let restored_result =
        recall(&source, &EmptySearch, &context(), &restored, "now".into()).unwrap();
    let restored_ids: Vec<_> = restored_result
        .memories
        .iter()
        .map(|m| m.memory_id.as_str())
        .collect();
    for expected in [USER, B, MACHINE, EXTRA, BOTH] {
        assert!(restored_ids.contains(&expected));
    }
    let id_only = RecallRequest {
        memory_ids: Some(vec![EXTRA.into()]),
        ..Default::default()
    };
    assert_eq!(
        recall(&source, &Search, &context(), &id_only, "now".into())
            .unwrap()
            .memories
            .len(),
        1
    );
    let mixed = RecallRequest {
        memory_ids: Some(vec![OTHER.into()]),
        ..contextual
    };
    let mixed_result = recall(&source, &EmptySearch, &context(), &mixed, "now".into()).unwrap();
    assert!(mixed_result.memories.iter().any(|m| m.memory_id == OTHER));
}

#[test]
fn repeated_candidate_discoveries_preserve_views_sources_tasks_and_targets() {
    const C: &str = "33333333-3333-4333-8333-333333333333";
    const EXACT: &str = "44444444-4444-4444-8444-444444444444";
    const REQUIRED: &str = "55555555-5555-4555-8555-555555555555";
    struct PathsSearch;
    fn candidate(id: &str, preview: &str, paths: &[(&str, &str)]) -> MemoryCandidate {
        MemoryCandidate {
            memory_id: id.into(),
            excerpt: preview.into(),
            applicability: memory(id).applicability,
            retrieval_paths: paths
                .iter()
                .map(|(via, clue)| RetrievalPath {
                    via: vec![(*via).into()],
                    clue: (*clue).into(),
                    source_memory_id: None,
                })
                .collect(),
        }
    }
    impl RecallSearch for PathsSearch {
        fn search(&self, _: &RequestContext, query: &str, _: &[String]) -> Result<Discovery> {
            let candidates = if query == "topic" {
                vec![
                    candidate(A, "First preview", &[("lexical", "shared clue")]),
                    candidate(A, "Other head preview", &[("lexical", "other head clue")]),
                    candidate(B, "Source B preview", &[("lexical", "source B clue")]),
                    candidate(C, "Source C preview", &[("lexical", "source C clue")]),
                ]
            } else {
                vec![candidate(
                    A,
                    "Later view preview",
                    &[("vector", "shared clue"), ("lexical", "other view clue")],
                )]
            };
            Ok(Discovery {
                candidates,
                full_ids: vec![],
                diagnostics: vec![],
            })
        }
    }
    let mut source = Snapshot::default();
    let mut task = memory(A);
    task.in_working_set = Some(true);
    task.work_targets.push(StoredTarget {
        project_id: B.into(),
        path: "src/task.rs".into(),
        kind: TargetKind::File,
        reading: Reading::Related,
    });
    source.insert(task.clone());
    task.memory_ref.observed_version = "v2".into();
    task.what_to_remember = "Other current head, preserved by exact recall".into();
    task.in_working_set = None;
    source.0.get_mut(A).unwrap().heads.push(task);
    for id in [B, C] {
        let mut linked = memory(id);
        linked.related_memory_ids = vec![A.into(), A.into()];
        source.insert(linked);
    }
    let mut exact = memory(EXACT);
    exact.required_memory_ids = vec![REQUIRED.into()];
    source.insert(exact.clone());
    exact.memory_ref.observed_version = "v2".into();
    exact.what_to_remember = "Complete second exact head".into();
    source.0.get_mut(EXACT).unwrap().heads.push(exact);
    source.insert(memory(REQUIRED));
    let result = recall(
        &source,
        &PathsSearch,
        &context(),
        &RecallRequest {
            recall_for: Some("topic".into()),
            restore_context: Some(true),
            include_active_tasks: Some(true),
            memory_candidate_limit: Some(1),
            memory_ids: Some(vec![EXACT.into()]),
            work_targets: Some(vec![WorkTarget {
                path: "src/task.rs".into(),
                kind: TargetKind::File,
            }]),
            ..Default::default()
        },
        "now".into(),
    )
    .unwrap();
    let c = &result.candidates[0];
    assert_eq!(c.memory_id, A);
    assert_eq!(c.excerpt, "First preview");
    assert_eq!(c.retrieval_paths.len(), 7);
    assert_eq!(c.retrieval_paths[0].via, ["lexical", "vector"]);
    assert_eq!(c.retrieval_paths[1].clue, "other head clue");
    assert_eq!(c.retrieval_paths[2].clue, "other view clue");
    let sources: Vec<_> = c
        .retrieval_paths
        .iter()
        .filter_map(|p| p.source_memory_id.as_deref())
        .collect();
    assert_eq!(sources, [B, C]);
    assert!(c.retrieval_paths.iter().any(|p| p.via == ["active_task"]));
    assert!(c
        .retrieval_paths
        .iter()
        .any(|p| p.via == ["work_target"] && p.clue == "src/task.rs"));
    let counts = &result.candidate_counts.as_ref().unwrap().memories;
    assert_eq!((counts.matched, counts.shown), (3, 1));
    assert_eq!(result.active_task_count, Some(1));
    assert_eq!(result.memories.len(), 2);
    assert_eq!(
        result
            .memories
            .iter()
            .find(|m| m.memory_id == EXACT)
            .unwrap()
            .current_heads
            .len(),
        2
    );
    let full = recall(
        &source,
        &Search,
        &context(),
        &RecallRequest {
            memory_ids: Some(vec![A.into()]),
            ..Default::default()
        },
        "now".into(),
    )
    .unwrap();
    assert_eq!(full.memories[0].current_heads.len(), 2);
    assert_eq!(
        full.memories[0].current_heads[1].what_to_remember,
        "Other current head, preserved by exact recall"
    );
}
struct Reviewed;
impl ReviewGate for Reviewed {
    fn review(&self, _: &RequestContext, _: &[MemoryProposal], _: &ReadSet) -> Result<()> {
        Ok(())
    }
}
fn proposal() -> MemoryProposal {
    match parse_request(r#"{"action":"remember_this","remember_this":{"memories":[{"operation":"create","what_to_remember":"body","evidence":"observed","applies_to":["user"]}]}}"#).unwrap(){Request::RememberThis(r)=>r.memories.unwrap().remove(0),_=>unreachable!()}
}
#[test]
fn exact_required_cycles_and_sparse_counts() {
    let mut source = Snapshot::default();
    let mut a = memory(A);
    a.required_memory_ids.push(B.into());
    let mut b = memory(B);
    b.required_memory_ids.push(A.into());
    source.insert(a);
    source.insert(b);
    let r = RecallRequest {
        memory_ids: Some(vec![A.into()]),
        ..Default::default()
    };
    let result = recall(&source, &Search, &context(), &r, "now".into()).unwrap();
    assert_eq!(result.memories.len(), 2);
    assert!(result.candidate_counts.is_none());
    assert!(serde_json::to_value(result)
        .unwrap()
        .get("status")
        .is_none());
}
#[test]
fn missing_exact_is_not_empty_success() {
    let r = RecallRequest {
        memory_ids: Some(vec![A.into()]),
        ..Default::default()
    };
    let result = recall(&Snapshot::default(), &Search, &context(), &r, "now".into()).unwrap();
    assert_eq!(result.code.as_deref(), Some("exact_selection_incomplete"));
}
#[test]
fn component_boundaries() {
    let stored = StoredTarget {
        project_id: B.into(),
        path: "src/payment/file.rs".into(),
        kind: TargetKind::File,
        reading: Reading::Required,
    };
    assert!(!work_target_matches(
        &stored,
        &WorkTarget {
            path: "src/pay".into(),
            kind: TargetKind::Directory
        },
        B
    )
    .unwrap());
    assert!(work_target_matches(
        &stored,
        &WorkTarget {
            path: "src/payment".into(),
            kind: TargetKind::Directory
        },
        B
    )
    .unwrap());
    assert!(relative_path("../secret").is_err());
}
#[test]
fn active_task_cannot_implicitly_remain_active() {
    let mut source = Snapshot::default();
    let mut m = memory(A);
    m.in_working_set = Some(true);
    source.insert(m.clone());
    let mut p = proposal();
    p.operation = Operation::Evolve;
    p.applies_to = None;
    p.parent_refs = Some(vec![m.memory_ref]);
    assert_eq!(
        preflight(&source, &Reviewed, &context(), &[p])
            .unwrap_err()
            .code,
        "working_set_disposition_required"
    );
}
#[test]
fn every_head_required() {
    let mut source = Snapshot::default();
    let m = memory(A);
    source.insert(m.clone());
    let mut second = m.clone();
    second.memory_ref.observed_version = "v2".into();
    source.0.get_mut(A).unwrap().heads.push(second);
    let mut p = proposal();
    p.operation = Operation::Evolve;
    p.applies_to = None;
    p.parent_refs = Some(vec![m.memory_ref]);
    assert_eq!(
        preflight(&source, &Reviewed, &context(), &[p])
            .unwrap_err()
            .code,
        "stale_memory_ref"
    );
}
#[test]
fn unsupported_review_cannot_publish() {
    assert_eq!(
        preflight(
            &Snapshot::default(),
            &UnavailableReview,
            &context(),
            &[proposal()]
        )
        .unwrap_err()
        .code,
        "unsupported"
    );
}
#[test]
fn pending_delta_is_atomic_and_preserves_omitted_proposals() {
    let batch = PendingBatch::new(&context(), vec![proposal(), proposal()]);
    let mut replacement = batch.proposals[1].clone();
    replacement.what_to_remember = "replacement".into();
    let next = batch
        .apply_delta(&context(), &batch.reference, &[replacement])
        .unwrap();
    assert_eq!(next.proposals.len(), 2);
    assert_eq!(next.proposals[0].what_to_remember, "body");
    assert_eq!(batch.proposals[1].what_to_remember, "body");
    assert_ne!(next.reference, batch.reference);
    assert!(next
        .apply_delta(&context(), &batch.reference, &[proposal()])
        .is_err());
}
#[test]
fn task_headings_ignore_fenced_decoys() {
    let good="# Objective\na\n# Current position\nb\n```md\n# Objective\n```\n# Resume point\nc\n# References\nd";
    assert!(task_headings(good).is_ok());
    assert!(task_headings(&(good.to_owned() + "\n# Objective\nx")).is_err());
}
#[test]
fn paths_are_not_inferred() {
    assert!(absolute_project_path("relative").is_err());
    assert!(absolute_project_path("C:/work/app").is_ok());
    assert!(validate_id("../escape").is_err());
}
#[test]
fn inherited_scope_keeps_parent_identity() {
    let mut source = Snapshot::default();
    let m = memory(A);
    source.insert(m.clone());
    let mut p = proposal();
    p.operation = Operation::Evolve;
    p.applies_to = None;
    p.parent_refs = Some(vec![m.memory_ref]);
    let mut other = context();
    other.project_id = Some(A.into());
    let prepared = preflight(&source, &Reviewed, &other, &[p]).unwrap();
    assert_eq!(
        prepared.resolved_applicability[0].project_id.as_deref(),
        Some(B)
    );
}
#[test]
fn task_compact_sections_are_complete_and_fences_do_not_end_them() {
    let body="# Objective\nDeliver fully.\n# Current position\nBefore fence.\n```md\n# Resume point\nNot a boundary\n```\nAfter fence.\n# Resume point\nResume all steps.\n# References\nNot in compact output.\n";
    let sections = agentlaw_flows::write::task_sections(body).unwrap();
    assert_eq!(sections[0], "Deliver fully.\n");
    assert!(sections[1].contains("Not a boundary"));
    assert!(sections[1].ends_with("After fence.\n"));
    assert_eq!(sections[2], "Resume all steps.\n");
}
