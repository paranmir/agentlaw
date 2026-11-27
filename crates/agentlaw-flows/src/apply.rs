//! Publishing retained work and recording its results.
use crate::control::{record_prefix_outcome, PrefixTerminalOutcome};
use crate::prepare::{ConnectPlanKind, PendingExpectation, Prepared};
use crate::store::*;
use crate::*;
use agentlaw_storage::UnitState;
use rusqlite::params;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

#[cfg(test)]
mod tests;
pub enum Applied {
    Response(Value),
    Connected {
        response: ConnectResponse,
        follow_up: Option<FollowUp>,
    },
}
pub struct FollowUp {
    pub restore: bool,
    pub recall_for: Option<String>,
}

/// Applies a prepared decision while the caller owns the store mutation lane.
pub fn apply(ctx: &mut LaneCtx<'_>, prepared: Prepared) -> Result<Applied> {
    ctx.request.check()?;
    match prepared {
        Prepared::WithPrefixes(plan, prefixes) => apply_with_prefixes(ctx, *plan, prefixes),
        Prepared::Replay(value) => Ok(Applied::Response(value)),
        Prepared::Remember(plan) => apply_remember(ctx, plan, None),
        Prepared::Discard(plan) => {
            ensure_subject_idle(
                &ctx.control.connection,
                &ctx.identity.binding_id,
                &plan.reference.pending_batch_id,
            )?;
            let (retained, status, _) = crate::control::load_pending(
                &ctx.control.connection,
                &ctx.identity.binding_id,
                &plan.reference,
            )?;
            if retained.batch.reference != plan.reference {
                return Err(DomainError::new(
                    "stale_pending_ref",
                    "Pending state changed.",
                ));
            }
            if status != "pending" || retained.operation_id.is_some() {
                return Err(DomainError::new(
                    "publication_in_progress",
                    "A decided or completed publication cannot be discarded as pending.",
                ));
            }
            let changed = sql(ctx.control.connection.execute(
                "UPDATE pending SET status='discarded',discard_reason=?1 WHERE id=?2 AND version=?3 AND status='pending'",
                params![plan.reason, plan.reference.pending_batch_id, plan.reference.observed_version],
            ))?;
            if changed != 1 {
                return Err(DomainError::new(
                    "stale_pending_ref",
                    "Pending state changed.",
                ));
            }
            Ok(Applied::Response(
                json!({"status":"discarded","pending_batch_ref":plan.reference}),
            ))
        }
        Prepared::Authoring(plan) => {
            sql(ctx.control.connection.execute(
                "INSERT INTO authoring(token,binding,basis) VALUES(?1,?2,?3)",
                params![plan.token.to_string(), ctx.identity.binding_id, plan.basis],
            ))?;
            Ok(Applied::Response(plan.response))
        }
        Prepared::Procedure(plan) => {
            ensure_subject_idle(
                &ctx.control.connection,
                &ctx.identity.binding_id,
                &plan.token,
            )?;
            let tx = sql(ctx.control.connection.transaction())?;
            let changed = sql(tx.execute(
                "UPDATE authoring SET used=1 WHERE token=?1 AND binding=?2 AND used=0",
                params![plan.token, ctx.identity.binding_id],
            ))?;
            if changed != 1 {
                return Err(DomainError::new(
                    "stale_authoring_ref",
                    "Authoring context was used concurrently; prepare again.",
                ));
            }
            sql(tx.execute("INSERT INTO authoring_executions(token,binding,operation,generation,mutation,submission,status) VALUES(?1,?2,?3,?4,?5,?6,'pending')", params![plan.token, ctx.identity.binding_id, plan.operation_id.to_string(), plan.generation.to_string(), encode(&plan.mutation)?, plan.submission]))?;
            sql(tx.commit())?;
            let result = publish_procedure(
                ctx,
                &plan.token,
                &plan.operation_id.to_string(),
                plan.generation,
                plan.mutation,
                &plan.submission,
            )?;
            Ok(Applied::Response(result))
        }
        Prepared::Connect(plan) => {
            let response = match plan.kind {
                ConnectPlanKind::Discovered(response) => response,
                ConnectPlanKind::Select { path, project_id } => connected(ctx, &path, &project_id)?,
                ConnectPlanKind::Create { path, saga } => {
                    if saga.path != path {
                        return Err(DomainError::new(
                            "invalid_input",
                            "Project saga path does not match.",
                        ));
                    }
                    crate::context::absolute_project_path(&path)?;
                    let associated: Option<String> = sql(ctx
                        .control
                        .connection
                        .query_row(
                            "SELECT project FROM associations WHERE binding=?1 AND path=?2",
                            params![ctx.identity.binding_id, path],
                            |row| row.get(0),
                        )
                        .optional())?;
                    if let Some(id) = associated {
                        return Ok(Applied::Connected {
                            response: connected(ctx, &path, &id)?,
                            follow_up: plan.follow_up,
                        });
                    }
                    let _ = saga.resume;
                    let connection = create_and_bind_parts(
                        ctx.source,
                        &ctx.control.connection,
                        &ctx.identity.binding_id,
                        &path,
                        &saga.project_id,
                        &saga.operation_id.to_string(),
                        &saga.payload,
                    )?;
                    connection_response(connection)
                }
            };
            Ok(Applied::Connected {
                response,
                follow_up: plan.follow_up,
            })
        }
    }
}

