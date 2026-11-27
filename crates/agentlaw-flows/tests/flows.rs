use agentlaw_flows::*;
use agentlaw_flows::{context::*, recall::*, write::*};
use std::cell::RefCell;
use std::collections::BTreeMap;
const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";
const C: &str = "33333333-3333-4333-8333-333333333333";
const EXPECTED_SEMANTIC_NOTICE: &str = "Briefly disclose this recall's incomplete semantic search in the user's language; combine any required-context warning into the same sentence. Omit only an unchanged semantic notice already visible for this task. Do not auto-retry or repair.";
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

#[derive(Default)]
struct ObservedSearch {
    candidates: Vec<MemoryCandidate>,
    full_ids: Vec<String>,
    diagnostics: Vec<DomainError>,
    queries: RefCell<Vec<String>>,
}
impl RecallSearch for ObservedSearch {
    fn search(&self, _: &RequestContext, query: &str, _: &[String]) -> Result<Discovery> {
        self.queries.borrow_mut().push(query.into());
        Ok(Discovery {
            candidates: self.candidates.clone(),
            full_ids: self.full_ids.clone(),
            diagnostics: self.diagnostics.clone(),
        })
    }
}
fn discovery_candidate(id: &str, channels: &[&str]) -> MemoryCandidate {
    MemoryCandidate {
        memory_id: id.into(),
        excerpt: format!("Candidate {id}"),
        applicability: memory(id).applicability,
        retrieval_paths: vec![RetrievalPath {
            via: channels.iter().map(|channel| (*channel).into()).collect(),
            clue: "Matching saved evidence".into(),
            source_memory_ids: None,
        }],
    }
}

#[test]
fn contextual_recall_without_semantic_degradation_has_no_turn_instruction() {
    let request = RecallRequest {
        recall_for: Some("topic".into()),
        ..Default::default()
    };
    let source = Snapshot::default();
    let empty = recall(&source, &EmptySearch, &context(), &request, "now".into()).unwrap();
    assert!(empty.memories.is_empty());
    assert!(empty.candidates.is_empty());
    let counts = &empty.candidate_counts.as_ref().unwrap().memories;
    assert_eq!((counts.matched, counts.shown), (0, 0));
    assert!(serde_json::to_value(&empty)
        .unwrap()
        .get("turn_instruction")
        .is_none());

    // A similar code must not be mistaken for the exact semantic diagnostic.
    let unrelated = DomainError::new(
        "semantic_channel_incomplete_unrelated",
        "Unrelated provider detail.",
    );
    for diagnostics in [vec![], vec![unrelated]] {
        let search = ObservedSearch {
            candidates: vec![discovery_candidate(C, &["lexical", "vector"])],
            diagnostics: diagnostics.clone(),
            ..Default::default()
        };
        let result = recall(&source, &search, &context(), &request, "now".into()).unwrap();
        assert_eq!(search.queries.borrow().as_slice(), ["topic"]);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].memory_id, C);
        assert_eq!(
            result.candidates[0].retrieval_paths[0].via,
            ["lexical", "vector"]
        );
        assert_eq!(
            &result.diagnostics[..diagnostics.len()],
            diagnostics.as_slice()
        );
        let counts = &result.candidate_counts.as_ref().unwrap().memories;
        assert_eq!((counts.matched, counts.shown), (1, 1));
        assert!(serde_json::to_value(&result)
            .unwrap()
            .get("turn_instruction")
            .is_none());
    }
}

#[test]
fn semantic_degradation_notice_preserves_each_retrieval_outcome() {
    let cases = [
        (
            "No semantic worker is attached; exact and indexed lexical retrieval remain available.",
            &["lexical"][..],
        ),
        (
            "The semantic worker is not ready; exact and indexed lexical retrieval remain available.",
            &["lexical"][..],
        ),
        (
            "The worker did not establish a complete semantic view.",
            &["lexical", "vector"][..],
        ),
        (
            "Semantic retrieval failed; exact and indexed lexical retrieval remain available.",
            &["lexical"][..],
        ),
    ];
    let mut source = Snapshot::default();
    let mut selected = memory(A);
    selected.what_to_remember = "Full discovered memory.\nPreserve the second paragraph.".into();
    selected.required_memory_ids.push(B.into());
    source.insert(selected.clone());
    source.insert(memory(B));
    source.insert(memory(C));
    for (message, channels) in cases {
        let diagnostic = DomainError::new("semantic_channel_incomplete", message);
        let candidate = discovery_candidate(C, channels);
        let search = ObservedSearch {
            candidates: vec![candidate.clone()],
            full_ids: vec![A.into()],
            diagnostics: vec![diagnostic.clone()],
            ..Default::default()
        };
        let result = recall(
            &source,
            &search,
            &context(),
            &RecallRequest {
                recall_for: Some("topic".into()),
                ..Default::default()
            },
            "now".into(),
        )
        .unwrap();
        assert_eq!(search.queries.borrow().as_slice(), ["topic"]);
        assert_eq!(
            result.turn_instruction.as_deref(),
            Some(EXPECTED_SEMANTIC_NOTICE)
        );
        assert_eq!(result.diagnostics[0], diagnostic);
        assert!(!result.diagnostics[0].retryable);
        assert_eq!(result.diagnostics.len(), 2);
        assert_eq!(
            serde_json::to_value(&result.candidates).unwrap(),
            serde_json::to_value([candidate]).unwrap()
        );
        let counts = &result.candidate_counts.as_ref().unwrap().memories;
        assert_eq!((counts.matched, counts.shown), (1, 1));
        assert_eq!(result.memories.len(), 2);
        let full = result.memories.iter().find(|m| m.memory_id == A).unwrap();
        assert_eq!(
            full.current_heads[0].what_to_remember,
            selected.what_to_remember
        );
        assert_eq!(full.current_heads[0].required_memory_ids, [B]);
        assert!(result.memories.iter().any(|m| m.memory_id == B));
        assert!(result.undelivered_required.is_empty());
        assert!(result.code.is_none());
    }
}

#[test]
fn id_only_recall_does_not_search_or_add_semantic_notice() {
    let mut source = Snapshot::default();
    source.insert(memory(A));
    let search = ObservedSearch {
        candidates: vec![discovery_candidate(C, &["vector"])],
        diagnostics: vec![DomainError::new(
            "semantic_channel_incomplete",
            "No semantic worker is attached; exact and indexed lexical retrieval remain available.",
        )],
        ..Default::default()
    };
    let result = recall(
        &source,
        &search,
        &context(),
        &RecallRequest {
            memory_ids: Some(vec![A.into()]),
            ..Default::default()
        },
        "now".into(),
    )
    .unwrap();
    assert!(search.queries.borrow().is_empty());
    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0].memory_id, A);
    assert_eq!(
        result.memories[0].current_heads[0].what_to_remember,
        "Exact content"
    );
    assert!(result.candidates.is_empty());
    assert!(result.candidate_counts.is_none());
    assert!(result.diagnostics.is_empty());
    assert!(serde_json::to_value(result)
        .unwrap()
        .get("turn_instruction")
        .is_none());
}

#[test]
fn mixed_exact_and_contextual_recall_retains_notice_and_complete_heads() {
    let mut source = Snapshot::default();
    let mut exact = memory(A);
    exact.what_to_remember = "First exact head".into();
    exact.required_memory_ids.push(B.into());
    source.insert(exact.clone());
    exact.memory_ref.observed_version = "v2".into();
    exact.what_to_remember = "Second exact head".into();
    source.0.get_mut(A).unwrap().heads.push(exact);
    source.insert(memory(B));
    source.insert(memory(C));
    let diagnostic = DomainError::new(
        "semantic_channel_incomplete",
        "Semantic retrieval failed; exact and indexed lexical retrieval remain available.",
    );
    let search = ObservedSearch {
        candidates: vec![discovery_candidate(C, &["lexical"])],
        diagnostics: vec![diagnostic.clone()],
        ..Default::default()
    };
    let result = recall(
        &source,
        &search,
        &context(),
        &RecallRequest {
            memory_ids: Some(vec![A.into()]),
            recall_for: Some("topic".into()),
            ..Default::default()
        },
        "now".into(),
    )
    .unwrap();
    assert_eq!(search.queries.borrow().as_slice(), ["topic"]);
    assert_eq!(
        result.turn_instruction.as_deref(),
        Some(EXPECTED_SEMANTIC_NOTICE)
    );
    assert_eq!(result.diagnostics[0], diagnostic);
    assert_eq!(result.memories.len(), 2);
    let full = result.memories.iter().find(|m| m.memory_id == A).unwrap();
    assert_eq!(full.current_heads.len(), 2);
    assert_eq!(full.current_heads[0].what_to_remember, "First exact head");
    assert_eq!(full.current_heads[1].what_to_remember, "Second exact head");
    assert_eq!(full.head_reconciliation_required, Some(true));
    assert!(result.memories.iter().any(|m| m.memory_id == B));
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].memory_id, C);
    let counts = &result.candidate_counts.as_ref().unwrap().memories;
    assert_eq!((counts.matched, counts.shown), (1, 1));
}

