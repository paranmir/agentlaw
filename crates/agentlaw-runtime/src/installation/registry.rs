//! Publishes configuration snapshots and remembers issued bindings in RAM.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::installation::config::Config;
use crate::protocol::{BindingError, BindingRef, ConfigurationFailure};
use agentlaw_storage::BindingKey;

#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    pub config: Config,
    pub key: BindingKey,
    pub reference: BindingRef,
}

#[derive(Debug, Clone)]
pub enum ConfigState {
    Missing,
    Invalid(ConfigurationFailure),
    Ready(Arc<ConfigSnapshot>),
}

struct State {
    current: ConfigState,
    issued: VecDeque<Arc<ConfigSnapshot>>,
}

pub struct ConfigRegistry {
    state: Mutex<State>,
}

impl Default for ConfigRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigRegistry {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                current: ConfigState::Missing,
                issued: VecDeque::new(),
            }),
        }
    }

    pub fn publish(&self, state: ConfigState) {
        let mut inner = self.state.lock().unwrap();
        if let ConfigState::Ready(snapshot) = &state {
            if let Some(entry) = inner
                .issued
                .iter_mut()
                .find(|entry| entry.reference == snapshot.reference)
            {
                *entry = Arc::clone(snapshot);
            }
        }
        inner.current = state;
    }

    pub fn current(&self) -> ConfigState {
        let mut inner = self.state.lock().unwrap();
        let current = inner.current.clone();
        if let ConfigState::Ready(snapshot) = &current {
            Self::touch(&mut inner, Arc::clone(snapshot));
        }
        current
    }

    pub fn resolve(&self, reference: &BindingRef) -> Result<Arc<ConfigSnapshot>, BindingError> {
        let mut inner = self.state.lock().unwrap();
        let snapshot = inner
            .issued
            .iter()
            .find(|s| &s.reference == reference)
            .cloned()
            .or_else(|| match &inner.current {
                ConfigState::Ready(s) if &s.reference == reference => Some(Arc::clone(s)),
                _ => None,
            });
        if let Some(snapshot) = snapshot {
            Self::touch(&mut inner, Arc::clone(&snapshot));
            return Ok(snapshot);
        }
        match &inner.current {
            ConfigState::Missing if inner.issued.is_empty() => {
                Err(BindingError::ConnectionRequired)
            }
            ConfigState::Invalid(reason) if inner.issued.is_empty() => {
                Err(BindingError::ConfigInvalid(reason.clone()))
            }
            _ => Err(BindingError::BindingChanged),
        }
    }

    fn touch(inner: &mut State, snapshot: Arc<ConfigSnapshot>) {
        if let Some(index) = inner
            .issued
            .iter()
            .position(|s| s.reference == snapshot.reference)
        {
            inner.issued.remove(index);
        }
        inner.issued.push_back(snapshot);
        if inner.issued.len() > 64 {
            inner.issued.pop_front();
        }
    }
}

#[cfg(test)]
mod tests;
