use crate::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const SEMANTIC_CHANNEL_INCOMPLETE_INSTRUCTION: &str = "Briefly disclose this recall's incomplete semantic search in the user's language; combine any required-context warning into the same sentence. Omit only an unchanged semantic notice already visible for this task. Do not auto-retry or repair.";
const REQUIRED_CONTEXT_RECOVERY: &str = "Recover the missing required context using the provided steps. If recovery remains blocked, tell the user what could not be checked. Do not make changes that depend on that context; independent investigation may continue.";

fn add_candidate(
    candidates: &mut Vec<MemoryCandidate>,
    positions: &mut BTreeMap<String, usize>,
    candidate: MemoryCandidate,
) {
    if let Some(&index) = positions.get(&candidate.memory_id) {
        candidates[index]
            .retrieval_paths
            .extend(candidate.retrieval_paths);
    } else {
        positions.insert(candidate.memory_id.clone(), candidates.len());
        candidates.push(candidate);
    }
}

pub struct Discovery {
    pub candidates: Vec<MemoryCandidate>,
    pub full_ids: Vec<String>,
    pub diagnostics: Vec<DomainError>,
}
pub trait RecallSearch {
    fn search(&self, context: &RequestContext, query: &str, terms: &[String]) -> Result<Discovery>;
}
pub const RESTORE_PERSPECTIVES: [&str; 6] = [
    "intent and goals",
    "choices and alternatives",
    "responsibilities and flow",
    "conditions and exceptions",
    "failures and verification",
    "progress and unresolved questions",
];
pub fn work_target_matches(
    stored: &StoredTarget,
    requested: &WorkTarget,
    project_id: &str,
) -> Result<bool> {
    let s = relative_path(&stored.path)?;
    let r = relative_path(&requested.path)?;
    Ok(stored.project_id == project_id
        && (s == r || requested.kind == TargetKind::Directory && s.starts_with(&(r + "/"))))
}
/// Assemble using an owned, single-fence snapshot. Required closure is exact and
/// independent of discovery limits. No byte truncation or invented continuation.
pub fn recall(
    source: &impl SourceSnapshot,
    search: &impl RecallSearch,
    context: &RequestContext,
    r: &RecallRequest,
    current_time: String,
) -> Result<RecallResponse> {
    let (mut response, handoff_id) =
        recall_with_control(source, search, context, r, current_time, None, None)?;
    if let Some(id) = handoff_id {
        let sections = source.current(&id).and_then(|state| {
            let state = state.ok_or_else(task_source_unavailable)?;
            state
                .heads
                .iter()
                .map(|head| crate::write::task_sections(&head.what_to_remember))
                .collect()
        });
        deliver_task_handoff(&mut response, id, sections);
    }
    finalize_recall_guidance(&mut response);
    Ok(response)
}

pub(crate) fn finalize_recall_guidance(response: &mut RecallResponse) {
    let degraded = response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "semantic_channel_incomplete");
    let instructions: Vec<_> = [
        (!response.undelivered_required.is_empty()).then_some(REQUIRED_CONTEXT_RECOVERY),
        response.turn_instruction.as_deref(),
        degraded.then_some(SEMANTIC_CHANNEL_INCOMPLETE_INSTRUCTION),
    ]
    .into_iter()
    .flatten()
    .collect();
    response.turn_instruction = (!instructions.is_empty()).then(|| instructions.join(" "));
}

pub(crate) fn task_source_unavailable() -> DomainError {
    DomainError::new("source_corrupt", "No current Task heads could be read.")
}

pub(crate) fn deliver_task_handoff(
    response: &mut RecallResponse,
    memory_id: String,
    sections: Result<Vec<[String; 3]>>,
) {
    match sections {
        Ok(heads) if !heads.is_empty() => {
            response.active_task_handoff = Some(ActiveTaskHandoff {
                memory_id,
                current_heads: heads
                    .into_iter()
                    .map(
                        |[objective, current_position, resume_point]| TaskHandoffHead {
                            objective,
                            current_position,
                            resume_point,
                        },
                    )
                    .collect(),
            });
            response.task_instruction = Some("Only handoff sections are shown. Look up this Task's memory_id in memory_ids for References, full current heads and required references. Read all current bodies and refs before editing or reconciling.".into());
        }
        sections => {
            let error = sections.err().unwrap_or_else(task_source_unavailable);
            let instruction = format!("The handoff for Task {memory_id} could not be delivered. Check the source and use memory_ids to read all current heads and required references before resuming dependent work.");
            response.turn_instruction = Some(match response.turn_instruction.take() {
                Some(existing) => format!("{existing} {instruction}"),
                None => instruction,
            });
            response.undelivered_required.push(UndeliveredRequired {
                memory_id,
                reason: error.message,
            });
        }
    }
}

