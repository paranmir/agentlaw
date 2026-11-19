//! One serialized owner for model-process transitions. Native inference is never in this process.
use crate::execution::SelectionCache;
use crate::{
    model_child::{self, Input, Launch, Output, Result},
    EmbeddingError, RuntimeConfig, WorkerState,
};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

/// One number per model computation, issued by the host for both queries and indexing and
/// never reused. A result carrying an unknown number is discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelRequestId(pub u64);

/// Host-lifetime request number source shared by queries and index feeders.
#[derive(Clone, Default)]
pub struct ModelRequestIds {
    counter: Arc<std::sync::Mutex<u64>>,
}
impl ModelRequestIds {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn next(&self) -> Option<ModelRequestId> {
        let mut counter = self.counter.lock().unwrap();
        *counter = counter.checked_add(1)?;
        Some(ModelRequestId(*counter))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleAction {
    None,
    Start,
}

#[derive(Default)]
struct ModelLifecycle {
    child: bool,
    stopping: bool,
    failures: u32,
    retry_at: Option<Instant>,
    ready_since: Option<Instant>,
    cause: Option<String>,
}
impl ModelLifecycle {
    fn action(&mut self, now: Instant, demand: bool, configured: bool) -> LifecycleAction {
        if !demand || !configured {
            return LifecycleAction::None;
        }
        if !self.child && !self.stopping && self.retry_at.is_none_or(|at| now >= at) {
            return LifecycleAction::Start;
        }
        LifecycleAction::None
    }
    fn started(&mut self) {
        self.child = true;
        self.stopping = false;
        self.retry_at = None;
        self.ready_since = None;
    }
    fn ready(&mut self, now: Instant) {
        self.ready_since = Some(now);
        self.cause = None;
    }
    fn exited(&mut self, now: Instant, cause: String) {
        if self
            .ready_since
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(60))
        {
            self.failures = 0;
        }
        self.child = false;
        self.stopping = false;
        self.ready_since = None;
        self.cause = Some(cause);
        let seconds = (1_u64 << self.failures.min(5)).min(30);
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(now + Duration::from_secs(seconds));
    }
    fn stopped(&mut self) {
        self.child = false;
        self.stopping = false;
        self.ready_since = None;
    }
}

#[derive(Debug, Clone)]
pub struct IndexWork {
    pub id: ModelRequestId,
    pub task: IndexTask,
}

#[derive(Debug, Clone)]
pub enum IndexTask {
    Split(Arc<str>),
    Embed(Arc<str>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum IndexResult {
    Pieces(Vec<(u32, u32)>),
    Vector(Vec<f32>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexWorkStatus {
    pub runnable: bool,
    pub next_retry_at: Option<Instant>,
    pub awaiting_model: bool,
}

/// Implemented by the runtime and called by the model supervisor.
/// Calls handle RAM values only and must not wait for SQL or files.
pub trait IndexWorkPort: Send + Sync {
    fn model_ready(&self, model_digest: &str);
    fn next_work(&self, model_digest: &str) -> Option<IndexWork>;
    fn finish(
        &self,
        id: ModelRequestId,
        outcome: std::result::Result<IndexResult, crate::EmbeddingError>,
    );
    fn status(&self) -> IndexWorkStatus;
}

/// A RAM-only view of the current model child.
#[derive(Clone, Debug)]
pub struct ModelSnapshot {
    pub state: WorkerState,
    pub incarnation: Option<String>,
    pub model_digest: Option<String>,
    pub cause: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryVector {
    pub model_digest: String,
    pub vector: Vec<f32>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryError {
    Busy,
    Unavailable(String),
    Cancelled,
    Failed(String),
}

struct QueryFlight {
    key: (String, [u8; 32]),
    text: String,
    waiters: usize,
    running: bool,
    result: Option<std::result::Result<QueryVector, QueryError>>,
}
#[derive(Default)]
struct QueryQueue {
    flights: HashMap<ModelRequestId, QueryFlight>,
    keys: HashMap<(String, [u8; 32]), ModelRequestId>,
    pending: VecDeque<ModelRequestId>,
}
#[allow(dead_code)]
enum QueryOperation {
    Prefix,
    Split,
    Embed,
}

impl QueryQueue {
    #[allow(dead_code)]
    fn submit_operation(
        &mut self,
        _operation: QueryOperation,
        _model: &str,
        _original_text: &str,
        _transport_text: &str,
        _ids: &ModelRequestIds,
    ) -> std::result::Result<ModelRequestId, QueryError> {
        Err(QueryError::Failed(
            "model query operation is not implemented".into(),
        ))
    }

    fn submit(
        &mut self,
        model: &str,
        text: &str,
        ids: &ModelRequestIds,
    ) -> std::result::Result<ModelRequestId, QueryError> {
        let key = (model.to_string(), Sha256::digest(text.as_bytes()).into());
        if let Some(id) = self.keys.get(&key) {
            let flight = self.flights.get_mut(id).unwrap();
            flight.waiters += 1;
            return Ok(*id);
        }
        if self.keys.len() >= 64 || text.len() > 4 * 1024 * 1024 {
            return Err(QueryError::Busy);
        }
        let id = ids
            .next()
            .ok_or_else(|| QueryError::Failed("model request ID exhausted".into()))?;
        self.flights.insert(
            id,
            QueryFlight {
                key: key.clone(),
                text: text.into(),
                waiters: 1,
                running: false,
                result: None,
            },
        );
        self.keys.insert(key, id);
        self.pending.push_back(id);
        Ok(id)
    }
    fn take(&mut self) -> Option<ModelRequestId> {
        while let Some(id) = self.pending.pop_front() {
            if let Some(flight) = self.flights.get_mut(&id) {
                flight.running = true;
                return Some(id);
            }
        }
        None
    }
    fn release(&mut self, id: ModelRequestId) {
        if let Some(flight) = self.flights.get_mut(&id) {
            flight.waiters -= 1;
            if flight.waiters == 0 && (!flight.running || flight.result.is_some()) {
                self.remove(id);
            }
        }
    }
    fn remove(&mut self, id: ModelRequestId) {
        if let Some(flight) = self.flights.remove(&id) {
            self.keys.remove(&flight.key);
        }
        self.pending.retain(|queued| *queued != id);
    }
    fn complete(
        &mut self,
        id: ModelRequestId,
        result: std::result::Result<QueryVector, QueryError>,
    ) {
        if let Some(flight) = self.flights.get_mut(&id) {
            if flight.result.is_some() {
                return;
            }
            flight.result = Some(result);
            if flight.waiters == 0 {
                self.remove(id);
            }
        }
    }
    fn fail_all(&mut self, cause: &str) {
        self.pending.clear();
        let ids: Vec<_> = self.flights.keys().copied().collect();
        for id in ids {
            self.complete(id, Err(QueryError::Unavailable(cause.into())));
        }
    }
}
fn validate_query_vector(
    model_digest: String,
    vector: Vec<f32>,
) -> std::result::Result<QueryVector, QueryError> {
    if vector.len() != crate::DIMENSIONS
        || vector.iter().any(|x| !x.is_finite())
        || !vector.iter().any(|x| *x != 0.0)
    {
        return Err(QueryError::Failed(
            "model returned an invalid vector".into(),
        ));
    }
    Ok(QueryVector {
        model_digest,
        vector,
    })
}

enum RamWork {
    Query {
        id: ModelRequestId,
        text: String,
    },
    Index {
        id: ModelRequestId,
        task: IndexTask,
        origin: IndexOrigin,
    },
}

#[derive(Clone, PartialEq, Eq)]
struct IndexOrigin {
    incarnation: String,
    model_digest: String,
}

struct HeldIndex {
    work: IndexWork,
    origin: IndexOrigin,
}

impl RamWork {
    fn id(&self) -> ModelRequestId {
        match self {
            Self::Query { id, .. } | Self::Index { id, .. } => *id,
        }
    }
}
struct SupervisorState {
    snapshot: ModelSnapshot,
    warm: bool,
    stop: bool,
    loading_demand: bool,
    queries: QueryQueue,
    terminated: Option<String>,
}
struct SupervisorShared {
    state: std::sync::Mutex<SupervisorState>,
    signal: Arc<RamSignal>,
    changed: std::sync::Condvar,
    events: mpsc::Sender<Event>,
    wake_pending: AtomicBool,
}
// Wakeups are sent through the RAM supervisor's event channel.
struct RamSignal {
    events: mpsc::Sender<Event>,
}
impl RamSignal {
    fn notify(&self) {
        let _ = self.events.send(Event::Wake);
    }
}

/// Owns one model-child process. The host supplies the shared request ID source.
pub struct ModelSupervisor {
    shared: Arc<SupervisorShared>,
    thread: Option<std::thread::JoinHandle<std::result::Result<(), String>>>,
    ids: ModelRequestIds,
    stop_result: Option<std::result::Result<(), String>>,
}
impl ModelSupervisor {
    pub fn start(
        config: RuntimeConfig,
        index: Arc<dyn IndexWorkPort>,
        ids: ModelRequestIds,
    ) -> Self {
        let (events, incoming) = mpsc::channel();
        let shared = Arc::new(SupervisorShared {
            state: std::sync::Mutex::new(SupervisorState {
                snapshot: ModelSnapshot {
                    state: WorkerState::Stopped,
                    incarnation: None,
                    model_digest: None,
                    cause: None,
                },
                warm: false,
                stop: false,
                loading_demand: false,
                queries: QueryQueue::default(),
                terminated: None,
            }),
            signal: Arc::new(RamSignal {
                events: events.clone(),
            }),
            changed: std::sync::Condvar::new(),
            events,
            wake_pending: AtomicBool::new(false),
        });
        let worker = shared.clone();
        let thread = std::thread::spawn(move || {
            model_supervision_loop(config, index, worker, NativeProcess, Instant::now, incoming)
        });
        Self {
            shared,
            thread: Some(thread),
            ids,
            stop_result: None,
        }
    }
    pub fn snapshot(&self) -> ModelSnapshot {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .snapshot
            .clone()
    }
    pub fn embed_query(
        &self,
        text: &str,
        cancelled: &AtomicBool,
    ) -> std::result::Result<QueryVector, QueryError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(reason) = &state.terminated {
            return Err(QueryError::Unavailable(reason.clone()));
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(QueryError::Cancelled);
        }
        if state.snapshot.state != WorkerState::Ready {
            state.loading_demand = true;
            let reason = state
                .snapshot
                .cause
                .clone()
                .unwrap_or_else(|| "model loading".into());
            drop(state);
            self.wake();
            return Err(QueryError::Unavailable(reason));
        }
        let digest = state.snapshot.model_digest.clone().unwrap();
        let id = state.queries.submit(&digest, text, &self.ids)?;
        self.wake();
        loop {
            if cancelled.load(Ordering::Acquire) {
                state.queries.release(id);
                drop(state);
                self.wake();
                return Err(QueryError::Cancelled);
            }
            if let Some(result) = state
                .queries
                .flights
                .get(&id)
                .and_then(|q| q.result.clone())
            {
                state.queries.release(id);
                drop(state);
                self.wake();
                return result;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, Duration::from_millis(50))
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
    }
    pub fn set_warm(&self, warm: bool) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .warm = warm;
        self.wake();
    }
    /// Test-only observation of the currently applied host lease demand.
    #[doc(hidden)]
    pub fn is_warm(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .warm
    }
    /// Returns query loading demand retained by this supervisor.
    pub fn has_query_demand(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .loading_demand
    }
    /// Retains query demand when the caller could not submit to a supervisor.
    pub fn request_load(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.snapshot.state == WorkerState::Ready {
            return;
        }
        state.loading_demand = true;
        drop(state);
        self.wake();
    }
    pub fn wake(&self) {
        self.shared.signal.notify();
        if !self.shared.wake_pending.swap(true, Ordering::AcqRel) {
            let _ = self.shared.events.send(Event::Wake);
        }
    }
    /// Waits for the supervisor thread to stop, returning its reason when it has terminated.
    pub fn wait_terminated(&self, timeout: Duration) -> Option<String> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| state.terminated.is_none())
            .unwrap_or_else(|poison| poison.into_inner());
        state.terminated.clone()
    }
    /// Signals shutdown without waiting for the supervisor thread.
    pub fn request_stop(&self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .stop = true;
        self.wake();
    }
    pub fn stop(&mut self) -> std::result::Result<(), String> {
        if let Some(result) = &self.stop_result {
            return result.clone();
        }
        self.request_stop();
        let result = match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| "model supervisor thread panicked".to_string())
                .and_then(|result| result),
            None => Ok(()),
        };
        self.stop_result = Some(result.clone());
        result
    }
}
impl Drop for ModelSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct RamChild {
    child: OwnedChild,
    incarnation: String,
    model_digest: Option<String>,
    secret: String,
    control: Option<String>,
    last_control: Instant,
    input: mpsc::Sender<Input>,
    running: Option<RamWork>,
}

struct RamStartError {
    cause: String,
    owned: Option<RamChild>,
}
fn failed_ram_start(
    port: &impl WorkerProcessPort,
    mut child: OwnedChild,
    incarnation: String,
    secret: String,
    now: Instant,
    cause: String,
) -> RamStartError {
    let owned = if port.stop(&mut child).is_err() {
        let (input, _) = mpsc::channel();
        Some(RamChild {
            child,
            incarnation,
            model_digest: None,
            secret,
            control: None,
            last_control: now,
            input,
            running: None,
        })
    } else {
        None
    };
    RamStartError { cause, owned }
}

fn start_ram_child(
    port: &impl WorkerProcessPort,
    config: &RuntimeConfig,
    events: &mpsc::Sender<Event>,
    signal: &Arc<RamSignal>,
    now: Instant,
    cached: Option<(String, crate::execution::Selection)>,
) -> std::result::Result<RamChild, RamStartError> {
    std::fs::create_dir_all(&config.state_dir).map_err(|error| RamStartError {
        cause: error.to_string(),
        owned: None,
    })?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.state_dir.join("model.lock"))
        .map_err(|error| RamStartError {
            cause: error.to_string(),
            owned: None,
        })?;
    lock.try_lock_exclusive().map_err(|error| RamStartError {
        cause: if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
            "previous model child still running".into()
        } else {
            error.to_string()
        },
        owned: None,
    })?;
    FileExt::unlock(&lock).map_err(|error| RamStartError {
        cause: error.to_string(),
        owned: None,
    })?;
    let incarnation = uuid::Uuid::new_v4().to_string();
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut child = OwnedChild(
        port.spawn(config).map_err(|error| RamStartError {
            cause: error.to_string(),
            owned: None,
        })?,
        None,
    );
    #[cfg(windows)]
    {
        child.1 = Some(kill_on_close_job(&child).map_err(|error| RamStartError {
            cause: error.to_string(),
            owned: None,
        })?);
    }
    if let Err(error) = port.observe(&child, incarnation.clone(), events.clone(), Arc::new(|| {})) {
        return Err(failed_ram_start(
            port,
            child,
            incarnation,
            secret,
            now,
            error.to_string(),
        ));
    }
    let Some(mut stdin) = child.stdin.take() else {
        return Err(failed_ram_start(
            port,
            child,
            incarnation,
            secret,
            now,
            "missing model stdin".into(),
        ));
    };
    let Some(mut stdout) = child.stdout.take() else {
        return Err(failed_ram_start(
            port,
            child,
            incarnation,
            secret,
            now,
            "missing model stdout".into(),
        ));
    };
    let launch = Launch {
        incarnation: incarnation.clone(),
        parent_pid: std::process::id(),
        secret: secret.clone(),
        state: config.state_dir.clone(),
        assets: config.model.clone(),
        cached,
    };
    let (input, incoming) = mpsc::channel::<Input>();
    let tx = events.clone();
    let notify = signal.clone();
    let inc = incarnation.clone();
    std::thread::spawn(move || {
        let write_result = model_child::write(&mut stdin, &launch).and_then(|()| {
            while let Ok(message) = incoming.recv() {
                model_child::write(&mut stdin, &message)?;
            }
            Ok(())
        });
        if let Err(error) = write_result {
            let _ = tx.send(Event::Pipe(inc, error.to_string()));
            notify.notify();
        }
    });
    let tx = events.clone();
    let notify = signal.clone();
    let inc = incarnation.clone();
    std::thread::spawn(move || loop {
        match model_child::read(&mut stdout) {
            Ok(output) => {
                if tx.send(Event::Output(inc.clone(), output)).is_err() {
                    break;
                }
                notify.notify();
            }
            Err(error) => {
                let _ = tx.send(Event::Pipe(inc, error.to_string()));
                notify.notify();
                break;
            }
        }
    });
    Ok(RamChild {
        child,
        incarnation,
        model_digest: None,
        secret,
        control: None,
        last_control: now,
        input,
        running: None,
    })
}

