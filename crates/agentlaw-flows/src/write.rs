use crate::*;
use serde_json::{json, Value};
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
pub(crate) struct ProposalLocation {
    pub(crate) index: usize,
    field: &'static str,
    element: Option<(usize, Option<&'static str>)>,
}

impl ProposalLocation {
    pub(crate) fn field(index: usize, field: &'static str) -> Self {
        Self {
            index,
            field,
            element: None,
        }
    }

    fn element(mut self, index: usize, member: Option<&'static str>) -> Self {
        self.element = Some((index, member));
        self
    }

    pub(crate) fn input_path(self, input_index: usize) -> String {
        use std::fmt::Write;

        let mut path = format!("/memories/{input_index}/{}", self.field);
        if let Some((index, member)) = self.element {
            write!(path, "/{index}").expect("write to String");
            if let Some(member) = member {
                write!(path, "/{member}").expect("write to String");
            }
        }
        path
    }
}

pub(crate) struct ProposalDetail {
    pub(crate) location: ProposalLocation,
    pub(crate) value: Value,
}

impl ProposalDetail {
    fn field(index: usize, field: &'static str, value: Value) -> Self {
        Self {
            location: ProposalLocation::field(index, field),
            value,
        }
    }
}

pub(crate) enum PreflightError {
    Domain(DomainError),
    Located(DomainError, Vec<ProposalDetail>),
    Stale(DomainError, Vec<ProposalLocation>),
}

impl From<DomainError> for PreflightError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

impl PreflightError {
    fn into_domain(self) -> DomainError {
        match self {
            Self::Domain(error) => error,
            Self::Located(error, details) => {
                let details: Vec<_> = details
                    .into_iter()
                    .map(|mut detail| {
                        detail.value["path"] =
                            json!(detail.location.input_path(detail.location.index));
                        detail.value
                    })
                    .collect();
                error.with_details(json!(details))
            }
            Self::Stale(error, proposals) => {
                let indices: Vec<_> = proposals
                    .into_iter()
                    .map(|location| location.index)
                    .collect();
                error.with_details(json!({"proposal_indices": indices}))
            }
        }
    }
}

fn field_error(
    error: DomainError,
    index: usize,
    field: &'static str,
    constraint: &str,
) -> PreflightError {
    PreflightError::Located(
        error.with_next_action(constraint),
        vec![ProposalDetail::field(
            index,
            field,
            json!({"constraint":constraint}),
        )],
    )
}

fn identity_error(code: &str, location: ProposalLocation, provided: Value) -> PreflightError {
    PreflightError::Located(
        validation::identity_error(code),
        vec![ProposalDetail {
            location,
            value: json!({"provided_value":provided}),
        }],
    )
}

fn consolidation_conflicts(
    index: usize,
    proposal: &MemoryProposal,
    parents: &[Memory],
    relations: &impl Fn(&Memory, &str) -> Result<Value>,
) -> Result<Vec<ProposalDetail>> {
    let mut conflicts = Vec::new();
    for (field, explicit) in [
        ("applies_to", proposal.applies_to.is_some()),
        ("is_rule", proposal.is_rule.is_some()),
        ("in_working_set", proposal.in_working_set.is_some()),
        ("related_memory_ids", proposal.related_memory_ids.is_some()),
        (
            "required_memory_ids",
            proposal.required_memory_ids.is_some(),
        ),
        ("work_targets", proposal.work_targets.is_some()),
    ] {
        if explicit {
            continue;
        }
        let values = parents
            .iter()
            .map(|parent| {
                Ok(match field {
                    "applies_to" => json!(parent.applicability),
                    "is_rule" => json!(parent.is_rule),
                    "in_working_set" => json!(parent.in_working_set),
                    "related_memory_ids" => relations(parent, "related")?,
                    "required_memory_ids" => relations(parent, "required")?,
                    "work_targets" => json!(parent.work_targets),
                    _ => unreachable!(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if values
            .first()
            .is_some_and(|first| values.iter().any(|value| value != first))
        {
            let heads: Vec<_> = parents
                .iter()
                .zip(values)
                .map(|(parent, value)| json!({"memory_ref":parent.memory_ref,"value":value}))
                .collect();
            conflicts.push(ProposalDetail::field(index, field, json!({"heads":heads})));
        }
    }
    Ok(conflicts)
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
    preflight_with_relations(source, gate, context, input, |parent, kind| {
        let ids = if kind == "related" {
            &parent.related_memory_ids
        } else {
            &parent.required_memory_ids
        };
        let ids: BTreeSet<_> = ids.iter().collect();
        Ok(Value::Array(
            ids.into_iter()
                .map(|id| json!({"kind":kind,"target_memory_id":id}))
                .collect(),
        ))
    })
    .map_err(PreflightError::into_domain)
}

pub(crate) fn preflight_with_relations(
    source: &impl SourceSnapshot,
    gate: &impl ReviewGate,
    context: &RequestContext,
    input: &[MemoryProposal],
    relations: impl Fn(&Memory, &str) -> Result<Value>,
) -> std::result::Result<PreparedIntent, PreflightError> {
    if input.is_empty() {
        return Err(DomainError::new("invalid_input", "A write batch cannot be empty.").into());
    }
    let mut proposals = input.to_vec();
    let mut affected = HashSet::new();
    let mut resolved_applicability = Vec::new();
    let mut stale_proposals = Vec::new();
    let mut conflicts = Vec::new();
    let mut missing_dispositions = Vec::new();
    for (index, p) in proposals.iter_mut().enumerate() {
        let explicit_scope = p.applies_to.is_some();
        if (p.operation == Operation::Evolve && p.consolidation_refs.is_some())
            || (p.operation == Operation::Consolidate && p.parent_refs.is_some())
        {
            return Err(DomainError::new(
                "invalid_input",
                "Parent and consolidation reference fields cannot be mixed.",
            )
            .into());
        }
        if p.evidence.is_empty() {
            return Err(field_error(
                DomainError::new("invalid_input", "Evidence is required."),
                index,
                "evidence",
                "Provide nonempty evidence and resubmit.",
            ));
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
                    )
                    .into());
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
        let reference_field = if p.operation == Operation::Consolidate {
            "consolidation_refs"
        } else {
            "parent_refs"
        };
        for (position, r) in refs.iter().enumerate() {
            let location = ProposalLocation::field(index, reference_field)
                .element(position, Some("memory_id"));
            let error = |code| identity_error(code, location, json!(r.memory_id));
            if !validation::is_entity_identity(&r.memory_id) {
                return Err(error("invalid_input"));
            }
            validate_id(&r.memory_id)?;
            if !submitted.insert(r.clone()) {
                return Err(error("invalid_input"));
            }
            let state = source
                .current(&r.memory_id)?
                .ok_or_else(|| error("not_found"))?;
            if state.resolved_id != r.memory_id {
                return Err(DomainError::new(
                    "redirect_requires_review",
                    "Use the current reference returned for the redirect target.",
                )
                .into());
            }
            states.insert(state.resolved_id.clone(), state);
        }
        if p.operation == Operation::Evolve && states.len() != 1 {
            return Err(DomainError::new(
                "invalid_parent_identity",
                "Evolve requires all heads of exactly one memory.",
            )
            .into());
        }
        if p.operation == Operation::Consolidate && states.len() < 2 {
            return Err(DomainError::new(
                "invalid_consolidation",
                "Consolidation requires at least two resolved identities.",
            )
            .into());
        }
        let mut parents = Vec::new();
        let mut stale = false;
        for (id, state) in states {
            if !affected.insert(id) {
                return Err(DomainError::new(
                    "duplicate_batch_target",
                    "Existing identities cannot be affected by multiple proposals.",
                )
                .into());
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
            stale_proposals.push(ProposalLocation::field(index, reference_field));
            continue;
        }
        let mut has_conflict = false;
        let mut disposition_conflict = false;
        if p.operation == Operation::Consolidate {
            let unresolved = consolidation_conflicts(index, p, &parents, &relations)?;
            has_conflict = !unresolved.is_empty();
            disposition_conflict = p.in_working_set.is_none()
                && parents.first().is_some_and(|first| {
                    parents
                        .iter()
                        .any(|parent| parent.in_working_set != first.in_working_set)
                });
            conflicts.extend(unresolved);
        }
        if parents.iter().any(|p| p.in_working_set == Some(true)) && p.in_working_set.is_none() {
            if !disposition_conflict {
                let heads: Vec<_> = parents.iter().map(|parent| {
                json!({"memory_ref":parent.memory_ref,"value":parent.in_working_set})
            }).collect();
                missing_dispositions.push(ProposalDetail::field(index, "in_working_set", json!({
                    "constraint":"Set in_working_set to true to keep the Task active or false to close it.",
                    "heads":heads
                })));
            }
            continue;
        }
        if has_conflict {
            continue;
        }
        if p.what_to_remember.is_none() {
            p.what_to_remember = Some(proposal_body(p, &parents)?.to_owned());
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
            )
            .into());
        }
        inherited(
            &mut p.applies_to,
            &parents,
            |m| m.applies_to.clone(),
            "applies_to",
        )?;
        if p.applies_to.is_none() {
            return Err(DomainError::new("invalid_input", "Create requires applies_to.").into());
        }
        inherited(&mut p.is_rule, &parents, |m| m.is_rule, "is_rule")?;
        let inherited_ids = |ids: &[String]| {
            let mut ids = ids.to_vec();
            if p.operation == Operation::Consolidate {
                ids.sort_unstable();
            }
            ids
        };
        inherited(
            &mut p.related_memory_ids,
            &parents,
            |m| inherited_ids(&m.related_memory_ids),
            "related_memory_ids",
        )?;
        inherited(
            &mut p.required_memory_ids,
            &parents,
            |m| inherited_ids(&m.required_memory_ids),
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
                )
                .into());
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
            return Err(DomainError::new("invalid_scope", "Unsupported scope combination.").into());
        }
        let applicability = if !explicit_scope && !parents.is_empty() {
            parents[0].applicability.clone()
        } else {
            if scopes.contains(&ScopeKind::Project) && context.project_id.is_none() {
                return Err(DomainError::new(
                    "project_connection_required",
                    "Project-scoped memory requires a resolved project.",
                )
                .into());
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
                return Err(field_error(
                    DomainError::new(
                        "task_scope_required",
                        "Task memory must have project-only scope.",
                    ),
                    index,
                    "applies_to",
                    "Set applies_to to [\"project\"] for project-only scope.",
                ));
            }
            task_headings(proposal_body(p, &parents)?).map_err(|error| field_error(
                error, index, "what_to_remember",
                "Supply the Task headings in order: Objective, Current position, Resume point, References.",
            ))?;
        }
        for (field, ids) in [
            ("related_memory_ids", &p.related_memory_ids),
            ("required_memory_ids", &p.required_memory_ids),
        ] {
            let mut seen = BTreeSet::new();
            for (position, id) in ids.iter().flatten().enumerate() {
                let location = ProposalLocation::field(index, field).element(position, None);
                let error = |code| identity_error(code, location, json!(id));
                if !validation::is_entity_identity(id) || !seen.insert(id) {
                    return Err(error("invalid_input"));
                }
                validate_id(id)?;
                if source.current(id)?.is_none() {
                    return Err(error("not_found"));
                }
            }
        }
        for target in p.work_targets.iter().flatten() {
            validate_id(&target.project_id)?;
            relative_path(&target.path)?;
        }
    }
    if !stale_proposals.is_empty() {
        return Err(PreflightError::Stale(
            DomainError::new(
                "stale_memory_ref",
                "Copy all current head references and review their full contents.",
            ),
            stale_proposals,
        ));
    }
    if !conflicts.is_empty() {
        conflicts.extend(missing_dispositions);
        return Err(PreflightError::Located(DomainError::new(
            "metadata_resolution_required",
            "Specify the final values of the listed consolidation fields.",
        )
        .with_next_action("Submit applies_to as a scope array, is_rule and in_working_set as booleans, related_memory_ids and required_memory_ids as target ID arrays, and work_targets as an array of work-target objects for the listed fields.")
        , conflicts));
    }
    if !missing_dispositions.is_empty() {
        return Err(PreflightError::Located(
            DomainError::new(
                "working_set_disposition_required",
                "Explicitly retain or close each active Task.",
            )
            .with_next_action(
                "Set in_working_set to true to keep the Task active or false to close it.",
            ),
            missing_dispositions,
        ));
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
        self.apply_delta_with_positions(context, reference, delta)
            .map(|(batch, _)| batch)
    }
    pub(crate) fn apply_delta_with_positions(
        &self,
        context: &RequestContext,
        reference: &PendingBatchRef,
        delta: &[MemoryProposal],
    ) -> Result<(Self, Vec<Option<usize>>)> {
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
        let mut positions = vec![None; next.proposals.len()];
        for (input_index, proposal) in delta.iter().enumerate() {
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
                    next.proposals[position] = proposal;
                    positions[position] = Some(input_index);
                }
                None => {
                    proposal.proposal_id = Some(uuid::Uuid::new_v4().to_string());
                    next.proposals.push(proposal);
                    positions.push(Some(input_index));
                }
            }
        }
        next.reference.observed_version = uuid::Uuid::new_v4().to_string();
        Ok((next, positions))
    }
}
