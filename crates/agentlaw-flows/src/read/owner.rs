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
        let catalog = source(ctx.source.list_catalog())?;
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
    let query = prefix_query(ctx, query.to_owned(), query.to_owned())?;
    let channels = channels(search_one(ctx, exact, &query, &scopes)?, query.truncated);
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
impl AcquiredClosure {
    fn contains(&self, id: &str) -> bool {
        match self {
            Self::Owned(units) => units.iter().any(|unit| unit.current.entity_id == id),
            Self::Spooled(units) => units
                .iter()
                .any(|unit| unit.current.state["memory_id"] == id),
        }
    }
}
#[derive(Clone)]
struct PreparedQuery {
    text: String,
    combined: String,
    vectors: Option<Vec<QueryVector>>,
    truncated: bool,
}
struct Found {
    hits: Vec<Hit>,
    lexical: Vec<Hit>,
    vector: Vec<Hit>,
    lexical_ids: BTreeSet<String>,
    complete: bool,
    matched: u64,
}

fn query_texts(ctx: &ReadCtx<'_>, request: &RecallRequest) -> RecallResult<Vec<PreparedQuery>> {
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
        check_recall(ctx.request)?;
        let combined = match &request.search_terms {
            Some(terms) => format!("{text} {}", terms.join(" ")),
            None => text.clone(),
        };
        prepared.push(prefix_query(ctx, text, combined)?);
    }
    Ok(prepared)
}

fn prefix_query(ctx: &ReadCtx<'_>, text: String, combined: String) -> RecallResult<PreparedQuery> {
    let (vectors, truncated) = match ctx.semantic.embed_prefix(&combined, ctx.request) {
        Ok(result) => (Some(vec![result.vector]), result.truncated),
        Err(SemanticError::Cancelled) => {
            check_recall(ctx.request)?;
            return Err(RecallFailure::Cancelled(DomainError::new(
                "cancelled",
                "Query embedding was cancelled.",
            )));
        }
        Err(_) => (None, false),
    };
    Ok(PreparedQuery {
        text,
        combined,
        vectors,
        truncated,
    })
}

fn truncated_diagnostic() -> DomainError {
    DomainError::new(
        "semantic_query_truncated",
        "Semantic search used the beginning of the query.",
    )
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
) -> RecallResult<Found> {
    check_recall(ctx.request)?;
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
    let (vector, complete) = match &query.vectors {
        None => (Vec::new(), false),
        Some(vectors) => {
            let mut best = BTreeMap::<String, Hit>::new();
            let mut complete = true;
            for vector in vectors {
                let (hits, covered) = search_vector(ctx, exact, &query.combined, scopes, vector)?;
                complete &= covered;
                for hit in hits {
                    let current = best
                        .entry(hit.memory_id.clone())
                        .or_insert_with(|| hit.clone());
                    if hit.score.total_cmp(&current.score).is_gt() {
                        *current = hit;
                    }
                }
            }
            let mut hits: Vec<_> = best.into_values().collect();
            hits.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then(a.memory_id.cmp(&b.memory_id))
            });
            (hits, complete)
        }
    };
    let lexical_ids = lexical.iter().map(|hit| hit.memory_id.clone()).collect();
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