fn model_supervision_loop(
    config: RuntimeConfig,
    index: Arc<dyn IndexWorkPort>,
    shared: Arc<SupervisorShared>,
    port: impl WorkerProcessPort,
    clock: impl Fn() -> Instant,
    incoming: mpsc::Receiver<Event>,
) -> std::result::Result<(), String> {
    let owned_index = std::sync::Mutex::new(std::collections::HashSet::new());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        model_supervision_run(
            config,
            index.clone(),
            shared.clone(),
            port,
            clock,
            incoming,
            &owned_index,
        )
    }))
    .unwrap_or_else(|panic| {
        let detail = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Err(format!("model supervisor panicked: {detail}"))
    });
    let reason = result
        .as_ref()
        .err()
        .cloned()
        .unwrap_or_else(|| "model supervisor stopped".into());
    {
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if result.is_err() {
            state.snapshot.state = WorkerState::Failed;
            state.snapshot.cause = Some(reason.clone());
            state.snapshot.incarnation = None;
            state.snapshot.model_digest = None;
        }
        state.queries.fail_all(&reason);
        shared.changed.notify_all();
    }
    let pending: Vec<_> = owned_index
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .drain()
        .collect();
    for id in pending {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            index.finish(id, Err(EmbeddingError::Unavailable(reason.clone())))
        }));
    }
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    state.terminated = Some(reason);
    shared.changed.notify_all();
    result
}