#[test]
fn restored_recall_expands_search_and_emits_one_semantic_notice() {
    let diagnostic = DomainError::new(
        "semantic_channel_incomplete",
        "The worker did not establish a complete semantic view.",
    );
    let search = ObservedSearch {
        candidates: vec![discovery_candidate(C, &["lexical", "vector"])],
        diagnostics: vec![diagnostic.clone()],
        ..Default::default()
    };
    let result = recall(
        &Snapshot::default(),
        &search,
        &context(),
        &RecallRequest {
            recall_for: Some("topic".into()),
            restore_context: Some(true),
            ..Default::default()
        },
        "now".into(),
    )
    .unwrap();
    let queries = search.queries.borrow();
    assert_eq!(
        queries.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "topic",
            "topic\nPerspective: intent and goals",
            "topic\nPerspective: choices and alternatives",
            "topic\nPerspective: responsibilities and flow",
            "topic\nPerspective: conditions and exceptions",
            "topic\nPerspective: failures and verification",
            "topic\nPerspective: progress and unresolved questions",
        ]
    );
    assert_eq!(
        result.turn_instruction.as_deref(),
        Some(EXPECTED_SEMANTIC_NOTICE)
    );
    assert_eq!(&result.diagnostics[..7], vec![diagnostic; 7].as_slice());
    assert_eq!(result.diagnostics.len(), 8);
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].memory_id, C);
    assert_eq!(result.candidates[0].retrieval_paths.len(), 1);
    assert_eq!(
        result.candidates[0].retrieval_paths[0].via,
        ["lexical", "vector"]
    );
    let counts = &result.candidate_counts.as_ref().unwrap().memories;
    assert_eq!((counts.matched, counts.shown), (1, 1));
}

#[test]
fn duplicate_and_distinct_semantic_diagnostics_preserve_one_notice() {
    let repeated = DomainError::new(
        "semantic_channel_incomplete",
        "The semantic worker is not ready; exact and indexed lexical retrieval remain available.",
    );
    let mut distinct = DomainError::new(
        "semantic_channel_incomplete",
        "The worker did not establish a complete semantic view.",
    )
    .with_details(serde_json::json!({"provider_detail": "preserve this detail"}));
    distinct.retryable = true;
    let diagnostics = vec![
        repeated.clone(),
        DomainError::new("unrelated_diagnostic", "Preserve this diagnostic too."),
        repeated,
        distinct,
    ];
    let search = ObservedSearch {
        diagnostics: diagnostics.clone(),
        ..Default::default()
    };
    let result = recall(
        &Snapshot::default(),
        &search,
        &context(),
        &RecallRequest {
            recall_for: Some("topic".into()),
            ..Default::default()
        },
        "now".into(),
    )
    .unwrap();
    assert_eq!(search.queries.borrow().as_slice(), ["topic"]);
    assert_eq!(
        result.turn_instruction.as_deref(),
        Some(EXPECTED_SEMANTIC_NOTICE)
    );
    assert_eq!(
        &result.diagnostics[..diagnostics.len()],
        diagnostics.as_slice()
    );
    assert_eq!(result.diagnostics.len(), diagnostics.len() + 1);
    assert!(result.diagnostics[3].retryable);
    assert_eq!(
        result.diagnostics[3].details,
        Some(serde_json::json!({"provider_detail": "preserve this detail"}))
    );
}

