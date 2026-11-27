//! Coordinates pinned views and owned source bytes for one request.

use crate::projection::ProjectionHandle;
use crate::protocol::QueryRequest;
use agentlaw_flows::exact::{DeltaBudget, ExactSnapshot};
use agentlaw_flows::prepare::Prepared;
use agentlaw_flows::read::recall_source_error;
use agentlaw_flows::store::ReadCtx;
use agentlaw_flows::{
    ConnectRequest, DomainError, RecallRequest, RememberRequest, Result as FlowResult,
};
use serde_json::Value;

/// Runs ordinary queries and command preparation against one pinned projection
/// generation and source position.
pub struct ReadCoordinator {
    projection: ProjectionHandle,
    budget: DeltaBudget,
}
impl ReadCoordinator {
    pub fn search_procedures(
        &self,
        ctx: &ReadCtx<'_>,
        query: &str,
        filter: &crate::protocol::InventoryFilter,
        limit: usize,
    ) -> FlowResult<Value> {
        self.with_exact(ctx, |exact| {
            agentlaw_flows::read::search_procedures_filtered(
                ctx,
                exact,
                query,
                filter.scope.as_deref(),
                filter.project.as_deref(),
                filter.machine.as_deref(),
                limit,
            )
        })
        .map_err(|error| acquisition_guidance(error, "procedure search"))
    }
    pub fn new(projection: ProjectionHandle, budget: DeltaBudget) -> Self {
        Self { projection, budget }
    }
    pub fn run_query(&self, ctx: &ReadCtx<'_>, request: QueryRequest) -> FlowResult<Value> {
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
    ) -> FlowResult<Value> {
        self.run_recall(ctx, &request, Some(project_id))
    }

    pub(super) fn run_restore_for_project(
        &self,
        ctx: &ReadCtx<'_>,
        request: RecallRequest,
        project_id: &str,
    ) -> FlowResult<(Value, Vec<DomainError>)> {
        self.run_recall_with_diagnostics(ctx, &request, Some(project_id))
    }

    /// Prepares a remember or procedure command outside the mutation lane.
    pub fn run_prepare(&self, ctx: &ReadCtx<'_>, request: RememberRequest) -> FlowResult<Prepared> {
        ctx.request.check()?;
        self.with_exact(ctx, |exact| {
            agentlaw_flows::prepare::prepare(ctx, exact, request)
        })
        .map_err(|error| acquisition_guidance(error, "write preparation"))
    }

    /// Prepares a connect command outside the mutation lane.
    pub fn run_prepare_connect(
        &self,
        ctx: &ReadCtx<'_>,
        request: ConnectRequest,
    ) -> FlowResult<Prepared> {
        ctx.request.check()?;
        agentlaw_flows::prepare::prepare_connect(ctx, request)
    }

    fn run_recall(
        &self,
        ctx: &ReadCtx<'_>,
        request: &RecallRequest,
        project_id: Option<&str>,
    ) -> FlowResult<Value> {
        self.run_recall_with_diagnostics(ctx, request, project_id)
            .map(|(value, _)| value)
    }

    /// Runs recall and returns its semantic completeness diagnostics separately from the
    /// delivered value, which may hold the content inline or point to a file.
    fn run_recall_with_diagnostics(
        &self,
        ctx: &ReadCtx<'_>,
        request: &RecallRequest,
        project_id: Option<&str>,
    ) -> FlowResult<(Value, Vec<DomainError>)> {
        agentlaw_flows::read::recall_with_snapshot(
            ctx,
            request,
            project_id,
            self.budget,
            || {
                let pinned = self.projection.generations().pin()?;
                let guard = ctx
                    .source
                    .read_snapshot_for_recall()
                    .map_err(recall_source_error)?;
                ExactSnapshot::build_for_recall(pinned, &guard, self.budget)
            },
            |error| self.wake_projection_after(error),
        )
    }

    /// Pins the active generation at the current source position and wakes
    /// projection recovery when the pinned view cannot serve the request.
    fn with_exact<T>(
        &self,
        ctx: &ReadCtx<'_>,
        work: impl FnOnce(&mut ExactSnapshot) -> FlowResult<T>,
    ) -> FlowResult<T> {
        let result = (|| {
            let pinned = self.projection.generations().pin()?;
            let guard = ctx.source.read_snapshot().map_err(source_view_error)?;
            let mut exact = ExactSnapshot::build(pinned, &guard, self.budget)?;
            drop(guard);
            work(&mut exact)
        })();
        if let Err(error) = &result {
            self.wake_projection_after(error);
        }
        result
    }

    fn wake_projection_after(&self, error: &DomainError) {
        if matches!(
            error.code.as_str(),
            "capacity"
                | "exact_not_ready"
                | "exact_index_corrupt"
                | "source_epoch_changed"
                | "source_coverage_lost"
        ) {
            self.projection.wake();
        }
    }
}

fn source_view_error(error: agentlaw_storage::Error) -> DomainError {
    use agentlaw_contracts::{AcquisitionDetails, AcquisitionFailure};
    let (code, cause) = match error {
        agentlaw_storage::Error::Corrupt(_) => {
            ("source_corrupt", Some(AcquisitionFailure::SourceCorrupt))
        }
        agentlaw_storage::Error::RecoveryRequired(_) => ("recovery_required", None),
        agentlaw_storage::Error::Io(_) | agentlaw_storage::Error::SourceRead { .. } => (
            "source_operation_failed",
            Some(AcquisitionFailure::SourceRead),
        ),
        _ => ("source_operation_failed", None),
    };
    let error = DomainError::new(
        code,
        format!("Cannot acquire the source read view: {error}"),
    );
    if let Some(acquisition_failure) = cause {
        error.with_diagnostic(AcquisitionDetails {
            acquisition_failure,
        })
    } else {
        error
    }
}

fn acquisition_guidance(error: DomainError, operation: &str) -> DomainError {
    use agentlaw_contracts::{AcquisitionDetails, AcquisitionFailure};
    let Some(details) = error.diagnostic::<AcquisitionDetails>() else {
        return error;
    };
    let action = match details.acquisition_failure {
        AcquisitionFailure::ProjectionCapacity => "Wait for the index to catch up",
        AcquisitionFailure::SourceRead => {
            "Restore source access by checking permissions, file sharing and storage availability"
        }
        AcquisitionFailure::SourceCorrupt => {
            "Preserve the source, run doctor --audit, and restore damaged data from a verified copy"
        }
    };
    error.with_next_action(format!(
        "{action}, then repeat the original {operation} request."
    ))
}

#[cfg(test)]
mod tests;
