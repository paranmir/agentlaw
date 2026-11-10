//! Shared host dependencies, health and activity tracking.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

pub struct HostDeps {
    pub state: PathBuf,
    pub executable: PathBuf,
    pub assets: Option<agentlaw_worker::ModelAssets>,
    pub installation_build: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ComponentKind {
    Host,
    Setup,
    Store,
    Control,
    Exact,
    Semantic,
    Model,
    Git,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ComponentId {
    pub binding: Option<String>,
    pub kind: ComponentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComponentState {
    Starting,
    Ready,
    Busy,
    Recovering,
    Degraded,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentStatus {
    pub state: ComponentState,
    pub observed_at: SystemTime,
    pub reason: Option<String>,
}

pub const MAX_HEALTH_REASON_BYTES: usize = 256;
pub const MAX_HEALTH_COMPONENTS: usize = 512;

/// Clone-able RAM handle. It never writes operating state or diagnostics to disk.
#[derive(Debug, Clone, Default)]
pub struct HealthSink {
    inner: Arc<Mutex<BTreeMap<ComponentId, ComponentStatus>>>,
}

impl HealthSink {
    pub fn publish(&self, component: ComponentId, mut status: ComponentStatus) {
        if let Some(reason) = &mut status.reason {
            if reason.len() > MAX_HEALTH_REASON_BYTES {
                let mut end = MAX_HEALTH_REASON_BYTES;
                while !reason.is_char_boundary(end) {
                    end -= 1;
                }
                reason.truncate(end);
            }
        }
        let mut entries = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.insert(component.clone(), status);
        if entries.len() > MAX_HEALTH_COMPONENTS {
            let oldest = entries
                .iter()
                .filter(|(id, _)| id.binding.is_some() && **id != component)
                .min_by_key(|(_, status)| status.observed_at)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
    }

    pub fn snapshot(&self) -> Vec<(ComponentId, ComponentStatus)> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(component, status)| (component.clone(), status.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Execution,
    CanonicalRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActivityCounts {
    pub executions: u32,
    pub canonical_recoveries: u32,
}

/// Clone-able RAM handle.
#[derive(Debug, Clone, Default)]
pub struct ActivityTracker {
    inner: Arc<Mutex<ActivityCounts>>,
}

impl ActivityTracker {
    pub fn begin(&self, kind: ActivityKind) -> RuntimeResult<ActivityGuard> {
        let mut counts = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = match kind {
            ActivityKind::Execution => &mut counts.executions,
            ActivityKind::CanonicalRecovery => &mut counts.canonical_recoveries,
        };
        *count = count
            .checked_add(1)
            .ok_or_else(|| RuntimeError::Capacity(format!("too many {kind:?} activities")))?;
        Ok(ActivityGuard {
            inner: Arc::clone(&self.inner),
            kind,
        })
    }

    pub fn counts(&self) -> ActivityCounts {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Lowers its count exactly once, when dropped.
#[must_use]
pub struct ActivityGuard {
    inner: Arc<Mutex<ActivityCounts>>,
    kind: ActivityKind,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut counts = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match self.kind {
            ActivityKind::Execution => counts.executions -= 1,
            ActivityKind::CanonicalRecovery => counts.canonical_recoveries -= 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub components: Vec<(ComponentId, ComponentStatus)>,
    pub activity: ActivityCounts,
}

pub type RuntimeResult<T> = Result<T, RuntimeError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    #[error("runtime unavailable: {0}")]
    Unavailable(String),
    #[error("capacity exceeded: {0}")]
    Capacity(String),
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("the selected store changed")]
    BindingChanged,
    #[error("store unavailable: {0}")]
    StoreUnavailable(String),
    #[error(transparent)]
    Domain(#[from] agentlaw_contracts::DomainError),
}

#[cfg(test)]
mod tests;