fn finish_owned_index(
    index: &dyn IndexWorkPort,
    owned: &std::sync::Mutex<std::collections::HashSet<ModelRequestId>>,
    id: ModelRequestId,
    result: std::result::Result<IndexResult, EmbeddingError>,
) {
    let was_owned = owned.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
    if was_owned {
        index.finish(id, result);
    }
}

fn return_stale_held(
    held: &mut Option<HeldIndex>,
    child: Option<&RamChild>,
    index: &dyn IndexWorkPort,
    owned: &std::sync::Mutex<std::collections::HashSet<ModelRequestId>>,
) {
    let stale = held
        .as_ref()
        .is_some_and(|held| child.is_none_or(|child| child.incarnation != held.origin.incarnation));
    if stale {
        let held = held.take().unwrap();
        finish_owned_index(
            index,
            owned,
            held.work.id,
            Err(EmbeddingError::Unavailable(
                "index work model incarnation ended".into(),
            )),
        );
    }
}

fn model_supervision_run(
    config: RuntimeConfig,
    index: Arc<dyn IndexWorkPort>,
    shared: Arc<SupervisorShared>,
    port: impl WorkerProcessPort,
    clock: impl Fn() -> Instant,
    incoming: mpsc::Receiver<Event>,
    owned_index: &std::sync::Mutex<std::collections::HashSet<ModelRequestId>>,
) -> std::result::Result<(), String> {
    let events = shared.events.clone();
    let mut lifecycle = ModelLifecycle::default();
    let mut child: Option<RamChild> = None;
    let mut held_index: Option<HeldIndex> = None;
    let mut cleanup_at: Option<Instant> = None;
    let mut cache: Option<SelectionCache> = None;
    let mut pending_event = None;
    // Only the most recently retired incarnation can still refine its failure.
    let mut retired_incarnation: Option<String> = None;
    loop {
        let stopping = {
            let state = shared
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.stop
        };
        if stopping {
            if let Some(mut active) = child.take() {
                if let Err(error) = port.stop(&mut active.child) {
                    let cause = format!("model child termination not confirmed: {error}");
                    if let Some(RamWork::Index { id, .. }) = active.running.take() {
                        finish_owned_index(
                            index.as_ref(),
                            owned_index,
                            id,
                            Err(EmbeddingError::Unavailable(cause.clone())),
                        );
                    }
                    let mut state = shared
                        .state
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    state.queries.fail_all(&cause);
                    state.snapshot.state = WorkerState::Failed;
                    state.snapshot.cause = Some(cause.clone());
                    shared.changed.notify_all();
                    return Err(cause);
                }
                if let Some(RamWork::Index { id, .. }) = active.running.take() {
                    finish_owned_index(
                        index.as_ref(),
                        owned_index,
                        id,
                        Err(EmbeddingError::Unavailable(
                            "model supervisor stopped".into(),
                        )),
                    );
                }
            }
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.queries.fail_all("model supervisor stopped");
            state.snapshot.state = WorkerState::Stopped;
            state.snapshot.incarnation = None;
            state.snapshot.model_digest = None;
            shared.changed.notify_all();
            return Ok(());
        }
        while let Some(event) = pending_event.take().or_else(|| incoming.try_recv().ok()) {
            if matches!(&event, Event::Wake) {
                shared.wake_pending.store(false, Ordering::Release);
                continue;
            }
            let Some(active) = child.as_mut() else {
                if let Event::Output(incarnation, Output::Unavailable { cause }) = event {
                    if retired_incarnation.as_deref() == Some(incarnation.as_str()) {
                        lifecycle.cause = Some(cause.clone());
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        state.snapshot.cause = Some(cause);
                        shared.changed.notify_all();
                        retired_incarnation = None;
                    }
                }
                continue;
            };
            let event_inc = match &event {
                Event::Output(i, _)
                | Event::Exit(i)
                | Event::Pipe(i, _)
                | Event::ControlLost(i, _) => i,
                Event::Wake => unreachable!(),
            };
            if event_inc != &active.incarnation {
                continue;
            }
            match event {
                Event::Wake => unreachable!(),
                Event::Output(
                    _,
                    Output::Hello {
                        incarnation,
                        control,
                    },
                ) if incarnation == active.incarnation => {
                    active.control = Some(control);
                }
                Event::Output(_, Output::Ready { model }) => {
                    lifecycle.ready(clock());
                    active.model_digest = Some(model.clone());
                    let mut state = shared
                        .state
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    state.snapshot.state = WorkerState::Ready;
                    state.snapshot.model_digest = Some(model.clone());
                    state.snapshot.cause = None;
                    state.loading_demand = false;
                    drop(state);
                    index.model_ready(&model);
                }
                Event::Output(
                    _,
                    Output::Selection {
                        fingerprint: Some(fingerprint),
                        selection,
                    },
                ) => {
                    if let Some(cache) = &cache {
                        cache.accept(fingerprint, selection);
                    }
                }
                Event::Output(_, Output::Pieces { id, .. }) => {
                    let id = ModelRequestId(id);
                    if matches!(&active.running, Some(RamWork::Index { id: running, task: IndexTask::Split(_), .. }) if *running == id)
                    {
                        active.running.take();
                        finish_owned_index(
                            index.as_ref(),
                            owned_index,
                            id,
                            Err(EmbeddingError::Failed(
                                "split completion is not implemented".into(),
                            )),
                        );
                        shared.signal.notify();
                    }
                }
                Event::Output(_, Output::Result { id, result, .. }) => {
                    let id = ModelRequestId(id);
                    if active.running.as_ref().is_none_or(|work| work.id() != id) {
                        continue;
                    }
                    let work = active.running.take().unwrap();
                    let mut state = shared
                        .state
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    let digest = state.snapshot.model_digest.clone().unwrap_or_default();
                    if let RamWork::Index { origin, .. } = work {
                        drop(state);
                        let result = if origin.incarnation != active.incarnation {
                            Err(EmbeddingError::Unavailable(
                                "index work model changed before completion".into(),
                            ))
                        } else {
                            result.and_then(|vector| {
                                validate_query_vector(origin.model_digest, vector)
                                    .map(|value| value.vector)
                                    .map_err(|error| EmbeddingError::Failed(format!("{error:?}")))
                            })
                        };
                        finish_owned_index(
                            index.as_ref(),
                            owned_index,
                            id,
                            result.map(IndexResult::Vector),
                        );
                        shared.signal.notify();
                        continue;
                    }
                    let result = match result {
                        Ok(vector) => validate_query_vector(digest, vector),
                        Err(EmbeddingError::Cancelled) => Err(QueryError::Cancelled),
                        Err(EmbeddingError::Loading) => {
                            Err(QueryError::Unavailable("model loading".into()))
                        }
                        Err(EmbeddingError::Unavailable(cause)) => {
                            Err(QueryError::Unavailable(cause))
                        }
                        Err(error) => Err(QueryError::Failed(error.to_string())),
                    };
                    state.queries.complete(id, result);
                    shared.changed.notify_all();
                }
                Event::ControlLost(_, cause) => {
                    let reconnected = active.control.as_ref().is_some_and(|address| {
                        port.control(address, &active.incarnation, &active.secret, false)
                            .is_ok()
                    });
                    if !reconnected {
                        let _ = events.send(Event::Pipe(active.incarnation.clone(), cause));
                        shared.signal.notify();
                    }
                }
                Event::Output(_, Output::Unavailable { cause }) | Event::Pipe(_, cause) => {
                    let mut old = child.take().unwrap();
                    let failure_at = clock();
                    let stopped = port.stop(&mut old.child).is_ok();
                    let stop_finished_at = clock();
                    if stopped {
                        retired_incarnation = Some(old.incarnation.clone());
                        cleanup_at = None;
                        if let Some(RamWork::Index { id, .. }) = old.running.take() {
                            finish_owned_index(
                                index.as_ref(),
                                owned_index,
                                id,
                                Err(EmbeddingError::Unavailable(cause.clone())),
                            );
                        }
                        if lifecycle.stopping {
                            lifecycle.stopped();
                        } else {
                            lifecycle.exited(failure_at, cause.clone());
                        }
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        state.queries.fail_all(&cause);
                        state.loading_demand = false;
                        state.snapshot.state = WorkerState::Failed;
                        state.snapshot.cause = Some(cause);
                        state.snapshot.model_digest = None;
                        state.snapshot.incarnation = None;
                        shared.changed.notify_all();
                    } else {
                        lifecycle.exited(failure_at, cause.clone());
                        lifecycle.child = true;
                        lifecycle.stopping = true;
                        cleanup_at = Some(stop_finished_at + Duration::from_secs(1));
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        state.queries.fail_all(&cause);
                        state.snapshot.state = WorkerState::Failed;
                        state.snapshot.cause = Some(cause);
                        state.snapshot.model_digest = None;
                        shared.changed.notify_all();
                        child = Some(old);
                    }
                }
                Event::Exit(_) => {
                    let mut old = child.take().unwrap();
                    let observed_at = clock();
                    let exited = old.child.wait().is_ok();
                    if exited {
                        retired_incarnation = Some(old.incarnation.clone());
                        cleanup_at = None;
                        let cause = "model child exited".to_string();
                        if let Some(RamWork::Index { id, .. }) = old.running.take() {
                            finish_owned_index(
                                index.as_ref(),
                                owned_index,
                                id,
                                Err(EmbeddingError::Unavailable(cause.clone())),
                            );
                        }
                        if lifecycle.stopping {
                            lifecycle.stopped();
                        } else {
                            lifecycle.exited(observed_at, cause.clone());
                        }
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        state.queries.fail_all(&cause);
                        state.loading_demand = false;
                        state.snapshot.state = WorkerState::Failed;
                        state.snapshot.cause = Some(cause);
                        state.snapshot.model_digest = None;
                        state.snapshot.incarnation = None;
                        shared.changed.notify_all();
                    } else {
                        child = Some(old);
                    }
                }
                _ => {}
            }
        }
        let warm = {
            let state = shared
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.stop {
                continue;
            }
            state.warm
        };
        if cleanup_at.is_some_and(|at| clock() >= at) {
            if let Some(mut active) = child.take() {
                match port.stop(&mut active.child) {
                    Ok(()) => {
                        if let Some(RamWork::Index { id, .. }) = active.running.take() {
                            finish_owned_index(
                                index.as_ref(),
                                owned_index,
                                id,
                                Err(EmbeddingError::Unavailable(
                                    "model child stopped after control failure".into(),
                                )),
                            );
                        }
                        lifecycle.stopped();
                        cleanup_at = None;
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        state.snapshot.incarnation = None;
                        state.snapshot.model_digest = None;
                        if lifecycle.cause.is_none() {
                            state.snapshot.state = WorkerState::Stopped;
                        }
                    }
                    Err(_) => {
                        child = Some(active);
                        cleanup_at = Some(clock() + Duration::from_secs(1));
                    }
                }
            } else {
                cleanup_at = None;
            }
        }
        // A work item can only be dispatched by the incarnation and digest that
        // supplied it, including when termination succeeds on a later stop retry.
        return_stale_held(&mut held_index, child.as_ref(), index.as_ref(), owned_index);
        let status = index.status();
        let now = clock();
        let query_demand = {
            let state = shared
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.loading_demand
                || !state.queries.flights.is_empty()
                || child.as_ref().is_some_and(|c| c.running.is_some())
        };
        let demand = warm
            || query_demand
            || held_index.is_some()
            || status.runnable
            || status.awaiting_model
            || status.next_retry_at.is_some_and(|at| at <= now);
        match lifecycle.action(now, demand, config.model.is_some()) {
            LifecycleAction::Start => {
                let cache = cache.get_or_insert_with(|| {
                    SelectionCache::open(config.state_dir.join("model-execution-selection.json"))
                });
                match start_ram_child(&port, &config, &events, &shared.signal, now, cache.get()) {
                    Ok(mut active) => {
                        retired_incarnation = None;
                        active.last_control = clock();
                        lifecycle.started();
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        state.snapshot.state = WorkerState::Loading;
                        state.snapshot.incarnation = Some(active.incarnation.clone());
                        child = Some(active);
                    }
                    Err(error) => {
                        let cause = error.cause;
                        let failure_at = clock();
                        lifecycle.exited(failure_at, cause.clone());
                        if let Some(owned) = error.owned {
                            lifecycle.child = true;
                            lifecycle.stopping = true;
                            child = Some(owned);
                            cleanup_at = Some(failure_at + Duration::from_secs(1));
                        }
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        state.snapshot.state = WorkerState::Failed;
                        state.snapshot.cause = Some(cause);
                    }
                }
            }
            LifecycleAction::None => {}
        }
        if let Some(active) = child.as_mut() {
            if active.running.is_none() {
                let mut state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if state.snapshot.state == WorkerState::Ready {
                    let digest = state.snapshot.model_digest.clone().unwrap();
                    if state.queries.pending.is_empty()
                        && held_index.is_none()
                        && (status.runnable || status.next_retry_at.is_some_and(|at| at <= now))
                    {
                        drop(state);
                        held_index = index.next_work(&digest).map(|work| HeldIndex {
                            work,
                            origin: IndexOrigin {
                                incarnation: active.incarnation.clone(),
                                model_digest: digest.clone(),
                            },
                        });
                        if let Some(held) = &held_index {
                            owned_index
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .insert(held.work.id);
                        }
                        state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                    }
                    if state.stop {
                        drop(state);
                        continue;
                    }
                    let query = state.queries.take().map(|id| RamWork::Query {
                        id,
                        text: state.queries.flights[&id].text.clone(),
                    });
                    let work = query.or_else(|| {
                        held_index.take().map(|held| RamWork::Index {
                            id: held.work.id,
                            task: held.work.task,
                            origin: held.origin,
                        })
                    });
                    if let Some(work) = work {
                        let id = work.id().0;
                        let request = match &work {
                            RamWork::Index {
                                task: IndexTask::Split(text),
                                ..
                            } => Input::Split {
                                id,
                                attempt: 1,
                                text: text.to_string(),
                            },
                            RamWork::Query { text, .. } => Input::Embed {
                                id,
                                attempt: 1,
                                text: text.clone(),
                                overflow: crate::model_child::Overflow::Reject,
                            },
                            RamWork::Index {
                                task: IndexTask::Embed(text),
                                ..
                            } => Input::Embed {
                                id,
                                attempt: 1,
                                text: text.to_string(),
                                overflow: crate::model_child::Overflow::Reject,
                            },
                        };
                        active.running = Some(work);
                        if active.input.send(request).is_err() {
                            let _ = events.send(Event::Pipe(
                                active.incarnation.clone(),
                                "model input closed".into(),
                            ));
                            shared.signal.notify();
                        }
                    }
                }
            }
            if now.saturating_duration_since(active.last_control) >= Duration::from_secs(20) {
                if let Some(address) = &active.control {
                    if let Err(error) =
                        port.control(address, &active.incarnation, &active.secret, false)
                    {
                        let _ = events.send(Event::ControlLost(
                            active.incarnation.clone(),
                            error.to_string(),
                        ));
                        shared.signal.notify();
                    }
                }
                active.last_control = clock();
            }
        }
        let mut until = [
            if demand { lifecycle.retry_at } else { None },
            status.next_retry_at,
            cleanup_at,
        ]
        .into_iter()
        .flatten()
        .filter(|at| *at > now)
        .min();
        if let Some(active) = &child {
            let heartbeat = active.last_control + Duration::from_secs(20);
            until = Some(until.map_or(heartbeat, |at| at.min(heartbeat)));
        }
        pending_event = match until {
            Some(at) => incoming
                .recv_timeout(at.saturating_duration_since(clock()))
                .ok(),
            None => incoming.recv().ok(),
        };
    }
}
enum Event {
    Wake,
    Output(String, Output),
    Exit(String),
    Pipe(String, String),
    ControlLost(String, String),
}
/// OS I/O is separated from transition policy; a test adapter can replace this boundary.
trait WorkerProcessPort {
    fn spawn(&self, config: &RuntimeConfig) -> Result<Child>;
    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<()>;
    fn stop(&self, child: &mut Child) -> Result<()>;
    fn control(&self, address: &str, inc: &str, secret: &str, stop: bool) -> Result<Option<u64>>;
}
struct NativeProcess;
impl WorkerProcessPort for NativeProcess {
    fn spawn(&self, c: &RuntimeConfig) -> Result<Child> {
        let mut cmd = Command::new(&c.executable);
        cmd.arg("model-child")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        Ok(cmd.spawn()?)
    }
    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<()> {
        observe_exit(child, move || {
            let _ = events.send(Event::Exit(inc));
            signal();
        })
    }
    fn stop(&self, child: &mut Child) -> Result<()> {
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        child.wait()?;
        Ok(())
    }
    fn control(&self, address: &str, inc: &str, secret: &str, stop: bool) -> Result<Option<u64>> {
        model_child::control(address, inc, secret, stop)
    }
}
#[cfg(windows)]
fn observe_exit(child: &Child, notify: impl FnOnce() + Send + 'static) -> Result<()> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn DuplicateHandle(
            source: *mut c_void,
            handle: *mut c_void,
            target: *mut c_void,
            output: *mut *mut c_void,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let mut handle = std::ptr::null_mut();
    unsafe {
        let process = GetCurrentProcess();
        if DuplicateHandle(
            process,
            child.as_raw_handle(),
            process,
            &mut handle,
            0,
            0,
            2,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let handle = handle as usize;
    std::thread::spawn(move || unsafe {
        let result = WaitForSingleObject(handle as *mut c_void, u32::MAX);
        CloseHandle(handle as *mut c_void);
        if result == 0 {
            notify();
        }
    });
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn observe_exit(child: &Child, notify: impl FnOnce() + Send + 'static) -> Result<()> {
    // WNOWAIT observes exit without reaping/releasing the PID: the owning Child
    // remains the sole reaper and termination authority until the event is handled.
    let pid = libc::id_t::try_from(child.id())?;
    std::thread::spawn(move || {
        // libc supplies the platform ABI and flags: notably macOS WNOWAIT differs
        // from Linux. waitid initializes siginfo; no guessed size/alignment is used.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::uninit();
        loop {
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if result == 0 {
                notify();
                break;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
    });
    Ok(())
}
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn observe_exit(_: &Child, _: impl FnOnce() + Send + 'static) -> Result<()> {
    Err("native child exit adapter is unavailable on this platform".into())
}
#[cfg(windows)]
type ChildJob = std::os::windows::io::OwnedHandle;
#[cfg(not(windows))]
type ChildJob = ();
struct OwnedChild(Child, Option<ChildJob>);
#[cfg(windows)]
fn kill_on_close_job(child: &Child) -> Result<ChildJob> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let job = unsafe { ChildJob::from_raw_handle(raw) };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
        || unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(job)
}
impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
#[cfg(test)]
#[path = "supervisor/tests/mod.rs"]
mod native_tests;
