use super::*;

mod index_handoff;
mod recovery;

#[cfg(windows)]
pub(super) fn fixture_shared(warm: bool, events: &mpsc::Sender<Event>) -> Arc<SupervisorShared> {
    Arc::new(SupervisorShared {
        state: std::sync::Mutex::new(SupervisorState {
            snapshot: ModelSnapshot {
                state: WorkerState::Stopped,
                incarnation: None,
                model_digest: None,
                cause: None,
            },
            warm,
            stop: false,
            loading_demand: false,
            queries: QueryQueue::default(),
            terminated: None,
        }),
        signal: Arc::new(RamSignal {
            events: events.clone(),
        }),
        changed: std::sync::Condvar::new(),
        events: events.clone(),
        wake_pending: AtomicBool::new(false),
    })
}
