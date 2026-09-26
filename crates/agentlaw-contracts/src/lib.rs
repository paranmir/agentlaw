//! Shared, typed transport contracts. Parsing validates the executable schema.
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub mod validation;
pub const INPUT_SCHEMA: &str =
    include_str!("../../../docs/design/contracts/agentlaw-input.schema.json");

/// Publish the same contract without local references: clients can see every
/// action and nested field without implementing JSON Schema reference resolution.
pub fn tool_input_schema() -> Value {
    fn inline(node: &Value, root: &Value, stack: &mut Vec<String>) -> Value {
        match node {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    assert!(
                        !stack.iter().any(|r| r == reference),
                        "recursive tool schema"
                    );
                    let pointer = reference.strip_prefix('#').expect("local schema reference");
                    let target = root.pointer(pointer).expect("resolved schema reference");
                    stack.push(reference.to_owned());
                    let mut expanded = inline(target, root, stack);
                    stack.pop();
                    // Current contract uses description siblings, which must survive.
                    for (key, value) in object.iter().filter(|(k, _)| *k != "$ref") {
                        assert!(
                            !expanded.as_object().unwrap().contains_key(key),
                            "conflicting reference sibling"
                        );
                        expanded[key] = inline(value, root, stack);
                    }
                    expanded
                } else {
                    Value::Object(
                        object
                            .iter()
                            .filter(|(k, _)| *k != "$defs")
                            .map(|(k, v)| (k.clone(), inline(v, root, stack)))
                            .collect(),
                    )
                }
            }
            Value::Array(values) => {
                Value::Array(values.iter().map(|v| inline(v, root, stack)).collect())
            }
            _ => node.clone(),
        }
    }
    let root: Value = serde_json::from_str(INPUT_SCHEMA).expect("bundled schema");
    inline(&root, &root, &mut Vec::new())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct DomainError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}