// Exercise the disk-backed public response from the same logical heads used by
// the in-memory selection tests.
fn with_public_recall(
    snapshot: &Snapshot,
    degraded: bool,
    check: impl FnOnce(&mut dyn FnMut(&RecallRequest) -> serde_json::Value),
) {
    use agentlaw_flows::{
        control::ControlReader,
        exact::{DeltaBudget, ExactIndexWriter, ExactSnapshot, ProjectionGenerations},
        history_cache::HistoryCacheGuards,
        semantic::*,
        store::{ReadCtx, StoreIdentity},
    };
    use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
    use serde_json::json;

    struct SemanticFixture(bool);
    impl SemanticPort for SemanticFixture {
        fn embed_prefix(
            &self,
            _: &str,
            _: &RequestControl,
        ) -> std::result::Result<PrefixEmbedding, SemanticError> {
            if self.0 {
                return Err(SemanticError::Unavailable("Model unavailable".into()));
            }
            Ok(PrefixEmbedding {
                vector: QueryVector {
                    model: "test".into(),
                    values: std::sync::Arc::from([1.0]),
                },
                truncated: false,
            })
        }
        fn plan_pieces(
            &self,
            _: &[&str],
            _: &RequestControl,
        ) -> std::result::Result<PiecePlan, SemanticError> {
            unreachable!("recall only embeds query prefixes")
        }
        fn embed_planned(
            &self,
            _: &PiecePlan,
            _: usize,
            _: &PieceVectorReuse,
            _: &RequestControl,
        ) -> std::result::Result<Vec<QueryVector>, SemanticError> {
            unreachable!("recall only embeds query prefixes")
        }
        fn search_available(
            &self,
            query: SemanticQuery<'_>,
            _: &RequestControl,
        ) -> std::result::Result<SemanticCandidates, SemanticError> {
            Ok(SemanticCandidates {
                hits: vec![],
                coverage: SemanticCoverage::CompleteAt(query.source.clone()),
            })
        }
    }
    impl HistorySemanticPort for SemanticFixture {
        fn offer(&self, _: HistoryIngestHint) -> OfferResult {
            unreachable!("recall does not ingest history")
        }
        fn search_available(
            &self,
            _: &str,
            _: &str,
            _: &QueryVector,
            _: &[String],
            _: usize,
            _: &RequestControl,
        ) -> std::result::Result<SemanticCandidates, SemanticError> {
            unreachable!("recall does not search history")
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&local).unwrap();
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let new_id = || uuid::Uuid::new_v4().to_string();
    let mutations = snapshot.0.values().map(|state| {
        let heads = state.heads.iter().map(|head| {
            let applicability = &head.applicability;
            let scope = match applicability.scope.as_slice() {
                [ScopeKind::User] => "user",
                [ScopeKind::Project] => "project",
                [ScopeKind::Machine] => "machine",
                [ScopeKind::Project, ScopeKind::Machine] => "project_machine",
                _ => panic!("unexpected scope for recall fixture"),
            };
            let mut scope = json!({"scope":scope});
            if let Some(project) = &applicability.project_id {
                scope["project_id"] = json!(project);
            }
            if let Some(machine) = &applicability.machine_id {
                scope["machine_id"] = json!(machine);
            }
            let relations: Vec<_> = head.required_memory_ids.iter().map(|id| {
                json!({"kind":"required","target_memory_id":id})
            }).collect();
            Head {
                metadata: json!({"change_id":new_id(),"applicability":scope,"origin":{"machine_id":A,"user_id":"user"},"recorded_at_ms":0,"is_rule":head.is_rule,"relations":relations,"work_targets":[]}),
                body: head.what_to_remember.clone(),
            }
        }).collect();
        Mutation {
            unit: CurrentUnit {
                entity_id: state.resolved_id.clone(),
                entity_type: "memory".into(),
                state: UnitState::Live { heads },
            },
            expected_versions: vec![],
            evidence: "Observed recall fixture".into(),
        }
    }).collect();
    store
        .publish_catalog(
            &new_id(),
            B,
            json!({"project_id":B,"name":"Recall fixture","description":null,"aliases":[],"repository_observations":[],"catalog_revision":new_id()}),
        )
        .unwrap();
    store.publish(&new_id(), mutations).unwrap();
    let identity = StoreIdentity {
        root: std::fs::canonicalize(root).unwrap(),
        local,
        user_id: "user".into(),
        machine_id: A.into(),
        binding_id: new_id(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: None,
    };
    let mut writer = agentlaw_flows::control::ControlWriter::open(&identity.local).unwrap();
    writer.bind_identity(&identity).unwrap();
    writer.finish_binding_initialization(&identity).unwrap();
    let source = store.owned_published_reader();
    let projection = identity.local.join("projection");
    let control = RequestControl::default();
    let shadow = ExactIndexWriter::rebuild_shadow_in(&projection, &source, &control).unwrap();
    let guard = source.read_snapshot().unwrap();
    let registry = ProjectionGenerations::open_for_recovery(
        "bootstrap",
        &guard.position().epoch,
        projection.join("exact-bootstrap.sqlite"),
        projection.join("lexical-bootstrap.sqlite"),
    )
    .unwrap();
    let staged = registry.stage(shadow).unwrap();
    registry.commit(staged).unwrap();
    let mut exact =
        ExactSnapshot::build_for_recall(registry.pin().unwrap(), &guard, DeltaBudget::default())
            .unwrap();
    drop(guard);
    let reader = ControlReader::open(&identity.local).unwrap();
    let semantic = SemanticFixture(degraded);
    let cache = HistoryCacheGuards::new();
    let ctx = ReadCtx {
        identity: &identity,
        source: &source,
        control: &reader,
        semantic: &semantic,
        history_semantic: &semantic,
        history_cache: &cache,
        request: &control,
    };
    check(&mut |request| {
        agentlaw_flows::read::recall_for_project(&ctx, &mut exact, request, B).unwrap()
    });
}

#[test]
fn fr_06_5_degraded_recall_appends_notice_to_required_context_warning() {
    const REQUIRED_WARNING: &str = "Recover the missing required context using the provided steps. If recovery remains blocked, tell the user what could not be checked. Do not make changes that depend on that context; independent investigation may continue.";
    let mut source = Snapshot::default();
    let mut rule = memory(A);
    rule.is_rule = true;
    rule.required_memory_ids.push(B.into());
    source.insert(rule);
    let request = RecallRequest {
        recall_for: Some("topic".into()),
        ..Default::default()
    };
    let baseline = recall(&source, &EmptySearch, &context(), &request, "now".into()).unwrap();
    assert_eq!(baseline.turn_instruction.as_deref(), Some(REQUIRED_WARNING));
    assert_eq!(baseline.memories.len(), 1);
    assert_eq!(baseline.undelivered_required.len(), 1);
    assert_eq!(baseline.undelivered_required[0].memory_id, B);
    assert_eq!(
        baseline.undelivered_required[0].reason,
        "The referenced current identity is absent; required context was not delivered."
    );
    let diagnostic = DomainError::new(
        "semantic_channel_incomplete",
        "Semantic retrieval failed; exact and indexed lexical retrieval remain available.",
    );
    let search = ObservedSearch {
        diagnostics: vec![diagnostic.clone()],
        ..Default::default()
    };
    let result = recall(&source, &search, &context(), &request, "now".into()).unwrap();
    assert_eq!(search.queries.borrow().as_slice(), ["topic"]);
    let mut expected = baseline;
    expected.turn_instruction = Some(format!("{REQUIRED_WARNING} {EXPECTED_SEMANTIC_NOTICE}"));
    expected.diagnostics.insert(0, diagnostic);
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let public = |degraded| {
        let mut result = serde_json::Value::Null;
        with_public_recall(&source, degraded, |query| result = query(&request));
        result
    };
    let baseline = public(false);
    let degraded = public(true);
    assert_eq!(
        baseline["rules"],
        serde_json::json!([{
            "applicability":{"scope":["project"],"project_id":B},
            "items":[{"memory_id":A,"what_to_remember":"Exact content"}]
        }])
    );
    assert!(baseline["memories"].as_array().unwrap().is_empty());
    assert_eq!(
        baseline["undelivered_required"].as_array().unwrap().len(),
        1
    );
    assert_eq!(baseline["undelivered_required"][0]["memory_id"], B);
    assert_eq!(baseline["turn_instruction"], REQUIRED_WARNING);
    assert_eq!(
        degraded["turn_instruction"],
        format!("{REQUIRED_WARNING} {EXPECTED_SEMANTIC_NOTICE}")
    );
    for field in [
        "rules",
        "memories",
        "undelivered_required",
        "candidates",
        "candidate_counts",
    ] {
        assert_eq!(degraded[field], baseline[field], "{field}");
    }
    let diagnostic_codes = |result: &serde_json::Value| {
        result["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| d["code"].clone())
            .collect::<Vec<_>>()
    };
    let mut codes = diagnostic_codes(&degraded);
    let index = codes
        .iter()
        .position(|code| code == "semantic_channel_incomplete")
        .unwrap();
    codes.remove(index);
    assert_eq!(codes, diagnostic_codes(&baseline));
    for response in [&baseline, &degraded] {
        let codes = diagnostic_codes(response);
        assert!(!codes
            .iter()
            .any(|code| code == "required_context_recovery" || code == "rule_review_guidance"));
    }
    let mut complete = memory(B);
    complete.what_to_remember = "Required material".into();
    source.insert(complete);
    with_public_recall(&source, false, |query| {
        let intact = query(&request);
        assert_eq!(intact["rules"], baseline["rules"]);
        assert!(intact.get("undelivered_required").is_none());
        assert!(intact.get("turn_instruction").is_none());
        assert!(!intact["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|diagnostic| {
                diagnostic["code"] == "required_context_recovery"
                    || diagnostic["code"] == "rule_review_guidance"
            }));
    });
}

#[test]
fn fr_06_5_contextual_recall_selects_same_head_applicable_rules_outside_search_limits() {
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
        ..contextual.clone()
    };
    let mixed_result = recall(&source, &EmptySearch, &context(), &mixed, "now".into()).unwrap();
    assert!(mixed_result.memories.iter().any(|m| m.memory_id == OTHER));
    use serde_json::json;
    use serde_json::Value;
    let full_head = |memory: &Value| {
        for head in memory["current_heads"].as_array().unwrap() {
            assert_eq!(head["what_to_remember"], "Exact content");
            assert_eq!(head["memory_ref"]["memory_id"], memory["memory_id"]);
            assert!(head["memory_ref"]["observed_version"].is_string());
            assert!(head["origin"].is_object());
        }
    };
    with_public_recall(&source, false, |query| {
        for request in [&contextual, &restored] {
            let result = query(request);
            let groups = result["rules"].as_array().unwrap();
            assert_eq!(groups.len(), 3);
            for (scope, id) in [
                (json!({"scope":["user"]}), USER),
                (json!({"scope":["project"],"project_id":B}), B),
                (json!({"scope":["machine"],"machine_id":A}), MACHINE),
            ] {
                assert!(groups.contains(&json!({"applicability":scope,"items":[{"memory_id":id,"what_to_remember":"Exact content"}]})));
            }
            let memories = result["memories"].as_array().unwrap();
            assert_eq!(memories.len(), 2);
            let extra = memories.iter().find(|m| m["memory_id"] == EXTRA).unwrap();
            assert_eq!(extra["current_heads"].as_array().unwrap().len(), 1);
            full_head(extra);
            let joint = memories.iter().find(|m| m["memory_id"] == BOTH).unwrap();
            assert_eq!(joint["current_heads"].as_array().unwrap().len(), 2);
            assert_eq!(joint["head_reconciliation_required"], true);
            full_head(joint);
            assert_eq!(result["candidate_counts"]["memories"]["shown"], 0);
        }
        let exact = query(&id_only);
        assert!(exact.get("rules").is_none());
        assert_eq!(exact["memories"].as_array().unwrap().len(), 1);
        assert_eq!(exact["memories"][0]["memory_id"], EXTRA);
        assert_eq!(
            exact["memories"][0]["current_heads"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        full_head(&exact["memories"][0]);
        let mixed = query(&mixed);
        let explicit = mixed["memories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["memory_id"] == OTHER)
            .unwrap();
        assert_eq!(explicit["current_heads"].as_array().unwrap().len(), 1);
        full_head(explicit);
    });
}

#[test]
fn fr_05_6_repeated_candidate_discoveries_preserve_views_sources_tasks_and_targets() {
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
                    source_memory_ids: None,
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
        if id == B {
            linked.in_working_set = Some(true);
        }
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
    assert_eq!(c.retrieval_paths.len(), 6);
    assert_eq!(c.retrieval_paths[0].via, ["lexical", "vector"]);
    assert_eq!(c.retrieval_paths[1].clue, "other head clue");
    assert_eq!(c.retrieval_paths[2].clue, "other view clue");
    let sources: Vec<_> = c
        .retrieval_paths
        .iter()
        .filter_map(|p| p.source_memory_ids.as_deref())
        .collect();
    assert_eq!(sources, [&[B, C][..]]);
    assert!(c
        .retrieval_paths
        .iter()
        .any(|p| p.via == ["active_task", "active_working_set"]));
    assert!(c
        .retrieval_paths
        .iter()
        .any(|p| p.via == ["work_target"] && p.clue == "src/task.rs"));
    let counts = &result.candidate_counts.as_ref().unwrap().memories;
    assert_eq!((counts.matched, counts.shown), (3, 1));
    assert_eq!(result.active_task_count, Some(2));
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
    replacement.what_to_remember = Some("replacement".into());
    let next = batch
        .apply_delta(&context(), &batch.reference, &[replacement])
        .unwrap();
    assert_eq!(next.proposals.len(), 2);
    assert_eq!(next.proposals[0].what_to_remember.as_deref(), Some("body"));
    assert_eq!(batch.proposals[1].what_to_remember.as_deref(), Some("body"));
    assert_ne!(next.reference, batch.reference);
    assert!(next
        .apply_delta(&context(), &batch.reference, &[proposal()])
        .is_err());
}
#[test]
fn task_headings_ignore_fenced_decoys() {
    let good = "# Objective\na\n# Current position\nb\n```md\n# Objective\n```\n# Resume point\nc\n# References\nd";
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
    let body = "# Objective\nDeliver fully.\n# Current position\nBefore fence.\n```md\n# Resume point\nNot a boundary\n```\nAfter fence.\n# Resume point\nResume all steps.\n# References\nNot in compact output.\n";
    let sections = agentlaw_flows::write::task_sections(body).unwrap();
    assert_eq!(sections[0], "Deliver fully.\n");
    assert!(sections[1].contains("Not a boundary"));
    assert!(sections[1].ends_with("After fence.\n"));
    assert_eq!(sections[2], "Resume all steps.\n");
}

#[test]
fn fr_10_4_active_task_selection_omitted_empty_and_multiple() {
    let request = |include_active_tasks| RecallRequest {
        recall_for: Some("unrelated".into()),
        include_active_tasks,
        memory_candidate_limit: Some(1),
        ..Default::default()
    };
    let lookup = |source: &Snapshot, flag| {
        recall(
            source,
            &EmptySearch,
            &context(),
            &request(flag),
            "now".into(),
        )
        .unwrap()
    };
    let mut source = Snapshot::default();
    assert_eq!(lookup(&source, Some(true)).active_task_count, Some(0));
    let mut first = memory(A);
    first.in_working_set = Some(true);
    first.what_to_remember = "# Objective\nFinish implementation\n## Objective # #\nVerify the handoff\n# Current position\nHalfway through\n# Resume point\nRun verification\n# References\nNone\n".into();
    source.insert(first);
    let single = lookup(&source, Some(true));
    assert_eq!(single.active_task_count, Some(1));
    assert_eq!(
        serde_json::to_value(single.active_task_handoff).unwrap(),
        serde_json::json!({
            "memory_id":A,
            "current_heads":[{"objective":"Finish implementation\n## Objective # #\nVerify the handoff\n","current_position":"Halfway through\n","resume_point":"Run verification\n"}]
        })
    );
    assert!(single
        .undelivered_required
        .iter()
        .all(|item| item.memory_id != A));

    let mut second = memory(B);
    second.in_working_set = Some(true);
    second.what_to_remember = "# Objective\nSecond task\n# Current position\nReady\n# Resume point\nContinue\n# References\nNone".into();
    source.insert(second);
    let omitted = lookup(&source, None);
    assert_eq!(omitted.active_task_count, None);
    assert!(omitted.memories.is_empty() && omitted.candidates.is_empty());
    let multiple = lookup(&source, Some(true));
    assert_eq!(multiple.active_task_count, Some(2));
    assert!(multiple.memories.is_empty());
    let counts = &multiple.candidate_counts.unwrap().memories;
    assert_eq!((counts.matched, counts.shown), (2, 1));
    assert_eq!(multiple.candidates.len(), 1);
}

#[test]
fn fr_08_6_multi_head_evolve_requires_full_body_even_with_all_refs() {
    let mut source = Snapshot::default();
    let first = memory(A);
    let mut second = first.clone();
    second.memory_ref.observed_version = "v2".into();
    second.what_to_remember = "Other current understanding".into();
    source.insert(first.clone());
    source.0.get_mut(A).unwrap().heads.push(second.clone());
    let mut evolve = proposal();
    evolve.operation = Operation::Evolve;
    evolve.applies_to = None;
    evolve.parent_refs = Some(vec![first.memory_ref, second.memory_ref]);
    evolve.what_to_remember = None;
    let failure = preflight(
        &source,
        &Reviewed,
        &context(),
        &[proposal(), evolve.clone()],
    )
    .unwrap_err();
    assert_eq!(failure.code, "invalid_input");
    assert!(!failure.message.is_empty());
    evolve.what_to_remember = Some("Complete reconciled understanding and context".into());
    assert!(preflight(&source, &Reviewed, &context(), &[evolve]).is_ok());
}

fn valid_write_proposal() -> MemoryProposal {
    serde_json::from_value(serde_json::json!({
        "operation":"create","what_to_remember":"Release check",
        "evidence":"Observed","applies_to":["user"]
    }))
    .unwrap()
}
fn write_error_detail<'a>(
    error: &'a DomainError,
    criterion: &str,
    path: &str,
) -> &'a serde_json::Value {
    error
        .details
        .as_ref()
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| entries.iter().find(|entry| entry["path"] == path))
        .unwrap_or_else(|| panic!("{criterion}: missing {path} in {error:?}"))
}

#[test]
fn fr_01_5_duplicate_alias_in_relationship_rejects_the_second_item() {
    // ID-09
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let parent = f.seed(A, "Original deployment checklist.");
    f.seed(B, "Use the signed release manifest.");
    let generation = f.store.generation().unwrap();
    let result = f.try_submit(json!({"memories":[{
        "operation":"evolve", "parent_refs":[parent], "what_to_remember":"Revised deployment checklist.",
        "evidence":"Observed", "related_memory_ids":[&B[..8], B]
    }]}));
    let error = result.unwrap_err();
    assert_eq!(error.code, "invalid_input", "FR-01.5 ID-09: {error:?}");
    let detail = write_error_detail(&error, "FR-01.5 ID-09", "/memories/0/related_memory_ids/1");
    assert_eq!(detail["provided_value"], B);
    let heads = f.store.read_current(A).unwrap().references().unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].observed_version, parent.observed_version);
    assert_eq!(f.store.generation().unwrap(), generation);
}

