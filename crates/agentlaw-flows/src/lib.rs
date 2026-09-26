//! Context, recall assembly and write preflight, separated from canonical persistence.
pub mod artifacts;
pub mod context;
pub mod control;
pub use control::RequestControl;
pub mod derived;
pub mod exact;
pub mod history;
pub mod history_diff;
pub mod history_projection;
pub mod history_search;
pub mod recall;
pub mod runtime;
pub mod write;
pub use agentlaw_contracts::*;
pub use runtime::Runtime;

/// A snapshot owns all bytes needed by a request; implementations acquire it under
/// the canonical source read gate, not by re-opening files after releasing the gate.
pub trait SourceSnapshot {
    fn current(&self, id: &str) -> Result<Option<CurrentState>>;
    fn inventory(&self) -> Result<Vec<CurrentState>>;
    fn read_set(&self) -> ReadSet;
}
pub fn validate_id(id: &str) -> Result<()> {
    let parsed = uuid::Uuid::parse_str(id)
        .map_err(|_| DomainError::new("invalid_identity", "Identity must be a UUIDv4."))?;
    if parsed.get_version_num() != 4
        || parsed.get_variant() != uuid::Variant::RFC4122
        || parsed.to_string() != id
    {
        return Err(DomainError::new(
            "invalid_identity",
            "Identity must be a canonical lowercase UUIDv4.",
        ));
    }
    Ok(())
}
pub fn relative_path(path: &str) -> Result<String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains(':')
        || path.contains('\0')
    {
        return Err(DomainError::new(
            "invalid_work_target",
            "Expected a project-relative path.",
        ));
    }
    let normalized = path.replace('\\', "/");
    let components: Vec<_> = normalized
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if components.is_empty() || components.contains(&"..") {
        return Err(DomainError::new(
            "invalid_work_target",
            "Parent traversal and empty paths are forbidden.",
        ));
    }
    Ok(components.join("/"))
}
pub fn scope_matches(a: &Applicability, c: &RequestContext) -> bool {
    a.scope.iter().all(|s| match s {
        ScopeKind::User => true,
        ScopeKind::Machine => a.machine_id.as_deref() == Some(&c.machine_id),
        ScopeKind::Project => a.project_id.is_some() && a.project_id == c.project_id,
    })
}