impl DomainError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported", message)
    }
    pub fn needs_decision(message: impl Into<String>) -> Self {
        Self::new("needs_decision", message)
    }
}
pub type Result<T> = std::result::Result<T, DomainError>;
macro_rules! reference {
    ($name:ident,$id:ident) => {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub $id: String,
            pub observed_version: String,
        }
    };
}
reference!(MemoryRef, memory_id);
reference!(ProcedureRef, procedure_id);
reference!(PendingBatchRef, pending_batch_id);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    User,
    Project,
    Machine,
}
pub type AppliesTo = Vec<ScopeKind>;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    File,
    Directory,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Reading {
    Related,
    Required,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct WorkTarget {
    pub path: String,
    pub kind: TargetKind,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct StoredTarget {
    pub project_id: String,
    pub path: String,
    pub kind: TargetKind,
    pub reading: Reading,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Create,
    Evolve,
    Consolidate,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryProposal {
    pub operation: Operation,
    pub what_to_remember: String,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applies_to: Option<AppliesTo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_refs: Option<Vec<MemoryRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consolidation_refs: Option<Vec<MemoryRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_working_set: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_rule: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_memory_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_memory_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_targets: Option<Vec<StoredTarget>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RecallRequest {
    pub project_path: Option<String>,
    pub project_hint: Option<String>,
    pub selected_project_id: Option<String>,
    pub machine_id: Option<String>,
    pub recall_for: Option<String>,
    pub search_terms: Option<Vec<String>>,
    pub memory_ids: Option<Vec<String>>,
    pub procedure_ids: Option<Vec<String>>,
    pub restore_context: Option<bool>,
    pub include_active_tasks: Option<bool>,
    pub memory_candidate_limit: Option<u32>,
    pub procedure_candidate_limit: Option<u32>,
    pub work_targets: Option<Vec<WorkTarget>>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingAction {
    Inspect,
    Continue,
    Discard,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewJudgment {
    pub review_ref: String,
    pub decision: ReviewDecision,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Unchanged,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcedureProposal {
    pub operation: Operation,
    pub evidence_memory_ids: Vec<String>,
    pub parent_refs: Option<Vec<ProcedureRef>>,
    pub applies_to: Option<AppliesTo>,
    pub name: Option<String>,
    pub use_when: Option<String>,
    pub instructions: Option<String>,
    pub evidence: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RememberRequest {
    pub project_path: Option<String>,
    pub resolution_ref: Option<String>,
    pub memories: Option<Vec<MemoryProposal>>,
    pub pending_batch_ref: Option<PendingBatchRef>,
    pub pending_action: Option<PendingAction>,
    pub review_judgments: Option<Vec<ReviewJudgment>>,
    pub discard_reason: Option<String>,
    pub user_confirmed: Option<bool>,
    pub procedure: Option<ProcedureProposal>,
    pub authoring_ref: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRequest {
    pub memory_id: Option<String>,
    pub procedure_id: Option<String>,
    pub context_layers: Option<u32>,
    pub history_for: Option<String>,
    pub max_matches: Option<u32>,
    pub start_change_id: Option<String>,
    pub end_change_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectIntent {
    #[default]
    Discover,
    Connect,
    Create,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectClues {
    pub repository_url: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectRequest {
    pub project_path: String,
    pub intent: Option<ConnectIntent>,
    pub clues: Option<ProjectClues>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub project_description: Option<String>,
    pub memory_store_path: Option<String>,
    pub restore_context: Option<bool>,
    pub recall_for: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request {
    Recall(RecallRequest),
    RememberThis(RememberRequest),
    History(HistoryRequest),
    ConnectProjectMemory(ConnectRequest),
}
pub fn parse_request(json: &str) -> Result<Request> {
    let value = validation::decode_unique(json)?;
    validation::validate_input(&value)?;
    serde_json::from_value(value).map_err(|_| {
        DomainError::new(
            "invalid_input",
            "Input exceeds supported numeric bounds or has invalid typed fields.",
        )
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Origin {
    pub user_id: Option<String>,
    pub machine_id: String,
    pub project_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestContext {
    pub store_binding_id: String,
    pub user_id: String,
    pub machine_id: String,
    pub project_id: Option<String>,
    pub connection_version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub memory_ref: MemoryRef,
    pub what_to_remember: String,
    pub evidence: String,
    pub applies_to: AppliesTo,
    pub origin: Origin,
    pub applicability: Applicability,
    pub in_working_set: Option<bool>,
    pub is_rule: bool,
    pub related_memory_ids: Vec<String>,
    pub required_memory_ids: Vec<String>,
    pub work_targets: Vec<StoredTarget>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Applicability {
    pub scope: AppliesTo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
}
impl Applicability {
    pub fn validate(&self) -> Result<()> {
        let p = self.project_id.as_ref().is_some_and(|s| !s.is_empty());
        let m = self.machine_id.as_ref().is_some_and(|s| !s.is_empty());
        let valid = match self.scope.as_slice() {
            [ScopeKind::User] => self.project_id.is_none() && self.machine_id.is_none(),
            [ScopeKind::Project] => p && self.machine_id.is_none(),
            [ScopeKind::Machine] => m && self.project_id.is_none(),
            [ScopeKind::Project, ScopeKind::Machine] => p && m,
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(DomainError::new(
                "invalid_applicability",
                "Scope and project/machine identities form an invalid combination.",
            ))
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReadSet {
    pub memory_refs: Vec<MemoryRef>,
    pub fingerprint: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentState {
    pub requested_id: String,
    pub resolved_id: String,
    pub heads: Vec<Memory>,
    pub redirect_path: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedIntent {
    pub context: RequestContext,
    pub proposals: Vec<MemoryProposal>,
    pub resolved_applicability: Vec<Applicability>,
    pub read_set: ReadSet,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteResult {
    pub proposal_index: usize,
    pub memory_ref: MemoryRef,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RememberedResponse {
    pub status: RememberedStatus,
    pub results: Vec<WriteResult>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberedStatus {
    Remembered,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecallResponse {
    pub current_time: String,
    pub memories: Vec<RecallMemory>,
    pub candidates: Vec<MemoryCandidate>,
    pub learned_procedures: Vec<LearnedProcedure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_ids: Option<MissingIds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_counts: Option<CandidateCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_task_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<DomainError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undelivered_required: Vec<UndeliveredRequired>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_instruction: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndeliveredRequired {
    pub memory_id: String,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallMemory {
    pub memory_id: String,
    pub current_heads: Vec<RecallHead>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_reconciliation_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_reconciliation_instruction: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallHead {
    pub memory_ref: MemoryRef,
    pub what_to_remember: String,
    pub applicability: Applicability,
    pub origin: Origin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_working_set: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_rule: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_memory_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_memory_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work_targets: Vec<StoredTarget>,
}
impl From<Memory> for RecallHead {
    fn from(m: Memory) -> Self {
        Self {
            memory_ref: m.memory_ref,
            what_to_remember: m.what_to_remember,
            applicability: m.applicability,
            origin: m.origin,
            in_working_set: m.in_working_set,
            is_rule: m.is_rule.then_some(true),
            related_memory_ids: m.related_memory_ids,
            required_memory_ids: m.required_memory_ids,
            work_targets: m.work_targets,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalPath {
    pub via: String,
    pub clue: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_memory_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub memory_id: String,
    pub excerpt: String,
    pub applicability: Applicability,
    pub retrieval_paths: Vec<RetrievalPath>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullProcedure {
    pub procedure_id: String,
    pub name: String,
    pub use_when: String,
    pub applicability: Applicability,
    pub instructions: String,
    pub procedure_ref: ProcedureRef,
    pub evidence_memory_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LearnedProcedure {
    Full(FullProcedure),
    Multiple(ProcedureHeads),
    Candidate(ProcedureCandidate),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureHeads {
    pub procedure_id: String,
    pub current_heads: Vec<FullProcedure>,
    pub head_reconciliation_required: bool,
    pub head_reconciliation_instruction: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcedureCandidate {
    pub procedure_id: String,
    pub name: String,
    pub use_when: String,
    pub applicability: Applicability,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MissingIds {
    pub memory_ids: Vec<String>,
    pub procedure_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Counts {
    pub matched: usize,
    pub shown: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateCounts {
    pub memories: Counts,
    pub learned_procedures: Counts,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConnection {
    pub project_id: String,
    pub project_path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCandidate {
    pub project_id: String,
    pub name: String,
    pub description: Option<String>,
    pub reasons: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_connection: Option<ProjectConnection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<ProjectCandidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall_result: Option<RecallResponse>,
}
pub fn to_value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| {
        DomainError::new(
            "serialization_failed",
            "The result could not be serialized.",
        )
    })
}