#[test]
fn fr_01_5_unresolved_write_ids_reject_the_batch_with_proposal_path() {
    // ID-14
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let parent = f.seed(A, "Original deployment checklist.");
    let generation = f.store.generation().unwrap();
    for missing in [C, &C[..8]] {
        for relationship in [true, false] {
            let (change, path) = if relationship {
                (
                    json!({"operation":"create","applies_to":["user"],"what_to_remember":"New archival instructions",
                    "evidence":"Observed","required_memory_ids":[missing]}),
                    "/memories/1/required_memory_ids/0",
                )
            } else {
                (
                    json!({"operation":"consolidate","what_to_remember":"Combined deployment checklist",
                    "evidence":"Observed","consolidation_refs":[parent,{"memory_id":missing,"observed_version":parent.observed_version}]}),
                    "/memories/1/consolidation_refs/1/memory_id",
                )
            };
            let unrelated = json!({"operation":"create","applies_to":["user"],"what_to_remember":"Unrelated observation","evidence":"Observed"});
            let error = f
                .try_submit(json!({"memories":[unrelated, change]}))
                .unwrap_err();
            assert_eq!(error.code, "not_found", "FR-01.5 ID-14: {error:?}");
            let detail = write_error_detail(&error, "FR-01.5 ID-14", path);
            assert_eq!(detail["provided_value"], missing);
            assert_eq!(f.store.generation().unwrap(), generation);
            let heads = f.store.read_current(A).unwrap().references().unwrap();
            assert_eq!(heads.len(), 1);
            assert_eq!(heads[0].observed_version, parent.observed_version);
        }
    }
}

#[test]
fn fr_01_6_active_task_requires_explicit_disposition_with_a_field_error() {
    // ER-02
    let mut source = Snapshot::default();
    let mut active = memory(A);
    active.what_to_remember = "# Objective\nShip release\n# Current position\nBuild ready\n# Resume point\nRun tests\n# References\nRelease plan".into();
    active.in_working_set = Some(true);
    source.insert(active.clone());
    let mut change = valid_write_proposal();
    change.operation = Operation::Evolve;
    change.applies_to = None;
    change.what_to_remember = None;
    change.parent_refs = Some(vec![active.memory_ref.clone()]);
    let error = preflight(&source, &Reviewed, &context(), &[change])
        .err()
        .expect("FR-01.6 ER-02");
    assert_eq!(error.code, "working_set_disposition_required");
    let detail = write_error_detail(&error, "FR-01.6 ER-02", "/memories/0/in_working_set");
    let text = format!("{detail} {:?}", error.next_action).to_lowercase();
    assert!(
        (text.contains("keep") || text.contains("true"))
            && (text.contains("end") || text.contains("close") || text.contains("false")),
        "{error:?}"
    );
    let unchanged = &source.current(A).unwrap().unwrap().heads[0];
    assert_eq!(unchanged.memory_ref, active.memory_ref);
    assert_eq!(unchanged.in_working_set, Some(true));
}

