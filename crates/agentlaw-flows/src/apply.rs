//! Publishing retained work and recording its results.
use crate::prepare::{ConnectPlanKind, PendingExpectation, Prepared};
use crate::runtime::*;
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
        Prepared::Replay(value) => Ok(Applied::Response(value)),
        Prepared::Remember(plan) => {
            if plan.retained.batch.store_binding_id != ctx.identity.binding_id {
                return Err(DomainError::new(
                    "store_binding_mismatch",
                    "Retained work belongs to another binding.",
                ));
            }
            let old = match &plan.expectation {
                PendingExpectation::New => None,
                PendingExpectation::Version(version) => Some(version.as_str()),
            };
            save_pending(ctx, &plan.retained, old)?;
            let value = if plan.retained.issues.is_empty() {
                publish_retained(ctx, plan.retained)?
            } else {
                Runtime::pending_response(&plan.retained, "resolution_required")
            };
            Ok(Applied::Response(value))
        }
        Prepared::Discard(plan) => {
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
            let results=retained.mutations.iter().filter(|m|matches!(m.unit.state,UnitState::Live{..})).enumerate().map(|(index,m)| {
                let reference=receipt.references.iter().find(|r|r.memory_id==m.unit.entity_id).ok_or_else(||DomainError::new("publication_receipt_incomplete","Published receipt lacks a proposal destination; preserve retained execution and inspect source."))?;
                Ok(json!({"proposal_index":index,"memory_ref":{"memory_id":reference.memory_id,"observed_version":reference.observed_version}}))
            }).collect::<Result<Vec<_>>>()?;
            let result =
                json!({"status":"remembered","results":results,"diagnostics":retained.diagnostics});
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
            Ok(Runtime::pending_response(
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
            let reference = receipt.references.first().ok_or_else(|| {
                DomainError::new(
                    "publication_result_invalid",
                    "Procedure reference is absent.",
                )
            })?;
            let result = json!({"status":"remembered","procedure_ref":{"procedure_id":reference.memory_id,"observed_version":reference.observed_version}});
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

impl Runtime {
    pub(crate) fn publish_retained(&self, retained: Retained) -> Result<Value> {
        publish_retained_parts(&self.store, &self.control, &self.request_control, retained)
    }
    pub(crate) fn save_pending(&self, retained: &Retained, old: Option<&str>) -> Result<()> {
        save_pending_parts(&self.control, retained, old)
    }
    /// Schedule durable indexing through the authenticated worker IPC. Source
    /// access remains in this read-only adapter; the worker receives no paths or
    /// canonical write authority. Queue acceptance is not an index acknowledgment.
    pub fn enqueue_derived(&self) -> Result<()> {
        let worker = self.worker.as_ref().ok_or_else(|| {
            DomainError::new(
                "indexing_pending",
                "No worker is attached; source publication remains complete.",
            )
        })?;
        crate::read::pump(
            worker,
            &crate::derived::PublishedAdapter::new(
                self.store.owned_published_reader(),
                self.binding_id.clone(),
            ),
            &self.binding_id,
            &self.local,
            &self.request_control.cancel,
        )
    }
}