pub(crate) fn recall_with_control(
    source: &impl SourceSnapshot,
    search: &impl RecallSearch,
    context: &RequestContext,
    r: &RecallRequest,
    current_time: String,
    control: Option<&RequestControl>,
    known_active_tasks: Option<&[String]>,
) -> Result<(RecallResponse, Option<String>)> {
    if r.procedure_ids.is_some() {
        return Err(DomainError::unsupported(
            "Procedure current-state recall adapter is not connected.",
        ));
    }
    if r.project_hint.is_some() || r.selected_project_id.is_some() {
        return Err(DomainError::unsupported(
            "Cross-project candidate selection requires a project catalog adapter.",
        ));
    }
    let mut effective = context.clone();
    if let Some(machine) = &r.machine_id {
        validate_id(machine)?;
        effective.machine_id = machine.clone()
    }
    let mut response = RecallResponse {
        current_time,
        ..Default::default()
    };
    let mut selected: BTreeSet<String> = r
        .memory_ids
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    for id in &selected {
        validate_id(id)?
    }
    let mut sole_task = None;
    if let Some(query) = &r.recall_for {
        let mut search_selected = BTreeSet::new();
        let mut queries = vec![query.clone()];
        if r.restore_context == Some(true) {
            queries.extend(
                RESTORE_PERSPECTIVES
                    .iter()
                    .map(|p| format!("{query}\nPerspective: {p}")),
            )
        }
        let mut candidate_positions = BTreeMap::new();
        let mut discovered_ids = Vec::new();
        let mut discovered = BTreeSet::new();
        for query in queries {
            let result =
                search.search(&effective, &query, r.search_terms.as_deref().unwrap_or(&[]))?;
            response.diagnostics.extend(result.diagnostics);
            for candidate in result.candidates {
                if discovered.insert(candidate.memory_id.clone()) {
                    discovered_ids.push(candidate.memory_id.clone());
                }
                add_candidate(
                    &mut response.candidates,
                    &mut candidate_positions,
                    candidate,
                );
            }
            for id in result.full_ids {
                if discovered.insert(id.clone()) {
                    discovered_ids.push(id.clone());
                }
                search_selected.insert(id);
            }
        }
        // Exactly one related hop from discovery, distinct from required closure.
        for id in discovered_ids {
            if let Some(state) = source.current(&id)? {
                for head in state.heads {
                    for related in head.related_memory_ids {
                        if let Some(target) = source.current(&related)? {
                            if let Some(head) = target.heads.first() {
                                add_candidate(
                                    &mut response.candidates,
                                    &mut candidate_positions,
                                    MemoryCandidate {
                                        memory_id: target.resolved_id.clone(),
                                        excerpt: head.what_to_remember.chars().take(400).collect(),
                                        applicability: head.applicability.clone(),
                                        retrieval_paths: vec![RetrievalPath {
                                            via: vec!["related_memory".into()],
                                            clue: related,
                                            source_memory_ids: Some(vec![state
                                                .resolved_id
                                                .clone()]),
                                        }],
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
        // Rules are mandatory for every contextual recall; other inventory
        // selections remain controlled by their request options.
        {
            if (r.include_active_tasks == Some(true) || r.work_targets.is_some())
                && effective.project_id.is_none()
            {
                return Err(DomainError::new(
                    "project_connection_required",
                    "Task and work-target lookup require a resolved project.",
                ));
            }
            let inventory = source.inventory()?;
            let mut tasks = BTreeSet::new();
            for state in inventory {
                if let Some(control) = control {
                    control.check()?;
                }
                for head in &state.heads {
                    if !scope_matches(&head.applicability, &effective) {
                        continue;
                    }
                    if head.is_rule {
                        selected.insert(state.resolved_id.clone());
                    }
                    if r.include_active_tasks == Some(true) && head.in_working_set == Some(true) {
                        tasks.insert(state.resolved_id.clone());
                        add_candidate(
                            &mut response.candidates,
                            &mut candidate_positions,
                            MemoryCandidate {
                                memory_id: state.resolved_id.clone(),
                                excerpt: head.what_to_remember.clone(),
                                applicability: head.applicability.clone(),
                                retrieval_paths: vec![RetrievalPath {
                                    via: vec!["active_task".into(), "active_working_set".into()],
                                    clue: "Active Task".into(),
                                    source_memory_ids: None,
                                }],
                            },
                        );
                    }
                    if let Some(targets) = &r.work_targets {
                        for requested in targets {
                            relative_path(&requested.path)?;
                            for stored in &head.work_targets {
                                if work_target_matches(
                                    stored,
                                    requested,
                                    effective.project_id.as_deref().unwrap(),
                                )? {
                                    if stored.reading == Reading::Required {
                                        selected.insert(state.resolved_id.clone());
                                    } else {
                                        add_candidate(
                                            &mut response.candidates,
                                            &mut candidate_positions,
                                            MemoryCandidate {
                                                memory_id: state.resolved_id.clone(),
                                                excerpt: head.what_to_remember.clone(),
                                                applicability: head.applicability.clone(),
                                                retrieval_paths: vec![RetrievalPath {
                                                    via: vec!["work_target".into()],
                                                    clue: stored.path.clone(),
                                                    source_memory_ids: None,
                                                }],
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if r.include_active_tasks == Some(true) {
                let tasks = known_active_tasks
                    .map(<[String]>::to_vec)
                    .unwrap_or_else(|| tasks.into_iter().collect());
                response.active_task_count = Some(tasks.len());
                sole_task = match tasks.as_slice() {
                    [id] => Some(id.clone()),
                    _ => None,
                };
            }
        }
        for id in search_selected {
            if let Some(task_id) = &sole_task {
                if &id == task_id
                    || source
                        .current(&id)?
                        .is_some_and(|state| &state.resolved_id == task_id)
                {
                    continue;
                }
            }
            selected.insert(id);
        }
    }
    let exact: BTreeSet<_> = r
        .memory_ids
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut missing = BTreeSet::new();
    let mut missing_required = BTreeSet::new();
    let mut required = selected
        .difference(&exact)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut visited = BTreeSet::new();
    let mut queue: VecDeque<_> = selected.into_iter().collect();
    while let Some(id) = queue.pop_front() {
        if let Some(control) = control {
            control.check()?;
        }
        let state = match source.current(&id)? {
            Some(s) => s,
            None => {
                if exact.contains(&id) {
                    missing.insert(id.clone());
                }
                if required.contains(&id) && missing_required.insert(id.clone()) {
                    response.undelivered_required.push(UndeliveredRequired{memory_id:id,reason:"The referenced current identity is absent; required context was not delivered.".into()});
                }
                continue;
            }
        };
        if !visited.insert(state.resolved_id.clone()) {
            continue;
        }
        if state.heads.is_empty() {
            return Err(DomainError::new(
                "source_corrupt",
                "A live memory has no current heads.",
            ));
        }
        for head in &state.heads {
            required.extend(head.required_memory_ids.iter().cloned());
            queue.extend(head.required_memory_ids.clone());
        }
        let multiple = state.heads.len() > 1;
        response.memories.push(RecallMemory{memory_id:state.resolved_id,current_heads:state.heads.into_iter().map(Into::into).collect(),head_reconciliation_required:multiple.then_some(true),head_reconciliation_instruction:multiple.then(||"Review every current head before evolving this memory; do not assume a newest winner.".into())});
    }
    response
        .candidates
        .retain(|c| !visited.contains(&c.memory_id) && sole_task.as_ref() != Some(&c.memory_id));
    if r.recall_for.is_some() {
        if r.include_active_tasks == Some(true) {
            response.candidates.sort_by_key(|candidate| {
                !candidate
                    .retrieval_paths
                    .iter()
                    .any(|path| path.via.iter().any(|via| via == "active_task"))
            });
        }
        let matched = response.candidates.len();
        response
            .candidates
            .truncate(r.memory_candidate_limit.unwrap_or(10) as usize);
        for candidate in &mut response.candidates {
            candidate.retrieval_paths =
                group_retrieval_paths(std::mem::take(&mut candidate.retrieval_paths));
        }
        response.candidate_counts = Some(CandidateCounts {
            memories: Counts {
                matched,
                shown: response.candidates.len(),
            },
            learned_procedures: Counts {
                matched: 0,
                shown: 0,
            },
        });
        response.diagnostics.push(DomainError::unsupported("Learned procedure discovery is not connected; an empty procedure list is not evidence of absence."));
    }
    if !missing.is_empty() {
        response.code = Some("exact_selection_incomplete".into());
        response.missing_ids = Some(MissingIds {
            memory_ids: missing.into_iter().collect(),
            procedure_ids: vec![],
        })
    }
    let handoff_id = sole_task
        .filter(|id| !visited.contains(id) && !required.contains(id) && !exact.contains(id));
    Ok((response, handoff_id))
}