#[test]
fn fr_01_6_task_headings_error_identifies_the_body_field() {
    // ER-03
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let (project, _) = f.connect_project("release");
    let saved = f.submit(json!({"project_path":project, "memories":[{
        "operation":"create","applies_to":["project"],"in_working_set":true,
        "what_to_remember":RELEASE_TASK,"evidence":"Release review"
    }]}));
    let reference = &saved["results"][0]["memory_ref"];
    assert!(reference.is_object(), "{saved}");
    let wrong = "# Objective\nShip release\n# Resume point\nRun tests\n# Current position\nBuild ready\n# References\nRelease plan";
    let error = f
        .try_submit(json!({"project_path":project,"memories":[{
            "operation":"evolve","parent_refs":[reference],"in_working_set":true,
            "what_to_remember":wrong,"evidence":"Release review"
        }]}))
        .unwrap_err();
    assert_eq!(error.code, "invalid_task_headings");
    let detail = write_error_detail(&error, "FR-01.6 ER-03", "/memories/0/what_to_remember");
    let text = format!("{detail} {:?}", error.next_action).to_lowercase();
    assert!(
        text.contains("heading") && text.contains("order"),
        "{error:?}"
    );
    assert!(detail.get("provided_value").is_none());
    let recalled = f
        .with_read(|ctx, exact| {
            read::recall(
                ctx,
                exact,
                &RecallRequest {
                    memory_ids: Some(vec![reference["memory_id"].as_str().unwrap().to_owned()]),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let unchanged = &recalled["memories"][0]["current_heads"][0];
    assert_eq!(unchanged["memory_ref"], *reference);
    assert_eq!(unchanged["what_to_remember"], RELEASE_TASK);
}

#[test]
fn fr_01_6_task_scope_error_requires_project_only() {
    // ER-05
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    f.seed(A, "Existing release checklist");
    let inventory = |f: &PublishedWriteFixture| {
        let (_, entries, more) = f
            .store
            .owned_published_reader()
            .inventory_page(None, 32)
            .unwrap();
        assert!(!more);
        entries
            .into_iter()
            .map(|(_, id)| {
                let versions = f.store.read_current(&id).unwrap().references().unwrap();
                (id, versions)
            })
            .collect::<BTreeMap<_, _>>()
    };
    let before = inventory(&f);
    let error = f
        .try_submit(json!({"memories":[{
            "operation":"create","applies_to":["user"],"in_working_set":true,
            "what_to_remember":RELEASE_TASK,"evidence":"Release review"
        }]}))
        .unwrap_err();
    assert_eq!(error.code, "task_scope_required");
    let detail = write_error_detail(&error, "FR-01.6 ER-05", "/memories/0/applies_to");
    assert!(detail.get("proposal_id").is_none(), "{detail}");
    let text = format!("{detail} {:?}", error.next_action).to_lowercase();
    assert!(
        text.contains("project")
            && (text.contains("only") || text.contains("single") || text.contains("sole")),
        "{error:?}"
    );
    assert_eq!(inventory(&f), before);
}

#[test]
fn fr_01_6_stale_batch_indices_are_ordered_json_integers() {
    // ER-06, tool-error branch when no review can be constructed.
    let mut source = Snapshot::default();
    let mut changes = [
        valid_write_proposal(),
        valid_write_proposal(),
        valid_write_proposal(),
    ];
    for (index, id) in [(0, A), (2, C)] {
        let mut current = memory(id);
        changes[index].operation = Operation::Evolve;
        changes[index].applies_to = None;
        changes[index].parent_refs = Some(vec![current.memory_ref.clone()]);
        current.memory_ref.observed_version = "v2".into();
        source.insert(current);
    }
    let error = preflight(&source, &Reviewed, &context(), &changes)
        .err()
        .expect("FR-01.6 ER-06");
    assert_eq!(error.code, "stale_memory_ref");
    assert_eq!(
        error.details.unwrap()["proposal_indices"],
        serde_json::json!([0, 2])
    );
    assert_eq!(source.inventory().unwrap().len(), 2);
    for id in [A, C] {
        assert_eq!(
            source.current(id).unwrap().unwrap().heads[0]
                .memory_ref
                .observed_version,
            "v2"
        );
    }
}

#[test]
fn fr_01_6_consolidation_reports_all_unresolved_fields_and_each_parent_version() {
    // ER-01
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let (project, first_project) = f.connect_project("deployment");
    let (_, other_project) = f.connect_project("archive");
    f.seed(B, "Use the signed manifest.");
    let record =
        |description| json!({"target_memory_id":B,"kind":"related","description":description});
    let states = [
        (
            A,
            &first_project,
            true,
            false,
            record("Deployment evidence"),
        ),
        (
            A,
            &first_project,
            false,
            true,
            record("Previous deployment evidence"),
        ),
        (C, &other_project, false, true, record("Archive evidence")),
    ];
    let make_head = |project_id: &str, active, rule, relation: &serde_json::Value| {
        agentlaw_storage::Head {
            metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),
            "applicability":{"scope":"project","project_id":project_id},
            "origin":{"machine_id":f.identity.machine_id,"user_id":"user"},
            "recorded_at_ms":0,"is_rule":rule,"in_working_set":active,"relations":[relation],"work_targets":[]}),
            body: RELEASE_TASK.into(),
        }
    };
    let make_unit = |id, states: &[(&str, &String, bool, bool, serde_json::Value)]| {
        let mut mutation = f.mutation(id, RELEASE_TASK);
        mutation.unit.state = agentlaw_storage::UnitState::Live {
            heads: states
                .iter()
                .map(|(_, project, active, rule, relation)| {
                    make_head(project, *active, *rule, relation)
                })
                .collect(),
        };
        mutation
    };
    let first = make_unit(A, &states[..2]);
    let other = make_unit(C, &states[2..]);
    let mut expected = Vec::new();
    for (unit, expected_states) in [(&first.unit, &states[..2]), (&other.unit, &states[2..])] {
        for (head, (_, project_id, active, rule, relation)) in
            unit.heads().iter().zip(expected_states)
        {
            let reference = json!({"memory_id":unit.entity_id,"observed_version":agentlaw_storage::version(unit, head).unwrap()});
            expected.push((
                reference,
                json!({"scope":["project"],"project_id":project_id}),
                *active,
                *rule,
                relation,
            ));
        }
    }
    f.store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![first, other])
        .unwrap();
    let parents: Vec<_> = expected
        .iter()
        .map(|(reference, ..)| reference.clone())
        .collect();
    let proposal = json!({"operation":"consolidate","consolidation_refs":parents,
        "what_to_remember":RELEASE_TASK,"evidence":"Both archives reviewed","work_targets":[]});
    let before = f.store.generation().unwrap();
    let error = f
        .try_submit(json!({"project_path":project,"memories":[proposal]}))
        .unwrap_err();
    assert_eq!(error.code, "metadata_resolution_required", "{error:?}");
    assert_eq!(
        error.details.as_ref().unwrap().as_array().unwrap().len(),
        4,
        "{error:?}"
    );
    for (path, values) in [
        (
            "/memories/0/applies_to",
            expected
                .iter()
                .map(|head| head.1.clone())
                .collect::<Vec<_>>(),
        ),
        (
            "/memories/0/in_working_set",
            expected.iter().map(|head| json!(head.2)).collect(),
        ),
        (
            "/memories/0/is_rule",
            expected.iter().map(|head| json!(head.3)).collect(),
        ),
        (
            "/memories/0/related_memory_ids",
            expected.iter().map(|head| json!([head.4])).collect(),
        ),
    ] {
        let heads = write_error_detail(&error, "FR-01.6 ER-01", path)["heads"]
            .as_array()
            .unwrap();
        assert_eq!(heads.len(), expected.len());
        for (parent, value) in parents.iter().zip(values) {
            assert!(
                heads.contains(&json!({"memory_ref":parent,"value":value})),
                "{heads:?}"
            );
        }
    }
    assert_eq!(f.store.generation().unwrap(), before);
    let mut corrected = proposal;
    corrected["applies_to"] = json!(["project"]);
    corrected["in_working_set"] = json!(false);
    corrected["is_rule"] = json!(true);
    corrected["related_memory_ids"] = json!([B]);
    let result = f.submit(json!({"project_path":project,"memories":[corrected]}));
    assert_eq!(result["status"], "remembered", "{result}");
}

