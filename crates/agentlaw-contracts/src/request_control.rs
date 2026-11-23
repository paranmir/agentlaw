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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn clones_share_cancellation() {
        let control = RequestControl::default();
        assert!(control.check().is_ok());
        let clone = control.clone();
        clone.cancel.store(true, Ordering::Release);
        let error = control.check().unwrap_err();
        assert_eq!(error.code, "cancelled");
        assert_eq!(
            error.message,
            "The request was cancelled before canonical publication."
        );
    }

    #[test]
    fn phases_are_delivered_in_order_through_clones() {
        let phases = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&phases);
        let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
            captured.lock().unwrap().push(phase.to_owned());
        });
        let clone = control.clone();
        control.phase("first");
        clone.phase("second");
        control.phase("third");
        assert_eq!(*phases.lock().unwrap(), ["first", "second", "third"]);
    }
}
