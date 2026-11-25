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
#[derive(Clone, Copy)]
enum TaskHeadingState {
    Indentation,
    OpeningHashes(u8),
    BeforeName,
    Name,
    AfterName,
    ClosingHashes,
    TrailingWhitespace,
    Invalid,
}

/// Recognizes required headings without buffering a potentially unbounded line.
/// Only the final contiguous run of closing hashes is ignored.
pub(crate) struct TaskHeadingProbe {
    state: TaskHeadingState,
    possible_names: [bool; TASK_HEADINGS.len()],
    name_bytes: usize,
}

impl Default for TaskHeadingProbe {
    fn default() -> Self {
        Self {
            state: TaskHeadingState::Indentation,
            possible_names: [true; TASK_HEADINGS.len()],
            name_bytes: 0,
        }
    }
}

impl TaskHeadingProbe {
    pub(crate) fn push(&mut self, ch: char) {
        use TaskHeadingState::*;

        let whitespace = ch.is_whitespace();
        self.state = match self.state {
            Indentation if whitespace => Indentation,
            Indentation if ch == '#' => OpeningHashes(1),
            OpeningHashes(count) if ch == '#' && count < 6 => OpeningHashes(count + 1),
            OpeningHashes(_) if whitespace => BeforeName,
            BeforeName if whitespace => BeforeName,
            Name if ch == '#' && self.complete_name().is_some() => ClosingHashes,
            Name if whitespace && self.complete_name().is_some() => AfterName,
            BeforeName | Name => self.match_name_byte(ch),
            AfterName if whitespace => AfterName,
            AfterName | ClosingHashes if ch == '#' => ClosingHashes,
            ClosingHashes | TrailingWhitespace if whitespace => TrailingWhitespace,
            _ => Invalid,
        };
    }

    fn match_name_byte(&mut self, ch: char) -> TaskHeadingState {
        if !ch.is_ascii() {
            return TaskHeadingState::Invalid;
        }
        for (possible, name) in self.possible_names.iter_mut().zip(TASK_HEADINGS) {
            *possible &= name.as_bytes().get(self.name_bytes) == Some(&(ch as u8));
        }
        self.name_bytes += 1;
        if self.possible_names.iter().any(|possible| *possible) {
            TaskHeadingState::Name
        } else {
            TaskHeadingState::Invalid
        }
    }

    fn complete_name(&self) -> Option<&'static str> {
        TASK_HEADINGS
            .into_iter()
            .zip(self.possible_names)
            .find_map(|(name, possible)| {
                (possible && name.len() == self.name_bytes).then_some(name)
            })
    }

    pub(crate) fn done(&self) -> bool {
        matches!(self.state, TaskHeadingState::Invalid)
    }

    pub(crate) fn finish(&self) -> Option<&'static str> {
        use TaskHeadingState::*;
        match self.state {
            Name | AfterName | ClosingHashes | TrailingWhitespace => self.complete_name(),
            _ => None,
        }
    }
}

fn task_heading(line: &str) -> Option<&'static str> {
    let mut probe = TaskHeadingProbe::default();
    for ch in line.chars() {
        probe.push(ch);
        if probe.done() {
            break;
        }
    }
    probe.finish()
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
pub(crate) const TASK_HEADINGS: [&str; 4] = [
    "Objective",
    "Current position",
    "Resume point",
    "References",
];

pub(crate) enum TaskSectionParse<T> {
    Complete(T),
    InvalidHeadings,
}

fn task_heading_ranges(body: &str) -> TaskSectionParse<[(usize, usize); 4]> {
    let mut ranges = [(0, 0); TASK_HEADINGS.len()];
    let mut seen = 0;
    let mut fence: Option<(char, u64)> = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let s = line.trim_start();
        if let Some((c, n)) = task_fence(s) {
            match fence {
                None => fence = Some((c, n)),
                Some((fc, fn_)) if fc == c && n >= fn_ => fence = None,
                _ => {}
            }
            continue;
        }
        if fence.is_none() {
            if let Some(heading) = task_heading(s) {
                if TASK_HEADINGS.get(seen) != Some(&heading) {
                    return TaskSectionParse::InvalidHeadings;
                }
                ranges[seen] = (start, offset);
                seen += 1;
            }
        }
    }
    if seen == TASK_HEADINGS.len() {
        TaskSectionParse::Complete(ranges)
    } else {
        TaskSectionParse::InvalidHeadings
    }
}

fn invalid_task_headings() -> DomainError {
    DomainError::new("invalid_task_headings","Task must contain Objective / Current position / Resume point / References headings exactly once in that order.")
}

pub fn task_headings(body: &str) -> Result<()> {
    match task_heading_ranges(body) {
        TaskSectionParse::Complete(_) => Ok(()),
        TaskSectionParse::InvalidHeadings => Err(invalid_task_headings()),
    }
}

/// Copy complete handoff sections, preserving their bytes; fenced heading text is
/// content and cannot change section boundaries.
pub fn task_sections(body: &str) -> Result<[String; 3]> {
    match task_heading_ranges(body) {
        TaskSectionParse::Complete(ranges) => Ok(std::array::from_fn(|i| {
            body[ranges[i].1..ranges[i + 1].0].to_owned()
        })),
        TaskSectionParse::InvalidHeadings => Err(invalid_task_headings()),
    }
}
pub(crate) fn proposal_body<'a>(
    proposal: &'a MemoryProposal,
    parents: &'a [Memory],
) -> Result<&'a str> {
    match (proposal.what_to_remember.as_deref(), &proposal.operation, parents) {
        (Some(body), _, _) if !body.is_empty() => Ok(body),
        (None, Operation::Evolve, [parent]) => Ok(&parent.what_to_remember),
        _ => Err(DomainError::new(
            "invalid_input",
            "Submit the complete what_to_remember body. It can be omitted only when evolving one current head.",
        )),
    }
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
    let mut stale_proposals = Vec::new();
    for (index, p) in proposals.iter_mut().enumerate() {
        let explicit_scope = p.applies_to.is_some();
        if (p.operation == Operation::Evolve && p.consolidation_refs.is_some())
            || (p.operation == Operation::Consolidate && p.parent_refs.is_some())
        {
            return Err(DomainError::new(
                "invalid_input",
                "Parent and consolidation reference fields cannot be mixed.",
            ));
        }
        if p.evidence.is_empty() {
            return Err(DomainError::new("invalid_input", "Evidence is required."));
        }
        if p.operation != Operation::Evolve || p.what_to_remember.is_some() {
            proposal_body(p, &[])?;
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
        let mut stale = false;
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
                stale = true;
            }
            parents.extend(state.heads);
        }
        if stale {
            stale_proposals.push(index);
            continue;
        }
        if p.what_to_remember.is_none() {
            p.what_to_remember = Some(proposal_body(p, &parents)?.to_owned());
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
            task_headings(proposal_body(p, &parents)?)?
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
    if !stale_proposals.is_empty() {
        return Err(DomainError::new(
            "stale_memory_ref",
            "Copy all current head references and review their full contents.",
        )
        .with_details(serde_json::json!({"proposal_indices":stale_proposals})));
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