fn connected(ctx: &mut LaneCtx<'_>, path: &str, id: &str) -> Result<ConnectResponse> {
    Ok(connection_response(bind_parts(
        ctx.source,
        &ctx.control.connection,
        &ctx.identity.binding_id,
        path,
        id,
    )?))
}

fn connection_response(connection: ProjectConnection) -> ConnectResponse {
    ConnectResponse {
        code: None,
        project_connection: Some(connection),
        candidates: vec![],
        recall_result: None,
    }
}

pub(crate) fn bind_parts(
    store: &agentlaw_storage::Store,
    connection: &rusqlite::Connection,
    binding: &str,
    path: &str,
    id: &str,
) -> Result<ProjectConnection> {
    crate::context::absolute_project_path(path)?;
    validate_id(id)?;
    match store.read_catalog(id) {
        Ok(_) => {}
        Err(agentlaw_storage::Error::NotFound(_)) => {
            return Err(DomainError::new(
                "project_not_found",
                "Project identity does not exist.",
            ))
        }
        Err(e) => return source(Err(e)),
    }
    sql(connection.execute("INSERT INTO associations(binding,path,project) VALUES(?1,?2,?3) ON CONFLICT(binding,path) DO UPDATE SET project=excluded.project",params![binding,path,id]))?;
    Ok(ProjectConnection {
        project_id: id.into(),
        project_path: path.into(),
    })
}