fn search_vector(
    ctx: &ReadCtx<'_>,
    exact: &ExactSnapshot,
    text: &str,
    scopes: &[String],
    vector: &QueryVector,
) -> RecallResult<(Vec<Hit>, bool)> {
    let position = exact.position().clone();
    let (mut vector, complete) = match ctx.semantic.search_available(
        SemanticQuery {
            binding: &ctx.identity.binding_id,
            scopes,
            text,
            vector: Some(vector),
            limit: SEARCH_LIMIT,
            source: &position,
        },
        ctx.request,
    ) {
        Ok(result) => {
            let complete =
                matches!(result.coverage, SemanticCoverage::CompleteAt(ref p) if p == &position);
            (result.hits, complete)
        }
        Err(SemanticError::Cancelled) => {
            check_recall(ctx.request)?;
            return Err(RecallFailure::Cancelled(DomainError::new(
                "cancelled",
                "Semantic search was cancelled.",
            )));
        }
        Err(_) => (Vec::new(), false),
    };
    check_recall(ctx.request)?;
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
    Ok((vector, complete))
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
    let vectors = match planned {
        Some((plan, index, reuse)) => {
            match ctx.semantic.embed_planned(plan, index, reuse, ctx.request) {
                Ok(vectors) => plan.record(index).and_then(|record| {
                    let pieces = record.pieces();
                    if pieces.len() != vectors.len() {
                        return None;
                    }
                    let mut seen = std::collections::HashSet::new();
                    Some(
                        pieces
                            .zip(vectors)
                            .filter_map(|((_, digest), vector)| {
                                seen.insert(*digest).then_some(vector)
                            })
                            .collect(),
                    )
                }),
                Err(SemanticError::Cancelled) => {
                    ctx.request.check()?;
                    return Err(DomainError::new(
                        "cancelled",
                        "Piece embedding was cancelled.",
                    ));
                }
                Err(_) => None,
            }
        }
        None => None,
    };
    let query = PreparedQuery {
        text: text.to_owned(),
        combined: text.to_owned(),
        vectors,
        truncated: false,
    };
    let mut found = search_one(ctx, exact, &query, scopes)?;
    if !found.complete {
        found.vector.clear();
    }
    Ok(channels(found, false))
}

fn channels(found: Found, truncated: bool) -> Channels {
    let mut diagnostics = Vec::new();
    if !found.complete {
        diagnostics.push(DomainError::new(
            "semantic_channel_incomplete",
            "Semantic retrieval did not establish a complete view.",
        ));
    }
    if truncated {
        diagnostics.push(truncated_diagnostic());
    }
    Channels {
        hits: found.hits,
        lexical: found.lexical,
        vector: found.vector,
        complete: found.complete,
        diagnostics,
    }
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
    let catalog = source(ctx.source.list_catalog_for_discovery())?;
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
    prepare_queries(ctx, request).map_err(Into::into)
}

fn prepare_queries(ctx: &ReadCtx<'_>, request: &RecallRequest) -> RecallResult<PreparedRecall> {
    Ok(PreparedRecall {
        queries: query_texts(ctx, request)?,
    })
}

fn resolve_recall_ids(
    guard: &agentlaw_storage::published::SourceReadGuard<'_>,
    request: &RecallRequest,
) -> Result<RecallRequest> {
    let mut resolved = request.clone();
    for (field, kind, ids) in [
        (
            "memory_ids",
            agentlaw_storage::EntityKind::Memory,
            &mut resolved.memory_ids,
        ),
        (
            "procedure_ids",
            agentlaw_storage::EntityKind::Procedure,
            &mut resolved.procedure_ids,
        ),
    ] {
        let mut seen = BTreeSet::new();
        for (index, id) in ids.iter_mut().flatten().enumerate() {
            let path = format!("/{field}/{index}");
            let canonical = select_source_id(guard, kind, id, &path, recall_source_error)?;
            if !seen.insert(canonical.clone()) {
                return Err(validation::identity_input_error(
                    "invalid_input",
                    &path,
                    &json!(id),
                ));
            }
            *id = canonical;
        }
    }
    Ok(resolved)
}

fn has_identity_prefix(request: &RecallRequest) -> bool {
    request
        .memory_ids
        .iter()
        .flatten()
        .chain(request.procedure_ids.iter().flatten())
        .any(|id| validation::is_identity_prefix(id))
}

pub fn recall(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
) -> Result<Value> {
    execute_recall(
        ctx,
        request,
        None,
        DeltaBudget::default(),
        None,
        || Ok(exact),
        |_| {},
    )
    .map(|(value, _)| value)
}

/// Recall against the project identity fixed by a completed connection.
#[doc(hidden)]
pub fn recall_for_project(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
    project_id: &str,
) -> Result<Value> {
    execute_recall(
        ctx,
        request,
        Some(project_id),
        DeltaBudget::default(),
        None,
        || Ok(exact),
        |_| {},
    )
    .map(|(value, _)| value)
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
    execute_recall(
        ctx,
        request,
        project_id,
        budget,
        Some(prepared),
        || Ok(exact),
        |_| {},
    )
    .map(|(value, _)| value)
}

