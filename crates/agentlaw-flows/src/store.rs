//! Durable local context and pending state, connected to canonical storage.
use crate::write::*;
use crate::*;
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
pub(crate) fn sql<T>(r: std::result::Result<T, rusqlite::Error>) -> Result<T> {
    r.map_err(|_| {
        DomainError::new(
            "control_storage_failed",
            "Local control database operation failed; retained work was not discarded.",
        )
    })
}
pub(crate) fn control_backup_required() -> DomainError {
    DomainError::new("control_backup_required","Authoritative local control state is absent or cannot be retained. Existing pending proposals and authoring decisions cannot be reconstructed from canonical memories. Preserve database and WAL/SHM files, stop clients, and restore a consistent backup for this binding; no empty replacement was created.")
}
pub(crate) fn source<T>(r: agentlaw_storage::Result<T>) -> Result<T> {
    r.map_err(|e| match e {
        agentlaw_storage::Error::InsufficientResource { .. } => { let mut error = DomainError::new("insufficient_resource", "Canonical work requires more available disk or memory; source and retained pending work were preserved. Free resources and retry."); error.retryable = true; error },
        agentlaw_storage::Error::ResourceUnknown(_) => { let mut error = DomainError::new("resource_status_unknown", "Available resource capacity could not be established; source and retained pending work were preserved. Retry after checking host resource status."); error.retryable = true; error },
        agentlaw_storage::Error::Capacity => DomainError::new("source_payload_requires_spool", "Selected source requires file-backed delivery."),
        agentlaw_storage::Error::Stale(_) => DomainError::new("stale_memory_ref", "Source state changed; inspect current heads and retry review."),
        agentlaw_storage::Error::NotFound(_) => DomainError::new("memory_not_found", "Requested source identity is absent."),
        agentlaw_storage::Error::RecoveryRequired(_) => DomainError::new("recovery_required", "Source publication recovery must complete before access."),
        _ => DomainError::new("source_operation_failed", "Canonical operation failed; inspect diagnostics without discarding source or pending work."),
    })
}

