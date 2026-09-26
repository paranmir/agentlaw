use crate::*;
use std::collections::{BTreeSet, VecDeque};

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
    if let Some(query) = &r.recall_for {
        let mut queries = vec![query.clone()];
        if r.restore_context == Some(true) {
            queries.extend(
                RESTORE_PERSPECTIVES
                    .iter()
                    .map(|p| format!("{query}\nPerspective: {p}")),
            )
        }
        let mut candidate_ids = BTreeSet::new();
        let mut discovered_ids = BTreeSet::new();
        for query in queries {
            let result =
                search.search(&effective, &query, r.search_terms.as_deref().unwrap_or(&[]))?;
            discovered_ids.extend(result.full_ids.iter().cloned());
            selected.extend(result.full_ids);
            response.diagnostics.extend(result.diagnostics);
            for candidate in result.candidates {
                discovered_ids.insert(candidate.memory_id.clone());
                if candidate_ids.insert(candidate.memory_id.clone()) {
                    response.candidates.push(candidate)
                }
            }
        }
        // Exactly one related hop from discovery, distinct from required closure.
        for id in discovered_ids {
            if let Some(state) = source.current(&id)? {
                for head in state.heads {
                    for related in head.related_memory_ids {
                        if let Some(target) = source.current(&related)? {
                            if candidate_ids.insert(target.resolved_id.clone()) {
                                if let Some(head) = target.heads.first() {
                                    response.candidates.push(MemoryCandidate {
                                        memory_id: target.resolved_id.clone(),
                                        excerpt: head.what_to_remember.chars().take(400).collect(),
                                        applicability: head.applicability.clone(),
                                        retrieval_paths: vec![RetrievalPath {
                                            via: "related_memory".into(),
                                            clue: related,
                                            source_memory_id: Some(id.clone()),
                                        }],
                                    })
                                }
                            }
                        }
                    }
                }
            }
        }
        if r.restore_context == Some(true)
            || r.include_active_tasks == Some(true)
            || r.work_targets.is_some()
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
                for head in &state.heads {
                    if !scope_matches(&head.applicability, &effective) {
                        continue;
                    }
                    if r.restore_context == Some(true) && head.is_rule {
                        selected.insert(state.resolved_id.clone());
                    }
                    if r.include_active_tasks == Some(true) && head.in_working_set == Some(true) {
                        tasks.insert(state.resolved_id.clone());
                        if candidate_ids.insert(state.resolved_id.clone()) {
                            response.candidates.push(MemoryCandidate {
                                memory_id: state.resolved_id.clone(),
                                excerpt: head.what_to_remember.clone(),
                                applicability: head.applicability.clone(),
                                retrieval_paths: vec![RetrievalPath {
                                    via: "active_task".into(),
                                    clue: "Active Task".into(),
                                    source_memory_id: None,
                                }],
                            })
                        }
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
                                    } else if candidate_ids.insert(state.resolved_id.clone()) {
                                        response.candidates.push(MemoryCandidate {
                                            memory_id: state.resolved_id.clone(),
                                            excerpt: head.what_to_remember.clone(),
                                            applicability: head.applicability.clone(),
                                            retrieval_paths: vec![RetrievalPath {
                                                via: "work_target".into(),
                                                clue: stored.path.clone(),
                                                source_memory_id: None,
                                            }],
                                        })
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if r.include_active_tasks == Some(true) {
                response.active_task_count = Some(tasks.len())
            }
        }
    }
    let exact: BTreeSet<_> = r
        .memory_ids
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut missing = Vec::new();
    let mut visited = BTreeSet::new();
    let mut queue: VecDeque<_> = selected.into_iter().collect();
    while let Some(id) = queue.pop_front() {
        let state = match source.current(&id)? {
            Some(s) => s,
            None => {
                if exact.contains(&id) {
                    missing.push(id)
                } else {
                    response.undelivered_required.push(UndeliveredRequired{memory_id:id,reason:"The referenced current identity is absent; required context was not delivered.".into()});
                    response.turn_instruction=Some("Required context is unavailable. Explain this limitation in the user's language; do not treat absent required context as reviewed.".into());
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
            queue.extend(head.required_memory_ids.clone())
        }
        let multiple = state.heads.len() > 1;
        response.memories.push(RecallMemory{memory_id:state.resolved_id,current_heads:state.heads.into_iter().map(Into::into).collect(),head_reconciliation_required:multiple.then_some(true),head_reconciliation_instruction:multiple.then(||"Review every current head before evolving this memory; do not assume a newest winner.".into())});
    }
    response
        .candidates
        .retain(|c| !visited.contains(&c.memory_id));
    if r.recall_for.is_some() {
        let matched = response.candidates.len();
        response
            .candidates
            .truncate(r.memory_candidate_limit.unwrap_or(10) as usize);
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
            memory_ids: missing,
            procedure_ids: vec![],
        })
    }
    Ok(response)
}
