//! Resuming interrupted authoring and pending executions.
use crate::store::*;
use crate::*;
use agentlaw_storage::{Mutation, Store};
use rusqlite::{params, Connection};
use serde_json::json;

/// Recovery is independent of the request that caused the owner to open.
pub fn recover(ctx: &mut LaneCtx<'_>) -> Result<()> {
    let original = ctx.request.clone();
    let request = RequestControl::new(
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        move |phase| original.phase(phase),
    );
    source(ctx.source.recover())?;
    recover_authoring_parts(
        ctx.source,
        &ctx.control.connection,
        &ctx.identity.binding_id,
        &request,
    )?;
    recover_pending_parts(
        ctx.source,
        &ctx.control.connection,
        &ctx.identity.binding_id,
        &request,
    )
}

fn recover_authoring_parts(
    store: &Store,
    connection: &Connection,
    binding: &str,
    request: &RequestControl,
) -> Result<()> {
    let pending = {
        let mut statement=sql(connection.prepare("SELECT token,operation,generation,mutation FROM authoring_executions WHERE binding=?1 AND status='pending'"))?;
        let rows = sql(statement.query_map([binding], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        }))?;
        sql(rows.collect::<std::result::Result<Vec<_>, _>>())?
    };
    for (token, operation, generation, mutation) in pending {
        let generation = generation.parse::<u64>().map_err(|_| {
            DomainError::new(
                "control_corrupt",
                "Invalid retained authoring source generation.",
            )
        })?;
        let mutation: Mutation = decode(&mutation)?;
        request.phase("publishing");
        match store.publish_if_generation(&operation, vec![mutation], generation) {
            Ok(receipt) => {
                request.phase("published");
                let reference = receipt.references.first().ok_or_else(|| {
                    DomainError::new(
                        "publication_result_invalid",
                        "Recovered procedure has no reference.",
                    )
                })?;
                let result = json!({"status":"remembered","procedure_ref":{"procedure_id":reference.memory_id,"observed_version":reference.observed_version}});
                sql(connection.execute(
                    "UPDATE authoring_executions SET status='published',result=?1 WHERE token=?2",
                    params![encode(&result)?, token],
                ))?;
            }
            Err(agentlaw_storage::Error::Stale(_)) => {
                sql(connection.execute(
                    "UPDATE authoring_executions SET status='needs_review' WHERE token=?1",
                    [token],
                ))?;
            }
            Err(error) => return source(Err(error)),
        }
    }
    Ok(())
}

fn recover_pending_parts(
    store: &Store,
    connection: &Connection,
    binding: &str,
    request: &RequestControl,
) -> Result<()> {
    let payloads = {
        let mut statement =
            sql(connection
                .prepare("SELECT payload FROM pending WHERE binding=?1 AND status='pending'"))?;
        let rows = sql(statement.query_map([binding], |row| row.get::<_, String>(0)))?;
        sql(rows.collect::<std::result::Result<Vec<_>, _>>())?
    };
    for payload in payloads {
        let retained: Retained = decode(&payload)?;
        if retained.operation_id.is_some() {
            crate::apply::publish_retained_parts(store, connection, request, retained)?;
        }
    }
    Ok(())
}
