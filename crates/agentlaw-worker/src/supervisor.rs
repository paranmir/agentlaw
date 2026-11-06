//! One serialized owner for model-process transitions. Native inference is never in this process.
use crate::{
    daemon::{RuntimeConfig, Signal},
    model_child::{self, Input, Launch, Output, Result},
    Broker, EmbeddingError, Job, WorkerState, MODEL_ID,
};
use fs2::FileExt;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    io::Write,
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
    pub text: Arc<str>,
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
        outcome: std::result::Result<Vec<f32>, crate::EmbeddingError>,
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
impl QueryQueue {
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
        text: String,
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

enum CacheCommand {
    Save(String, crate::execution::Selection),
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
}
struct SelectionCache {
    value: Arc<std::sync::Mutex<Option<(String, crate::execution::Selection)>>>,
    commands: mpsc::Sender<CacheCommand>,
}
impl SelectionCache {
    fn open(path: std::path::PathBuf) -> Self {
        let value = Arc::new(std::sync::Mutex::new(None));
        let loaded = value.clone();
        let (commands, incoming) = mpsc::channel();
        std::thread::spawn(move || {
            let mut previous = std::fs::metadata(&path)
                .ok()
                .filter(|m| m.len() <= 65536)
                .and_then(|_| std::fs::read(&path).ok());
            if let Some(parsed) = previous
                .as_ref()
                .and_then(|bytes| serde_json::from_slice(bytes).ok())
            {
                *loaded.lock().unwrap() = Some(parsed);
            } else {
                previous = None;
            }
            while let Ok(command) = incoming.recv() {
                match command {
                    CacheCommand::Save(fingerprint, selection) => {
                        let item = (fingerprint, selection);
                        if let Ok(bytes) = serde_json::to_vec(&item) {
                            if bytes.len() <= 65536 {
                                if previous.as_ref() == Some(&bytes)
                                    || std::fs::write(&path, &bytes).is_ok()
                                {
                                    previous = Some(bytes);
                                    *loaded.lock().unwrap() = Some(item);
                                }
                            }
                        }
                    }
                    #[cfg(test)]
                    CacheCommand::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self { value, commands }
    }
    fn get(&self) -> Option<(String, crate::execution::Selection)> {
        self.value.lock().unwrap().clone()
    }
    fn save(&self, fingerprint: String, selection: crate::execution::Selection) {
        let _ = self
            .commands
            .send(CacheCommand::Save(fingerprint, selection));
    }
    #[cfg(test)]
    fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        self.commands.send(CacheCommand::Flush(tx)).unwrap();
        rx.recv().unwrap();
    }
}
impl RamWork {
    fn id(&self) -> ModelRequestId {
        match self {
            Self::Query { id, .. } | Self::Index { id, .. } => *id,
        }
    }
    fn text(&self) -> &str {
        match self {
            Self::Query { text, .. } | Self::Index { text, .. } => text,
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
    signal: Arc<Signal>,
    changed: std::sync::Condvar,
    events: mpsc::Sender<Event>,
    wake_pending: AtomicBool,
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
            signal: Arc::new(Signal::default()),
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
    pub fn wake(&self) {
        self.shared.signal.notify();
        if !self.shared.wake_pending.swap(true, Ordering::AcqRel) {
            let _ = self
                .shared
                .events
                .send(Event::ControlLost(String::new(), String::new()));
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
    pub fn stop(&mut self) -> std::result::Result<(), String> {
        if let Some(result) = &self.stop_result {
            return result.clone();
        }
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .stop = true;
        self.wake();
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
    signal: &Arc<Signal>,
    now: Instant,
    cached: Option<(String, crate::execution::Selection)>,
) -> std::result::Result<RamChild, RamStartError> {
    let incarnation = uuid::Uuid::new_v4().to_string();
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut child = OwnedChild(port.spawn(config).map_err(|error| RamStartError {
        cause: error.to_string(),
        owned: None,
    })?);
    if let Err(error) = port.observe(&child, incarnation.clone(), events.clone(), signal.clone()) {
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
    result: std::result::Result<Vec<f32>, EmbeddingError>,
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
    let stale = held.as_ref().is_some_and(|held| {
        child.is_none_or(|child| {
            child.incarnation != held.origin.incarnation
                || child.model_digest.as_deref() != Some(held.origin.model_digest.as_str())
        })
    });
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
            if matches!(&event, Event::ControlLost(inc, _) if inc.is_empty()) {
                shared.wake_pending.store(false, Ordering::Release);
                continue;
            }
            let Some(active) = child.as_mut() else {
                continue;
            };
            let event_inc = match &event {
                Event::Output(i, _)
                | Event::Exit(i)
                | Event::Pipe(i, _)
                | Event::ControlLost(i, _) => i,
            };
            if event_inc != &active.incarnation {
                continue;
            }
            match event {
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
                    if selection.cacheable {
                        if let Some(cache) = &cache {
                            cache.save(fingerprint, selection);
                        }
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
                        let result = if origin.incarnation != active.incarnation
                            || active.model_digest.as_deref() != Some(origin.model_digest.as_str())
                        {
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
                        finish_owned_index(index.as_ref(), owned_index, id, result);
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
                        state.loading_demand = false;
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
                            text: held.work.text.to_string(),
                            origin: held.origin,
                        })
                    });
                    if let Some(work) = work {
                        let id = work.id();
                        let text = work.text().to_owned();
                        active.running = Some(work);
                        if active
                            .input
                            .send(Input::Embed {
                                id: id.0,
                                attempt: 1,
                                text,
                            })
                            .is_err()
                        {
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
        signal: Arc<Signal>,
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
        signal: Arc<Signal>,
    ) -> Result<()> {
        observe_exit(child, move || {
            let _ = events.send(Event::Exit(inc));
            signal.notify();
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
struct Active {
    child: OwnedChild,
    inc: String,
    secret: String,
    control: Option<String>,
    job: Option<Job>,
    ready_since: Option<Instant>,
    last_control: Instant,
    unavailable: bool,
    exited: bool,
}
struct OwnedChild(Child);
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
impl Drop for Active {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
enum Slot {
    Stopped,
    Active(Active),
    Waiting(Instant),
    Blocked(String),
}
struct Recovery {
    streak: i64,
    config: String,
}
fn now() -> i64 {
    crate::util::now()
}
fn diagnostic(b: &Broker, name: &str, cause: &str) -> Result<()> {
    b.connection.execute("INSERT INTO diagnostics VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",params![name,cause])?;
    Ok(())
}
pub(crate) fn resolve_diagnostic(
    b: &mut Broker,
    name: &str,
    ready_inc: Option<&str>,
) -> Result<bool> {
    let tx = b
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(inc) = ready_inc {
        let (current, state): (String, String) =
            tx.query_row("SELECT incarnation,state FROM worker", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if current != inc || state != "ready" {
            return Ok(false);
        }
    }
    let cause: Option<String> = tx
        .query_row("SELECT cause FROM diagnostics WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(cause) = cause else {
        return Ok(false);
    };
    tx.execute(
        "INSERT INTO diagnostics(name,cause) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",
        params![format!("history:{name}"), serde_json::json!({"detail":cause,"resolved_at_ms":now()}).to_string()],
    )?;
    tx.execute("DELETE FROM diagnostics WHERE name=?1", [name])?;
    tx.commit()?;
    Ok(true)
}
fn heartbeat(port: &impl WorkerProcessPort, b: &Broker, active: &mut Active) -> Result<()> {
    let address = active.control.as_ref().ok_or("model control unavailable")?;
    let model = port.control(address, &active.inc, &active.secret, false)?;
    let broker = crate::execution::host_resident_bytes();
    let total = broker.zip(model).and_then(|(a, b)| a.checked_add(b));
    active.last_control = Instant::now();
    // A diagnostic disk error cannot negate an authenticated live-control proof.
    let _=diagnostic(b,"worker_resources",&serde_json::json!({"incarnation":active.inc,"observed_ms":now(),"broker_resident_bytes":broker,"model_resident_bytes":model,"combined_resident_bytes":total}).to_string());
    Ok(())
}
fn demand(b: &Broker, config: &str) -> Result<bool> {
    Ok(b.connection.query_row("SELECT EXISTS(SELECT 1 FROM leases WHERE expires>?1) OR EXISTS(SELECT 1 FROM jobs WHERE durable=1 AND model=(SELECT model FROM worker_model_configs WHERE config=?2) AND state IN('queued','running','retry_wait'))",params![now(),config],|r|r.get(0))?)
}
fn fingerprint(c: &RuntimeConfig) -> Result<String> {
    let mut h = Sha256::new();
    h.update(serde_json::to_vec(&c.model)?);
    if let Some(a) = &c.model {
        for p in [&a.onnx_model, &a.tokenizer_json, &a.runtime_library] {
            match std::fs::metadata(p) {
                Ok(m) => {
                    h.update(m.len().to_le_bytes());
                    h.update(format!("{:?}", m.modified()).as_bytes());
                }
                Err(e) => h.update(e.to_string().as_bytes()),
            }
        }
    }
    Ok(format!("{:x}", h.finalize()))
}
fn lose_claims(b: &mut Broker, inc: &str, cause: &str) -> Result<()> {
    // This transaction touches only C7 claims/waiters/control state, never source/index ACKs.
    let tx = b
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("INSERT OR IGNORE INTO waiter_failures(job,lease,cause) SELECT w.job,w.lease,?1 FROM waiters w JOIN jobs j ON j.id=w.job WHERE j.state IN('queued','running','retry_wait')",[cause])?;
    tx.execute("UPDATE jobs SET state='failed',error=?1,payload='' WHERE durable=0 AND state IN('queued','running','retry_wait')",[cause])?;
    tx.execute("UPDATE jobs SET state='retry_wait',next_attempt=?1,incarnation=NULL,error=?2 WHERE durable=1 AND state='running' AND incarnation=?3",params![now(),cause,inc])?;
    tx.execute(
        "UPDATE worker SET state='failed' WHERE incarnation=?1",
        [inc],
    )?;
    tx.commit()?;
    diagnostic(b, "worker_supervision", cause)
}
impl Recovery {
    fn load(b: &Broker, config: String) -> Result<(Self, Slot)> {
        let row: Option<(i64, i64, Option<String>)> = b
            .connection
            .query_row(
                "SELECT streak,next_at,blocked FROM worker_recovery WHERE config=?1",
                [&config],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (streak, next, blocked) = row.unwrap_or((0, 0, None));
        let slot = if let Some(f) = blocked {
            Slot::Blocked(f)
        } else {
            Slot::Waiting(
                Instant::now()
                    + Duration::from_millis(next.saturating_sub(now()).clamp(0, 300000) as u64),
            )
        };
        Ok((Self { streak, config }, slot))
    }
    fn save(&self, b: &Broker, next: i64, blocked: Option<&str>) -> Result<()> {
        b.connection.execute("INSERT INTO worker_recovery VALUES(?1,?2,?3,?4) ON CONFLICT(config) DO UPDATE SET streak=excluded.streak,next_at=excluded.next_at,blocked=excluded.blocked",params![self.config,self.streak,next,blocked])?;
        Ok(())
    }
    fn failed(&mut self, b: &Broker) -> Result<Slot> {
        self.streak = self
            .streak
            .checked_add(1)
            .ok_or("worker failure counter overflow")?;
        let delay = match self.streak {
            1 => 1000,
            2 => 2000,
            _ => 300000,
        };
        self.save(
            b,
            now()
                .checked_add(delay)
                .ok_or("worker retry timestamp overflow")?,
            None,
        )?;
        Ok(Slot::Waiting(
            Instant::now() + Duration::from_millis(delay as u64),
        ))
    }
}
fn start(
    port: &impl WorkerProcessPort,
    b: &mut Broker,
    c: &RuntimeConfig,
    events: &mpsc::Sender<Event>,
    signal: &Arc<Signal>,
) -> Result<Active> {
    let handle = b.reserve_worker_start(MODEL_ID, now())?;
    if !handle.should_start {
        return Err("supervisor start reservation already owned".into());
    }
    let inc = handle.incarnation;
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let cached = b
        .connection
        .query_row(
            "SELECT fingerprint,report FROM execution_selection ORDER BY rowid DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
        .and_then(|(f, j)| serde_json::from_str(&j).ok().map(|v| (f, v)));
    let mut child = OwnedChild(port.spawn(c)?);
    b.connection.execute("INSERT INTO diagnostics VALUES('worker_process',?1) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",[serde_json::json!({"pid":child.id(),"incarnation":inc}).to_string()])?;
    if let Err(e) = port.observe(&child, inc.clone(), events.clone(), signal.clone()) {
        let _ = port.stop(&mut child);
        return Err(e);
    }
    let launch = Launch {
        incarnation: inc.clone(),
        secret: secret.clone(),
        state: c.state_dir.clone(),
        assets: c.model.clone(),
        cached,
    };
    if let Err(e) = model_child::write(child.stdin.as_mut().ok_or("missing child input")?, &launch)
    {
        let _ = port.stop(&mut child);
        return Err(e);
    }
    let mut output = child.stdout.take().ok_or("missing child output")?;
    let events = events.clone();
    let signal = signal.clone();
    let observed = inc.clone();
    std::thread::spawn(move || loop {
        match model_child::read::<Output>(&mut output) {
            Ok(message) => {
                if events
                    .send(Event::Output(observed.clone(), message))
                    .is_err()
                {
                    break;
                }
                signal.notify();
            }
            Err(e) => {
                let _ = events.send(Event::Pipe(observed, e.to_string()));
                signal.notify();
                break;
            }
        }
    });
    b.transition(&inc, WorkerState::Loading, now())?;
    Ok(Active {
        child,
        inc,
        secret,
        control: None,
        job: None,
        ready_since: None,
        last_control: Instant::now(),
        unavailable: false,
        exited: false,
    })
}
/// Read-only doctor snapshot. Never attaches, launches, mutates schema, or exposes credentials/payloads.
pub fn inspect_runtime(state: &std::path::Path) -> Result<serde_json::Value> {
    let path = state.join("broker.sqlite");
    if !path.exists() {
        return Ok(serde_json::json!({"present":false}));
    }
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(2))?;
    db.execute_batch("BEGIN")?;
    let has = |table: &str| -> rusqlite::Result<bool> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )
    };
    let mut result = serde_json::json!({"present":true});
    if has("worker")? {
        result["worker"]=db.query_row("SELECT incarnation,state,model,heartbeat FROM worker",[],|r|Ok(serde_json::json!({"incarnation":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"model":r.get::<_,String>(2)?,"broker_heartbeat_ms":r.get::<_,i64>(3)?})))?;
    }
    if has("worker_recovery")? {
        let mut s=db.prepare("SELECT config,streak,next_at,blocked FROM worker_recovery ORDER BY next_at DESC LIMIT 64")?;
        result["recovery"]=s.query_map([],|r|Ok(serde_json::json!({"configuration":r.get::<_,String>(0)?,"failure_streak":r.get::<_,i64>(1)?,"next_restart_ms":r.get::<_,i64>(2)?,"startup_blocked":r.get::<_,Option<String>>(3)?.is_some()})))?.collect::<rusqlite::Result<Vec<_>>>()?.into();
    }
    if has("jobs")? {
        let mut s = db.prepare("SELECT state,durable,COUNT(*) FROM jobs GROUP BY state,durable")?;
        result["queue"]=s.query_map([],|r|Ok(serde_json::json!({"state":r.get::<_,String>(0)?,"durable":r.get::<_,bool>(1)?,"count":r.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?.into();
    }
    if has("diagnostics")? {
        let mut s=db.prepare("SELECT name,cause FROM diagnostics WHERE name IN('worker_supervision','worker_recovery_state','worker_process','worker_resources','model_load','supervisor_fatal','execution_selection','index_background','index_background_status') OR name LIKE 'vector_index:%' ORDER BY name LIMIT 128")?;
        result["diagnostics"] = s
            .query_map([], |r| {
                Ok(serde_json::json!({"name":r.get::<_,String>(0)?,"detail":r.get::<_,String>(1)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into();
        let mut history=db.prepare("SELECT name,cause FROM diagnostics WHERE name LIKE 'history:%' ORDER BY name LIMIT 128")?;
        result["diagnostic_history"] = history
            .query_map([], |r| {
                let name: String = r.get(0)?;
                let saved: String = r.get(1)?;
                let value: serde_json::Value = serde_json::from_str(&saved).unwrap_or_else(|_| serde_json::json!({"detail":saved}));
                Ok(serde_json::json!({"name":name.trim_start_matches("history:"),"detail":value["detail"],"resolved_at_ms":value["resolved_at_ms"]}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into();
    }
    if has("generation_ack")? {
        let mut s=db.prepare("SELECT repository,channel,epoch,sequence,index_commit_id FROM generation_ack ORDER BY repository,channel LIMIT 128")?;
        result["generation_ack_first_128"]=s.query_map([],|r|Ok(serde_json::json!({"repository":r.get::<_,String>(0)?,"channel":r.get::<_,String>(1)?,"epoch":r.get::<_,String>(2)?,"sequence":r.get::<_,i64>(3)?,"commit":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?.into();
    }
    db.execute_batch("ROLLBACK")?;
    Ok(result)
}
pub(crate) fn run(c: RuntimeConfig, stop: Arc<AtomicBool>, signal: Arc<Signal>) -> Result<()> {
    run_with_port(c, stop, signal, NativeProcess)
}
fn initialize(b: &Broker) -> Result<()> {
    b.connection.execute_batch("CREATE TABLE IF NOT EXISTS diagnostics(name TEXT PRIMARY KEY,cause TEXT NOT NULL);CREATE TABLE IF NOT EXISTS execution_selection(fingerprint TEXT PRIMARY KEY,report TEXT NOT NULL);")?;
    b.connection.execute_batch("CREATE TABLE IF NOT EXISTS worker_recovery(config TEXT PRIMARY KEY,streak INTEGER NOT NULL,next_at INTEGER NOT NULL,blocked TEXT);CREATE TABLE IF NOT EXISTS worker_model_configs(config TEXT PRIMARY KEY,model TEXT NOT NULL);CREATE TABLE IF NOT EXISTS waiter_failures(job INTEGER NOT NULL,lease TEXT NOT NULL,cause TEXT NOT NULL,PRIMARY KEY(job,lease));")?;
    Ok(())
}
// A private OS-boundary seam: tests still execute the production supervisor,
// native child, SQLite journal and IPC. No test command or MCP action is exposed.
fn run_with_port(
    c: RuntimeConfig,
    stop: Arc<AtomicBool>,
    signal: Arc<Signal>,
    port: impl WorkerProcessPort,
) -> Result<()> {
    let mut b = Broker::open(c.state_dir.join("broker.sqlite"), 64)?;
    initialize(&b)?;
    let old: String = b
        .connection
        .query_row("SELECT incarnation FROM worker", [], |r| r.get(0))?;
    lose_claims(
        &mut b,
        &old,
        "previous broker ownership ended; unfinished model claims recovered",
    )?;
    let config = format!("{:x}", Sha256::digest(serde_json::to_vec(&c.model)?));
    let (mut recovery, mut slot) = Recovery::load(&b, config)?;
    let (tx, rx) = mpsc::channel();
    loop {
        let observed = signal.version();
        if stop.load(Ordering::Relaxed) {
            if let Slot::Active(mut active) = slot {
                b.connection.execute(
                    "UPDATE worker SET state='stopping' WHERE incarnation=?1",
                    [&active.inc],
                )?;
                if let Some(address) = &active.control {
                    let _ = port.control(address, &active.inc, &active.secret, true);
                }
                port.stop(&mut active.child)?;
            }
            b.connection
                .execute("UPDATE worker SET state='stopped'", [])?;
            return Ok(());
        }
        for event in rx.try_iter() {
            let Slot::Active(active) = &mut slot else {
                continue;
            };
            let inc = match &event {
                Event::Output(i, _)
                | Event::Exit(i)
                | Event::Pipe(i, _)
                | Event::ControlLost(i, _) => i,
            };
            if inc != &active.inc {
                continue;
            }
            match event {
                Event::Output(
                    _,
                    Output::Hello {
                        incarnation,
                        control,
                    },
                ) => {
                    if incarnation != active.inc {
                        let _ = tx.send(Event::Pipe(
                            active.inc.clone(),
                            "model handshake incarnation mismatch".into(),
                        ));
                        signal.notify();
                        continue;
                    }
                    active.control = Some(control);
                    if let Err(error) = heartbeat(&port, &b, active) {
                        let _ = tx.send(Event::Pipe(active.inc.clone(), error.to_string()));
                        signal.notify();
                        continue;
                    }
                }
                Event::Output(_, Output::Ready { model }) => {
                    b.connection.execute("INSERT INTO worker_model_configs VALUES(?1,?2) ON CONFLICT(config) DO UPDATE SET model=excluded.model",params![recovery.config,model])?;
                    b.connection.execute(
                        "UPDATE worker SET model=?1 WHERE incarnation=?2",
                        params![model, active.inc],
                    )?;
                    b.transition(&active.inc, WorkerState::Ready, now())?;
                    active.ready_since = Some(Instant::now());
                    match heartbeat(&port, &b, active) {
                        Ok(()) => {
                            let moved = !active.unavailable
                                && !active.exited
                                && resolve_diagnostic(&mut b, "model_load", Some(&active.inc))?;
                            diagnostic(
                                &b,
                                "worker_recovery_state",
                                if moved {
                                    "READY; previous model failure moved to diagnostic_history"
                                } else {
                                    "READY"
                                },
                            )?;
                        }
                        Err(error) => {
                            let _ =
                                tx.send(Event::ControlLost(active.inc.clone(), error.to_string()));
                            diagnostic(
                                &b,
                                "worker_recovery_state",
                                "READY; control verification pending",
                            )?;
                        }
                    }
                    signal.notify();
                }
                Event::Output(_, Output::Unavailable { cause }) => {
                    active.unavailable = true;
                    diagnostic(&b, "model_load", &cause)?;
                    lose_claims(&mut b, &active.inc, &cause)?;
                    let assets = fingerprint(&c)?;
                    recovery.save(&b, 0, Some(&assets))?;
                    signal.notify();
                }
                Event::Output(
                    _,
                    Output::Selection {
                        fingerprint,
                        selection,
                    },
                ) => {
                    diagnostic(
                        &b,
                        "execution_selection",
                        &serde_json::to_string(&selection)?,
                    )?;
                    if let Some(key) = fingerprint.filter(|_| selection.cacheable) {
                        b.connection.execute("INSERT INTO execution_selection VALUES(?1,?2) ON CONFLICT(fingerprint) DO UPDATE SET report=excluded.report",params![key,serde_json::to_string(&selection)?])?;
                    }
                }
                Event::Output(
                    _,
                    Output::Result {
                        id,
                        attempt,
                        result,
                    },
                ) => {
                    let Some(job) = active
                        .job
                        .as_ref()
                        .filter(|j| u64::try_from(j.id).ok() == Some(id) && j.attempts == attempt)
                    else {
                        continue;
                    };
                    match result {
                        Ok(vector) => match b.complete(job, &vector) {
                            Ok(()) => {
                                b.connection.execute("UPDATE jobs SET state='acknowledged',payload='' WHERE id=?1 AND durable=0",[id])?;
                            }
                            Err(crate::Error::InvalidState) => {
                                b.fail(job, "model returned invalid result", false, now())?
                            }
                            Err(crate::Error::ObsoleteIncarnation) => {}
                            Err(error) => return Err(error.into()),
                        },
                        Err(e) => b.fail(
                            job,
                            &e.to_string(),
                            matches!(e, EmbeddingError::Retryable(_)) && job.attempts < 8,
                            now(),
                        )?,
                    }
                    active.job = None;
                    signal.notify();
                }
                Event::ControlLost(_, cause) => {
                    if heartbeat(&port, &b, active).is_ok() {
                        if !active.unavailable
                            && !active.exited
                            && resolve_diagnostic(&mut b, "model_load", Some(&active.inc))?
                        {
                            diagnostic(
                                &b,
                                "worker_recovery_state",
                                "READY; previous model failure moved to diagnostic_history",
                            )?;
                        }
                        continue;
                    }
                    let _ = tx.send(Event::Pipe(active.inc.clone(), cause));
                    signal.notify();
                }
                Event::Exit(_) => {
                    active.exited = true; /* Drain ordered stdout results/diagnostics before handling its EOF. */
                }
                Event::Pipe(_, _) => {
                    let Slot::Active(mut ended) = std::mem::replace(&mut slot, Slot::Stopped)
                    else {
                        unreachable!()
                    };
                    let cause = match event {
                        Event::Pipe(_, cause) => format!("model IPC ended: {cause}"),
                        _ => unreachable!(),
                    };
                    // EOF alone is not proof of death. The exact owned handle is checked,
                    // authenticated control is retried, then owned stop+wait must succeed.
                    if ended.child.try_wait()?.is_none() {
                        if let Some(address) = &ended.control {
                            let _ = port.control(address, &ended.inc, &ended.secret, false);
                            let _ = port.control(address, &ended.inc, &ended.secret, true);
                        }
                        port.stop(&mut ended.child)?;
                    } else {
                        ended.child.wait()?;
                    }
                    if ended.unavailable {
                        slot = Slot::Blocked(fingerprint(&c)?);
                    } else {
                        lose_claims(&mut b, &ended.inc, &cause)?;
                        slot = recovery.failed(&b)?;
                    }
                    signal.notify();
                }
            }
        }
        if matches!(slot, Slot::Active(_))
            && b.idle_shutdown_due(now(), 600000)?
            && !demand(&b, &recovery.config)?
        {
            let Slot::Active(mut idle) = std::mem::replace(&mut slot, Slot::Stopped) else {
                unreachable!()
            };
            b.connection.execute(
                "UPDATE worker SET state='stopping' WHERE incarnation=?1",
                [&idle.inc],
            )?;
            if let Some(address) = &idle.control {
                let _ = port.control(address, &idle.inc, &idle.secret, true);
            }
            port.stop(&mut idle.child)?; // No reservation until exact owned termination completes.
            b.connection.execute(
                "UPDATE worker SET state='stopped' WHERE incarnation=?1",
                [&idle.inc],
            )?;
            diagnostic(
                &b,
                "worker_recovery_state",
                "STOPPED after idle grace; broker remains available",
            )?;
            signal.notify();
        }
        match &mut slot {
            Slot::Active(active) => {
                if !active.exited && active.last_control.elapsed() >= Duration::from_secs(10) {
                    if active.control.is_some() {
                        if let Err(error) = heartbeat(&port, &b, active) {
                            let _ = tx.send(Event::ControlLost(
                                active.inc.clone(),
                                format!("control reconnect failed: {error}"),
                            ));
                            signal.notify();
                        } else if !active.unavailable
                            && !active.exited
                            && resolve_diagnostic(&mut b, "model_load", Some(&active.inc))?
                        {
                            diagnostic(
                                &b,
                                "worker_recovery_state",
                                "READY; previous model failure moved to diagnostic_history",
                            )?;
                        }
                    }
                }
                if active
                    .ready_since
                    .is_some_and(|since| since.elapsed() >= Duration::from_secs(60))
                    && recovery.streak != 0
                    && active.last_control.elapsed() < Duration::from_secs(10)
                {
                    recovery.streak = 0;
                    recovery.save(&b, 0, None)?;
                }
                if active.ready_since.is_some()
                    && active.job.is_none()
                    && !active.unavailable
                    && !active.exited
                {
                    if let Some(job) = b.claim(&active.inc, now())? {
                        let bytes = match model_child::encode(&Input::Embed {
                            id: u64::try_from(job.id)?,
                            attempt: job.attempts,
                            text: job.payload.clone(),
                        }) {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                b.fail(
                                    &job,
                                    &format!("job serialization rejected: {error}"),
                                    false,
                                    now(),
                                )?;
                                signal.notify();
                                continue;
                            }
                        };
                        let input = active.child.stdin.as_mut().ok_or("child input closed")?;
                        let result = input.write_all(&bytes).and_then(|_| input.flush());
                        active.job = Some(job);
                        if let Err(error) = result {
                            let _ = tx.send(Event::Pipe(active.inc.clone(), error.to_string()));
                            signal.notify();
                        }
                    }
                }
            }
            Slot::Blocked(assets) => {
                if fingerprint(&c)? != *assets {
                    recovery.save(&b, 0, None)?;
                    slot = Slot::Waiting(Instant::now());
                }
            }
            Slot::Stopped => {
                if demand(&b, &recovery.config)? {
                    slot = Slot::Waiting(Instant::now());
                }
            }
            Slot::Waiting(until) => {
                if Instant::now() >= *until && demand(&b, &recovery.config)? {
                    let lock = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(c.state_dir.join("model.lock"))?;
                    if lock.try_lock_exclusive().is_ok() {
                        FileExt::unlock(&lock)?;
                        match start(&port, &mut b, &c, &tx, &signal) {
                            Ok(active) => slot = Slot::Active(active),
                            Err(error) => {
                                diagnostic(&b, "model_load", &error.to_string())?;
                                b.connection
                                    .execute("UPDATE worker SET state='failed'", [])?;
                                slot = recovery.failed(&b)?;
                            }
                        }
                    } else {
                        diagnostic(&b,"worker_supervision","incumbent model lock remains live; spawn withheld until owned orphan exits")?;
                        *until = Instant::now() + Duration::from_secs(5);
                    }
                }
            }
        }
        // Work/control/retry clocks wake the same state owner; process exit itself uses an OS wait.
        let mut wait = Duration::from_secs(10);
        if let Slot::Waiting(until) = &slot {
            if demand(&b, &recovery.config)? {
                wait = wait.min(until.saturating_duration_since(Instant::now()));
            }
        }
        if let Slot::Active(active) = &slot {
            if active.control.is_some() && !active.exited {
                wait =
                    wait.min(Duration::from_secs(10).saturating_sub(active.last_control.elapsed()));
            }
            if active.job.is_none() && active.ready_since.is_some() {
                let next:Option<i64>=b.connection.query_row("SELECT MIN(next_attempt) FROM jobs WHERE state='retry_wait' AND model=(SELECT model FROM worker)",[],|r|r.get(0))?;
                if let Some(next) = next {
                    wait = wait.min(Duration::from_millis(
                        next.saturating_sub(now()).max(1) as u64
                    ));
                }
            }
        }
        signal.wait(observed, wait.max(Duration::from_millis(1)));
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "supervisor_native_tests.rs"]
mod native_tests;

#[cfg(test)]
mod index_work_port_tests {
    use super::*;
    use std::sync::Mutex;

    struct TestPort {
        work: Mutex<Option<IndexWork>>,
        finished: Mutex<
            Option<(
                ModelRequestId,
                std::result::Result<Vec<f32>, EmbeddingError>,
            )>,
        >,
    }

    impl IndexWorkPort for TestPort {
        fn model_ready(&self, _: &str) {}

        fn next_work(&self, _: &str) -> Option<IndexWork> {
            self.work.lock().unwrap().take()
        }

        fn finish(
            &self,
            id: ModelRequestId,
            outcome: std::result::Result<Vec<f32>, EmbeddingError>,
        ) {
            *self.finished.lock().unwrap() = Some((id, outcome));
        }

        fn status(&self) -> IndexWorkStatus {
            IndexWorkStatus {
                runnable: false,
                next_retry_at: None,
                awaiting_model: false,
            }
        }
    }

    #[test]
    fn index_work_port_crosses_threads_and_records_outcome() {
        let concrete = Arc::new(TestPort {
            work: Mutex::new(Some(IndexWork {
                id: ModelRequestId(42),
                text: Arc::from("queued text"),
            })),
            finished: Mutex::new(None),
        });
        let port: Arc<dyn IndexWorkPort> = concrete.clone();
        std::thread::spawn(move || {
            let work = port.next_work("model").unwrap();
            assert_eq!(&*work.text, "queued text");
            port.finish(work.id, Ok(vec![1.0, 2.0]));
        })
        .join()
        .unwrap();
        let finished = concrete.finished.lock().unwrap().take().unwrap();
        assert_eq!(finished.0, ModelRequestId(42));
        assert_eq!(finished.1.unwrap(), vec![1.0, 2.0]);
    }
}
