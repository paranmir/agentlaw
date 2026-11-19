//! Read-only store-owner query assembly.
use super::*;
use crate::exact::{DeltaBudget, ExactSnapshot};
use crate::semantic::{QueryVector, SemanticCoverage, SemanticError, SemanticQuery};
use agentlaw_search::{Hit, ScopeFilter};

struct IndexedSearch {
    queries: BTreeMap<String, Discovery>,
}
impl RecallSearch for IndexedSearch {
    fn search(&self, _: &RequestContext, q: &str, _: &[String]) -> Result<Discovery> {
        let d = self.queries.get(q).ok_or_else(|| {
            DomainError::new(
                "search_basis_changed",
                "The prepared query was not part of this read view.",
            )
        })?;
        Ok(Discovery {
            candidates: d.candidates.clone(),
            full_ids: d.full_ids.clone(),
            diagnostics: d.diagnostics.clone(),
        })
    }
}
const SEARCH_LIMIT: usize = 10_000;

pub fn search_procedures_filtered(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    query: &str,
    scope: Option<&str>,
    project: Option<&str>,
    machine: Option<&str>,
    limit: usize,
) -> Result<Value> {
    ctx.request.check()?;
    let project = if let Some(selector) = project {
        let catalog = ctx
            .source
            .list_catalog()
            .map_err(|_| DomainError::new("source_unavailable", "Cannot read project catalog."))?;
        let matches: Vec<_> = catalog
            .iter()
            .filter(|p| {
                p["project_id"].as_str() == Some(selector)
                    || p["name"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(selector))
                    || p["aliases"].as_array().is_some_and(|aliases| {
                        aliases.iter().any(|alias| {
                            alias
                                .as_str()
                                .is_some_and(|name| name.eq_ignore_ascii_case(selector))
                        })
                    })
            })
            .filter_map(|p| p["project_id"].as_str())
            .collect();
        if matches.contains(&selector) {
            Some(selector.to_owned())
        } else {
            match matches.as_slice() {
                [id] => Some((*id).to_owned()),
                _ => {
                    return Err(DomainError::new(
                        "project_selection_required",
                        "Choose one explicit project ID for management search.",
                    ))
                }
            }
        }
    } else {
        None
    };
    if let Some(machine) = machine {
        validate_id(machine)?;
    }
    let scopes: Vec<_> = exact
        .all_scopes()?
        .into_iter()
        .filter(|s| {
            scope.is_none_or(|kind| match kind {
                "user" => s == "user",
                "project" => s.starts_with("project:") && !s.contains(":machine:"),
                "machine" => s.starts_with("machine:"),
                "project_machine" => s.starts_with("project:") && s.contains(":machine:"),
                _ => false,
            })
        })
        .filter(|s| {
            project.as_ref().is_none_or(|id| {
                s == &format!("project:{id}") || s.starts_with(&format!("project:{id}:machine:"))
            })
        })
        .filter(|s| {
            machine.is_none_or(|id| {
                s == &format!("machine:{id}") || s.ends_with(&format!(":machine:{id}"))
            })
        })
        .map(|s| format!("procedure:{s}"))
        .collect();
    let channels = preparation_channels(ctx, exact, query, &scopes, None)?;
    let matched = channels.hits.len();
    let mut results = Vec::new();
    let mut shown = 0;
    for hit in channels.hits.into_iter().take(limit) {
        ctx.request.check()?;
        let Some(id) = hit.memory_id.strip_prefix("procedure/") else {
            continue;
        };
        let mut descriptors = Vec::new();
        for h in exact.heads(&[id.to_owned()])? {
            let applicability = scope_value(&h.metadata["applicability"])?;
            if !scopes.contains(&format!("procedure:{}", scope_token(&applicability))) {
                continue;
            }
            let value = json!({"procedure_id":id,"name":h.metadata["name"],"use_when":h.metadata["use_when"],"applicability":applicability});
            if !descriptors.contains(&value) {
                descriptors.push(value);
            }
        }
        descriptors.sort_by_key(Value::to_string);
        if !descriptors.is_empty() {
            shown += 1;
            results.extend(descriptors);
        }
    }
    Ok(
        json!({"procedures":results,"matched":matched,"shown":shown,"diagnostics":channels.diagnostics}),
    )
}

enum AcquiredClosure {
    Owned(Vec<agentlaw_storage::ResolvedCurrent>),
    Spooled(Vec<agentlaw_storage::acquire::AcquiredResolvedCurrent>),
}
#[derive(Clone)]
struct PreparedQuery {
    text: String,
    combined: String,
    vector: Option<QueryVector>,
}
struct Found {
    hits: Vec<Hit>,
    lexical: Vec<Hit>,
    vector: Vec<Hit>,
    lexical_ids: BTreeSet<String>,
    complete: bool,
    matched: u64,
}

