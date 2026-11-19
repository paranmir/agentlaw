//! In-memory scheduling state shared with the model supervisor.

use super::source::Piece;
use crate::ports::ComponentState;
use agentlaw_search::vector_file::{SourcePosition, VectorFileIdentity};
use agentlaw_worker::supervisor::{IndexWork, ModelRequestId, ModelRequestIds};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum WorkKind {
    Current,
    History { subject: String },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct LaneKey {
    pub binding: String,
    pub kind: WorkKind,
}
impl LaneKey {
    pub fn current(binding: &str) -> Self {
        Self {
            binding: binding.to_owned(),
            kind: WorkKind::Current,
        }
    }

    pub fn history(binding: &str, subject: &str) -> Self {
        Self {
            binding: binding.to_owned(),
            kind: WorkKind::History {
                subject: subject.to_owned(),
            },
        }
    }
}

#[derive(Default)]
pub(super) struct Backoff {
    failures: u32,
    pub until: Option<Instant>,
}

impl Backoff {
    pub fn fail(&mut self, now: Instant) {
        let seconds = (1_u64 << self.failures.min(5)).min(30);
        self.failures = self.failures.saturating_add(1);
        self.until = Some(now + Duration::from_secs(seconds));
    }

    pub fn reset(&mut self) {
        self.failures = 0;
        self.until = None;
    }

    pub fn ready(&self, now: Instant) -> bool {
        self.until.is_none_or(|at| at <= now)
    }
}

pub(super) struct PendingWindow {
    pub piece: Piece,
    pub ranges: Vec<(u32, u32)>,
    pub next_range: usize,
}

pub(super) struct Lane {
    pub token: u64,
    pub generation: u64,
    pub work_revision: u64,
    pub queue: VecDeque<Piece>,
    pub running: HashMap<ModelRequestId, (u64, Piece)>,
    pub results: VecDeque<(Piece, Arc<[f32]>)>,
    pub committing: usize,
    pub pending_window: Option<PendingWindow>,
    pub retry: Backoff,
    pub retry_piece: Option<(String, String, String)>,
    pub writer_retry: Backoff,
    pub ack: Option<SourcePosition>,
    pub complete: bool,
    pub state: ComponentState,
    pub reason: Option<String>,
    pub dirty: bool,
    pub active: Option<ActiveFile>,
    pub repair: Option<Repair>,
    pub cleaned_dir: Option<std::path::PathBuf>,
    pub transition: Option<String>,
    pub cleanup_error: Option<String>,
    pub durability_reason: Option<String>,
    pub quarantined: bool,
    pub active_quarantined: bool,
    pub io_gate: Arc<std::sync::Mutex<()>>,
    pub readers: Arc<Readers>,
    pub history_target: Option<agentlaw_storage::published::SourcePosition>,
    pub history_started: bool,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
pub(super) struct Readers {
    pub count: Mutex<usize>,
    pub changed: Condvar,
    pub retired: Mutex<Option<std::path::PathBuf>>,
    pub wake: Mutex<Option<std::sync::Weak<super::Shared>>>,
}

pub(super) struct Repair {
    pub id: String,
    pub state: super::SemanticRepairState,
    pub published: bool,
    pub ack: Option<SourcePosition>,
    pub reason: Option<String>,
}

impl Readers {
    pub fn retire(&self, path: std::path::PathBuf) {
        *self.retired.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    }
}

pub(super) struct ReaderGuard(pub Arc<Readers>);
impl Drop for ReaderGuard {
    fn drop(&mut self) {
        let mut count = self
            .0
            .count
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *count -= 1;
        self.0.changed.notify_all();
        drop(count);
        if let Some(shared) = self
            .0
            .wake
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|w| w.upgrade())
        {
            let _state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            shared.changed.notify_all();
        }
    }
}

#[derive(Clone)]
pub(super) struct ActiveFile {
    pub path: std::path::PathBuf,
    pub identity: VectorFileIdentity,
}

impl Default for Lane {
    fn default() -> Self {
        Self {
            token: 0,
            generation: 0,
            work_revision: 0,
            queue: VecDeque::new(),
            running: HashMap::new(),
            results: VecDeque::new(),
            committing: 0,
            pending_window: None,
            retry: Backoff::default(),
            retry_piece: None,
            writer_retry: Backoff::default(),
            ack: None,
            complete: false,
            state: ComponentState::Starting,
            reason: None,
            dirty: true,
            active: None,
            repair: None,
            cleaned_dir: None,
            transition: None,
            cleanup_error: None,
            durability_reason: None,
            quarantined: false,
            active_quarantined: false,
            io_gate: Arc::new(std::sync::Mutex::new(())),
            readers: Arc::new(Readers::default()),
            history_target: None,
            history_started: false,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl Lane {
    /// A publication supersedes all work that may have been read before it.
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.queue.clear();
        self.pending_window = None;
        self.running.clear();
        self.results.clear();
        self.committing = 0;
        self.retry.reset();
        self.retry_piece = None;
    }

    pub fn occupied(&self) -> usize {
        self.running.len() + self.results.len() + self.committing
    }
}

type BlockedRevisions = HashSet<(String, String, String)>;

#[derive(Default)]
pub(super) struct Schedule {
    pub blocked: BTreeMap<LaneKey, BlockedRevisions>,
    pub model: Option<String>,
    pub lanes: BTreeMap<LaneKey, Lane>,
    pub(super) last: Option<LaneKey>,
}

impl Schedule {
    #[cfg(test)]
    pub fn next(
        &mut self,
        digest: &str,
        ids: &ModelRequestIds,
        capacity: usize,
        now: Instant,
    ) -> Option<IndexWork> {
        self.next_task(digest, ids, capacity, false, now)
    }

    pub fn next_task(
        &mut self,
        digest: &str,
        ids: &ModelRequestIds,
        capacity: usize,
        split_windows: bool,
        now: Instant,
    ) -> Option<IndexWork> {
        if self.model.as_deref() != Some(digest) {
            return None;
        }
        let names: Vec<LaneKey> = self.lanes.keys().cloned().collect();
        if names.is_empty() {
            return None;
        }
        let start = self
            .last
            .as_ref()
            .and_then(|last| names.iter().position(|s| s > last))
            .unwrap_or(0);
        for kind in [false, true] {
            for offset in 0..names.len() {
                let name = &names[(start + offset) % names.len()];
                if matches!(name.kind, WorkKind::History { .. }) != kind {
                    continue;
                }
                let lane = self.lanes.get_mut(name).unwrap();
                if lane.quarantined
                    || lane.occupied() >= capacity
                    || !lane.retry.ready(now)
                    || lane.queue.is_empty()
                {
                    continue;
                }
                let id = ids.next()?;
                let piece = lane.queue.pop_front().unwrap();
                let text: Arc<str> = Arc::from(piece.text.as_str());
                let task = if split_windows && piece.section.matches(':').count() == 1 {
                    agentlaw_worker::supervisor::IndexTask::Split(text)
                } else {
                    agentlaw_worker::supervisor::IndexTask::Embed(text)
                };
                lane.running.insert(id, (lane.generation, piece));
                lane.work_revision = lane.work_revision.wrapping_add(1);
                self.last = Some(name.clone());
                return Some(IndexWork { id, task });
            }
        }
        None
    }
}
