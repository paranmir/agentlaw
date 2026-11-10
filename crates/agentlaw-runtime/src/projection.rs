//! Event-driven maintenance of a binding's exact and lexical projection.
use crate::ports::{
    ComponentId, ComponentKind, ComponentState, ComponentStatus, HealthSink, RuntimeError,
    RuntimeResult,
};
use agentlaw_contracts::RequestControl;
use agentlaw_flows::exact::{ExactIndexWriter, ProjectionGenerations};
use agentlaw_storage::published::{OwnedPublishedReader, SourcePosition};
use std::{
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveGeneration {
    pub id: String,
    pub epoch: String,
    pub exact_file: PathBuf,
    pub lexical_file: PathBuf,
}
pub trait ActivePointerSink: Send + Sync {
    fn persist_active(&self, binding: &str, generation: &ActiveGeneration) -> RuntimeResult<()>;
}
pub trait RetiredFileSink: Send + Sync {
    fn retire(&self, binding: &str, files: &[PathBuf]) -> RuntimeResult<()>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionState {
    Starting,
    Running,
    Blocked,
    Recovering,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionStatus {
    pub state: ProjectionState,
    pub through: Option<SourcePosition>,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExactPreparation {
    Complete {
        through: SourcePosition,
    },
    Incomplete {
        target: SourcePosition,
        through: Option<SourcePosition>,
        reason: String,
    },
}

pub trait ProjectionClock: Send + Sync {
    fn now(&self) -> Instant;
}
struct SystemClock;
impl ProjectionClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
pub struct ProjectionConfig {
    pub clock: Arc<dyn ProjectionClock>,
    pub retry_initial: Duration,
    pub retry_max: Duration,
}
impl ProjectionConfig {
    pub fn new() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            retry_initial: Duration::from_secs(1),
            retry_max: Duration::from_secs(30),
        }
    }
}
impl Default for ProjectionConfig {
    fn default() -> Self {
        Self::new()
    }
}
struct State {
    status: ProjectionStatus,
    wake: bool,
    stopping: bool,
    stopped: bool,
}
struct Inner {
    state: Mutex<State>,
    changed: Condvar,
    generations: Arc<ProjectionGenerations>,
    cancel: RequestControl,
    thread: Mutex<Option<JoinHandle<()>>>,
    binding: String,
    health: HealthSink,
}
#[derive(Clone)]
pub struct ProjectionHandle {
    inner: Arc<Inner>,
}
impl Inner {
    fn publish(
        &self,
        state: ProjectionState,
        through: Option<SourcePosition>,
        reason: Option<String>,
    ) {
        let mut data = self.state.lock().unwrap_or_else(|e| e.into_inner());
        data.status = ProjectionStatus {
            state,
            through,
            reason: reason.clone(),
        };
        self.health.publish(
            ComponentId {
                binding: Some(self.binding.clone()),
                kind: ComponentKind::Exact,
            },
            ComponentStatus {
                state: match state {
                    ProjectionState::Starting => ComponentState::Starting,
                    ProjectionState::Running => ComponentState::Ready,
                    ProjectionState::Blocked => ComponentState::Degraded,
                    ProjectionState::Recovering => ComponentState::Recovering,
                    ProjectionState::Failed => ComponentState::Failed,
                },
                observed_at: SystemTime::now(),
                reason,
            },
        );
        self.changed.notify_all();
    }
}
impl ProjectionHandle {
    pub fn start(
        binding: String,
        source: OwnedPublishedReader,
        active: ActiveGeneration,
        pointer: Arc<dyn ActivePointerSink>,
        retired: Arc<dyn RetiredFileSink>,
        health: HealthSink,
        config: ProjectionConfig,
    ) -> RuntimeResult<Self> {
        let opened = ExactIndexWriter::open(&active.exact_file);
        let corrupt = matches!(&opened, Err(e) if e.code == "exact_index_corrupt");
        let unavailable = matches!(&opened, Err(e) if e.code == "exact_index_unavailable");
        let registry = if corrupt {
            ProjectionGenerations::open_for_recovery(
                &active.id,
                &active.epoch,
                &active.exact_file,
                &active.lexical_file,
            )?
        } else if unavailable {
            ProjectionGenerations::open_for_recovery(
                &active.id,
                &active.epoch,
                &active.exact_file,
                &active.lexical_file,
            )?
        } else {
            ProjectionGenerations::open(
                &active.id,
                &active.epoch,
                &active.exact_file,
                &active.lexical_file,
            )?
        };
        let initial_error = if unavailable {
            opened.as_ref().err().map(ToString::to_string)
        } else {
            None
        };
        let writer = match opened {
            Ok(writer) => Some(writer),
            Err(_) if corrupt || unavailable => None,
            Err(error) => return Err(error.into()),
        };
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                status: ProjectionStatus {
                    state: ProjectionState::Starting,
                    through: None,
                    reason: None,
                },
                wake: initial_error.is_none(),
                stopping: false,
                stopped: false,
            }),
            changed: Condvar::new(),
            generations: Arc::new(registry),
            cancel: RequestControl::default(),
            thread: Mutex::new(None),
            binding,
            health,
        });
        let initial_retry_at = initial_error
            .as_ref()
            .map(|_| config.clock.now() + config.retry_initial.max(Duration::from_millis(1)));
        if let Some(reason) = initial_error.clone() {
            inner.publish(ProjectionState::Blocked, None, Some(reason));
        } else {
            inner.publish(ProjectionState::Starting, None, None);
        }
        let worker = inner.clone();
        let handle = thread::Builder::new()
            .name("exact-maintainer".into())
            .spawn(move || {
                let task = worker.clone();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(
                        task,
                        source,
                        active,
                        writer,
                        pointer,
                        retired,
                        config,
                        corrupt,
                        initial_error,
                        initial_retry_at,
                    )
                }))
                .is_err()
                {
                    let through = worker
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .status
                        .through
                        .clone();
                    worker.publish(
                        ProjectionState::Failed,
                        through,
                        Some("exact maintainer panicked".into()),
                    );
                    let mut state = worker.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.stopped = true;
                    worker.changed.notify_all();
                }
            })
            .map_err(|e| RuntimeError::Unavailable(e.to_string()))?;
        *inner.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        Ok(Self { inner })
    }
    pub fn generations(&self) -> Arc<ProjectionGenerations> {
        self.inner.generations.clone()
    }
    pub fn wake(&self) {
        let mut s = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        s.wake = true;
        self.inner.changed.notify_all();
    }
    pub fn status(&self) -> ProjectionStatus {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn wait_exact(
        &self,
        target: &SourcePosition,
        control: &RequestControl,
    ) -> ExactPreparation {
        control.phase("waiting_exact");
        let mut s = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let status = &s.status;
            let reason = if control.check().is_err() {
                Some("cancelled".to_string())
            } else if s.stopping || s.stopped {
                Some("stopped".into())
            } else if let Some(pos) = &status.through {
                if pos.epoch != target.epoch {
                    Some("source epoch mismatch".into())
                } else {
                    None
                }
            } else {
                None
            };
            let reason = reason.or_else(|| match status.state {
                ProjectionState::Blocked
                | ProjectionState::Recovering
                | ProjectionState::Failed => Some(
                    status
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("{:?}", status.state)),
                ),
                _ => None,
            });
            if let Some(reason) = reason {
                return ExactPreparation::Incomplete {
                    target: target.clone(),
                    through: status.through.clone(),
                    reason,
                };
            }
            if let Some(through) = &status.through {
                if through.sequence >= target.sequence {
                    return ExactPreparation::Complete {
                        through: through.clone(),
                    };
                }
            }
            s = self
                .inner
                .changed
                .wait_timeout(s, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    pub fn shutdown(&self, timeout: Duration) -> bool {
        self.inner.cancel.cancel.store(true, Ordering::Release);
        let deadline = Instant::now() + timeout;
        let mut s = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        s.stopping = true;
        self.inner.changed.notify_all();
        while !s.stopped {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            s = self
                .inner
                .changed
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(s);
        if let Some(t) = self
            .inner
            .thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = t.join();
        }
        true
    }
}
fn projection_base(path: &Path) -> PathBuf {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return path.to_path_buf();
    };
    match name
        .strip_suffix("-wal")
        .or_else(|| name.strip_suffix("-shm"))
    {
        Some(base) if base.ends_with(".sqlite") => path.with_file_name(base),
        _ => path.to_path_buf(),
    }
}

