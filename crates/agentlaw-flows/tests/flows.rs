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
