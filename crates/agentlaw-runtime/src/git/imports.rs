//! In-memory progress and persisted import inventory.
use agentlaw_contracts::{DomainError, Result};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct ProgressRegistry {
    entries: Arc<Mutex<BTreeMap<(PathBuf, String), Progress>>>,
    lifetime: Arc<(
        Mutex<(BTreeSet<(PathBuf, String)>, BTreeSet<(PathBuf, String)>)>,
        Condvar,
    )>,
    scope: PathBuf,
}
pub struct ImportGuard {
    registry: ProgressRegistry,
    key: (PathBuf, String),
}
impl ImportGuard {
    pub fn completed(&self) {
        self.registry
            .lifetime
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .insert(self.key.clone());
    }
}
impl Drop for ImportGuard {
    fn drop(&mut self) {
        let mut state = self
            .registry
            .lifetime
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.0.remove(&self.key);
        self.registry.lifetime.1.notify_all();
    }
}
#[derive(Clone)]
struct Progress {
    phase: String,
    completed: u64,
    unit: String,
    elapsed: Duration,
    observed: Instant,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    InProgress,
    Completed,
    HandoffPending,
    ReviewWaiting,
    Incomplete,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportEntry {
    pub import_ref: String,
    pub status: ImportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}
impl ProgressRegistry {
    pub fn try_begin(&self, reference: &str) -> Option<ImportGuard> {
        let key = (self.scope.clone(), reference.to_owned());
        let mut state = self.lifetime.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.0.insert(key.clone()) {
            return None;
        }
        Some(ImportGuard {
            registry: self.clone(),
            key,
        })
    }
    pub fn scoped(&self, local: &Path) -> Self {
        Self {
            entries: self.entries.clone(),
            lifetime: self.lifetime.clone(),
            scope: local.to_owned(),
        }
    }
    pub fn begin(
        &self,
        reference: &str,
        allow_completed: bool,
        control: &agentlaw_contracts::RequestControl,
    ) -> Result<ImportGuard> {
        let key = (self.scope.clone(), reference.to_owned());
        let mut state = self.lifetime.0.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            control.check()?;
            if !allow_completed && state.1.contains(&key) {
                return Err(DomainError::new(
                    "import_already_confirmed",
                    "The completed review is closed.",
                ));
            }
            if state.0.insert(key.clone()) {
                return Ok(ImportGuard {
                    registry: self.clone(),
                    key,
                });
            }
            state = self
                .lifetime
                .1
                .wait_timeout(state, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    pub fn update(
        &self,
        import_ref: &str,
        phase: &str,
        completed: u64,
        unit: &str,
        elapsed: Duration,
    ) -> Result<()> {
        self.entries
            .lock()
            .map_err(|_| {
                DomainError::new(
                    "import_progress_unavailable",
                    "Import progress is unavailable.",
                )
            })?
            .insert(
                (self.scope.clone(), import_ref.to_owned()),
                Progress {
                    phase: phase.into(),
                    completed,
                    unit: unit.into(),
                    elapsed,
                    observed: Instant::now(),
                },
            );
        Ok(())
    }
    pub fn finish(&self, import_ref: &str) -> Result<()> {
        self.entries
            .lock()
            .map_err(|_| {
                DomainError::new(
                    "import_progress_unavailable",
                    "Import progress is unavailable.",
                )
            })?
            .remove(&(self.scope.clone(), import_ref.to_owned()));
        Ok(())
    }
}
fn record(path: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 16 * 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
pub fn list(local: &Path, progress: &ProgressRegistry) -> Result<Vec<ImportEntry>> {
    let scoped = progress
        .entries
        .lock()
        .map_err(|_| {
            DomainError::new(
                "import_progress_unavailable",
                "Import progress is unavailable.",
            )
        })?
        .clone();
    let mut found = BTreeMap::new();
    let root = local.join("imports");
    match std::fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                let id = entry.file_name().to_string_lossy().into_owned();
                if !entry.file_type().is_ok_and(|kind| kind.is_dir())
                    || uuid::Uuid::parse_str(&id).is_err()
                {
                    continue;
                }
                let dir = entry.path();
                let completed = record(&dir.join("completed.json"))
                    .is_some_and(super::git_ops::valid_completion_record);
                let handoff = record(&dir.join("handoff.json"))
                    .is_some_and(|v| super::git_ops::valid_handoff_record(v, &id));
                let review = record(&dir.join("review.json"))
                    .is_some_and(|v| super::git_ops::valid_review_record(v, &id));
                let status = if completed {
                    ImportStatus::Completed
                } else if dir.join("completed.json").exists() {
                    ImportStatus::Incomplete
                } else if handoff {
                    ImportStatus::HandoffPending
                } else if dir.join("handoff.json").exists() {
                    ImportStatus::Incomplete
                } else if review {
                    ImportStatus::ReviewWaiting
                } else {
                    ImportStatus::Incomplete
                };
                found.insert(
                    id.clone(),
                    ImportEntry {
                        import_ref: id,
                        status,
                        phase: None,
                        completed: None,
                        unit: None,
                        elapsed_ms: None,
                    },
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(DomainError::new(
                "import_list_unavailable",
                error.to_string(),
            ))
        }
    }
    for ((scope, id), item) in scoped {
        if scope != progress.scope
            && !(scope.as_os_str().is_empty() && progress.scope.as_os_str().is_empty())
        {
            continue;
        }
        found.insert(
            id.clone(),
            ImportEntry {
                import_ref: id,
                status: ImportStatus::InProgress,
                phase: Some(item.phase),
                completed: Some(item.completed),
                unit: Some(item.unit),
                elapsed_ms: Some(
                    item.elapsed
                        .saturating_add(item.observed.elapsed())
                        .as_millis()
                        .min(u64::MAX as u128) as u64,
                ),
            },
        );
    }
    Ok(found.into_values().collect())
}
#[cfg(test)]
mod tests;