#[doc(hidden)]
pub fn recall_prepared_with_diagnostics(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
    project_id: Option<&str>,
    budget: DeltaBudget,
    prepared: &PreparedRecall,
) -> Result<(Value, Vec<DomainError>)> {
    execute_recall(
        ctx,
        request,
        project_id,
        budget,
        Some(prepared),
        || Ok(exact),
        |_| {},
    )
}

#[doc(hidden)]
pub fn recall_with_snapshot(
    ctx: &ReadCtx<'_>,
    request: &RecallRequest,
    project_id: Option<&str>,
    budget: DeltaBudget,
    acquire: impl FnOnce() -> Result<ExactSnapshot>,
    observe_failure: impl FnOnce(&DomainError),
) -> Result<(Value, Vec<DomainError>)> {
    execute_recall(
        ctx,
        request,
        project_id,
        budget,
        None,
        acquire,
        observe_failure,
    )
}

enum RecallScope {
    Ready(RequestContext),
    Unbound(RequestContext, Value),
    Selection(Value),
}

fn resolve_recall_scope(
    ctx: &ReadCtx<'_>,
    request: &RecallRequest,
    fixed_project: Option<&str>,
) -> Result<RecallScope> {
    let machine = request
        .machine_id
        .as_ref()
        .unwrap_or(&ctx.identity.machine_id);
    validate_id(machine)?;
    let mut context = RequestContext {
        store_binding_id: ctx.identity.binding_id.clone(),
        user_id: ctx.identity.user_id.clone(),
        machine_id: machine.clone(),
        project_id: fixed_project.map(str::to_owned),
        connection_version: String::new(),
    };
    if let Some(id) = fixed_project {
        validate_id(id)?;
    } else if let Some(path) = &request.project_path {
        absolute_project_path(path)?;
        if let Some(association) = ctx
            .control
            .exact_association(&ctx.identity.binding_id, path)?
        {
            context.project_id = Some(association.project_id);
        } else {
            let candidates = catalog_candidates(ctx, None)?;
            let result = json!({"status":"needs_user_input", "code":"project_connection_required", "message":"Choose a project memory for this folder.", "project_path":path, "candidates":candidates, "turn_instruction":"Project rules, active Tasks and work targets have not been checked. Read partial_recall or its error; select or create a project explicitly, then repeat the original request."});
            return Ok(RecallScope::Unbound(context, result));
        }
    }
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
            return Ok(RecallScope::Selection(
                json!({"code":"project_selection_required","project_candidates":options,"turn_instruction":"Repeat with the same project hint and selected project ID."}),
            ));
        }
    }
    Ok(RecallScope::Ready(context))
}

fn execute_recall<S: std::borrow::BorrowMut<ExactSnapshot>>(
    ctx: &ReadCtx<'_>,
    request: &RecallRequest,
    fixed_project: Option<&str>,
    budget: DeltaBudget,
    prepared: Option<&PreparedRecall>,
    acquire: impl FnOnce() -> Result<S>,
    observe_failure: impl FnOnce(&DomainError),
) -> Result<(Value, Vec<DomainError>)> {
    ctx.request.check()?;
    let scope = resolve_recall_scope(ctx, request, fixed_project)?;
    if !matches!(scope, RecallScope::Ready(_)) && has_identity_prefix(request) {
        let guard = ctx
            .source
            .read_snapshot_for_recall()
            .map_err(recall_source_error)?;
        resolve_recall_ids(&guard, request)?;
    }
    let (context, outer) = match scope {
        RecallScope::Ready(context) => (context, None),
        RecallScope::Unbound(context, value) => {
            if request.recall_for.is_none() {
                return Ok((value, vec![]));
            }
            (context, Some(value))
        }
        RecallScope::Selection(value) => return Ok((value, vec![])),
    };
    let partial;
    let request = if outer.is_some() {
        partial = RecallRequest {
            project_path: None,
            project_hint: None,
            selected_project_id: None,
            memory_ids: None,
            procedure_ids: None,
            restore_context: None,
            include_active_tasks: None,
            work_targets: None,
            ..request.clone()
        };
        &partial
    } else {
        request
    };
    let mut diagnostics = Vec::new();
    let result = (|| -> RecallResult<Value> {
        let owned;
        let prepared = match prepared {
            Some(prepared) => prepared,
            None => {
                owned = prepare_queries(ctx, request)?;
                &owned
            }
        };
        let queries = if outer.is_some() {
            &prepared.queries[..prepared.queries.len().min(1)]
        } else {
            &prepared.queries
        };
        check_recall(ctx.request)?;
        let exact = acquire();
        check_recall(ctx.request)?;
        recall_scoped(
            ctx,
            exact?.borrow_mut(),
            request,
            context,
            fixed_project,
            budget,
            queries,
            outer.as_ref(),
            &mut diagnostics,
        )
    })();
    match result {
        Ok(value) => Ok((value, diagnostics)),
        Err(RecallFailure::Recoverable(error)) if outer.is_some() => {
            observe_failure(&error);
            let mut value = outer.unwrap();
            value["partial_recall_error"] = to_value(error)?;
            Ok((value, diagnostics))
        }
        Err(failure) => {
            let error = failure.into();
            observe_failure(&error);
            Err(error)
        }
    }
}