fn query_texts(ctx: &ReadCtx<'_>, request: &RecallRequest) -> Result<Vec<PreparedQuery>> {
    let Some(query) = &request.recall_for else {
        return Ok(vec![]);
    };
    let mut texts = vec![query.clone()];
    if request.restore_context == Some(true) {
        texts.extend(
            RESTORE_PERSPECTIVES
                .iter()
                .map(|p| format!("{query}\nPerspective: {p}")),
        );
    }
    let mut prepared = Vec::new();
    for text in texts {
        ctx.request.check()?;
        let combined = match &request.search_terms {
            Some(terms) => format!("{text} {}", terms.join(" ")),
            None => text.clone(),
        };
        let vector = match ctx.semantic.embed_query(&combined, ctx.request) {
            Ok(vector) => Some(vector),
            Err(SemanticError::Cancelled) => {
                ctx.request.check()?;
                return Err(DomainError::new(
                    "cancelled",
                    "Query embedding was cancelled.",
                ));
            }
            Err(_) => None,
        };
        prepared.push(PreparedQuery {
            text,
            combined,
            vector,
        });
    }
    Ok(prepared)
}
fn winner(hits: &[Hit], min: f64, margin: f64) -> Option<&str> {
    let top = hits.first()?;
    if !top.score.is_finite()
        || top.score < min
        || hits
            .get(1)
            .is_some_and(|next| top.score - next.score < margin)
    {
        return None;
    }
    Some(&top.memory_id)
}
fn uncontested(lexical: &[Hit], vector: &[Hit]) -> Option<String> {
    match (winner(lexical, 0.90, 0.05), winner(vector, 0.92, 0.03)) {
        (Some(a), Some(b)) if a != b => None,
        (Some(a), _) => Some(a.to_owned()),
        (_, Some(b)) => Some(b.to_owned()),
        _ => None,
    }
}
fn search_error(_: agentlaw_search::Error) -> DomainError {
    DomainError::new(
        "lexical_search_failed",
        "The lexical read view could not be searched.",
    )
}
fn search_one(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    query: &PreparedQuery,
    scopes: &[String],
) -> Result<Found> {
    ctx.request.check()?;
    let (lexical, strength, matched) = {
        let mut corpus = exact.lexical(&ScopeFilter {
            allowed_scopes: scopes.to_vec(),
        })?;
        let lexical = corpus
            .search(&query.combined, SEARCH_LIMIT)
            .map_err(search_error)?;
        let strength = corpus
            .strength(&query.combined, SEARCH_LIMIT)
            .map_err(search_error)?;
        let matched = corpus.matched(&query.combined).map_err(search_error)?;
        (lexical, strength, matched)
    };
    let position = exact.position().clone();
    let (mut vector, complete) = if let Some(vector) = &query.vector {
        match ctx.semantic.search_available(
            SemanticQuery {
                binding: &ctx.identity.binding_id,
                scopes,
                text: &query.combined,
                vector: Some(vector),
                limit: SEARCH_LIMIT,
                source: &position,
            },
            ctx.request,
        ) {
            Ok(result) => {
                let complete = matches!(result.coverage, SemanticCoverage::CompleteAt(ref p) if p == &position);
                (result.hits, complete)
            }
            Err(SemanticError::Cancelled) => {
                ctx.request.check()?;
                return Err(DomainError::new(
                    "cancelled",
                    "Semantic search was cancelled.",
                ));
            }
            Err(_) => (vec![], false),
        }
    } else {
        (vec![], false)
    };
    ctx.request.check()?;
    // Never present a stale vector revision as a current result.
    let ids: Vec<_> = vector
        .iter()
        .map(|h| {
            h.memory_id
                .strip_prefix("procedure/")
                .unwrap_or(&h.memory_id)
                .to_owned()
        })
        .collect();
    let heads = exact.heads(&ids)?;
    vector.retain(|hit| {
        let procedure = hit.memory_id.starts_with("procedure/");
        let id = hit
            .memory_id
            .strip_prefix("procedure/")
            .unwrap_or(&hit.memory_id);
        heads.iter().any(|h| {
            if h.entity_id != id
                || h.observed_version != hit.change_id
                || (h.entity_type == "learned_procedure") != procedure
            {
                return false;
            }
            let Ok(applicability) = scope_value(&h.metadata["applicability"]) else {
                return false;
            };
            let scope = crate::store::scope_token(&applicability);
            let scope = if procedure {
                format!("procedure:{scope}")
            } else {
                scope
            };
            scopes.contains(&scope)
        })
    });
    vector.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.memory_id.cmp(&b.memory_id))
    });
    let lexical_ids = lexical.iter().map(|h| h.memory_id.clone()).collect();
    let hits = agentlaw_search::reciprocal_rank_fusion(&[lexical, vector.clone()]);
    Ok(Found {
        hits,
        lexical: strength,
        vector,
        lexical_ids,
        complete,
        matched,
    })
}

pub(crate) fn preparation_channels(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    text: &str,
    scopes: &[String],
    planned: Option<(
        &crate::semantic::PiecePlan,
        usize,
        &crate::semantic::PieceVectorReuse,
    )>,
) -> Result<Channels> {
    if planned.is_some() {
        return Err(DomainError::new(
            "internal_error",
            "Planned preparation search is not implemented.",
        ));
    }
    let vector = match ctx.semantic.embed_query(text, ctx.request) {
        Ok(vector) => Some(vector),
        Err(SemanticError::Cancelled) => {
            ctx.request.check()?;
            return Err(DomainError::new(
                "cancelled",
                "Query embedding was cancelled.",
            ));
        }
        Err(_) => None,
    };
    let query = PreparedQuery {
        text: text.to_owned(),
        combined: text.to_owned(),
        vector,
    };
    let found = search_one(ctx, exact, &query, scopes)?;
    let diagnostics = if found.complete {
        vec![]
    } else {
        vec![DomainError::new(
            "semantic_channel_incomplete",
            "Semantic retrieval did not establish a complete view.",
        )]
    };
    Ok(Channels {
        hits: found.hits,
        lexical: found.lexical,
        vector: found.vector,
        complete: found.complete,
        diagnostics,
    })
}

fn connection(ctx: &ReadCtx<'_>, path: Option<&str>) -> Result<Option<String>> {
    match path {
        None => Ok(None),
        Some(path) => {
            absolute_project_path(path)?;
            ctx.control
                .exact_association(&ctx.identity.binding_id, path)?
                .map(|p| p.project_id)
                .ok_or_else(|| {
                    DomainError::new(
                        "project_connection_required",
                        "Discover and explicitly select or create a project connection.",
                    )
                })
                .map(Some)
        }
    }
}
fn context(ctx: &ReadCtx<'_>, request: &RecallRequest) -> Result<RequestContext> {
    let machine = request
        .machine_id
        .as_ref()
        .unwrap_or(&ctx.identity.machine_id);
    validate_id(machine)?;
    Ok(RequestContext {
        store_binding_id: ctx.identity.binding_id.clone(),
        user_id: ctx.identity.user_id.clone(),
        machine_id: machine.clone(),
        project_id: connection(ctx, request.project_path.as_deref())?,
        connection_version: String::new(),
    })
}

pub fn discover(ctx: &ReadCtx<'_>, request: &ConnectRequest) -> Result<Value> {
    ctx.request.check()?;
    if request.restore_context == Some(true) {
        return Err(DomainError::new(
            "query_classification_error",
            "Discover with restoration must be classified as a command.",
        ));
    }
    absolute_project_path(&request.project_path)?;
    if request
        .intent
        .as_ref()
        .is_some_and(|v| *v != ConnectIntent::Discover)
    {
        return Err(DomainError::new(
            "invalid_input",
            "This read path supports project discovery only.",
        ));
    }
    if let Some(path) = &request.memory_store_path {
        if std::path::Path::new(path) != ctx.identity.root {
            return Err(DomainError::new(
                "store_binding_transition_required",
                "Select a different store through installation configuration.",
            ));
        }
    }
    let existing = ctx
        .control
        .exact_association(&ctx.identity.binding_id, &request.project_path)?;
    if let Some(existing) = existing {
        return to_value(ConnectResponse {
            code: None,
            project_connection: Some(existing),
            candidates: vec![],
            recall_result: None,
        });
    }
    let candidates = catalog_candidates(ctx, request.clues.as_ref())?;
    let mut value = to_value(ConnectResponse {
        code: Some("project_connection_required".into()),
        project_connection: None,
        candidates,
        recall_result: None,
    })?;
    value["next_action"] = json!("Select a project explicitly or approve creation before connecting; a single candidate is not an automatic selection.");
    Ok(value)
}