pub(crate) fn digest(v: &impl Serialize) -> Result<String> {
    let b = serde_json::to_vec(v).map_err(|_| {
        DomainError::new("serialization_failed", "Could not serialize review basis.")
    })?;
    Ok(format!("{:x}", Sha256::digest(b)))
}
pub(crate) fn external_current(state: &CurrentState) -> Value {
    json!({"memory_id":state.resolved_id,"current_heads":state.heads.iter().cloned().map(RecallHead::from).collect::<Vec<_>>(),"head_reconciliation_required":state.heads.len()>1})
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|_| {
        DomainError::new(
            "control_corrupt",
            "Stored local control payload is invalid; no work was discarded.",
        )
    })
}
pub(crate) fn encode(v: &impl Serialize) -> Result<String> {
    serde_json::to_string(v)
        .map_err(|_| DomainError::new("serialization_failed", "Could not encode control state."))
}
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub struct StoreIdentity {
    pub root: PathBuf,
    pub local: PathBuf,
    pub user_id: String,
    pub machine_id: String,
    pub binding_id: String,
    pub history_limit: usize,
    pub response_limit: usize,
    pub config_file: Option<PathBuf>,
}
pub struct ReadCtx<'a> {
    pub identity: &'a StoreIdentity,
    pub source: &'a agentlaw_storage::published::OwnedPublishedReader,
    pub control: &'a crate::control::ControlReader,
    pub semantic: &'a dyn crate::semantic::SemanticPort,
    pub history_semantic: &'a dyn crate::semantic::HistorySemanticPort,
    pub history_cache: &'a crate::history_cache::HistoryCacheGuards,
    pub request: &'a RequestControl,
}
pub struct LaneCtx<'a> {
    pub identity: &'a StoreIdentity,
    pub source: &'a Store,
    pub control: &'a mut crate::control::ControlWriter,
    pub request: &'a RequestControl,
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) streaming: crate::read::streaming::StreamedSelection,
    pub(crate) generation: u64,
    pub(crate) states: BTreeMap<String, CurrentState>,
    pub(crate) units: Vec<CurrentUnit>,
    pub(crate) basis: ReadSet,
    pub(crate) publication_basis: agentlaw_storage::ReadSet,
}
impl SourceSnapshot for Snapshot {
    fn current(&self, id: &str) -> Result<Option<CurrentState>> {
        validate_id(id)?;
        let mut cursor = id.to_owned();
        let mut visited = BTreeSet::new();
        let mut path = Vec::new();
        loop {
            if !visited.insert(cursor.clone()) {
                return Err(DomainError::new(
                    "source_corrupt",
                    "Redirect cycle detected.",
                ));
            }
            if let Some(s) = self.states.get(&cursor) {
                let mut s = s.clone();
                s.requested_id = id.into();
                if !path.is_empty() {
                    s.redirect_path = path;
                }
                return Ok(Some(s));
            }
            match self
                .units
                .iter()
                .find(|u| u.entity_type == "memory" && u.entity_id == cursor)
            {
                Some(CurrentUnit {
                    state: UnitState::Redirect { redirect_to, .. },
                    ..
                }) => {
                    path.push(cursor);
                    cursor = redirect_to.clone()
                }
                _ => return Ok(None),
            }
        }
    }
    fn inventory(&self) -> Result<Vec<CurrentState>> {
        Ok(self
            .states
            .values()
            .map(|s| (s.resolved_id.clone(), s.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect())
    }
    fn read_set(&self) -> ReadSet {
        self.basis.clone()
    }
}
pub(crate) fn scope_value(v: &Value) -> Result<Applicability> {
    let mut v = v.clone();
    let scope = v["scope"].as_str().ok_or_else(|| {
        DomainError::new("source_corrupt", "Canonical scope must be a named variant.")
    })?;
    v["scope"] = match scope {
        "project_machine" => json!(["project", "machine"]),
        "user" | "project" | "machine" => json!([scope]),
        _ => {
            return Err(DomainError::new(
                "source_corrupt",
                "Unknown canonical scope variant.",
            ))
        }
    };
    let a: Applicability = serde_json::from_value(v)
        .map_err(|_| DomainError::new("source_corrupt", "Invalid resolved applicability."))?;
    a.validate()?;
    for id in a.project_id.iter().chain(a.machine_id.iter()) {
        validate_id(id)?;
    }
    Ok(a)
}
pub(crate) fn canonical_scope(a: &Applicability) -> Result<Value> {
    a.validate()?;
    let mut value = to_value(a)?;
    value["scope"] = json!(match a.scope.as_slice() {
        [ScopeKind::User] => "user",
        [ScopeKind::Project] => "project",
        [ScopeKind::Machine] => "machine",
        [ScopeKind::Project, ScopeKind::Machine] => "project_machine",
        _ => unreachable!(),
    });
    Ok(value)
}
pub(crate) fn memory_from(unit: &CurrentUnit, head: &Head) -> Result<Memory> {
    let a = scope_value(&head.metadata["applicability"])?;
    let origin: Origin = serde_json::from_value(head.metadata["origin"].clone())
        .map_err(|_| DomainError::new("source_corrupt", "Invalid source origin."))?;
    let relations = head.metadata["relations"]
        .as_array()
        .cloned()
        .ok_or_else(|| {
            DomainError::new(
                "source_corrupt",
                "Memory relations must be explicitly present as an array.",
            )
        })?;
    for relation in &relations {
        let id = relation["target_memory_id"].as_str().ok_or_else(|| {
            DomainError::new("source_corrupt", "Relation target identity is missing.")
        })?;
        validate_id(id)?;
        if !matches!(relation["kind"].as_str(), Some("related" | "required")) {
            return Err(DomainError::new("source_corrupt", "Unknown relation kind."));
        }
    }
    if head
        .metadata
        .get("in_working_set")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(DomainError::new(
            "source_corrupt",
            "Task membership must be a boolean when present.",
        ));
    }
    let relation_ids = |kind: &str| {
        relations
            .iter()
            .filter(|r| r["kind"] == kind)
            .filter_map(|r| r["target_memory_id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    Ok(Memory {
        memory_ref: MemoryRef {
            memory_id: unit.entity_id.clone(),
            observed_version: source(agentlaw_storage::version(unit, head))?,
        },
        what_to_remember: head.body.clone(),
        evidence: String::new(),
        applies_to: a.scope.clone(),
        applicability: a,
        origin,
        in_working_set: head.metadata["in_working_set"].as_bool(),
        is_rule: head.metadata["is_rule"].as_bool().ok_or_else(|| {
            DomainError::new(
                "source_corrupt",
                "Memory is_rule must be explicitly present as a boolean.",
            )
        })?,
        related_memory_ids: relation_ids("related"),
        required_memory_ids: relation_ids("required"),
        work_targets: serde_json::from_value(
            head.metadata.get("work_targets").cloned().ok_or_else(|| {
                DomainError::new(
                    "source_corrupt",
                    "Memory work_targets must be explicitly present.",
                )
            })?,
        )
        .map_err(|_| DomainError::new("source_corrupt", "Invalid work-target metadata."))?,
    })
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Retained {
    #[serde(default)]
    pub(crate) diagnostics: Vec<DomainError>,
    pub(crate) batch: PendingBatch,
    pub(crate) context: RequestContext,
    pub(crate) issues: Vec<Value>,
    pub(crate) basis: String,
    pub(crate) resolution_ref: Option<String>,
    pub(crate) review_refs: Vec<String>,
    pub(crate) approved_review_refs: Vec<String>,
    pub(crate) overlap_approved: bool,
    pub(crate) operation_id: Option<String>,
    pub(crate) mutations: Vec<Mutation>,
    pub(crate) expected_generation: Option<u64>,
    #[serde(default)]
    pub(crate) publication_basis: Option<agentlaw_storage::ReadSet>,
}
pub(crate) fn scope_token(a: &Applicability) -> String {
    match a.scope.as_slice() {
        [ScopeKind::User] => "user".into(),
        [ScopeKind::Machine] => format!("machine:{}", a.machine_id.as_deref().unwrap_or("")),
        [ScopeKind::Project] => format!("project:{}", a.project_id.as_deref().unwrap_or("")),
        [ScopeKind::Project, ScopeKind::Machine] => format!(
            "project:{}:machine:{}",
            a.project_id.as_deref().unwrap_or(""),
            a.machine_id.as_deref().unwrap_or("")
        ),
        _ => "invalid".into(),
    }
}
#[cfg(test)]
mod tests;
