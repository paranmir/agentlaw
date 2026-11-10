//! Per-key build guards for history lexical caches.

#[cfg(test)]
mod tests;
use crate::{RequestControl, Result};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
pub struct HistoryCacheGuards {
    keys: Mutex<HashMap<String, Arc<KeyState>>>,
}
#[derive(Default)]
struct KeyState {
    busy: Mutex<bool>,
    ready: Condvar,
}
struct BuildGuard<'a> {
    owner: &'a HistoryCacheGuards,
    key: String,
    state: Option<Arc<KeyState>>,
}
impl Drop for BuildGuard<'_> {
    fn drop(&mut self) {
        let mut keys = self.owner.keys.lock().unwrap_or_else(|e| e.into_inner());
        let state = self.state.take().expect("build guard state");
        let mut busy = state.busy.lock().unwrap_or_else(|e| e.into_inner());
        *busy = false;
        state.ready.notify_all();
        drop(busy);
        drop(state);
        remove_unreferenced(&mut keys, &self.key);
    }
}

fn remove_unreferenced(keys: &mut HashMap<String, Arc<KeyState>>, key: &str) {
    if keys
        .get(key)
        .is_some_and(|state| Arc::strong_count(state) == 1)
    {
        keys.remove(key);
    }
}

impl HistoryCacheGuards {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_build<T>(
        &self,
        key: &str,
        control: &RequestControl,
        build: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        control.check()?;
        let state = {
            let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.entry(key.to_owned()).or_default().clone()
        };
        let mut busy = state.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy {
            if let Err(error) = control.check() {
                drop(busy);
                let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
                drop(state);
                remove_unreferenced(&mut keys, key);
                return Err(error);
            }
            busy = state
                .ready
                .wait_timeout(busy, Duration::from_millis(25))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if let Err(error) = control.check() {
            drop(busy);
            let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
            drop(state);
            remove_unreferenced(&mut keys, key);
            return Err(error);
        }
        *busy = true;
        drop(busy);
        let _guard = BuildGuard {
            owner: self,
            key: key.to_owned(),
            state: Some(state),
        };
        build()
    }
}
