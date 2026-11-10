//! Holds progress and terminal results apart from socket writes.
use crate::protocol::{DataFrame, RequestId};
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
    time::Duration,
};
pub struct Mailbox {
    id: RequestId,
    capacity: usize,
    state: Mutex<State>,
    changed: Condvar,
}
struct State {
    progress: VecDeque<String>,
    terminal: Option<DataFrame>,
    terminal_set: bool,
}
impl Mailbox {
    pub fn new(id: RequestId, progress_capacity: usize) -> Self {
        Self {
            id,
            capacity: progress_capacity,
            state: Mutex::new(State {
                progress: VecDeque::new(),
                terminal: None,
                terminal_set: false,
            }),
            changed: Condvar::new(),
        }
    }
    pub fn publish_progress(&self, phase: &str) {
        let mut s = self.state.lock().unwrap();
        if !s.terminal_set && self.capacity > 0 {
            if s.progress.len() == self.capacity {
                s.progress.pop_front();
            }
            s.progress.push_back(phase.into());
            self.changed.notify_one();
        }
    }
    pub fn publish_terminal(&self, terminal: DataFrame) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.terminal_set {
            return false;
        }
        s.terminal_set = true;
        s.terminal = Some(terminal);
        self.changed.notify_one();
        true
    }
    pub fn receive(&self, timeout: Duration) -> Option<DataFrame> {
        let s = self.state.lock().unwrap();
        let (mut s, _) = self
            .changed
            .wait_timeout_while(s, timeout, |s| {
                s.progress.is_empty() && s.terminal.is_none()
            })
            .unwrap();
        if let Some(phase) = s.progress.pop_front() {
            return Some(DataFrame::Progress { id: self.id, phase });
        }
        s.terminal.take()
    }
}
#[cfg(test)]
mod tests;