fn recall_scoped(
    ctx: &ReadCtx<'_>,
    exact: &mut ExactSnapshot,
    request: &RecallRequest,
    context: RequestContext,
    fixed_project: Option<&str>,
    budget: DeltaBudget,
    prepared: &[PreparedQuery],
    outer: Option<&Value>,
    completeness_diagnostics: &mut Vec<DomainError>,
) -> RecallResult<Value> {
    check_recall(ctx.request)?;
    let submitted_request = request;
    let has_prefix = has_identity_prefix(submitted_request);
    if (request.include_active_tasks == Some(true) || request.work_targets.is_some())
        && context.project_id.is_none()
    {
        return Err(DomainError::new(
            "project_connection_required",
            "Task and work-target lookup require a resolved project.",
        )
        .into());
    }

    let scopes = crate::exact::allowed_scopes(&context);
    let scope_list: Vec<String> = scopes.into_iter().collect();
    let procedure_scopes: Vec<String> = scope_list
        .iter()
        .map(|s| format!("procedure:{s}"))
        .collect();
    let (
        mut snapshot,
        findings,
        procedures,
        proc_search,
        active_tasks,
        rules,
        mut full_ids,
        handoff,
        resolved_request,
    ) = loop {
        check_recall(ctx.request)?;
        let position = exact.position().clone();
        let selected_request = {
            let guard = ctx
                .source
                .read_snapshot_for_recall()
                .map_err(recall_source_error)?;
            if guard.position() != &position {
                exact.advance(&guard, budget)?;
                continue;
            }
            resolve_recall_ids(&guard, request)?
        };
        let request = &selected_request;
        let mut findings = BTreeMap::new();
        let mut procedure_findings = BTreeMap::new();
        let mut selected: BTreeSet<String> = request
            .memory_ids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let active_tasks = if request.include_active_tasks == Some(true) {
            exact.ids_matching(&context, false, true)?
        } else {
            Vec::new()
        };
        let mut previews = BTreeSet::new();
        for query in prepared {
            let found = search_one(ctx, exact, query, &scope_list)?;
            let winner = uncontested(&found.lexical, &found.vector)
                .filter(|id| !matches!(active_tasks.as_slice(), [task] if task == id));
            previews.extend(found.hits.iter().map(|h| h.memory_id.clone()));
            findings.insert(query.text.clone(), (found, winner));
            let found = search_one(ctx, exact, query, &procedure_scopes)?;
            procedure_findings.insert(query.text.clone(), found);
        }
        let rules = if request.recall_for.is_some() {
            exact.ids_matching(&context, true, false)?
        } else {
            Vec::new()
        };
        if request.recall_for.is_some() {
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
        let full_ids = selected.clone();
        selected.extend(findings.values().filter_map(|(_, winner)| winner.clone()));
        selected.extend(rules.iter().cloned());
        // Compact Task choices still need their original handoff sections.
        if active_tasks.len() > 1 {
            selected.extend(active_tasks.iter().cloned());
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
        let guard = ctx
            .source
            .read_snapshot_for_recall()
            .map_err(recall_source_error)?;
        if let Some(project_id) = fixed_project {
            match guard.read_catalog(project_id) {
                Ok(_) => {}
                Err(agentlaw_storage::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Err(DomainError::new(
                        "project_not_found",
                        "The fixed project no longer exists in the catalog.",
                    )
                    .into());
                }
                Err(agentlaw_storage::Error::NotFound(_)) => {
                    return Err(DomainError::new(
                        "project_not_found",
                        "The fixed project no longer exists in the catalog.",
                    )
                    .into())
                }
                Err(error) => return Err(recall_source_error(error).into()),
            }
        }
        exact.advance(&guard, budget)?;
        if exact.position() != &position {
            drop(guard);
            continue;
        }
        if has_prefix {
            let selected = resolve_recall_ids(&guard, submitted_request)?;
            if selected.memory_ids != request.memory_ids
                || selected.procedure_ids != request.procedure_ids
            {
                return Err(DomainError::new(
                    "source_changed",
                    "A prefix matched a different identity while acquiring recall material.",
                )
                .with_next_action("Repeat the original recall against the current source.")
                .into());
            }
        }
        let selected_ids: Vec<_> = selected.iter().cloned().collect();
        let (resolved, missing) = match guard.read_closure(&selected_ids) {
            Ok((_, units, missing)) => (AcquiredClosure::Owned(units), missing),
            Err(agentlaw_storage::Error::Capacity) => {
                let (_, units, missing) = guard
                    .acquire_closure(&selected_ids)
                    .map_err(recall_source_error)?;
                (AcquiredClosure::Spooled(units), missing)
            }
            Err(error) => return Err(recall_source_error(error).into()),
        };
        // A sole Task outside the required closure is acquired at the same
        // source position without acquiring its required references.
        let handoff = match active_tasks.as_slice() {
            [id] if !resolved.contains(id) && !selected.contains(id) && !missing.contains(id) => {
                match guard.acquire_current(id) {
                    Ok(current) => Some(current),
                    Err(agentlaw_storage::Error::NotFound(_)) => None,
                    Err(error) => return Err(recall_source_error(error).into()),
                }
            }
            _ => None,
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
            check_recall(ctx.request)?;
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
                Err(error) => return Err(recall_source_error(error).into()),
            }
        }
        let mut procedure_units = Vec::new();
        let mut procedure_spools = Vec::new();
        for id in &procedure_ids {
            check_recall(ctx.request)?;
            match guard.read_procedure(id) {
                Ok(unit) => procedure_units.push(unit),
                Err(agentlaw_storage::Error::NotFound(_)) => {}
                Err(agentlaw_storage::Error::Capacity) => {
                    procedure_spools.push(guard.acquire_procedure(id).map_err(recall_source_error)?)
                }
                Err(error) => return Err(recall_source_error(error).into()),
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
                    check_recall(ctx.request)?;
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
                    check_recall(ctx.request)?;
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
            check_recall(ctx.request)?;
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
                    agentlaw_storage::version(&unit, head).map_err(recall_source_error)?,
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
            check_recall(ctx.request)?;
            if states.contains_key(&id) || missing.contains(&id) {
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
            active_tasks,
            rules,
            full_ids,
            handoff,
            selected_request,
        );
    };
    let request = &resolved_request;
    let mut queries = BTreeMap::new();
    let mut weak = BTreeSet::new();
    let mut strong = BTreeSet::new();
    let complete = findings.values().all(|(found, _)| found.complete)
        && proc_search.values().all(|f| f.complete);
    for (query, (found, winner)) in &findings {
        check_recall(ctx.request)?;
        let mut candidates = Vec::new();
        for hit in &found.hits {
            check_recall(ctx.request)?;
            if let Some(state) = snapshot.current(&hit.memory_id)? {
                for head in state
                    .heads
                    .iter()
                    .filter(|head| scope_matches(&head.applicability, &context))
                {
                    let excerpt = snapshot
                        .streaming
                        .recall_excerpt(&head.what_to_remember, 400)?;
                    let mut paths = Vec::new();
                    if found.lexical_ids.contains(&hit.memory_id) {
                        paths.push(RetrievalPath {
                            via: vec!["lexical".into()],
                            clue: excerpt.chars().take(80).collect(),
                            source_memory_ids: None,
                        });
                    }
                    if found.vector.iter().any(|h| h.memory_id == hit.memory_id) {
                        paths.push(RetrievalPath {
                            via: vec!["vector".into()],
                            clue: excerpt.chars().take(80).collect(),
                            source_memory_ids: None,
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
        if prepared
            .iter()
            .any(|input| input.text == *query && input.truncated)
        {
            diagnostics.push(truncated_diagnostic());
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
                full_ids: winner.iter().cloned().collect(),
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
    let assembled = crate::recall::recall_with_control(
        &snapshot,
        &search,
        &context,
        &memory_request,
        now,
        Some(ctx.request),
        Some(&active_tasks),
    );
    check_recall(ctx.request)?;
    let (mut assembled, handoff_id) = assembled?;
    if let Some(id) = handoff_id {
        let sections = match handoff {
            Some(current) => {
                match acquired_task_sections(&mut snapshot.streaming, current, ctx.request)? {
                    TaskSectionParse::Complete(sections) => Ok(sections),
                    TaskSectionParse::InvalidHeadings => {
                        Err(streaming::invalid_spooled_task_headings())
                    }
                }
            }
            None => Err(crate::recall::task_source_unavailable()),
        };
        crate::recall::deliver_task_handoff(&mut assembled, id, sections);
    }
    let explicit = request.memory_ids.as_deref().unwrap_or(&[]);
    for id in &snapshot.publication_basis.absent {
        if explicit.contains(id)
            || assembled
                .undelivered_required
                .iter()
                .any(|item| &item.memory_id == id)
        {
            continue;
        }
        // An unavailable exact rule or Task may never have reached inventory.
        if !full_ids.contains(id) && !rules.contains(id) {
            continue;
        }
        assembled.undelivered_required.push(UndeliveredRequired {
            memory_id: id.clone(),
            reason: "An exact selection could not be confirmed in the current source.".into(),
        });
    }
    assembled
        .diagnostics
        .retain(|d| !d.message.starts_with("Learned procedure discovery"));
    let mut reported_truncation = false;
    assembled.diagnostics.retain(|diagnostic| {
        if diagnostic.code != "semantic_query_truncated" {
            return true;
        }
        !std::mem::replace(&mut reported_truncation, true)
    });
    completeness_diagnostics.extend(
        assembled
            .diagnostics
            .iter()
            .filter(|d| d.code.starts_with("semantic_channel_"))
            .cloned(),
    );
    for memory in &assembled.memories {
        for head in &memory.current_heads {
            full_ids.extend(head.required_memory_ids.iter().cloned());
        }
    }
    if let [id] = active_tasks.as_slice() {
        if rules.contains(id) {
            full_ids.insert(id.clone());
        }
    }
    let mut protected = BTreeSet::new();
    for id in full_ids {
        if let Some(state) = snapshot.current(&id)? {
            protected.insert(state.resolved_id);
        }
    }
    let mut rules: Vec<(Applicability, Vec<Value>)> = Vec::new();
    if request.recall_for.is_some() {
        assembled.memories.retain(|memory| {
            let [head] = memory.current_heads.as_slice() else {
                return true;
            };
            if protected.contains(&memory.memory_id)
                || head.is_rule != Some(true)
                || !scope_matches(&head.applicability, &context)
            {
                return true;
            }
            let item =
                json!({"memory_id":memory.memory_id,"what_to_remember":head.what_to_remember});
            if let Some((_, items)) = rules
                .iter_mut()
                .find(|(scope, _)| scope == &head.applicability)
            {
                items.push(item);
            } else {
                rules.push((head.applicability.clone(), vec![item]));
            }
            false
        });
    }
    crate::recall::finalize_recall_guidance(&mut assembled);
    let delivered_tasks: BTreeSet<_> = assembled
        .memories
        .iter()
        .filter(|memory| {
            memory
                .current_heads
                .iter()
                .any(|head| head.in_working_set.is_some())
        })
        .map(|memory| memory.memory_id.clone())
        .chain(
            assembled
                .active_task_handoff
                .iter()
                .map(|task| task.memory_id.clone()),
        )
        .collect();
    let mut result = to_value(assembled)?;
    if !rules.is_empty() {
        result["rules"] = json!(rules
            .into_iter()
            .map(|(applicability, items)| { json!({"applicability":applicability,"items":items}) })
            .collect::<Vec<_>>());
    }
    if request.recall_for.is_some() && !complete {
        result["semantic_complete"] = json!(false);
    }
    if request.include_active_tasks == Some(true) {
        let mut active = Vec::new();
        let mut ordinary = Vec::new();
        for candidate in result["candidates"].as_array().cloned().unwrap_or_default() {
            check_recall(ctx.request)?;
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
    check_recall(ctx.request)?;
    fit_response(
        ctx,
        &snapshot.streaming,
        result,
        required_procedure_count,
        outer,
        &delivered_tasks,
    )
}

fn acquired_task_sections(
    streaming: &mut streaming::StreamedSelection,
    current: agentlaw_storage::acquire::AcquiredCurrent,
    request: &RequestControl,
) -> RecallResult<TaskSectionParse<Vec<[String; 3]>>> {
    let heads = current.state["current_heads"]
        .as_array()
        .ok_or_else(crate::recall::task_source_unavailable)?;
    let mut sections = Vec::with_capacity(heads.len());
    let mut invalid_headings = false;
    for head in heads {
        check_recall(request)?;
        let body = head["change_id"]
            .as_str()
            .and_then(|id| current.bodies.get(id))
            .ok_or_else(crate::recall::task_source_unavailable)?;
        // Continue after a heading mismatch so a later unreadable head still fails the recall.
        match streaming.parse_spooled_task_sections(body, Some(request))? {
            TaskSectionParse::Complete(parts) => sections.push(parts),
            TaskSectionParse::InvalidHeadings => invalid_headings = true,
        }
    }
    if invalid_headings {
        Ok(TaskSectionParse::InvalidHeadings)
    } else {
        Ok(TaskSectionParse::Complete(sections))
    }
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

fn recall_file_error(error: DomainError, delivered_tasks: &BTreeSet<String>) -> DomainError {
    let next_action = "Check that the response-artifact directory exists or can be created, is writable, and has available space. Then repeat the original recall.";
    let error = error.with_next_action(next_action);
    if delivered_tasks.is_empty() {
        return error;
    }
    let details: Vec<_> = delivered_tasks
        .iter()
        .map(|id| {
            json!({
                "memory_id": id,
                "reason": error.message,
                "next_action": next_action
            })
        })
        .collect();
    error.with_details(json!(details))
}

fn fit_response(
    ctx: &ReadCtx<'_>,
    streaming: &streaming::StreamedSelection,
    value: Value,
    required_procedure_count: usize,
    outer: Option<&Value>,
    delivered_tasks: &BTreeSet<String>,
) -> RecallResult<Value> {
    let limit = ctx.identity.response_limit.min(256 * 1024 * 1024);
    let wrap = |partial: &Value| {
        if let Some(mut result) = outer.cloned() {
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
    let candidates = value["candidates"].as_array().cloned().unwrap_or_default();
    let procedures = value["learned_procedures"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let required = required_procedure_count.min(procedures.len());
    let user_notice = if !candidates.is_empty() || procedures.len() > required {
        "The result exceeds the configured response size. Refine the search or change the displayed candidate count with memory_candidate_limit or procedure_candidate_limit."
    } else {
        "The result exceeds the configured response size."
    };
    let notice = json!({"applied_bytes":limit,"config_key":"response_limit_bytes","config_file":config_file,"user_notice":user_notice});
    let mut base = value.clone();
    base["response_limit"] = notice.clone();
    base["candidates"] = json!([]);
    base["learned_procedures"] = json!(procedures[..required]);
    if base.get("candidate_counts").is_some() {
        base["candidate_counts"]["memories"]["shown"] = json!(0);
        base["candidate_counts"]["learned_procedures"]["shown"] = json!(0);
    }
    update_candidate_diagnostics(&mut base, &value, required);
    if streaming.serialized_size(&wrap(&base), ctx.request)? > limit {
        let mut complete = wrap(&value);
        complete["response_limit"] = notice;
        return streaming
            .finish_recall(&complete, &ctx.identity.local, ctx.request)
            .map_err(|failure| match failure {
                RecallFailure::DeliveryFailed(error) => {
                    RecallFailure::DeliveryFailed(recall_file_error(error, delivered_tasks))
                }
                other => other,
            });
    }
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
        check_recall(ctx.request)?;
        let mut trial = base.clone();
        trial["candidates"].as_array_mut().unwrap().extend(group);
        trial["candidate_counts"]["memories"]["shown"] = json!(shown_memories + 1);
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
        check_recall(ctx.request)?;
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
