use crate::*;
use std::collections::{BTreeSet, HashSet};

/// Semantic overlap / dependent review is never inferred from a shape-valid write.
pub trait ReviewGate {
    fn review(
        &self,
        context: &RequestContext,
        proposals: &[MemoryProposal],
        basis: &ReadSet,
    ) -> Result<()>;
}
pub struct UnavailableReview;
impl ReviewGate for UnavailableReview {
    fn review(&self, _: &RequestContext, _: &[MemoryProposal], _: &ReadSet) -> Result<()> {
        Err(DomainError::unsupported(
            "Write overlap and dependent review are not connected to a review backend.",
        ))
    }
}
fn inherited<T: Clone + PartialEq>(
    value: &mut Option<T>,
    parents: &[Memory],
    get: impl Fn(&Memory) -> T,
    field: &str,
) -> Result<()> {
    if value.is_none() && !parents.is_empty() {
        let first = get(&parents[0]);
        if parents.iter().any(|p| get(p) != first) {
            return Err(DomainError::new(
                "metadata_resolution_required",
                format!("Specify {field}; parent values differ."),
            ));
        }
        *value = Some(first)
    }
    Ok(())
}
/// Parse a Task heading consistently for owned and streamed bodies.
pub(crate) fn task_heading(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes)
        || !line[hashes..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        return None;
    }
    Some(line[hashes..].trim().trim_end_matches('#').trim())
}
/// Incremental fence prefix parser shared by owned and streamed Task bodies.
#[derive(Default)]
pub(crate) struct TaskFenceProbe {
    marker: Option<char>,
    run: u64,
    done: bool,
}
impl TaskFenceProbe {
    pub(crate) fn push(&mut self, ch: char) {
        if self.done {
            return;
        }
        match self.marker {
            None if ch.is_whitespace() => {}
            None if ch == '`' || ch == '~' => {
                self.marker = Some(ch);
                self.run = 1;
            }
            Some(marker) if marker == ch => self.run = self.run.saturating_add(1),
            _ => self.done = true,
        }
    }
    pub(crate) fn done(&self) -> bool {
        self.done
    }
    pub(crate) fn finish(&self) -> Option<(char, u64)> {
        self.marker
            .filter(|_| self.run >= 3)
            .map(|marker| (marker, self.run))
    }
}
fn task_fence(line: &str) -> Option<(char, u64)> {
    let mut probe = TaskFenceProbe::default();
    for ch in line.chars() {
        probe.push(ch);
        if probe.done() {
            break;
        }
    }
    probe.finish()
}
pub fn task_headings(body: &str) -> Result<()> {
    let required = [
        "Objective",
        "Current position",
        "Resume point",
        "References",
    ];
    let mut found = Vec::new();
    let mut fence: Option<(char, u64)> = None;
    for line in body.lines() {
        let s = line.trim_start();
        if let Some((c, n)) = task_fence(s) {
            match fence {
                None => fence = Some((c, n)),
                Some((fc, fn_)) if fc == c && n >= fn_ => fence = None,
                _ => {}
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if let Some(heading) = task_heading(s) {
            if required.contains(&heading) {
                found.push(heading)
            }
        }
    }
    if found != required {
        return Err(DomainError::new("invalid_task_headings","Task must contain Objective / Current position / Resume point / References headings exactly once in that order."));
    }
    Ok(())
}
/// Copy complete handoff sections, preserving their bytes; fenced heading text is
/// content and cannot change section boundaries.
pub fn task_sections(body: &str) -> Result<[String; 3]> {
    task_headings(body)?;
    let names = [
        "Objective",
        "Current position",
        "Resume point",
        "References",
    ];
    let mut ranges = Vec::new();
    let mut fence: Option<(char, u64)> = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let s = line.trim_start();
        if let Some((c, n)) = task_fence(s) {
            match fence {
                None => fence = Some((c, n)),
                Some((fc, fn_)) if fc == c && n >= fn_ => fence = None,
                _ => {}
            }
            offset += line.len();
            continue;
        }
        if fence.is_none() {
            if let Some(heading) = task_heading(s) {
                if names.contains(&heading) {
                    ranges.push((offset, offset + line.len()));
                }
            }
        }
        offset += line.len();
    }
    Ok(std::array::from_fn(|i| {
        body[ranges[i].1..ranges[i + 1].0].to_owned()
    }))
}
pub fn preflight(
    source: &impl SourceSnapshot,
    gate: &impl ReviewGate,
    context: &RequestContext,
    input: &[MemoryProposal],
) -> Result<PreparedIntent> {
    if input.is_empty() {
        return Err(DomainError::new(
            "invalid_input",
            "A write batch cannot be empty.",
        ));
    }
    let mut proposals = input.to_vec();
    let mut affected = HashSet::new();
    let mut resolved_applicability = Vec::new();
    for p in &mut proposals {
        let explicit_scope = p.applies_to.is_some();
        if (p.operation == Operation::Evolve && p.consolidation_refs.is_some())
            || (p.operation == Operation::Consolidate && p.parent_refs.is_some())
        {
            return Err(DomainError::new(
                "invalid_input",
                "Parent and consolidation reference fields cannot be mixed.",
            ));
        }
        if p.what_to_remember.is_empty() || p.evidence.is_empty() {
            return Err(DomainError::new(
                "invalid_input",
                "Complete content and evidence are required.",
            ));
        }
        let refs = match p.operation {
            Operation::Create => {
                if p.parent_refs.is_some() || p.consolidation_refs.is_some() {
                    return Err(DomainError::new(
                        "invalid_input",
                        "Create cannot contain parent references.",
                    ));
                }
                Vec::new()
            }
            Operation::Evolve => p
                .parent_refs
                .clone()
                .ok_or_else(|| DomainError::new("invalid_input", "parent_refs is required."))?,
            Operation::Consolidate => p.consolidation_refs.clone().ok_or_else(|| {
                DomainError::new("invalid_input", "consolidation_refs is required.")
            })?,
        };
        let mut states = std::collections::BTreeMap::new();
        let mut submitted = BTreeSet::new();
        for r in &refs {
            validate_id(&r.memory_id)?;
            if !submitted.insert(r.clone()) {
                return Err(DomainError::new(
                    "invalid_input",
                    "Duplicate parent reference.",
                ));
            }
            let state = source.current(&r.memory_id)?.ok_or_else(|| {
                DomainError::new("memory_not_found", "A referenced memory does not exist.")
            })?;
            if state.resolved_id != r.memory_id {
                return Err(DomainError::new(
                    "redirect_requires_review",
                    "Use the current reference returned for the redirect target.",
                ));
            }
            states.insert(state.resolved_id.clone(), state);
        }
        if p.operation == Operation::Evolve && states.len() != 1 {
            return Err(DomainError::new(
                "invalid_parent_identity",
                "Evolve requires all heads of exactly one memory.",
            ));
        }
        if p.operation == Operation::Consolidate && states.len() < 2 {
            return Err(DomainError::new(
                "invalid_consolidation",
                "Consolidation requires at least two resolved identities.",
            ));
        }
        let mut parents = Vec::new();
        for (id, state) in states {
            if !affected.insert(id) {
                return Err(DomainError::new(
                    "duplicate_batch_target",
                    "Existing identities cannot be affected by multiple proposals.",
                ));
            }
            let current: BTreeSet<_> = state.heads.iter().map(|h| h.memory_ref.clone()).collect();
            let supplied: BTreeSet<_> = refs
                .iter()
                .filter(|r| r.memory_id == state.resolved_id)
                .cloned()
                .collect();
            if current != supplied {
                return Err(DomainError::new(
                    "stale_memory_ref",
                    "Copy all current head references and review their full contents.",
                ));
            }
            parents.extend(state.heads);
        }
        if parents.iter().any(|p| p.in_working_set == Some(true)) && p.in_working_set.is_none() {
            return Err(DomainError::new(
                "working_set_disposition_required",
                "Explicitly retain or close each active Task.",
            ));
        }
        if p.applies_to.is_none()
            && !parents.is_empty()
            && parents
                .iter()
                .any(|m| m.applicability != parents[0].applicability)
        {
            return Err(DomainError::new(
                "metadata_resolution_required",
                "Specify applies_to; resolved parent scopes differ.",
            ));
        }
        inherited(
            &mut p.applies_to,
            &parents,
            |m| m.applies_to.clone(),
            "applies_to",
        )?;
        if p.applies_to.is_none() {
            return Err(DomainError::new(
                "invalid_input",
                "Create requires applies_to.",
            ));
        }
        inherited(&mut p.is_rule, &parents, |m| m.is_rule, "is_rule")?;
        inherited(
            &mut p.related_memory_ids,
            &parents,
            |m| m.related_memory_ids.clone(),
            "related_memory_ids",
        )?;
        inherited(
            &mut p.required_memory_ids,
            &parents,
            |m| m.required_memory_ids.clone(),
            "required_memory_ids",
        )?;
        inherited(
            &mut p.work_targets,
            &parents,
            |m| m.work_targets.clone(),
            "work_targets",
        )?;
        if p.in_working_set.is_none() {
            let roles: HashSet<_> = parents.iter().map(|m| m.in_working_set).collect();
            if roles.len() > 1 {
                return Err(DomainError::new(
                    "metadata_resolution_required",
                    "Specify in_working_set; parent Task roles differ.",
                ));
            }
            p.in_working_set = parents.first().and_then(|m| m.in_working_set)
        }
        let scopes = p.applies_to.as_ref().unwrap();
        if ![
            vec![ScopeKind::User],
            vec![ScopeKind::Project],
            vec![ScopeKind::Machine],
            vec![ScopeKind::Project, ScopeKind::Machine],
        ]
        .contains(scopes)
        {
            return Err(DomainError::new(
                "invalid_scope",
                "Unsupported scope combination.",
            ));
        }
        let applicability = if !explicit_scope && !parents.is_empty() {
            parents[0].applicability.clone()
        } else {
            if scopes.contains(&ScopeKind::Project) && context.project_id.is_none() {
                return Err(DomainError::new(
                    "project_connection_required",
                    "Project-scoped memory requires a resolved project.",
                ));
            }
            Applicability {
                scope: scopes.clone(),
                project_id: if scopes.contains(&ScopeKind::Project) {
                    context.project_id.clone()
                } else {
                    None
                },
                machine_id: scopes
                    .contains(&ScopeKind::Machine)
                    .then(|| context.machine_id.clone()),
            }
        };
        applicability.validate()?;
        resolved_applicability.push(applicability);
        if p.in_working_set.is_some() {
            if scopes != &vec![ScopeKind::Project] {
                return Err(DomainError::new(
                    "task_scope_required",
                    "Task memory must have project-only scope.",
                ));
            }
            task_headings(&p.what_to_remember)?
        }
        for id in p
            .related_memory_ids
            .iter()
            .flatten()
            .chain(p.required_memory_ids.iter().flatten())
        {
            validate_id(id)?;
            if source.current(id)?.is_none() {
                return Err(DomainError::new(
                    "memory_not_found",
                    "A related or required memory does not exist.",
                ));
            }
        }
        for target in p.work_targets.iter().flatten() {
            validate_id(&target.project_id)?;
            relative_path(&target.path)?;
        }
    }
    let basis = source.read_set();
    gate.review(context, &proposals, &basis)?;
    Ok(PreparedIntent {
        context: context.clone(),
        proposals,
        resolved_applicability,
        read_set: basis,
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingBatch {
    pub reference: PendingBatchRef,
    pub store_binding_id: String,
    pub proposals: Vec<MemoryProposal>,
}
impl PendingBatch {
    pub fn new(context: &RequestContext, mut proposals: Vec<MemoryProposal>) -> Self {
        for p in &mut proposals {
            p.proposal_id = Some(uuid::Uuid::new_v4().to_string())
        }
        Self {
            reference: PendingBatchRef {
                pending_batch_id: uuid::Uuid::new_v4().to_string(),
                observed_version: uuid::Uuid::new_v4().to_string(),
            },
            store_binding_id: context.store_binding_id.clone(),
            proposals,
        }
    }
    /// Pure candidate update. A durable repository must compare-and-swap the old
    /// version and persist this complete value before acknowledging retention.
    pub fn apply_delta(
        &self,
        context: &RequestContext,
        reference: &PendingBatchRef,
        delta: &[MemoryProposal],
    ) -> Result<Self> {
        if self.store_binding_id != context.store_binding_id {
            return Err(DomainError::new(
                "pending_store_mismatch",
                "Pending work belongs to a different store binding.",
            ));
        }
        if &self.reference != reference {
            return Err(DomainError::new(
                "stale_pending_ref",
                "Inspect the latest pending version before changing it.",
            ));
        }
        let mut next = self.clone();
        let mut seen = HashSet::new();
        for proposal in delta {
            let mut proposal = proposal.clone();
            match &proposal.proposal_id {
                Some(id) => {
                    if !seen.insert(id.clone()) {
                        return Err(DomainError::new(
                            "duplicate_proposal_id",
                            "A pending proposal can be replaced only once per request.",
                        ));
                    }
                    let position = next
                        .proposals
                        .iter()
                        .position(|p| p.proposal_id.as_ref() == Some(id))
                        .ok_or_else(|| {
                            DomainError::new(
                                "unknown_proposal_id",
                                "The proposal does not belong to this pending batch.",
                            )
                        })?;
                    next.proposals[position] = proposal
                }
                None => {
                    proposal.proposal_id = Some(uuid::Uuid::new_v4().to_string());
                    next.proposals.push(proposal)
                }
            }
        }
        next.reference.observed_version = uuid::Uuid::new_v4().to_string();
        Ok(next)
    }
}