fn add_retired_file(pending: &mut Vec<PathBuf>, path: PathBuf) {
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        if sidecar.is_file() {
            pending.push(sidecar);
        }
    }
    pending.push(path);
}

struct MaintainerWriter {
    generations: Arc<ProjectionGenerations>,
    current: Option<ExactIndexWriter>,
}
impl Drop for MaintainerWriter {
    fn drop(&mut self) {
        let _ = self.generations.mark_untrusted();
        drop(self.current.take());
    }
}

fn run(
    inner: Arc<Inner>,
    source: OwnedPublishedReader,
    mut active: ActiveGeneration,
    writer: Option<ExactIndexWriter>,
    pointer: Arc<dyn ActivePointerSink>,
    retired: Arc<dyn RetiredFileSink>,
    config: ProjectionConfig,
    mut recovery: bool,
    initial_error: Option<String>,
    initial_retry_at: Option<Instant>,
) {
    let mut writer = MaintainerWriter {
        generations: inner.generations.clone(),
        current: writer,
    };
    let mut retry = config.retry_initial.max(Duration::from_millis(1));
    let maximum = config.retry_max.max(retry);
    let mut retry_at = initial_retry_at;
    let mut cleanup_at: Option<Instant> = None;
    let mut sync_pending = initial_error.is_some();
    let mut reopen = initial_error.is_some();
    if reopen {
        retry = (retry * 2).min(maximum);
    }
    let mut failures = 0u32;
    let mut recovery_reason = "exact_index_corrupt: exact projection corrupt".to_owned();
    let mut pending: Vec<PathBuf> = active
        .exact_file
        .parent()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            let base = projection_base(path);
            let name = base.file_name().and_then(|v| v.to_str()).unwrap_or("");
            (name.starts_with("exact-") || name.starts_with("lexical-"))
                && name.ends_with(".sqlite")
                && agentlaw_flows::exact::projection_path_key(&base)
                    != agentlaw_flows::exact::projection_path_key(&active.exact_file)
                && agentlaw_flows::exact::projection_path_key(&base)
                    != agentlaw_flows::exact::projection_path_key(&active.lexical_file)
        })
        .collect();
    let mut retired_generations = 0usize;
    loop {
        let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let needs_sync = loop {
            sync_pending |= state.wake;
            state.wake = false;
            if state.stopping {
                break false;
            }
            let now = config.clock.now();
            let sync_ready = sync_pending && retry_at.is_none_or(|deadline| now >= deadline);
            let cleanup_ready = cleanup_at.is_some_and(|deadline| now >= deadline);
            if sync_ready || cleanup_ready {
                break sync_ready;
            }
            let deadline = match (if sync_pending { retry_at } else { None }, cleanup_at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            };
            state = if let Some(deadline) = deadline {
                inner
                    .changed
                    .wait_timeout(state, deadline.saturating_duration_since(now))
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            } else {
                inner.changed.wait(state).unwrap_or_else(|e| e.into_inner())
            };
        };
        if state.stopping {
            break;
        }
        if needs_sync {
            sync_pending = false;
            retry_at = None;
        }
        if cleanup_at.is_some_and(|deadline| config.clock.now() >= deadline) {
            cleanup_at = None;
        }
        drop(state);
        let mut through = inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .through
            .clone();
        if reopen && needs_sync {
            match ExactIndexWriter::open(&active.exact_file) {
                Ok(opened) => {
                    writer.current = Some(opened);
                    if inner.generations.mark_trusted().is_ok() {
                        reopen = false;
                    } else {
                        sync_pending = true;
                        retry_at = Some(config.clock.now() + retry);
                        retry = (retry * 2).min(maximum);
                        inner.publish(
                            ProjectionState::Blocked,
                            through.clone(),
                            Some("projection registry unavailable".into()),
                        );
                    }
                }
                Err(error) if error.code == "exact_index_corrupt" => {
                    reopen = false;
                    recovery = true;
                    recovery_reason = format!("{}: {}", error.code, error.message);
                    let _ = inner.generations.mark_untrusted();
                    inner.publish(
                        ProjectionState::Recovering,
                        through.clone(),
                        Some(error.to_string()),
                    );
                }
                Err(error) => {
                    sync_pending = true;
                    retry_at = Some(config.clock.now() + retry);
                    retry = (retry * 2).min(maximum);
                    inner.publish(
                        ProjectionState::Blocked,
                        through.clone(),
                        Some(error.to_string()),
                    );
                }
            }
        }
        if recovery && needs_sync {
            inner.publish(
                ProjectionState::Recovering,
                through.clone(),
                Some(recovery_reason.clone()),
            );
            let _ = inner.generations.mark_untrusted();
            let dir = active
                .exact_file
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            let result = ExactIndexWriter::rebuild_shadow_in(dir, &source, &inner.cancel)
                .map_err(RuntimeError::from)
                .and_then(|shadow| inner.generations.stage(shadow).map_err(RuntimeError::from))
                .and_then(|staged| {
                    let next = ActiveGeneration {
                        id: staged.generation_id().to_owned(),
                        epoch: staged.epoch().to_owned(),
                        exact_file: staged.exact_file().to_owned(),
                        lexical_file: staged.lexical_file().to_owned(),
                    };
                    let opened = match ExactIndexWriter::open(&next.exact_file) {
                        Ok(opened) => opened,
                        Err(error) => {
                            let (a, b) = staged.into_files();
                            add_retired_file(&mut pending, a);
                            add_retired_file(&mut pending, b);
                            return Err(error.into());
                        }
                    };
                    if let Err(error) = pointer.persist_active(&inner.binding, &next) {
                        let (a, b) = staged.into_files();
                        add_retired_file(&mut pending, a);
                        add_retired_file(&mut pending, b);
                        return Err(error);
                    }
                    writer.current = Some(opened);
                    inner.generations.commit(staged)?;
                    active = next;
                    retired_generations += 1;
                    recovery = false;
                    failures = 0;
                    Ok(())
                });
            match result {
                Ok(()) => {
                    recovery = false;
                    failures = 0;
                    retry_at = None;
                    retry = config.retry_initial.max(Duration::from_millis(1));
                }
                Err(e) => {
                    failures += 1;
                    let state = if failures >= 3 {
                        ProjectionState::Failed
                    } else {
                        ProjectionState::Blocked
                    };
                    sync_pending = true;
                    retry_at = Some(config.clock.now() + retry);
                    retry = (retry * 2).min(maximum);
                    inner.publish(
                        state,
                        through.clone(),
                        Some(format!("{recovery_reason}; {e}")),
                    );
                }
            }
        }
        if !recovery && !reopen && needs_sync {
            let current = writer.current.as_mut().expect("active writer");
            match current
                .validate_active()
                .and_then(|()| current.synchronize(&source, &inner.cancel))
            {
                Ok(pos) => {
                    through = Some(pos);
                    inner.publish(ProjectionState::Running, through.clone(), None);
                    retry_at = None;
                    retry = config.retry_initial.max(Duration::from_millis(1));
                }
                Err(e)
                    if matches!(
                        e.code.as_str(),
                        "exact_index_corrupt" | "source_epoch_changed" | "source_coverage_lost"
                    ) =>
                {
                    let _ = inner.generations.mark_untrusted();
                    writer.current = None;
                    recovery = true;
                    recovery_reason = format!("{}: {}", e.code, e.message);
                    inner.publish(
                        ProjectionState::Recovering,
                        through.clone(),
                        Some(recovery_reason.clone()),
                    );
                    sync_pending = true;
                }
                Err(e) => {
                    if e.code == "exact_index_unavailable" {
                        let _ = inner.generations.mark_untrusted();
                        writer.current = None;
                        reopen = true;
                    }
                    sync_pending = true;
                    retry_at = Some(config.clock.now() + retry);
                    retry = (retry * 2).min(maximum);
                    inner.publish(
                        ProjectionState::Blocked,
                        through.clone(),
                        Some(e.to_string()),
                    );
                }
            }
        }
        for (a, b) in inner.generations.take_retired() {
            retired_generations = retired_generations.saturating_sub(1);
            add_retired_file(&mut pending, a);
            add_retired_file(&mut pending, b);
        }
        let exact_key = agentlaw_flows::exact::projection_path_key(&active.exact_file);
        let lexical_key = agentlaw_flows::exact::projection_path_key(&active.lexical_file);
        let mut seen = std::collections::HashSet::new();
        pending.retain(|p| {
            let base_key = agentlaw_flows::exact::projection_path_key(&projection_base(p));
            let file_key = agentlaw_flows::exact::projection_path_key(p);
            base_key != exact_key && base_key != lexical_key && seen.insert(file_key)
        });
        let ready: Vec<_> = pending
            .iter()
            .filter(|path| {
                agentlaw_flows::exact::projection_file_unreferenced(&projection_base(path))
            })
            .cloned()
            .collect();
        if !ready.is_empty() && retired.retire(&inner.binding, &ready).is_ok() {
            let keys: std::collections::HashSet<_> = ready
                .iter()
                .map(|path| agentlaw_flows::exact::projection_path_key(path))
                .collect();
            pending
                .retain(|path| !keys.contains(&agentlaw_flows::exact::projection_path_key(path)));
        }
        if (!pending.is_empty() || retired_generations > 0) && cleanup_at.is_none() {
            cleanup_at =
                Some(config.clock.now() + config.retry_initial.max(Duration::from_millis(1)));
        }
    }
    drop(writer);
    let mut s = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    s.stopped = true;
    inner.changed.notify_all();
}

#[cfg(test)]
mod tests;