#[test]
fn fr_01_5_memory_writes_keep_canonical_parents_and_relationships() {
    // ID-10, memory branch
    use serde_json::json;
    let related = "44444444-4444-4444-8444-444444444444";
    for operation in ["evolve", "consolidate"] {
        let mut f = PublishedWriteFixture::new();
        let parent = f.seed(A, "Use a durable release check.");
        f.seed(C, "Keep the signed release manifest.");
        f.seed(related, "Record the release audit date.");
        let prefix_ref = |reference: &MemoryRef| json!({"memory_id":&reference.memory_id[..8],"observed_version":reference.observed_version});
        let mut change = json!({"operation":operation, "what_to_remember":"Review signed release records.",
            "evidence":"Updated release procedure","related_memory_ids":[&related[..8]],
            "required_memory_ids":[&C[..8]]});
        if operation == "evolve" {
            change["parent_refs"] = json!([prefix_ref(&parent)]);
        } else {
            let another = f.seed(B, "Check the release signatures.");
            change["consolidation_refs"] = json!([prefix_ref(&parent), prefix_ref(&another)]);
        }
        let response = f
            .try_submit(json!({"memories":[change]}))
            .unwrap_or_else(|error| panic!("FR-01.5 ID-10: {error:?}"));
        assert_eq!(response["status"], "remembered", "{response}");
        let reference = &response["results"][0]["memory_ref"];
        let id = reference["memory_id"].as_str().unwrap();
        let unit = f.store.read_current(id).unwrap();
        assert_eq!(
            unit.references().unwrap()[0].observed_version,
            reference["observed_version"]
        );
        let result = f
            .with_read(|ctx, exact| {
                read::recall(
                    ctx,
                    exact,
                    &RecallRequest {
                        memory_ids: Some(vec![id.into()]),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let memory = result["memories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["memory_id"] == id)
            .unwrap();
        let head = &memory["current_heads"][0];
        assert_eq!(head["memory_ref"], *reference);
        assert_eq!(head["related_memory_ids"], json!([related]));
        assert_eq!(head["required_memory_ids"], json!([C]));
    }
}

#[test]
fn fr_01_5_procedure_writes_keep_canonical_parent_and_evidence() {
    // ID-10, procedure branch
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    f.seed(A, "Release signatures come from the signed manifest.");
    f.seed(B, "The archive contains the approved record.");
    let mut proposal = json!({"operation":"create","evidence_memory_ids":[A]});
    let authoring = f.submit(json!({"procedure":proposal}));
    assert_eq!(authoring["status"], "authoring_required");
    proposal["applies_to"] = json!(["machine"]);
    proposal["name"] = json!("Check release signatures");
    proposal["use_when"] = json!("During release review");
    proposal["instructions"] = json!("Read the signed manifest.");
    proposal["evidence"] = json!("Documented release practice");
    let saved = f.submit(json!({"procedure":proposal,"authoring_ref":authoring["authoring_ref"]}));
    assert_eq!(saved["status"], "remembered", "{saved}");
    let parent = &saved["procedure_ref"];
    let id = parent["procedure_id"].as_str().unwrap();
    let mut change = json!({"operation":"evolve","parent_refs":[{
        "procedure_id":&id[..8],"observed_version":parent["observed_version"]
    }],"evidence_memory_ids":[&B[..8]]});
    let prepared = f
        .try_submit(json!({"procedure":change}))
        .unwrap_or_else(|error| panic!("FR-01.5 ID-10: {error:?}"));
    assert_eq!(prepared["status"], "authoring_required", "{prepared}");
    change["name"] = proposal["name"].clone();
    change["use_when"] = proposal["use_when"].clone();
    change["instructions"] = json!("Check the approved archival record.");
    change["evidence"] = json!("Reviewed the approved archive");
    let response = f.submit(json!({"procedure":change,"authoring_ref":prepared["authoring_ref"]}));
    assert_eq!(response["status"], "remembered", "{response}");
    assert_eq!(response["procedure_ref"]["procedure_id"], id);
    assert_ne!(
        response["procedure_ref"]["observed_version"],
        parent["observed_version"]
    );
    let result = f
        .with_read(|ctx, exact| {
            read::recall(
                ctx,
                exact,
                &RecallRequest {
                    procedure_ids: Some(vec![id.into()]),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let head = &result["learned_procedures"][0];
    assert_eq!(head["procedure_ref"], response["procedure_ref"]);
    assert_eq!(head["evidence_memory_ids"], json!([B]));
}

#[test]
fn fr_01_5_redirect_identity_remains_a_prefix_candidate() {
    // ID-12
    use agentlaw_storage::{CurrentUnit, Mutation, UnitState};
    let old = "abcdef12-b000-4000-8000-000000000002";
    let surviving = "abcdef12-a000-4000-8000-000000000001";
    let f = PublishedWriteFixture::new();
    let parent = f.seed(old, "Use the original checklist.");
    let destination = f.seed(surviving, "Record release verification.");
    let body = "Use the combined release checklist and record verification.";
    let mut combined = f.mutation(surviving, body);
    combined.expected_versions = vec![destination.observed_version];
    let change = combined.unit.heads()[0].metadata["change_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let redirect = Mutation {
        unit: CurrentUnit {
            entity_id: old.into(),
            entity_type: "memory".into(),
            state: UnitState::Redirect {
                redirect_to: surviving.into(),
                consolidation_change_id: change,
            },
        },
        expected_versions: vec![parent.observed_version],
        evidence: "Both source records reviewed".into(),
    };
    f.store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![combined, redirect])
        .unwrap();
    assert!(f
        .identity
        .root
        .join("current/memory/ab")
        .join(format!("{old}.md"))
        .is_file());
    assert_eq!(
        f.store.resolve_current(old).unwrap().current.entity_id,
        surviving
    );
    let lookup = |id: &str| {
        f.with_read(|ctx, exact| {
            read::recall(
                ctx,
                exact,
                &RecallRequest {
                    memory_ids: Some(vec![id.into()]),
                    ..Default::default()
                },
            )
        })
    };
    assert_eq!(
        lookup("abcdef12").unwrap_err().code,
        "ambiguous",
        "FR-01.5 ID-12"
    );
    let exact = lookup(old).unwrap();
    let short = lookup("abcdef12b").unwrap();
    assert_eq!(short["memories"], exact["memories"]);
    assert_eq!(short["redirects"], exact["redirects"]);
    assert_eq!(
        short["memories"][0]["current_heads"][0]["what_to_remember"],
        body
    );
}

struct PublishedWriteFixture {
    _journal: agentlaw_storage::JournalWriter,
    store: agentlaw_storage::Store,
    writer: control::ControlWriter,
    identity: store::StoreIdentity,
    projection: exact::ProjectionGenerations,
    _temp: tempfile::TempDir,
}

struct NoWriteEmbeddings;
impl semantic::SemanticPort for NoWriteEmbeddings {
    fn embed_prefix(
        &self,
        _: &str,
        _: &RequestControl,
    ) -> std::result::Result<semantic::PrefixEmbedding, semantic::SemanticError> {
        Err(semantic::SemanticError::Unavailable(
            "No model in write fixture".into(),
        ))
    }
    fn plan_pieces(
        &self,
        _: &[&str],
        _: &RequestControl,
    ) -> std::result::Result<semantic::PiecePlan, semantic::SemanticError> {
        Err(semantic::SemanticError::Unavailable(
            "No model in write fixture".into(),
        ))
    }
    fn embed_planned(
        &self,
        _: &semantic::PiecePlan,
        _: usize,
        _: &semantic::PieceVectorReuse,
        _: &RequestControl,
    ) -> std::result::Result<Vec<semantic::QueryVector>, semantic::SemanticError> {
        Err(semantic::SemanticError::Unavailable(
            "No model in write fixture".into(),
        ))
    }
    fn search_available(
        &self,
        _: semantic::SemanticQuery<'_>,
        _: &RequestControl,
    ) -> std::result::Result<semantic::SemanticCandidates, semantic::SemanticError> {
        Err(semantic::SemanticError::Unavailable(
            "No model in write fixture".into(),
        ))
    }
}
impl semantic::HistorySemanticPort for NoWriteEmbeddings {
    fn offer(&self, _: semantic::HistoryIngestHint) -> semantic::OfferResult {
        semantic::OfferResult::Accepted
    }
    fn search_available(
        &self,
        _: &str,
        _: &str,
        _: &semantic::QueryVector,
        _: &[String],
        _: usize,
        _: &RequestControl,
    ) -> std::result::Result<semantic::SemanticCandidates, semantic::SemanticError> {
        Err(semantic::SemanticError::Unavailable(
            "No model in write fixture".into(),
        ))
    }
}

impl PublishedWriteFixture {
    fn connect_project(&mut self, name: &str) -> (std::path::PathBuf, String) {
        let path = self._temp.path().join(name);
        std::fs::create_dir(&path).unwrap();
        let fields = serde_json::json!({"intent":"create","project_path":path,"project_name":name});
        let plan = self
            .with_read(|ctx, _| {
                prepare::prepare_connect(ctx, serde_json::from_value(fields).unwrap())
            })
            .unwrap();
        let apply::Applied::Connected { response, .. } =
            self.with_lane(|ctx| apply::apply(ctx, plan)).unwrap()
        else {
            panic!("Expected project connection");
        };
        let response = serde_json::to_value(response).unwrap();
        (
            path,
            response["project_connection"]["project_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    }
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        let local = temp.path().join("local");
        let store = agentlaw_storage::Store::open(&root, local.join("canonical")).unwrap();
        let journal = store.hold_journal_writer().unwrap();
        let identity = store::StoreIdentity {
            root: std::fs::canonicalize(root).unwrap(),
            local,
            user_id: "user".into(),
            machine_id: A.into(),
            binding_id: C.into(),
            history_limit: 8192,
            response_limit: 65536,
            config_file: None,
        };
        let mut writer = control::ControlWriter::open(&identity.local).unwrap();
        writer.bind_identity(&identity).unwrap();
        writer.finish_binding_initialization(&identity).unwrap();
        let source = store.owned_published_reader();
        let directory = identity.local.join("projection");
        let shadow = exact::ExactIndexWriter::rebuild_shadow_in(
            &directory,
            &source,
            &RequestControl::default(),
        )
        .unwrap();
        let epoch = source.read_snapshot().unwrap().position().epoch.clone();
        let projection = exact::ProjectionGenerations::open_for_recovery(
            "bootstrap",
            &epoch,
            directory.join("exact-bootstrap.sqlite"),
            directory.join("lexical-bootstrap.sqlite"),
        )
        .unwrap();
        projection
            .commit(projection.stage(shadow).unwrap())
            .unwrap();
        Self {
            _temp: temp,
            _journal: journal,
            store,
            writer,
            identity,
            projection,
        }
    }

    fn mutation(&self, id: &str, body: &str) -> agentlaw_storage::Mutation {
        use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
        use serde_json::json;
        Mutation {
            unit: CurrentUnit {
                entity_id: id.into(),
                entity_type: "memory".into(),
                state: UnitState::Live {
                    heads: vec![Head {
                        metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},
                        "origin":{"machine_id":self.identity.machine_id,"user_id":"user"},
                        "recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                        body: body.into(),
                    }],
                },
            },
            expected_versions: vec![],
            evidence: "Observed test publication".into(),
        }
    }

    fn seed(&self, id: &str, body: &str) -> MemoryRef {
        let receipt = self
            .store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![self.mutation(id, body)],
            )
            .unwrap();
        let reference = &receipt.references[0];
        MemoryRef {
            memory_id: reference.memory_id.clone(),
            observed_version: reference.observed_version.clone(),
        }
    }

    fn with_read<T>(
        &self,
        operation: impl FnOnce(&store::ReadCtx<'_>, &mut exact::ExactSnapshot) -> Result<T>,
    ) -> Result<T> {
        let source = self.store.owned_published_reader();
        let reader = control::ControlReader::open(&self.identity.local)?;
        let cache = history_cache::HistoryCacheGuards::new();
        let control = RequestControl::default();
        let ctx = store::ReadCtx {
            identity: &self.identity,
            source: &source,
            control: &reader,
            semantic: &NoWriteEmbeddings,
            history_semantic: &NoWriteEmbeddings,
            history_cache: &cache,
            request: &control,
        };
        let guard = source.read_snapshot().unwrap();
        let mut exact = exact::ExactSnapshot::build(
            self.projection.pin()?,
            &guard,
            exact::DeltaBudget::default(),
        )?;
        drop(guard);
        operation(&ctx, &mut exact)
    }

    fn prepare(&self, fields: serde_json::Value) -> Result<prepare::Prepared> {
        self.with_read(|ctx, exact| {
            prepare::prepare(ctx, exact, serde_json::from_value(fields).unwrap())
        })
    }

    fn with_lane<T>(
        &mut self,
        operation: impl FnOnce(&mut store::LaneCtx<'_>) -> Result<T>,
    ) -> Result<T> {
        let request = RequestControl::default();
        operation(&mut store::LaneCtx {
            identity: &self.identity,
            source: &self.store,
            control: &mut self.writer,
            request: &request,
        })
    }

    fn try_submit(&mut self, fields: serde_json::Value) -> Result<serde_json::Value> {
        let plan = self.prepare(fields)?;
        match self.with_lane(|ctx| apply::apply(ctx, plan))? {
            apply::Applied::Response(response) => Ok(response),
            _ => panic!("Expected remember_this response"),
        }
    }

    fn submit(&mut self, fields: serde_json::Value) -> serde_json::Value {
        self.try_submit(fields).unwrap()
    }
}

#[test]
fn fr_01_5_pending_proposal_keeps_its_canonical_target_after_prefix_collision() {
    // ID-16
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let parent = "abcdef12-a000-4000-8000-000000000001";
    let concurrent = "abcdef12-b000-4000-8000-000000000002";
    let reference = f.seed(parent, "Persist the transaction before acknowledging it.");
    let dependent = f.submit(
        json!({"memories":[{"operation":"create","applies_to":["user"],
        "what_to_remember":"Clients can retry after the durable acknowledgment.",
        "evidence":"Documented protocol dependency","required_memory_ids":[parent]}]}),
    );
    assert_eq!(dependent["status"], "remembered");
    let pending = f.submit(json!({"memories":[{"operation":"evolve",
        "parent_refs":[{"memory_id":"abcdef12","observed_version":reference.observed_version}],
        "what_to_remember":"Acknowledge after persisting the transaction and its journal.",
        "evidence":"Revised protocol"}]}));
    assert_eq!(
        pending["status"], "resolution_required",
        "FR-01.5 ID-16: {pending}"
    );
    let pending_ref = &pending["pending_batch_ref"];
    let inspect = |f: &PublishedWriteFixture| {
        f.with_read(|ctx, _| {
            read::pending_inspect(
                ctx,
                &serde_json::from_value(
                    json!({"pending_action":"inspect","pending_batch_ref":pending_ref}),
                )
                .unwrap(),
            )
        })
        .unwrap()
    };
    let retained = inspect(&f);
    assert_eq!(
        retained["proposals"][0]["parent_refs"],
        json!([reference]),
        "{retained}"
    );
    let other = f.seed(concurrent, "Record unrelated deployment evidence.");
    let resumed = f.submit(json!({"pending_action":"continue","pending_batch_ref":pending_ref}));
    assert_eq!(resumed["status"], "resolution_required", "{resumed}");
    assert!(
        resumed["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["code"] == "required_dependent_review"),
        "{resumed}"
    );
    for head in [&reference, &other] {
        let current = f.store.read_current(&head.memory_id).unwrap();
        let references = current.references().unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].observed_version, head.observed_version);
    }
}

#[test]
fn fr_01_6_interrupted_decided_publication_guides_state_check_before_resubmit() {
    // ER-15
    use agentlaw_storage::{Error, FaultPoint};
    let mut f = PublishedWriteFixture::new();
    let id = "abcdef12-a000-4000-8000-000000000001";
    let original = f.seed(id, "Keep the original release record.");
    let body = "Preserve the verified release record.";
    let operation = uuid::Uuid::new_v4().to_string();
    let mut mutation = f.mutation(id, body);
    mutation.expected_versions = vec![original.observed_version];
    let fault = f
        .store
        .publish_with_fault(&operation, vec![mutation], Some(FaultPoint::Decision));
    assert!(
        matches!(fault, Err(Error::Interrupted(FaultPoint::Decision))),
        "{fault:?}"
    );

    let shard = f.identity.root.join("current/memory").join(&id[..2]);
    let moved = f._temp.path().join("moved-memory-directory");
    std::fs::rename(&shard, &moved).unwrap();
    std::fs::write(&shard, "A file obstructs installation of the decided state").unwrap();
    let blocked = f.with_lane(recovery::recover);
    std::fs::remove_file(&shard).unwrap();
    std::fs::rename(moved, shard).unwrap();

    f.with_lane(recovery::recover).unwrap();
    f.with_lane(recovery::recover).unwrap();
    let current = f.store.read_current(id).unwrap();
    assert_eq!(current.heads().len(), 1);
    assert_eq!(current.heads()[0].body, body);
    let history = f.store.history(id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history.iter().filter(|entry| entry.body == body).count(), 1);
    let error = blocked.expect_err("FR-01.6 ER-15: obstruction must prevent recovery");
    let action = error
        .next_action
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        (action.contains("check") || action.contains("confirm"))
            && (action.contains("source")
                || action.contains("state")
                || action.contains("operation")),
        "{error:?}"
    );
}

#[test]
fn fr_01_6_stale_batch_review_keeps_correct_indices_and_current_heads() {
    // ER-06, recoverable review branch.
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let first = f.seed(A, "Reviewed the deployment checklist.");
    let last = f.seed(B, "Verified the archived manifest.");
    let mut current_refs = Vec::new();
    for (head, body) in [
        (&first, "Revised the deployment checklist."),
        (&last, "Confirmed the latest manifest."),
    ] {
        let mut update = f.mutation(&head.memory_id, body);
        update.expected_versions = vec![head.observed_version.clone()];
        let saved = f
            .store
            .publish(&uuid::Uuid::new_v4().to_string(), vec![update])
            .unwrap();
        current_refs.push(serde_json::to_value(&saved.references[0]).unwrap());
    }
    let generation = f.store.generation().unwrap();
    let response = f.submit(json!({"memories":[
        {"operation":"evolve","parent_refs":[first],"what_to_remember":"Caller deployment notes","evidence":"Review"},
        {"operation":"create","applies_to":["user"],"what_to_remember":"New unrelated observation","evidence":"Observation"},
        {"operation":"evolve","parent_refs":[last],"what_to_remember":"Caller manifest notes","evidence":"Review"}
    ]}));
    assert_eq!(
        response["status"], "resolution_required",
        "FR-01.6 ER-06: {response}"
    );
    let issues: Vec<_> = response["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|issue| issue["code"] == "stale_memory_ref")
        .collect();
    assert_eq!(issues.len(), 2, "{response}");
    for (index, head) in [0, 2].into_iter().zip(current_refs) {
        let issue = issues
            .iter()
            .find(|issue| issue["proposal_index"] == index)
            .unwrap_or_else(|| panic!("{response}"));
        assert_eq!(
            issue["current_states"][0]["current_heads"][0]["memory_ref"],
            head
        );
    }
    assert_eq!(f.store.generation().unwrap(), generation);
}

#[test]
fn fr_01_6_rejected_consolidation_preserves_pending_version_for_corrected_submission() {
    // ER-01
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let original = f.seed(A, "Check the previous release checklist.");
    let mut updated = f.mutation(A, "Verify the latest release checklist.");
    updated.expected_versions = vec![original.observed_version.clone()];
    let mut rule = f.mutation(B, "Always check signed deployment results.");
    let agentlaw_storage::UnitState::Live { heads } = &mut rule.unit.state else {
        unreachable!()
    };
    heads[0].metadata["is_rule"] = json!(true);
    let receipt = f
        .store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![updated, rule])
        .unwrap();
    let current = |id| {
        receipt
            .references
            .iter()
            .find(|reference| reference.memory_id == id)
            .unwrap()
    };
    let options = json!({"operation":"consolidate","consolidation_refs":[original,current(B)],
        "what_to_remember":"Verify the signed results and latest release checklist.",
        "evidence":"Both records reviewed","applies_to":["user"]});
    let pending = f.submit(json!({"memories":[options]}));
    assert_eq!(pending["status"], "resolution_required", "{pending}");
    let pending_ref = &pending["pending_batch_ref"];
    let mut replacement = options;
    replacement["proposal_id"] = pending["proposal_map"][0]["proposal_id"].clone();
    replacement["consolidation_refs"] = json!([current(A), current(B)]);
    let resume = |proposal| {
        json!({"pending_action":"continue","pending_batch_ref":pending_ref,
        "memories":[proposal]})
    };
    let generation = f.store.generation().unwrap();
    let error = f.try_submit(resume(replacement.clone())).unwrap_err();
    assert_eq!(
        error.code, "metadata_resolution_required",
        "FR-01.6 ER-01: {error:?}"
    );
    assert_eq!(f.store.generation().unwrap(), generation);
    let retained = f
        .with_read(|ctx, _| {
            read::pending_inspect(
                ctx,
                &serde_json::from_value(
                    json!({"pending_action":"inspect","pending_batch_ref":pending_ref}),
                )
                .unwrap(),
            )
        })
        .unwrap();
    assert_eq!(retained["pending_batch_ref"], *pending_ref);
    assert_eq!(retained["proposals"], pending["proposals"]);
    let heads = write_error_detail(&error, "FR-01.6 ER-01", "/memories/0/is_rule")["heads"]
        .as_array()
        .unwrap();
    assert!(
        heads.contains(&json!({"memory_ref":current(A),"value":false})),
        "{heads:?}"
    );
    assert!(
        heads.contains(&json!({"memory_ref":current(B),"value":true})),
        "{heads:?}"
    );
    replacement["is_rule"] = heads
        .iter()
        .find(|head| head["memory_ref"]["memory_id"] == B)
        .unwrap()["value"]
        .clone();
    let corrected = f.submit(resume(replacement));
    assert_eq!(corrected["status"], "remembered", "{corrected}");
    let result = &corrected["results"][0]["memory_ref"];
    let head = f
        .store
        .read_current(result["memory_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(head.heads()[0].metadata["is_rule"], true);
}

const RELEASE_TASK: &str = "# Objective\nShip release\n# Current position\nBuild ready\n# Resume point\nRun tests\n# References\nRelease plan";

fn evolve_fixture_parent(f: &PublishedWriteFixture, parent: &MemoryRef, body: &str) -> MemoryRef {
    let mut mutation = f.mutation(&parent.memory_id, body);
    mutation.expected_versions = vec![parent.observed_version.clone()];
    let receipt = f
        .store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![mutation])
        .unwrap();
    MemoryRef {
        memory_id: parent.memory_id.clone(),
        observed_version: receipt.references[0].observed_version.clone(),
    }
}

#[test]
fn unique_prefix_with_changed_heads_returns_current_review() {
    // ER-23
    use serde_json::json;
    for identity in [A, &A[..8]] {
        let mut f = PublishedWriteFixture::new();
        let parent = f.seed(A, "Preserve the release record.");
        let body = "Preserve the signed release record.";
        let plan = f.prepare(json!({"memories":[{
            "operation":"evolve","parent_refs":[{"memory_id":identity,"observed_version":parent.observed_version}],
            "what_to_remember":body,"evidence":"Caller inspected the release"
        }]})).unwrap();
        let concurrent = evolve_fixture_parent(&f, &parent, "Concurrent release verification.");
        let generation = f.store.generation().unwrap();
        let apply::Applied::Response(response) =
            f.with_lane(|ctx| apply::apply(ctx, plan)).unwrap()
        else {
            panic!("FR-01.6 ER-23: review response required");
        };
        assert_eq!(response["status"], "resolution_required", "{response}");
        assert!(response["pending_batch_ref"].is_object(), "{response}");
        let stale = response["issues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|issue| issue["current_states"].is_array())
            .unwrap_or_else(|| panic!("FR-01.6 ER-23: {identity}: {response}"));
        let heads = stale["current_states"][0]["current_heads"]
            .as_array()
            .unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0]["memory_ref"], json!(concurrent));
        assert_eq!(
            heads[0]["what_to_remember"],
            "Concurrent release verification."
        );
        assert_eq!(stale["submitted_proposal"]["what_to_remember"], body);
        assert_eq!(f.store.generation().unwrap(), generation);
        let mut revised = stale["submitted_proposal"].clone();
        revised["parent_refs"] = json!([concurrent]);
        let completed = f.submit(json!({"pending_action":"continue",
            "pending_batch_ref":response["pending_batch_ref"], "memories":[revised]}));
        assert_eq!(completed["status"], "remembered", "{completed}");
    }
}

fn pending_three_proposals(
    f: &mut PublishedWriteFixture,
) -> (serde_json::Value, MemoryRef, MemoryRef) {
    use serde_json::json;
    let old = f.seed(A, "Review deployment notes.");
    let last = f.seed(B, "Inspect the signed manifest.");
    let current = evolve_fixture_parent(f, &old, "Review latest deployment notes.");
    let pending = f.submit(json!({"memories":[
        {"operation":"evolve","parent_refs":[old],"what_to_remember":"Include the deployment schedule.","evidence":"Observed"},
        {"operation":"create","applies_to":["user"],"what_to_remember":"Archive the verified image.","evidence":"Observed"},
        {"operation":"evolve","parent_refs":[last],"what_to_remember":"Review the manifest and its signature.","evidence":"Observed"}
    ]}));
    assert_eq!(pending["status"], "resolution_required", "{pending}");
    assert_eq!(pending["proposal_map"].as_array().unwrap().len(), 3);
    (pending, current, last)
}

#[test]
fn continued_proposal_errors_use_submitted_input_order() {
    // ER-24 and DD-T04
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let (pending, current, _) = pending_three_proposals(&mut f);
    let reference = &pending["pending_batch_ref"];
    let mut last = pending["proposals"][2].clone();
    last["required_memory_ids"] = json!([C]);
    let mut first = pending["proposals"][0].clone();
    first["parent_refs"] = json!([current]);
    let before = f.store.generation().unwrap();
    let error = f
        .try_submit(
            json!({"pending_action":"continue","pending_batch_ref":reference,
        "memories":[last,first]}),
        )
        .unwrap_err();
    assert_eq!(error.code, "not_found", "FR-01.6 ER-24: {error:?}");
    let detail = write_error_detail(&error, "FR-01.6 ER-24", "/memories/0/required_memory_ids/0");
    assert_eq!(detail["proposal_index"], 2);
    assert_eq!(
        detail["proposal_id"],
        pending["proposal_map"][2]["proposal_id"]
    );
    let retained = f
        .with_read(|ctx, _| {
            read::pending_inspect(
                ctx,
                &serde_json::from_value(
                    json!({"pending_action":"inspect","pending_batch_ref":reference}),
                )
                .unwrap(),
            )
        })
        .unwrap();
    assert_eq!(retained["pending_batch_ref"], *reference);
    assert_eq!(retained["proposals"], pending["proposals"]);
    assert_eq!(f.store.generation().unwrap(), before);
}

#[test]
fn retained_only_review_does_not_invent_an_input_pointer() {
    // ER-25 and DD-T04
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let (pending, current, last) = pending_three_proposals(&mut f);
    let concurrent = evolve_fixture_parent(&f, &last, "Use the latest signed manifest.");
    let before = f.store.generation().unwrap();
    let mut first = pending["proposals"][0].clone();
    first["parent_refs"] = json!([current]);
    let review = f.submit(
        json!({"pending_action":"continue","pending_batch_ref":pending["pending_batch_ref"],
        "memories":[first]}),
    );
    assert_eq!(
        review["status"], "resolution_required",
        "FR-01.6 ER-25: {review}"
    );
    let issue = review["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "stale_memory_ref" && issue["proposal_index"] == 2)
        .unwrap();
    assert_eq!(
        issue["proposal_id"],
        pending["proposal_map"][2]["proposal_id"]
    );
    assert_eq!(issue["submitted_proposal"], pending["proposals"][2]);
    assert!(issue.get("path").is_none(), "{issue}");
    assert_eq!(
        issue["current_states"][0]["current_heads"][0]["memory_ref"],
        json!(concurrent)
    );
    assert_eq!(
        issue["current_states"][0]["current_heads"][0]["what_to_remember"],
        "Use the latest signed manifest."
    );
    assert_eq!(f.store.generation().unwrap(), before);
}

#[test]
fn consolidation_collects_dispositions_across_the_batch() {
    // ER-26
    use serde_json::json;
    let mut f = PublishedWriteFixture::new();
    let (project, project_id) = f.connect_project("release");
    let evidence = "44444444-4444-4444-8444-444444444444";
    f.seed(evidence, "Use the verified release manifest.");
    let relation = |description| json!({"target_memory_id":evidence,"kind":"related","description":description});
    let first = relation("Verify release");
    let second = relation("Verify archive");
    let mutations: Vec<_> = [(A, json!([first])), (B, json!([second])), (C, json!([]))]
        .into_iter()
        .map(|(id, relations)| {
            let mut mutation = f.mutation(id, RELEASE_TASK);
            let agentlaw_storage::UnitState::Live { heads } = &mut mutation.unit.state else {
                unreachable!()
            };
            heads[0].metadata["in_working_set"] = json!(true);
            heads[0].metadata["applicability"] = json!({"scope":"project","project_id":project_id});
            heads[0].metadata["relations"] = relations;
            mutation
        })
        .collect();
    let receipt = f
        .store
        .publish(&uuid::Uuid::new_v4().to_string(), mutations)
        .unwrap();
    let reference = |id| {
        receipt
            .references
            .iter()
            .find(|reference| reference.memory_id == id)
            .unwrap()
    };
    let parents = [reference(A), reference(B)];
    let mut consolidate = json!({"operation":"consolidate","consolidation_refs":parents,
        "what_to_remember":RELEASE_TASK,"evidence":"Reconcile both Task sources"});
    let mut evolve = json!({"operation":"evolve","parent_refs":[reference(C)],
        "what_to_remember":RELEASE_TASK,"evidence":"Close the independent release Task"});
    let generation = f.store.generation().unwrap();
    let error = f
        .try_submit(json!({"project_path":project,"memories":[consolidate,evolve]}))
        .unwrap_err();
    assert_eq!(
        error.code, "metadata_resolution_required",
        "FR-01.6 ER-26: {error:?}"
    );
    let heads = write_error_detail(&error, "FR-01.6 ER-26", "/memories/0/related_memory_ids")
        ["heads"]
        .as_array()
        .unwrap();
    assert_eq!(heads.len(), 2);
    assert!(heads.contains(&json!({"memory_ref":parents[0],"value":[first]})));
    assert!(heads.contains(&json!({"memory_ref":parents[1],"value":[second]})));
    for (path, refs) in [
        ("/memories/0/in_working_set", parents.as_slice()),
        ("/memories/1/in_working_set", &[reference(C)][..]),
    ] {
        let detail = write_error_detail(&error, "FR-01.6 ER-26", path);
        let heads = detail["heads"].as_array().unwrap();
        assert_eq!(heads.len(), refs.len());
        assert!(refs
            .iter()
            .all(|parent| heads.contains(&json!({"memory_ref":parent,"value":true}))));
    }
    assert_eq!(f.store.generation().unwrap(), generation);
    consolidate["related_memory_ids"] = json!([evidence]);
    consolidate["in_working_set"] = json!(true);
    evolve["in_working_set"] = json!(false);
    let response = f.submit(json!({"project_path":project,"memories":[consolidate,evolve]}));
    assert_eq!(response["status"], "remembered", "{response}");
    assert_eq!(response["results"].as_array().unwrap().len(), 2);
}