fn catalog_candidates(
    ctx: &ReadCtx<'_>,
    clues: Option<&ProjectClues>,
) -> Result<Vec<ProjectCandidate>> {
    let catalog = {
        let guard = source(ctx.source.read_snapshot())?;
        source(guard.list_catalog())?
    };
    let mut candidates = Vec::new();
    for p in catalog {
        ctx.request.check()?;
        let id = p["project_id"]
            .as_str()
            .ok_or_else(|| DomainError::new("catalog_corrupt", "Project identity is missing."))?;
        validate_id(id)?;
        let name = p["name"]
            .as_str()
            .ok_or_else(|| DomainError::new("catalog_corrupt", "Project name is missing."))?
            .to_string();
        let description = p["description"].as_str().map(str::to_owned);
        let mut reasons = Vec::new();
        if let Some(clues) = clues {
            if clues.repository_url.as_ref().is_some_and(|url| {
                p["repository_observations"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|v| v["repository_address"].as_str() == Some(url))
                })
            }) {
                reasons.push("Observed repository URL matches.".into());
            }
            if clues
                .name
                .as_ref()
                .is_some_and(|n| name.to_lowercase().contains(&n.to_lowercase()))
            {
                reasons.push("Observed project name matches.".into());
            }
            if clues.name.as_ref().is_some_and(|n| {
                p["aliases"].as_array().is_some_and(|a| {
                    a.iter().any(|v| {
                        v.as_str()
                            .is_some_and(|s| s.to_lowercase().contains(&n.to_lowercase()))
                    })
                })
            }) {
                reasons.push("Observed name matches a known project alias.".into());
            }
            if clues.description.as_ref().is_some_and(|d| {
                description
                    .as_ref()
                    .is_some_and(|s| s.to_lowercase().contains(&d.to_lowercase()))
            }) {
                reasons.push("Observed description matches.".into());
            }
        } else {
            reasons.push("Available project identity; no identifying clues supplied.".into());
        }
        if !reasons.is_empty() {
            candidates.push(ProjectCandidate {
                project_id: id.to_string(),
                name,
                description,
                reasons,
            });
        }
    }
    candidates.sort_by(|a, b| a.project_id.cmp(&b.project_id));
    Ok(candidates)
}

/// Query embeddings prepared before a projection generation is pinned.
#[doc(hidden)]
pub struct PreparedRecall {
    queries: Vec<PreparedQuery>,
}

#[doc(hidden)]
pub fn prepare_recall(ctx: &ReadCtx<'_>, request: &RecallRequest) -> Result<PreparedRecall> {
    Ok(PreparedRecall {
        queries: query_texts(ctx, request)?,
    })
}

pub fn recall(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
) -> Result<Value> {
    let prepared = prepare_recall(ctx, request)?;
    recall_scoped(
        ctx,
        exact,
        request,
        None,
        DeltaBudget::default(),
        &prepared.queries,
        None,
        &mut Vec::new(),
    )
}

/// Recall against the project identity fixed by a completed connection.
#[doc(hidden)]
pub fn recall_for_project(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
    project_id: &str,
) -> Result<Value> {
    let prepared = prepare_recall(ctx, request)?;
    recall_prepared(
        ctx,
        exact,
        request,
        Some(project_id),
        DeltaBudget::default(),
        &prepared,
    )
}

#[doc(hidden)]
pub fn recall_prepared(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
    project_id: Option<&str>,
    budget: DeltaBudget,
    prepared: &PreparedRecall,
) -> Result<Value> {
    recall_prepared_with_diagnostics(ctx, exact, request, project_id, budget, prepared)
        .map(|(value, _)| value)
}

/// Returns semantic completeness diagnostics independently of inline or file delivery.
#[doc(hidden)]
pub fn recall_prepared_with_diagnostics(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
    project_id: Option<&str>,
    budget: DeltaBudget,
    prepared: &PreparedRecall,
) -> Result<(Value, Vec<DomainError>)> {
    if let Some(id) = project_id {
        validate_id(id)?;
    }
    let mut diagnostics = Vec::new();
    let value = recall_scoped(
        ctx,
        exact,
        request,
        project_id,
        budget,
        &prepared.queries,
        None,
        &mut diagnostics,
    )?;
    Ok((value, diagnostics))
}

