//! Resuming interrupted authoring and pending executions.
use crate::runtime::*;
use crate::*;
use agentlaw_storage::Mutation;
use rusqlite::params;
use serde_json::json;
impl Runtime {
    pub(crate) fn recover_authoring(&self) -> Result<()> {
        let mut statement=sql(self.control.prepare("SELECT token,operation,generation,mutation FROM authoring_executions WHERE binding=?1 AND status='pending'"))?;
        let rows = sql(statement.query_map([&self.binding_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        }))?;
        let pending = sql(rows.collect::<std::result::Result<Vec<_>, _>>())?;
        drop(statement);
        for (token, operation, generation, mutation) in pending {
            let generation = generation.parse::<u64>().map_err(|_| {
                DomainError::new(
                    "control_corrupt",
                    "Invalid retained authoring source generation.",
                )
            })?;
            let mutation: Mutation = decode(&mutation)?;
            match self
                .store
                .publish_if_generation(&operation, vec![mutation], generation)
            {
                Ok(receipt) => {
                    let r = receipt.references.first().ok_or_else(|| {
                        DomainError::new(
                            "publication_result_invalid",
                            "Recovered procedure has no reference.",
                        )
                    })?;
                    let result = json!({"status":"remembered","procedure_ref":{"procedure_id":r.memory_id,"observed_version":r.observed_version}});
                    sql(self.control.execute("UPDATE authoring_executions SET status='published',result=?1 WHERE token=?2",params![encode(&result)?,token]))?;
                }
                Err(agentlaw_storage::Error::Stale(_)) => {
                    sql(self.control.execute(
                        "UPDATE authoring_executions SET status='needs_review' WHERE token=?1",
                        [token],
                    ))?;
                }
                Err(error) => return source(Err(error)),
            }
        }
        Ok(())
    }
    pub(crate) fn recover_pending_executions(&self) -> Result<()> {
        let mut st = sql(self
            .control
            .prepare("SELECT payload FROM pending WHERE binding=?1 AND status='pending'"))?;
        let rows = sql(st.query_map([&self.binding_id], |r| r.get::<_, String>(0)))?;
        let payloads = sql(rows.collect::<std::result::Result<Vec<_>, _>>())?;
        drop(st);
        for payload in payloads {
            let retained: Retained = decode(&payload)?;
            if retained.operation_id.is_some() {
                self.publish_retained(retained)?;
            }
        }
        Ok(())
    }
}
