use crate::{DomainError, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
/// Cancellation and progress are request-scoped, not part of canonical identity.
#[derive(Clone)]
pub struct RequestControl {
    pub cancel: Arc<AtomicBool>,
    progress: Arc<dyn Fn(&str) + Send + Sync>,
}
impl Default for RequestControl {
    fn default() -> Self {
        Self::new(Arc::new(AtomicBool::new(false)), |_| {})
    }
}
impl RequestControl {
    pub fn new(cancel: Arc<AtomicBool>, progress: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            cancel,
            progress: Arc::new(progress),
        }
    }
    pub fn phase(&self, phase: &str) {
        (self.progress)(phase)
    }
    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            Err(DomainError::new(
                "cancelled",
                "The request was cancelled before canonical publication.",
            ))
        } else {
            Ok(())
        }
    }
}