pub(crate) fn create_and_bind_parts(
    store: &agentlaw_storage::Store,
    connection: &rusqlite::Connection,
    binding: &str,
    path: &str,
    proposed_id: &str,
    proposed_operation: &str,
    proposed_payload: &Value,
) -> Result<ProjectConnection> {
    let existing: Option<(String, String, String)> = sql(connection
        .query_row(
            "SELECT project,operation,payload FROM project_creations WHERE binding=?1 AND path=?2",
            params![binding, path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional())?;
    let (id, operation, payload) = if let Some(existing) = existing {
        existing
    } else {
        let payload = encode(proposed_payload)?;
        sql(connection.execute("INSERT INTO project_creations(binding,path,project,operation,payload) VALUES(?1,?2,?3,?4,?5)",params![binding,path,proposed_id,proposed_operation,payload]))?;
        (
            proposed_id.to_owned(),
            proposed_operation.to_owned(),
            payload,
        )
    };
    source(store.publish_catalog(&operation, &id, decode(&payload)?))?;
    let result = bind_parts(store, connection, binding, path, &id)?;
    sql(connection.execute(
        "UPDATE project_creations SET done=1 WHERE binding=?1 AND path=?2",
        params![binding, path],
    ))?;
    Ok(result)
}

pub(crate) fn save_pending(
    ctx: &mut LaneCtx<'_>,
    retained: &Retained,
    old: Option<&str>,
) -> Result<()> {
    save_pending_parts(&ctx.control.connection, retained, old)
}
pub(crate) fn publish_retained(ctx: &mut LaneCtx<'_>, retained: Retained) -> Result<Value> {
    publish_retained_parts(ctx.source, &ctx.control.connection, ctx.request, retained)
}
pub(crate) fn publish_procedure(
    ctx: &mut LaneCtx<'_>,
    token: &str,
    operation: &str,
    generation: u64,
    mutation: agentlaw_storage::Mutation,
    submission: &str,
) -> Result<Value> {
    publish_procedure_parts(
        ctx.source,
        &ctx.control.connection,
        ctx.request,
        token,
        operation,
        generation,
        mutation,
        submission,
    )
}
pub(crate) fn save_pending_parts(
    connection: &rusqlite::Connection,
    retained: &Retained,
    old: Option<&str>,
) -> Result<()> {
    let payload = encode(retained)?;
    if let Some(old) = old {
        let n=sql(connection.execute("UPDATE pending SET version=?1,payload=?2 WHERE id=?3 AND version=?4 AND status='pending'",params![retained.batch.reference.observed_version,payload,retained.batch.reference.pending_batch_id,old]))?;
        if n != 1 {
            return Err(DomainError::new(
                "stale_pending_ref",
                "Pending proposals changed concurrently; inspect latest retained work.",
            ));
        }
    } else {
        sql(connection.execute(
            "INSERT INTO pending(id,version,binding,payload,status) VALUES(?1,?2,?3,?4,'pending')",
            params![
                retained.batch.reference.pending_batch_id,
                retained.batch.reference.observed_version,
                retained.batch.store_binding_id,
                payload
            ],
        ))?;
    }
    Ok(())
}

pub(crate) fn publish_retained_parts(
    store: &agentlaw_storage::Store,
    connection: &rusqlite::Connection,
    request: &RequestControl,
    mut retained: Retained,
) -> Result<Value> {
    let generation = retained.expected_generation.ok_or_else(|| {
        DomainError::new(
            "control_corrupt",
            "Retained execution lacks its source generation.",
        )
    })?;
    let operation = retained.operation_id.as_ref().ok_or_else(|| {
        DomainError::new("control_corrupt", "Retained execution lacks its operation.")
    })?;
    request.phase("publishing");
    let publication = if let Some(basis) = &retained.publication_basis {
        store.publish_with_readset(
            operation,
            retained.mutations.clone(),
            basis,
            Some(&request.cancel),
        )
    } else {
        store.publish_if_generation_control(
            operation,
            retained.mutations.clone(),
            generation,
            &request.cancel,
        )
    };
    match publication {
        Ok(receipt) => {
            request.phase("published");
            let result = remember_receipt(&retained, &receipt)?;
            sql(connection.execute(
                "UPDATE pending SET status='published',result=?1 WHERE id=?2",
                params![encode(&result)?, retained.batch.reference.pending_batch_id],
            ))?;
            Ok(result)
        }
        Err(
            error @ (agentlaw_storage::Error::CancelledBeforeDecision
            | agentlaw_storage::Error::Stale(_)),
        ) => {
            if matches!(error, agentlaw_storage::Error::Stale(_)) {
                let issues = stale_parent_issues(store, request, &retained)?;
                if !issues.is_empty() {
                    let old = retained.batch.reference.observed_version.clone();
                    transition_to_review(&mut retained, issues);
                    save_pending_parts(connection, &retained, Some(&old))?;
                    return Ok(crate::prepare::pending_response(
                        &retained,
                        "resolution_required",
                    ));
                }
            }
            let cancelled = matches!(error, agentlaw_storage::Error::CancelledBeforeDecision);
            let old = retained.batch.reference.observed_version.clone();
            retained.operation_id = None;
            retained.expected_generation = None;
            retained.resolution_ref = Some(uuid::Uuid::new_v4().to_string());
            retained.mutations.clear();
            if !cancelled {
                retained.basis.clear();
            }
            retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
            retained.issues = vec![if cancelled {
                json!({"code":"cancelled","message":"Publication was cancelled before its durable decision. The proposal remains retained and will not publish automatically."})
            } else {
                json!({"code":"source_changed","message":"The source changed after review. Inspect retained proposals and continue to revalidate all heads and new dependencies."})
            }];
            save_pending_parts(connection, &retained, Some(&old))?;
            Ok(crate::prepare::pending_response(
                &retained,
                if cancelled {
                    "pending"
                } else {
                    "resolution_required"
                },
            ))
        }
        Err(error) => source(Err(error)),
    }
}

pub(crate) fn publish_procedure_parts(
    store: &agentlaw_storage::Store,
    connection: &rusqlite::Connection,
    request: &RequestControl,
    token: &str,
    operation: &str,
    generation: u64,
    mutation: agentlaw_storage::Mutation,
    submission: &str,
) -> Result<Value> {
    request.phase("publishing");
    match store.publish_if_generation_control(
        operation,
        vec![mutation],
        generation,
        &request.cancel,
    ) {
        Ok(receipt) => {
            request.phase("published");
            let result = procedure_receipt(&receipt)?;
            sql(connection.execute(
                "UPDATE authoring_executions SET status='published',result=?1 WHERE token=?2",
                params![encode(&result)?, token],
            ))?;
            Ok(result)
        }
        Err(
            error @ (agentlaw_storage::Error::CancelledBeforeDecision
            | agentlaw_storage::Error::Stale(_)),
        ) => {
            sql(connection.execute(
                "UPDATE authoring_executions SET status='needs_review' WHERE token=?1",
                params![token],
            ))?;
            let cancelled = matches!(error, agentlaw_storage::Error::CancelledBeforeDecision);
            let code = if cancelled {
                "cancelled"
            } else {
                "source_changed"
            };
            let instruction = if cancelled {
                "Publication was cancelled before its durable decision. Repeat the complete retained final submission with its authoring_ref to inspect and revise the retained submission; it will not publish automatically."
            } else {
                "Source evidence changed before publication. Repeat the complete retained final submission with this authoring_ref to inspect the retained submission and fresh evidence."
            };
            let submission: Value = decode(submission)?;
            Ok(
                json!({"status":"authoring_required","code":code,"authoring_ref":token,"retained_submission":submission,"turn_instruction":instruction}),
            )
        }
        Err(error) => source(Err(error)),
    }
}
use agentlaw_storage::{Mutation, PublicationBasis, PublishReceipt};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
enum PrefixExecution {
    Remember {
        retained: Retained,
        old_version: Option<String>,
    },
    Procedure {
        token: String,
        generation: u64,
        mutation: Mutation,
        submission: String,
    },
}

#[derive(Serialize, Deserialize)]
struct PrefixIntent {
    execution: PrefixExecution,
    prefixes: Vec<SubmittedPrefix>,
}

pub(crate) fn publication_inspect_action(operation: &str) -> String {
    format!("Restore access if needed, then call remember_this with pending_action=inspect and publication_ref={operation} to inspect the original operation before resubmitting.")
}

fn publication_error(error: DomainError, operation: &str, state: PublicationState) -> DomainError {
    error
        .with_next_action(publication_inspect_action(operation))
        .with_diagnostic(PublicationDetails {
            publication_state: state,
            publication_ref: operation.to_owned(),
        })
}

fn unresolved_publication_state(
    store: &agentlaw_storage::Store,
    operation: &str,
) -> PublicationState {
    match store
        .owned_published_reader()
        .publication_evidence(operation)
    {
        Ok(agentlaw_storage::PublicationEvidence::Undecided) => PublicationState::BeforeDecision,
        _ => PublicationState::DecisionUnknown,
    }
}

fn ensure_subject_idle(
    connection: &rusqlite::Connection,
    binding: &str,
    subject: &str,
) -> Result<()> {
    let existing = sql(connection
        .query_row(
            "SELECT operation FROM prefix_executions WHERE binding=?1 AND subject=?2",
            params![binding, subject],
            |row| row.get::<_, String>(0),
        )
        .optional())?;
    if let Some(operation) = existing {
        return Err(publication_error(
            DomainError::new(
                "publication_in_progress",
                "Recover the recorded publication before changing this work.",
            ),
            &operation,
            PublicationState::DecisionUnknown,
        ));
    }
    Ok(())
}

fn stale_parent_issues(
    store: &agentlaw_storage::Store,
    request: &RequestControl,
    retained: &Retained,
) -> Result<Vec<Value>> {
    let refs = |proposal: &MemoryProposal| {
        proposal
            .parent_refs
            .as_ref()
            .or(proposal.consolidation_refs.as_ref())
            .cloned()
            .unwrap_or_default()
    };
    let ids: std::collections::BTreeSet<_> = retained
        .batch
        .proposals
        .iter()
        .flat_map(|proposal| {
            refs(proposal)
                .into_iter()
                .map(|reference| reference.memory_id)
        })
        .collect();
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let snapshot = crate::read::snapshot_selected_read(
        &store.owned_published_reader(),
        request,
        &ids.into_iter().collect::<Vec<_>>(),
        &[],
    )?;
    let mut issues = Vec::new();
    for (index, proposal) in retained.batch.proposals.iter().enumerate() {
        let references = refs(proposal);
        if references.is_empty() {
            continue;
        }
        let ids: std::collections::BTreeSet<_> = references
            .iter()
            .map(|reference| &reference.memory_id)
            .collect();
        let mut stale = false;
        let mut current_states = Vec::new();
        for id in ids {
            let state = snapshot.current(id)?;
            let current: std::collections::BTreeSet<_> = state
                .iter()
                .flat_map(|state| state.heads.iter())
                .map(|head| &head.memory_ref)
                .collect();
            stale |= current
                != references
                    .iter()
                    .filter(|reference| &reference.memory_id == id)
                    .collect();
            if let Some(state) = state {
                current_states.push(external_current(&state));
            }
        }
        if stale {
            issues.push(json!({
                "code":"source_changed",
                "message":"Copy all current head references and review their full contents.",
                "proposal_index":index,"proposal_id":proposal.proposal_id,
                "submitted_proposal":proposal,"current_states":current_states,
            }));
        }
    }
    Ok(issues)
}

fn transition_to_review(retained: &mut Retained, issues: Vec<Value>) {
    retained.operation_id = None;
    retained.expected_generation = None;
    retained.publication_basis = None;
    retained.resolution_ref = Some(uuid::Uuid::new_v4().to_string());
    retained.mutations.clear();
    retained.basis.clear();
    retained.overlap_approved = false;
    retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
    retained.issues = issues;
}

fn apply_remember(
    ctx: &mut LaneCtx<'_>,
    mut plan: crate::prepare::RememberPlan,
    guarded: Option<&agentlaw_storage::published::SourceReadGuard<'_>>,
) -> Result<Applied> {
    if plan.retained.batch.store_binding_id != ctx.identity.binding_id {
        return Err(DomainError::new(
            "store_binding_mismatch",
            "Retained work belongs to another binding.",
        ));
    }
    ensure_subject_idle(
        &ctx.control.connection,
        &ctx.identity.binding_id,
        &plan.retained.batch.reference.pending_batch_id,
    )?;
    let old = match &plan.expectation {
        PendingExpectation::New => None,
        PendingExpectation::Version(version) => Some(version.as_str()),
    };
    if !plan.retained.issues.is_empty() {
        if let Some(basis) = &plan.review_basis {
            let validation = if let Some(guard) = guarded {
                guard.validate_read_set(basis)
            } else {
                let reader = ctx.source.owned_published_reader();
                let guard = source(reader.read_snapshot())?;
                guard.validate_read_set(basis)
            };
            match validation {
                Ok(()) => {}
                Err(agentlaw_storage::Error::Stale(_)) => {
                    plan.retained.basis.clear();
                    plan.retained.resolution_ref = Some(uuid::Uuid::new_v4().to_string());
                    plan.retained.issues = vec![
                        json!({"code":"source_changed","message":"The source changed after review. Inspect retained proposals and continue to revalidate all heads and new dependencies."}),
                    ];
                }
                Err(error) => return source(Err(error)),
            }
        }
    }
    save_pending(ctx, &plan.retained, old)?;
    let response = if plan.retained.issues.is_empty() {
        publish_retained(ctx, plan.retained)?
    } else {
        crate::prepare::pending_response(&plan.retained, "resolution_required")
    };
    Ok(Applied::Response(response))
}

fn apply_with_prefixes(
    ctx: &mut LaneCtx<'_>,
    plan: Prepared,
    prefixes: Vec<SubmittedPrefix>,
) -> Result<Applied> {
    let conditions: Vec<_> = prefixes
        .iter()
        .map(|input| input.condition.clone())
        .collect();
    let (operation, subject, execution) = match plan {
        Prepared::Remember(plan) if plan.retained.issues.is_empty() => {
            let old_version = match plan.expectation {
                PendingExpectation::New => None,
                PendingExpectation::Version(version) => Some(version),
            };
            let id = plan.retained.batch.reference.pending_batch_id.clone();
            let op = plan
                .retained
                .operation_id
                .clone()
                .expect("prepared publication has an operation");
            (
                op,
                id,
                PrefixExecution::Remember {
                    retained: plan.retained,
                    old_version,
                },
            )
        }
        Prepared::Procedure(plan) => (
            plan.operation_id.to_string(),
            plan.token.clone(),
            PrefixExecution::Procedure {
                token: plan.token,
                generation: plan.generation,
                mutation: plan.mutation,
                submission: plan.submission,
            },
        ),
        other => {
            let reader = ctx.source.owned_published_reader();
            let guard = source(reader.read_snapshot())?;
            guard
                .validate_prefixes(&conditions)
                .map_err(|error| prefix_error(error, &prefixes))?;
            return match other {
                Prepared::Remember(plan) => apply_remember(ctx, plan, Some(&guard)),
                other => apply(ctx, other),
            };
        }
    };
    let binding = &ctx.identity.binding_id;
    let connection = &ctx.control.connection;
    let tx = sql(connection.unchecked_transaction())?;
    ensure_subject_idle(&tx, binding, &subject)?;
    match &execution {
        PrefixExecution::Remember {
            retained,
            old_version,
        } => {
            if retained.batch.store_binding_id != *binding {
                return Err(DomainError::new(
                    "store_binding_mismatch",
                    "Retained work belongs to another binding.",
                ));
            }
            let current = sql(tx
                .query_row(
                    "SELECT version,status FROM pending WHERE id=?1 AND binding=?2",
                    params![subject, binding],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional())?;
            let matches = match (&current, old_version) {
                (None, None) => true,
                (Some((version, status)), Some(old)) => version == old && status == "pending",
                _ => false,
            };
            if !matches {
                return Err(DomainError::new(
                    "stale_pending_ref",
                    "Inspect the latest retained proposals before continuing.",
                ));
            }
        }
        PrefixExecution::Procedure { token, .. } => {
            let current = sql(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM authoring WHERE token=?1 AND binding=?2 AND used=0)",
                params![token, binding],
                |row| row.get::<_, bool>(0),
            ))?;
            if !current {
                return Err(DomainError::new(
                    "stale_authoring_ref",
                    "Authoring context was used concurrently; prepare again.",
                ));
            }
        }
    }
    let intent = PrefixIntent {
        execution,
        prefixes,
    };
    sql(tx.execute(
        "INSERT INTO prefix_executions(binding,operation,subject,payload) VALUES(?1,?2,?3,?4)",
        params![binding, operation, subject, encode(&intent)?],
    ))?;
    sql(tx.commit())?;
    finish_prefix_execution(
        ctx.source,
        connection,
        binding,
        ctx.request,
        &operation,
        intent,
    )
    .map(Applied::Response)
    .map_err(PrefixRunError::into_domain)
}

fn delete_prefix_execution(
    tx: &rusqlite::Transaction<'_>,
    binding: &str,
    operation: &str,
) -> Result<()> {
    sql(tx.execute(
        "DELETE FROM prefix_executions WHERE binding=?1 AND operation=?2",
        params![binding, operation],
    ))?;
    Ok(())
}

fn reject_prefix_execution(
    connection: &rusqlite::Connection,
    binding: &str,
    operation: &str,
    error: DomainError,
) -> PrefixRunError {
    let settle = || {
        let tx = sql(connection.unchecked_transaction())?;
        record_prefix_outcome(
            &tx,
            binding,
            operation,
            PrefixTerminalOutcome::Rejected(&error.code),
        )?;
        delete_prefix_execution(&tx, binding, operation)?;
        sql(tx.commit())
    };
    match settle() {
        Ok(()) => PrefixRunError::Rejected(error),
        Err(error) => PrefixRunError::Incomplete(publication_error(
            error,
            operation,
            PublicationState::BeforeDecision,
        )),
    }
}

fn prefix_publication_cancelled() -> DomainError {
    DomainError::new(
        "cancelled",
        "Publication was cancelled before the decision.",
    )
}

fn save_prefix_review(
    connection: &rusqlite::Connection,
    binding: &str,
    operation: &str,
    retained: &Retained,
    old_version: Option<&str>,
) -> Result<()> {
    let tx = sql(connection.unchecked_transaction())?;
    save_pending_parts(&tx, retained, old_version)?;
    record_prefix_outcome(
        &tx,
        binding,
        operation,
        PrefixTerminalOutcome::ReviewRequired(&retained.batch.reference),
    )?;
    delete_prefix_execution(&tx, binding, operation)?;
    sql(tx.commit())
}

enum PrefixRunError {
    Rejected(DomainError),
    ParentRereadFailed(DomainError),
    Incomplete(DomainError),
}
impl From<DomainError> for PrefixRunError {
    fn from(error: DomainError) -> Self {
        Self::Incomplete(error)
    }
}
impl PrefixRunError {
    fn into_domain(self) -> DomainError {
        match self {
            Self::Rejected(error) | Self::ParentRereadFailed(error) | Self::Incomplete(error) => {
                error
            }
        }
    }
}
fn finish_prefix_execution(
    store: &agentlaw_storage::Store,
    connection: &rusqlite::Connection,
    binding: &str,
    request: &RequestControl,
    operation: &str,
    intent: PrefixIntent,
) -> std::result::Result<Value, PrefixRunError> {
    let conditions: Vec<_> = intent
        .prefixes
        .iter()
        .map(|input| input.condition.clone())
        .collect();
    let (basis, mutations) = match &intent.execution {
        PrefixExecution::Remember { retained, .. } => (
            PublicationBasis::ReadSet(
                retained
                    .publication_basis
                    .as_ref()
                    .expect("prepared publication has a read set"),
            ),
            retained.mutations.clone(),
        ),
        PrefixExecution::Procedure {
            generation,
            mutation,
            ..
        } => (
            PublicationBasis::Generation(*generation),
            vec![mutation.clone()],
        ),
    };
    request.phase("publishing");
    let receipt = match store.publish_conditionally(
        operation,
        mutations,
        basis,
        &conditions,
        &request.cancel,
    ) {
        Ok(receipt) => receipt,
        Err(agentlaw_storage::Error::HeadsChanged(changed)) => {
            if let PrefixExecution::Remember {
                mut retained,
                old_version,
            } = intent.execution
            {
                let mut issues = Vec::new();
                if changed.iter().all(|id| {
                    retained.batch.proposals.iter().any(|proposal| {
                        proposal
                            .parent_refs
                            .iter()
                            .flatten()
                            .chain(proposal.consolidation_refs.iter().flatten())
                            .any(|reference| &reference.memory_id == id)
                    })
                }) {
                    request.phase("rereading_changed_heads");
                    if request.check().is_err() {
                        return Err(reject_prefix_execution(
                            connection,
                            binding,
                            operation,
                            prefix_publication_cancelled(),
                        ));
                    }
                    let reread = RequestControl::default();
                    issues = stale_parent_issues(store, &reread, &retained).map_err(|error| {
                        PrefixRunError::ParentRereadFailed(publication_error(
                            error,
                            operation,
                            unresolved_publication_state(store, operation),
                        ))
                    })?;
                }
                if !issues.is_empty() {
                    transition_to_review(&mut retained, issues);
                    save_prefix_review(
                        connection,
                        binding,
                        operation,
                        &retained,
                        old_version.as_deref(),
                    )
                    .map_err(|error| {
                        publication_error(error, operation, PublicationState::BeforeDecision)
                    })?;
                    return Ok(crate::prepare::pending_response(
                        &retained,
                        "resolution_required",
                    ));
                }
            }
            return Err(reject_prefix_execution(
                connection,
                binding,
                operation,
                DomainError::new(
                    "source_changed",
                    "Current source changed before publication.",
                )
                .with_next_action(
                    "Inspect current heads and review the proposed changes before resubmitting.",
                ),
            ));
        }
        Err(
            error @ (agentlaw_storage::Error::PrefixSelection { .. }
            | agentlaw_storage::Error::Stale(_)
            | agentlaw_storage::Error::CancelledBeforeDecision
            | agentlaw_storage::Error::CoverageLost),
        ) => {
            let error = match error {
                agentlaw_storage::Error::Stale(_) | agentlaw_storage::Error::CoverageLost => DomainError::new("source_changed", "Source changed before publication.")
                    .with_next_action("Inspect current heads and review the proposed changes before resubmitting."),
                agentlaw_storage::Error::CancelledBeforeDecision => prefix_publication_cancelled(),
                other => prefix_error(other, &intent.prefixes),
            };
            return Err(reject_prefix_execution(
                connection, binding, operation, error,
            ));
        }
        Err(error) => {
            let state = if matches!(
                error.decided_recovery_cause().unwrap_or(&error),
                agentlaw_storage::Error::Io(_)
                    | agentlaw_storage::Error::SourceRead { .. }
                    | agentlaw_storage::Error::Sql(_)
            ) {
                unresolved_publication_state(store, operation)
            } else {
                PublicationState::DecisionUnknown
            };
            return Err(PrefixRunError::Incomplete(publication_error(
                source_error(error),
                operation,
                state,
            )));
        }
    };
    request.phase("published");
    let tx = sql(connection.unchecked_transaction())?;
    let result = match intent.execution {
        PrefixExecution::Remember {
            retained,
            old_version,
        } => {
            let result = remember_receipt(&retained, &receipt)?;
            save_pending_parts(&tx, &retained, old_version.as_deref())?;
            sql(tx.execute(
                "UPDATE pending SET status='published',result=?1 WHERE id=?2",
                params![encode(&result)?, retained.batch.reference.pending_batch_id],
            ))?;
            result
        }
        PrefixExecution::Procedure {
            token,
            generation,
            mutation,
            submission,
        } => {
            let result = procedure_receipt(&receipt)?;
            let changed = sql(tx.execute(
                "UPDATE authoring SET used=1 WHERE token=?1 AND binding=?2 AND used=0",
                params![token, binding],
            ))?;
            if changed != 1 {
                return Err(DomainError::new(
                    "control_corrupt",
                    "Published procedure lacks its reserved authoring context.",
                )
                .into());
            }
            sql(tx.execute("INSERT INTO authoring_executions(token,binding,operation,generation,mutation,submission,status,result) VALUES(?1,?2,?3,?4,?5,?6,'published',?7)",
                params![token,binding,operation,generation.to_string(),encode(&mutation)?,submission,encode(&result)?]))?;
            result
        }
    };
    delete_prefix_execution(&tx, binding, operation)?;
    sql(tx.commit())?;
    Ok(result)
}

pub(crate) fn recover_prefix_executions(
    store: &agentlaw_storage::Store,
    connection: &rusqlite::Connection,
    binding: &str,
    request: &RequestControl,
) -> Result<()> {
    let pending = {
        let mut statement =
            sql(connection
                .prepare("SELECT operation,payload FROM prefix_executions WHERE binding=?1"))?;
        let rows = sql(statement.query_map([binding], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }))?;
        sql(rows.collect::<std::result::Result<Vec<_>, _>>())?
    };
    for (operation, payload) in pending {
        match finish_prefix_execution(
            store,
            connection,
            binding,
            request,
            &operation,
            decode(&payload)?,
        ) {
            Ok(_) | Err(PrefixRunError::Rejected(_)) => {}
            Err(PrefixRunError::ParentRereadFailed(error)) => {
                use std::io::Write;

                let _ = writeln!(
                    std::io::stderr().lock(),
                    "Publication recovery deferred for {operation} ({}): {}",
                    error.code,
                    error.message,
                );
            }
            Err(PrefixRunError::Incomplete(error)) => return Err(error),
        }
    }
    Ok(())
}

fn remember_receipt(retained: &Retained, receipt: &PublishReceipt) -> Result<Value> {
    let results=retained.mutations.iter().filter(|m|matches!(m.unit.state,UnitState::Live{..})).enumerate().map(|(index,m)| {
                let reference=receipt.references.iter().find(|r|r.memory_id==m.unit.entity_id).ok_or_else(||DomainError::new("publication_receipt_incomplete","Published receipt lacks a proposal destination; preserve retained execution and inspect source."))?;
                Ok(json!({"proposal_index":index,"memory_ref":{"memory_id":reference.memory_id,"observed_version":reference.observed_version}}))
            }).collect::<Result<Vec<_>>>()?;
    let result =
        json!({"status":"remembered","results":results,"diagnostics":retained.diagnostics});
    Ok(result)
}

fn procedure_receipt(receipt: &PublishReceipt) -> Result<Value> {
    let reference = receipt.references.first().ok_or_else(|| {
        DomainError::new(
            "publication_result_invalid",
            "Procedure reference is absent.",
        )
    })?;
    let result = json!({"status":"remembered","procedure_ref":{"procedure_id":reference.memory_id,"observed_version":reference.observed_version}});
    Ok(result)
}
