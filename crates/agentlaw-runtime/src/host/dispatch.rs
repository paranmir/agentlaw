//! Routes validated requests to their store or setup work.
use crate::protocol::{CallTarget, Classified, RejectReason};
use agentlaw_contracts::{DomainError, RequestControl};
use serde_json::Value;

pub trait Dispatcher: Send + Sync + 'static {
    type Admitted: Send + 'static;
    fn admit(
        &self,
        target: &CallTarget,
        request: Classified,
    ) -> Result<Self::Admitted, RejectReason>;
    fn run(&self, admitted: Self::Admitted, control: &RequestControl)
        -> Result<Value, DomainError>;
}