fn recall_scoped(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
    fixed_project: Option<&str>,
    budget: DeltaBudget,
    prepared: &[PreparedQuery],
    outer: Option<Value>,
    completeness_diagnostics: &mut Vec<DomainError>,
) -> Result<Value> {
    ctx.request.check()?;
    let mut context = if let Some(project_id) = fixed_project {
        let machine = request
            .machine_id
            .as_ref()
            .unwrap_or(&ctx.identity.machine_id);
        validate_id(machine)?;
        RequestContext {
            store_binding_id: ctx.identity.binding_id.clone(),
            user_id: ctx.identity.user_id.clone(),
            machine_id: machine.clone(),
            project_id: Some(project_id.to_owned()),
            connection_version: String::new(),
        }
    } else {
        match context(ctx, request) {
            Ok(value) => value,
            Err(error) if error.code == "project_connection_required" => {
                let discovery = discover(
                    ctx,
                    &ConnectRequest {
                        project_path: request.project_path.clone().unwrap_or_default(),
                        intent: Some(ConnectIntent::Discover),
                        clues: None,
                        project_id: None,
                        project_name: None,
                        project_description: None,
                        memory_store_path: None,
                        restore_context: None,
                        recall_for: None,
                    },
                )?;
                let mut result = json!({"status":"needs_user_input", "code":"project_connection_required", "message":"Choose a project memory for this folder.", "project_path":request.project_path, "candidates":discovery["candidates"], "turn_instruction":"Project rules, active Tasks and work targets have not been checked. Read partial_recall or its error; select or create a project explicitly, then repeat the original request."});
                if request.recall_for.is_some() {
                    let mut partial = request.clone();
                    partial.project_path = None;
                    partial.project_hint = None;
                    partial.selected_project_id = None;
                    partial.memory_ids = None;
                    partial.procedure_ids = None;
                    partial.restore_context = None;
                    partial.include_active_tasks = None;
                    partial.work_targets = None;
                    match recall_scoped(
                        ctx,
                        exact,
                        &partial,
                        None,
                        budget,
                        &prepared[..prepared.len().min(1)],
                        Some(result.clone()),
                        completeness_diagnostics,
                    ) {
                        Ok(value) => return Ok(value),
                        Err(error) if error.code == "cancelled" => return Err(error),
                        Err(error) => result["partial_recall_error"] = to_value(error)?,
                    }
                }
                return Ok(result);
            }
            Err(error) => return Err(error),
        }
    };
    if let Some(hint) = request
        .project_hint
        .as_ref()
        .filter(|_| fixed_project.is_none())
    {
        let options = catalog_candidates(
            ctx,
            Some(&ProjectClues {
                name: Some(hint.clone()),
                description: Some(hint.clone()),
                repository_url: None,
            }),
        )?;
        if let Some(selected) = &request.selected_project_id {
            validate_id(selected)?;
            if !options.iter().any(|v| v.project_id == *selected) {
                return Err(DomainError::new(
                    "project_selection_invalid",
                    "The selected project no longer matches the project hint.",
                ));
            }
            context.project_id = Some(selected.clone());
        } else if options.len() == 1 {
            context.project_id = Some(options[0].project_id.clone());
        } else {
            return Ok(
                json!({"code":"project_selection_required","project_candidates":options,"turn_instruction":"Repeat with the same project hint and selected project ID."}),
            );
        }
    }
    if (request.include_active_tasks == Some(true) || request.work_targets.is_some())
        && context.project_id.is_none()
    {
        return Err(DomainError::new(
            "project_connection_required",
            "Task and work-target lookup require a resolved project.",
        ));
    }

    let scopes = crate::exact::allowed_scopes(&context);
    let scope_list: Vec<String> = scopes.into_iter().collect();
    let procedure_scopes: Vec<String> = scope_list
        .iter()
        .map(|s| format!("procedure:{s}"))
        .collect();
    let (mut snapshot, findings, procedures, proc_search) = loop {
        ctx.request.check()?;
        let position = exact.position().clone();
        let mut findings = BTreeMap::new();
        let mut procedure_findings = BTreeMap::new();
        let mut selected: BTreeSet<String> = request
            .memory_ids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut previews = BTreeSet::new();
        for query in prepared {
            let found = search_one(ctx, exact, query, &scope_list)?;
            if let Some(id) = uncontested(&found.lexical, &found.vector) {
                selected.insert(id);
            }
            previews.extend(found.hits.iter().map(|h| h.memory_id.clone()));
            findings.insert(query.text.clone(), found);
            let found = search_one(ctx, exact, query, &procedure_scopes)?;
            procedure_findings.insert(query.text.clone(), found);
        }
        if request.recall_for.is_some() {
            selected.extend(exact.ids_matching(&context, true, false)?);
            if request.include_active_tasks == Some(true) {
                selected.extend(exact.ids_matching(&context, false, true)?);
            }
            if let Some(targets) = &request.work_targets {
                for (id, reading) in
                    exact.work_targets(context.project_id.as_deref().unwrap(), targets)?
                {
                    match reading {
                        Reading::Required => {
                            selected.insert(id);
                        }
                        Reading::Related => {
                            previews.insert(id);
                        }
                    }
                }
            }
        }
        previews.extend(
            exact
                .related(&previews.iter().cloned().collect::<Vec<_>>())?
                .into_iter()
                .map(|(_, id)| id),
        );
        let preview_heads = exact.heads(&previews.iter().cloned().collect::<Vec<_>>())?;
        let mut procedure_ids: BTreeSet<String> = request
            .procedure_ids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut procedure_order = Vec::new();
        let mut automatic = None;
        for found in procedure_findings.values() {
            if automatic.is_none() {
                automatic = uncontested(&found.lexical, &found.vector)
                    .and_then(|id| id.strip_prefix("procedure/").map(str::to_owned));
            }
            for hit in &found.hits {
                if let Some(id) = hit.memory_id.strip_prefix("procedure/") {
                    if !procedure_order.iter().any(|v| v == id) {
                        procedure_order.push(id.to_owned());
                    }
                }
            }
        }
        procedure_ids.extend(automatic.iter().cloned());
        let mut all_procedures = procedure_ids.clone();
        all_procedures.extend(procedure_order.iter().cloned());
        let guard = source(ctx.source.read_snapshot())?;
        if let Some(project_id) = fixed_project {
            match guard.read_catalog(project_id) {
                Ok(_) => {}
                Err(agentlaw_storage::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Err(DomainError::new(
                        "project_not_found",
                        "The fixed project no longer exists in the catalog.",
                    ));
                }
                Err(agentlaw_storage::Error::NotFound(_)) => {
                    return Err(DomainError::new(
                        "project_not_found",
                        "The fixed project no longer exists in the catalog.",
                    ))
                }
                Err(error) => return source(Err(error)),
            }
        }
        exact.advance(&guard, budget)?;
        if exact.position() != &position {
            drop(guard);
            continue;
        }
        let selected_ids: Vec<_> = selected.iter().cloned().collect();
        let (resolved, missing) = match guard.read_closure(&selected_ids) {
            Ok((_, units, missing)) => (AcquiredClosure::Owned(units), missing),
            Err(agentlaw_storage::Error::Capacity) => {
                let (_, units, missing) = source(guard.acquire_closure(&selected_ids))?;
                (AcquiredClosure::Spooled(units), missing)
            }
            Err(error) => return source(Err(error)),
        };
        let mut redirect_probe: BTreeSet<String> = selected_ids.iter().cloned().collect();
        match &resolved {
            AcquiredClosure::Owned(items) => {
                for value in items {
                    for head in value.current.heads() {
                        for relation in head.metadata["relations"].as_array().into_iter().flatten()
                        {
                            if relation["kind"] == "required" {
                                if let Some(id) = relation["target_memory_id"].as_str() {
                                    redirect_probe.insert(id.to_owned());
                                }
                            }
                        }
                    }
                }
            }
            AcquiredClosure::Spooled(items) => {
                for value in items {
                    for head in value.current.state["current_heads"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        for relation in head["relations"].as_array().into_iter().flatten() {
                            if relation["kind"] == "required" {
                                if let Some(id) = relation["target_memory_id"].as_str() {
                                    redirect_probe.insert(id.to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut redirects = Vec::new();
        while let Some(id) = redirect_probe.pop_first() {
            ctx.request.check()?;
            match guard.read_current_compact(&id, false, 0) {
                Ok(unit) => {
                    if let UnitState::Redirect { redirect_to, .. } = &unit.state {
                        if !redirects
                            .iter()
                            .any(|item: &CurrentUnit| item.entity_id == unit.entity_id)
                        {
                            redirect_probe.insert(redirect_to.clone());
                            redirects.push(unit);
                        }
                    }
                }
                Err(agentlaw_storage::Error::Capacity | agentlaw_storage::Error::NotFound(_)) => {}
                Err(error) => return source(Err(error)),
            }
        }
        let mut procedure_units = Vec::new();
        let mut procedure_spools = Vec::new();
        for id in &procedure_ids {
            ctx.request.check()?;
            validate_id(id)?;
            match guard.read_procedure(id) {
                Ok(unit) => procedure_units.push(unit),
                Err(agentlaw_storage::Error::NotFound(_)) => {}
                Err(agentlaw_storage::Error::Capacity) => {
                    procedure_spools.push(source(guard.acquire_procedure(id))?)
                }
                Err(error) => return source(Err(error)),
            }
        }
        drop(guard);
        let mut streaming = streaming::StreamedSelection::default();
        let mut states = BTreeMap::new();
        let mut units = BTreeMap::new();
        let mut refs = Vec::new();
        match resolved {
            AcquiredClosure::Owned(resolved) => {
                for value in resolved {
                    ctx.request.check()?;
                    let unit = value.current;
                    let memories = unit
                        .heads()
                        .iter()
                        .map(|h| memory_from(&unit, h))
                        .collect::<Result<Vec<_>>>()?;
                    refs.extend(memories.iter().map(|m| m.memory_ref.clone()));
                    let state = CurrentState {
                        requested_id: value.requested_id.clone(),
                        resolved_id: unit.entity_id.clone(),
                        heads: memories,
                        redirect_path: value.redirect_path,
                    };
                    states.insert(
                        unit.entity_id.clone(),
                        CurrentState {
                            requested_id: unit.entity_id.clone(),
                            redirect_path: vec![],
                            ..state.clone()
                        },
                    );
                    states.insert(value.requested_id, state);
                    units.insert(unit.entity_id.clone(), unit);
                }
            }
            AcquiredClosure::Spooled(resolved) => {
                for value in resolved {
                    ctx.request.check()?;
                    let current = value.current;
                    let id = current.state["memory_id"]
                        .as_str()
                        .ok_or_else(|| {
                            DomainError::new(
                                "source_corrupt",
                                "Acquired memory identity is missing.",
                            )
                        })?
                        .to_owned();
                    let mut heads = Vec::new();
                    for metadata in current.state["current_heads"].as_array().ok_or_else(|| {
                        DomainError::new("source_corrupt", "Acquired heads are missing.")
                    })? {
                        let body = current
                            .bodies
                            .get(metadata["change_id"].as_str().unwrap_or(""))
                            .ok_or_else(|| {
                                DomainError::new("source_corrupt", "Acquired body is missing.")
                            })?
                            .clone();
                        let bytes = body.bytes;
                        heads.push(Head {
                            metadata: metadata.clone(),
                            body: streaming.insert(body, 0, bytes),
                        });
                    }
                    let unit = CurrentUnit {
                        entity_id: id.clone(),
                        entity_type: "memory".into(),
                        state: UnitState::Live { heads },
                    };
                    let mut memories = Vec::new();
                    for (head, reference) in unit.heads().iter().zip(&current.references) {
                        let mut memory = memory_from(&unit, head)?;
                        memory.memory_ref.observed_version = reference.observed_version.clone();
                        refs.push(memory.memory_ref.clone());
                        memories.push(memory);
                    }
                    let state = CurrentState {
                        requested_id: value.requested_id.clone(),
                        resolved_id: id.clone(),
                        heads: memories,
                        redirect_path: value.redirect_path,
                    };
                    states.insert(
                        id.clone(),
                        CurrentState {
                            requested_id: id.clone(),
                            redirect_path: vec![],
                            ..state.clone()
                        },
                    );
                    states.insert(value.requested_id, state);
                    units.insert(id, unit);
                }
            }
        }
        for unit in redirects {
            units.insert(unit.entity_id.clone(), unit);
        }
        for unit in procedure_units {
            units.insert(format!("procedure/{}", unit.entity_id), unit);
        }
        for current in procedure_spools {
            ctx.request.check()?;
            let id = current.state["procedure_id"]
                .as_str()
                .ok_or_else(|| {
                    DomainError::new("source_corrupt", "Acquired procedure identity is missing.")
                })?
                .to_owned();
            let mut heads = Vec::new();
            for metadata in current.state["current_heads"]
                .as_array()
                .ok_or_else(|| DomainError::new("source_corrupt", "Acquired heads are missing."))?
            {
                let body = current
                    .bodies
                    .get(metadata["change_id"].as_str().unwrap_or(""))
                    .ok_or_else(|| {
                        DomainError::new("source_corrupt", "Acquired instructions are missing.")
                    })?
                    .clone();
                let bytes = body.bytes;
                heads.push(Head {
                    metadata: metadata.clone(),
                    body: streaming.insert(body, 0, bytes),
                });
            }
            let unit = CurrentUnit {
                entity_id: id.clone(),
                entity_type: "learned_procedure".into(),
                state: UnitState::Live { heads },
            };
            for (head, reference) in unit.heads().iter().zip(&current.references) {
                streaming.register_version(
                    source(agentlaw_storage::version(&unit, head))?,
                    reference.observed_version.clone(),
                );
            }
            units.insert(format!("procedure/{id}"), unit);
        }
        let procedure_previews =
            exact.heads(&all_procedures.iter().cloned().collect::<Vec<_>>())?;
        for head in procedure_previews {
            if procedure_ids.contains(&head.entity_id) {
                continue;
            }
            let key = format!("procedure/{}", head.entity_id);
            let unit = units.entry(key).or_insert_with(|| CurrentUnit {
                entity_id: head.entity_id.clone(),
                entity_type: "learned_procedure".into(),
                state: UnitState::Live { heads: vec![] },
            });
            if let UnitState::Live { heads } = &mut unit.state {
                heads.push(Head {
                    metadata: head.metadata,
                    body: String::new(),
                });
            }
        }
        let mut projected: BTreeMap<String, Vec<crate::exact::HeadRecord>> = BTreeMap::new();
        for head in preview_heads {
            projected
                .entry(head.entity_id.clone())
                .or_default()
                .push(head);
        }
        for (id, heads) in projected {
            ctx.request.check()?;
            if states.contains_key(&id) {
                continue;
            }
            let unit = CurrentUnit {
                entity_id: id.clone(),
                entity_type: "memory".into(),
                state: UnitState::Live { heads: vec![] },
            };
            let memories = heads
                .into_iter()
                .map(|head| {
                    let mut memory = memory_from(
                        &unit,
                        &Head {
                            metadata: head.metadata,
                            body: head.excerpt,
                        },
                    )?;
                    memory.memory_ref.observed_version = head.observed_version;
                    Ok(memory)
                })
                .collect::<Result<Vec<_>>>()?;
            states.insert(
                id.clone(),
                CurrentState {
                    requested_id: id.clone(),
                    resolved_id: id,
                    heads: memories,
                    redirect_path: vec![],
                },
            );
        }
        let snapshot = Snapshot {
            streaming,
            generation: position.sequence,
            states,
            units: units.into_values().collect(),
            basis: ReadSet {
                memory_refs: refs,
                fingerprint: String::new(),
            },
            publication_basis: agentlaw_storage::ReadSet {
                source_position: position,
                observed: BTreeMap::new(),
                absent: missing,
                reverse_required: BTreeMap::new(),
                watched_scopes: vec![],
            },
        };
        break (
            snapshot,
            findings,
            (procedure_ids, procedure_order, automatic),
            procedure_findings,
        );
    };
    let mut queries = BTreeMap::new();
    let mut weak = BTreeSet::new();
    let mut strong = BTreeSet::new();
    let complete =
        findings.values().all(|f| f.complete) && proc_search.values().all(|f| f.complete);
    for (query, found) in &findings {
        ctx.request.check()?;
        let mut candidates = Vec::new();
        for hit in &found.hits {
            ctx.request.check()?;
            if let Some(state) = snapshot.current(&hit.memory_id)? {
                for head in state
                    .heads
                    .iter()
                    .filter(|head| scope_matches(&head.applicability, &context))
                {
                    let excerpt = snapshot.streaming.excerpt(&head.what_to_remember, 400)?;
                    let mut paths = Vec::new();
                    if found.lexical_ids.contains(&hit.memory_id) {
                        paths.push(RetrievalPath {
                            via: vec!["lexical".into()],
                            clue: excerpt.chars().take(80).collect(),
                            source_memory_id: None,
                        });
                    }
                    if found.vector.iter().any(|h| h.memory_id == hit.memory_id) {
                        paths.push(RetrievalPath {
                            via: vec!["vector".into()],
                            clue: excerpt.chars().take(80).collect(),
                            source_memory_id: None,
                        });
                    }
                    if !paths.is_empty() {
                        candidates.push(MemoryCandidate {
                            memory_id: state.resolved_id.clone(),
                            excerpt,
                            applicability: head.applicability.clone(),
                            retrieval_paths: paths,
                        });
                    }
                }
            }
            let lex = found
                .lexical
                .iter()
                .find(|h| h.memory_id == hit.memory_id)
                .map_or(0.0, |h| h.score);
            let sem = found
                .vector
                .iter()
                .find(|h| h.memory_id == hit.memory_id)
                .map_or(0.0, |h| h.score);
            if lex < 0.90 && sem < 0.92 {
                weak.insert(hit.memory_id.clone());
            } else {
                strong.insert(hit.memory_id.clone());
            }
        }
        let mut diagnostics = Vec::new();
        if !complete {
            diagnostics.push(DomainError::new("semantic_channel_incomplete", "Semantic search did not establish complete coverage; exact and lexical results remain available."));
        }
        if found.matched > SEARCH_LIMIT as u64 || found.hits.len() >= SEARCH_LIMIT {
            diagnostics.push(DomainError::new(
                "candidate_coverage_incomplete",
                "The candidate retrieval ceiling was reached; matched is a lower bound.",
            ));
        }
        queries.insert(
            query.clone(),
            Discovery {
                candidates,
                full_ids: uncontested(&found.lexical, &found.vector)
                    .into_iter()
                    .collect(),
                diagnostics,
            },
        );
    }
    let search = IndexedSearch { queries };
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| DomainError::new("clock_failed", "Cannot format the current time."))?;
    let mut memory_request = request.clone();
    memory_request.project_hint = None;
    memory_request.selected_project_id = None;
    memory_request.procedure_ids = None;
    let mut assembled = crate::recall::recall_with_control(
        &snapshot,
        &search,
        &context,
        &memory_request,
        now,
        Some(ctx.request),
    )?;
    assembled
        .diagnostics
        .retain(|d| !d.message.starts_with("Learned procedure discovery"));
    completeness_diagnostics.extend(
        assembled
            .diagnostics
            .iter()
            .filter(|d| d.code.starts_with("semantic_channel_"))
            .cloned(),
    );
    let mut protected = BTreeSet::new();
    for id in request.memory_ids.as_deref().unwrap_or(&[]) {
        if let Some(state) = snapshot.current(id)? {
            protected.insert(state.resolved_id);
        }
    }
    for memory in &assembled.memories {
        for head in &memory.current_heads {
            for id in &head.required_memory_ids {
                if let Some(state) = snapshot.current(id)? {
                    protected.insert(state.resolved_id);
                }
            }
        }
    }
    if let (Some(project), Some(targets)) =
        (context.project_id.as_deref(), request.work_targets.as_ref())
    {
        for (id, reading) in exact.work_targets(project, targets)? {
            if reading == Reading::Required {
                if let Some(state) = snapshot.current(&id)? {
                    protected.insert(state.resolved_id);
                }
            }
        }
    }
    let mut result = to_value(assembled)?;
    if request.recall_for.is_some() {
        if let Some(memories) = result["memories"].as_array_mut() {
            for memory in memories {
                let Some(id) = memory["memory_id"].as_str() else {
                    continue;
                };
                if protected.contains(id) {
                    continue;
                }
                let Some(heads) = memory["current_heads"].as_array_mut() else {
                    continue;
                };
                if heads.len() != 1 || heads[0]["is_rule"] != true {
                    continue;
                }
                let applicability: Applicability =
                    serde_json::from_value(heads[0]["applicability"].clone()).map_err(|_| {
                        DomainError::new("source_corrupt", "Rule applicability is invalid.")
                    })?;
                if !scope_matches(&applicability, &context) {
                    continue;
                }
                if let Some(head) = heads[0].as_object_mut() {
                    head.retain(|key, _| {
                        matches!(
                            key.as_str(),
                            "what_to_remember" | "applicability" | "is_rule"
                        )
                    });
                }
            }
        }
    }
    if request.recall_for.is_some() && !complete {
        result["semantic_complete"] = json!(false);
    }
    if request.include_active_tasks == Some(true) {
        let mut active = Vec::new();
        let mut ordinary = Vec::new();
        for candidate in result["candidates"].as_array().cloned().unwrap_or_default() {
            ctx.request.check()?;
            let id = candidate["memory_id"].as_str().unwrap_or("");
            let mut task = false;
            if let Some(state) = snapshot.states.get(id) {
                for head in state.heads.iter().filter(|h| {
                    h.in_working_set == Some(true) && scope_matches(&h.applicability, &context)
                }) {
                    let [objective, current_position, resume_point] =
                        snapshot.streaming.task_sections(&head.what_to_remember)?;
                    active.push(json!({"memory_id":id,"objective":objective,"current_position":current_position,"resume_point":resume_point,"retrieval_paths":candidate["retrieval_paths"]}));
                    task = true;
                }
            }
            if !task {
                ordinary.push(candidate);
            }
        }
        active.extend(ordinary);
        result["candidates"] = json!(active);
    }
    let exact_ids: BTreeSet<_> = request
        .procedure_ids
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut output = Vec::new();
    let mut missing = Vec::new();
    for id in &exact_ids {
        validate_id(id)?;
        if let Some(unit) = snapshot
            .units
            .iter()
            .find(|u| u.entity_type == "learned_procedure" && &u.entity_id == id)
        {
            output.push(procedure_output(unit, true)?);
        } else {
            missing.push(id.clone());
        }
    }
    if let Some(id) = &procedures.2 {
        if !exact_ids.contains(id) {
            if let Some(unit) = snapshot
                .units
                .iter()
                .find(|u| u.entity_type == "learned_procedure" && &u.entity_id == id)
            {
                output.push(procedure_output(unit, true)?);
            }
        }
    }
    let required_procedure_count = output.len();
    if request.recall_for.is_some() {
        let ids: Vec<_> = procedures
            .1
            .iter()
            .filter(|id| !exact_ids.contains(*id) && Some(*id) != procedures.2.as_ref())
            .collect();
        let matched = ids.len();
        let limit = request.procedure_candidate_limit.unwrap_or(5) as usize;
        let mut shown = 0;
        let mut unreconciled = Vec::new();
        for id in ids.into_iter().take(limit) {
            if let Some(unit) = snapshot
                .units
                .iter()
                .find(|u| u.entity_type == "learned_procedure" && &u.entity_id == id)
            {
                let mut descriptors = Vec::new();
                for head in unit.heads() {
                    let applicability = scope_value(&head.metadata["applicability"])?;
                    if scope_matches(&applicability, &context) {
                        let descriptor = json!({"procedure_id":id,"name":head.metadata["name"],"use_when":head.metadata["use_when"],"applicability":applicability});
                        if !descriptors.contains(&descriptor) {
                            descriptors.push(descriptor);
                        }
                    }
                }
                descriptors.sort_by_key(Value::to_string);
                if !descriptors.is_empty() {
                    shown += 1;
                    if unit.heads().len() > 1 {
                        unreconciled.push(id.clone());
                    }
                    output.extend(descriptors);
                }
            }
        }
        result["candidate_counts"]["learned_procedures"] = json!({"matched":matched,"shown":shown});
        if !unreconciled.is_empty() {
            if result.get("diagnostics").is_none() {
                result["diagnostics"] = json!([]);
            }
            result["diagnostics"].as_array_mut().unwrap().push(json!({"code":"procedure_head_reconciliation_required","procedure_ids":unreconciled,"message":"These procedures have multiple current heads; inspect all heads before revision."}));
        }
    }
    result["learned_procedures"] = json!(output);
    restore_procedure_versions(&mut result["learned_procedures"], &snapshot.streaming);
    if !missing.is_empty() {
        result["code"] = json!("exact_selection_incomplete");
        if result.get("missing_ids").is_none() {
            result["missing_ids"] = json!({"memory_ids":[],"procedure_ids":[]});
        }
        result["missing_ids"]["procedure_ids"] = json!(missing);
    }
    weak.retain(|id| !strong.contains(id));
    let displayed: Vec<String> = result["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["memory_id"].as_str())
        .filter(|id| weak.contains(*id))
        .map(str::to_owned)
        .collect();
    let mut weak_procedures = BTreeSet::new();
    let mut strong_procedures = BTreeSet::new();
    for found in proc_search.values() {
        for hit in &found.hits {
            let Some(id) = hit.memory_id.strip_prefix("procedure/") else {
                continue;
            };
            let lex = found
                .lexical
                .iter()
                .find(|h| h.memory_id == hit.memory_id)
                .map_or(0.0, |h| h.score);
            let sem = found
                .vector
                .iter()
                .find(|h| h.memory_id == hit.memory_id)
                .map_or(0.0, |h| h.score);
            if lex < 0.90 && sem < 0.92 {
                weak_procedures.insert(id.to_owned());
            } else {
                strong_procedures.insert(id.to_owned());
            }
        }
    }
    weak_procedures.retain(|id| !strong_procedures.contains(id));
    let displayed_procedures: BTreeSet<_> = result["learned_procedures"]
        .as_array()
        .into_iter()
        .flatten()
        .skip(required_procedure_count)
        .filter_map(|v| v["procedure_id"].as_str())
        .filter(|id| weak_procedures.contains(*id))
        .map(str::to_owned)
        .collect();
    if !displayed.is_empty() || !displayed_procedures.is_empty() {
        if result.get("diagnostics").is_none() {
            result["diagnostics"] = json!([]);
        }
        result["diagnostics"].as_array_mut().unwrap().push(json!({"code":"weak_retrieval_match", "memory_ids":displayed, "procedure_ids":displayed_procedures, "message":"These retrieval matches are weak; this does not assess factual reliability."}));
    }
    let notices = ctx
        .control
        .pending_notices(&ctx.identity.binding_id, context.project_id.as_deref())?;
    if !notices.is_empty() {
        result["pending_batches"] = json!(notices);
        result["pending_work_instruction"] = json!("Retained proposals need attention. Inspect the listed pending batch before revising or continuing it.");
    }
    ctx.request.check()?;
    fit_response(
        ctx,
        &snapshot.streaming,
        result,
        required_procedure_count,
        outer,
    )
}

fn update_candidate_diagnostics(value: &mut Value, original: &Value, required: usize) {
    let memory_ids: BTreeSet<String> = value["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["memory_id"].as_str().map(str::to_owned))
        .collect();
    let procedure_ids: BTreeSet<String> = value["learned_procedures"]
        .as_array()
        .into_iter()
        .flatten()
        .skip(required)
        .filter_map(|c| c["procedure_id"].as_str().map(str::to_owned))
        .collect();
    if let Some(originals) = original["diagnostics"].as_array() {
        if value.get("diagnostics").is_none() {
            value["diagnostics"] = json!([]);
        }
        let diagnostics = value["diagnostics"].as_array_mut().unwrap();
        diagnostics.retain(|d| {
            d["code"] != "weak_retrieval_match"
                && d["code"] != "procedure_head_reconciliation_required"
        });
        for diagnostic in originals.iter().filter(|d| {
            d["code"] == "weak_retrieval_match"
                || d["code"] == "procedure_head_reconciliation_required"
        }) {
            let mut diagnostic = diagnostic.clone();
            if let Some(ids) = diagnostic
                .get_mut("memory_ids")
                .and_then(Value::as_array_mut)
            {
                ids.retain(|id| id.as_str().is_some_and(|id| memory_ids.contains(id)));
            }
            if let Some(ids) = diagnostic
                .get_mut("procedure_ids")
                .and_then(Value::as_array_mut)
            {
                ids.retain(|id| id.as_str().is_some_and(|id| procedure_ids.contains(id)));
            }
            if diagnostic["memory_ids"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty())
                || diagnostic["procedure_ids"]
                    .as_array()
                    .is_some_and(|ids| !ids.is_empty())
            {
                diagnostics.push(diagnostic);
            }
        }
    }
}
fn fit_response(
    ctx: &ReadCtx<'_>,
    streaming: &streaming::StreamedSelection,
    value: Value,
    required_procedure_count: usize,
    outer: Option<Value>,
) -> Result<Value> {
    let limit = ctx.identity.response_limit.min(256 * 1024 * 1024);
    let wrap = |partial: &Value| {
        if let Some(mut result) = outer.clone() {
            result["partial_recall"] = partial.clone();
            result
        } else {
            partial.clone()
        }
    };
    if streaming.serialized_size(&wrap(&value), ctx.request)? <= limit {
        return streaming.inline(&wrap(&value), ctx.request);
    }
    let config_file = ctx
        .identity
        .config_file
        .as_ref()
        .map(|path| json!(path))
        .unwrap_or_else(|| {
            json!("Use agentlaw config path to obtain the installation configuration file.")
        });
    let notice = json!({"applied_bytes": limit, "config_key":"response_limit_bytes", "config_file":config_file, "user_notice":"The result exceeds the configured response size."});
    let candidates = value["candidates"].as_array().cloned().unwrap_or_default();
    let procedures = value["learned_procedures"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let required = required_procedure_count.min(procedures.len());
    let mut base = value.clone();
    base["response_limit"] = notice.clone();
    base["candidates"] = json!([]);
    base["learned_procedures"] = json!(procedures[..required]);
    if base.get("candidate_counts").is_some() {
        base["candidate_counts"]["memories"]["shown"] = json!(0);
        base["candidate_counts"]["learned_procedures"]["shown"] = json!(0);
    }
    if let Some(object) = base.as_object_mut() {
        object.remove("task_instruction");
    }
    update_candidate_diagnostics(&mut base, &value, required);
    if streaming.serialized_size(&wrap(&base), ctx.request)? > limit {
        let mut complete = wrap(&value);
        complete["response_limit"] = notice;
        return streaming.finish_read(&complete, &ctx.identity.local, ctx.request);
    }
    let task_instruction = value.get("task_instruction").cloned();
    let mut memory_groups: Vec<Vec<Value>> = Vec::new();
    for candidate in candidates {
        if memory_groups
            .last()
            .and_then(|group| group.first())
            .is_some_and(|previous| previous["memory_id"] == candidate["memory_id"])
        {
            memory_groups.last_mut().unwrap().push(candidate);
        } else {
            memory_groups.push(vec![candidate]);
        }
    }
    let mut shown_memories = 0;
    for group in memory_groups {
        ctx.request.check()?;
        let mut trial = base.clone();
        let is_task = group
            .iter()
            .any(|candidate| candidate.get("objective").is_some());
        trial["candidates"].as_array_mut().unwrap().extend(group);
        trial["candidate_counts"]["memories"]["shown"] = json!(shown_memories + 1);
        if is_task {
            if let Some(instruction) = &task_instruction {
                trial["task_instruction"] = instruction.clone();
            }
        }
        update_candidate_diagnostics(&mut trial, &value, required);
        if streaming.serialized_size(&wrap(&trial), ctx.request)? <= limit {
            base = trial;
            shown_memories += 1;
        }
    }
    let mut groups: Vec<Vec<Value>> = Vec::new();
    for candidate in procedures.into_iter().skip(required) {
        if groups
            .last()
            .and_then(|group| group.first())
            .is_some_and(|previous| previous["procedure_id"] == candidate["procedure_id"])
        {
            groups.last_mut().unwrap().push(candidate);
        } else {
            groups.push(vec![candidate]);
        }
    }
    let mut shown_procedures = 0;
    for group in groups {
        ctx.request.check()?;
        let mut trial = base.clone();
        trial["learned_procedures"]
            .as_array_mut()
            .unwrap()
            .extend(group);
        trial["candidate_counts"]["learned_procedures"]["shown"] = json!(shown_procedures + 1);
        update_candidate_diagnostics(&mut trial, &value, required);
        if streaming.serialized_size(&wrap(&trial), ctx.request)? <= limit {
            base = trial;
            shown_procedures += 1;
        }
    }
    streaming.inline(&wrap(&base), ctx.request)
}

fn restore_procedure_versions(value: &mut Value, streaming: &streaming::StreamedSelection) {
    match value {
        Value::Array(items) => {
            for item in items {
                restore_procedure_versions(item, streaming);
            }
        }
        Value::Object(fields) => {
            if let Some(reference) = fields
                .get_mut("procedure_ref")
                .and_then(Value::as_object_mut)
            {
                if let Some(version) = reference.get("observed_version").and_then(Value::as_str) {
                    reference.insert(
                        "observed_version".into(),
                        json!(streaming.actual_version(version.to_owned())),
                    );
                }
            }
            if let Some(heads) = fields.get_mut("current_heads") {
                restore_procedure_versions(heads, streaming);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
