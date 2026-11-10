//! Coordinates pinned views and owned source bytes for one request.

use crate::projection::ProjectionHandle;
use crate::protocol::QueryRequest;
use agentlaw_flows::exact::{DeltaBudget, ExactSnapshot};
use agentlaw_flows::runtime::ReadCtx;
use agentlaw_flows::{DomainError, RecallRequest};
use serde_json::Value;

/// Runs ordinary queries against one pinned projection generation and source position.
pub struct ReadCoordinator {
    projection: ProjectionHandle,
    budget: DeltaBudget,
}
impl ReadCoordinator {
    pub fn new(projection: ProjectionHandle, budget: DeltaBudget) -> Self {
        Self { projection, budget }
    }
    pub fn run_query(
        &self,
        ctx: &ReadCtx<'_>,
        request: QueryRequest,
    ) -> agentlaw_flows::Result<Value> {
        ctx.request.check()?;
        match request {
            QueryRequest::Recall(request) => self.run_recall(ctx, &request, None),
            QueryRequest::History(request) => {
                agentlaw_flows::history_projection::history(ctx, &request)
            }
            QueryRequest::PendingInspect(request) => {
                agentlaw_flows::read::pending_inspect(ctx, &request)
            }
            QueryRequest::Discover(request) => agentlaw_flows::read::discover(ctx, &request),
        }
    }

    /// Runs a recall whose project scope is fixed to `project_id`, for the
    /// restore that follows a saved connection.
    pub fn run_recall_for_project(
        &self,
        ctx: &ReadCtx<'_>,
        request: RecallRequest,
        project_id: &str,
    ) -> agentlaw_flows::Result<Value> {
        self.run_recall(ctx, &request, Some(project_id))
    }

    fn run_recall(
        &self,
        ctx: &ReadCtx<'_>,
        request: &RecallRequest,
        project_id: Option<&str>,
    ) -> agentlaw_flows::Result<Value> {
        ctx.request.check()?;
        let result = (|| {
            let prepared = agentlaw_flows::read::prepare_recall(ctx, request)?;
            let pinned = self.projection.generations().pin()?;
            let guard = ctx.source.read_snapshot().map_err(|_| {
                DomainError::new(
                    "source_operation_failed",
                    "Cannot acquire the source read view.",
                )
            })?;
            let mut exact = ExactSnapshot::build(pinned, &guard, self.budget)?;
            drop(guard);
            agentlaw_flows::read::recall_prepared(
                ctx,
                &mut exact,
                request,
                project_id,
                self.budget,
                &prepared,
            )
        })();
        if result.as_ref().err().is_some_and(|e| {
            matches!(
                e.code.as_str(),
                "capacity"
                    | "exact_not_ready"
                    | "exact_index_corrupt"
                    | "source_epoch_changed"
                    | "source_coverage_lost"
            )
        }) {
            self.projection.wake();
        }
        result
    }
}

#[cfg(test)]
mod tests;
