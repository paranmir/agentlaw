//! Serializes one store's control and source changes.

use agentlaw_flows::{
    apply::Applied,
    control::ControlWriter,
    prepare::Prepared,
    runtime::{LaneCtx, StoreIdentity},
    DomainError, RequestControl, Result,
};
use agentlaw_storage::{sync::OverlayGuard, PublishReceipt, Store};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(test)]
mod tests;

pub struct SyncOperationRow {
    pub id: String,
    pub source: String,
    pub state_json: String,
    pub active: bool,
}
pub struct SyncOverlayInput {
    pub publication_id: String,
    pub baseline_root: PathBuf,
    pub baseline_local: PathBuf,
    pub resolved_root: PathBuf,
    pub resolved_local: PathBuf,
    pub guard: OverlayGuard,
}
pub struct ImportPublishInput {
    pub operation_id: String,
    pub staged_root: PathBuf,
    pub staged_local: PathBuf,
    pub expected_generation: u64,
    pub staged_generation: u64,
}
pub enum LaneCommand {
    Apply(Prepared),
    Recover,
    SyncPersist(SyncOperationRow),
    SyncReserve {
        id: String,
        operation: String,
        digest: String,
    },
    SyncComplete {
        id: String,
        response_json: String,
    },
    PublishSyncOverlay(SyncOverlayInput),
    PublishImport(ImportPublishInput),
    ImportReceipt(String),
    ImportChoices {
        operation_id: String,
        choices: std::collections::BTreeMap<String, agentlaw_storage::import_conflict::ImportSide>,
    },
    ImportChoiceDecided(String),
    SetExactGeneration {
        generation_id: String,
        exact_file: PathBuf,
    },
}
pub enum LaneReply {
    Applied(Applied),
    Published(PublishReceipt),
    ImportReceipt(Option<PublishReceipt>),
    Reserved(bool),
    Done,
}
struct Work {
    command: LaneCommand,
    control: RequestControl,
    limits: Option<(usize, usize)>,
    state: Arc<Mutex<WorkState>>,
    reply: mpsc::Sender<Result<LaneReply>>,
}
enum Message {
    Work(Work),
    JournalWake,
    Stop,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkState {
    Queued,
    Started,
    Cancelled,
}
struct Shared {
    sender: SyncSender<Message>,
    closed: AtomicBool,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
#[derive(Clone)]
pub struct LaneSender {
    shared: Arc<Shared>,
}
pub struct MutationLane {
    sender: LaneSender,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn unavailable() -> DomainError {
    DomainError::new(
        "store_unavailable",
        "The store mutation lane is unavailable.",
    )
}
fn storage(error: agentlaw_storage::Error) -> DomainError {
    match error {
        agentlaw_storage::Error::CancelledBeforeDecision => DomainError::new(
            "cancelled",
            "Publication was cancelled before its durable decision.",
        ),
        agentlaw_storage::Error::Capacity => {
            DomainError::new("capacity", "Source capacity exceeded.")
        }
        agentlaw_storage::Error::Stale(reason) => DomainError::new("sync_inputs_changed", reason),
        other => DomainError::new("source_operation_failed", other.to_string()),
    }
}

impl MutationLane {
    pub fn start(identity: StoreIdentity, store: Store, writer: ControlWriter) -> Result<Self> {
        let journal_writer = store.hold_journal_writer().map_err(storage)?;
        let (sender, receiver) = mpsc::sync_channel(8);
        let shared = Arc::new(Shared {
            sender,
            closed: AtomicBool::new(false),
            wake: Mutex::new(None),
        });
        let journal_sender = shared.sender.clone();
        journal_writer.set_waker(Arc::new(move || {
            let _ = journal_sender.try_send(Message::JournalWake);
        }));
        let actor = shared.clone();
        let thread = thread::Builder::new()
            .name("store-mutation".into())
            .spawn(move || {
                let mut writer = writer;
                let mut retry_at: Option<Instant> = None;
                let mut retry_delay = Duration::from_secs(1);
                loop {
                    if journal_writer.needs_reopen() {
                        let deadline =
                            *retry_at.get_or_insert_with(|| Instant::now() + retry_delay);
                        if Instant::now() >= deadline {
                            match store.retry_journal_writer(&journal_writer) {
                                Ok(()) => {
                                    retry_at = None;
                                    retry_delay = Duration::from_secs(1);
                                    let callback = actor
                                        .wake
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .clone();
                                    if let Some(callback) = callback {
                                        callback();
                                    }
                                }
                                Err(_) => {
                                    retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                                    retry_at = Some(Instant::now() + retry_delay);
                                }
                            }
                        }
                    } else {
                        retry_at = None;
                        retry_delay = Duration::from_secs(1);
                    }
                    let message = match retry_at {
                        Some(deadline) => match receiver
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        {
                            Ok(message) => message,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        },
                        None => match receiver.recv() {
                            Ok(message) => message,
                            Err(_) => break,
                        },
                    };
                    let work = match message {
                        Message::Stop => break,
                        Message::JournalWake => continue,
                        Message::Work(work) => work,
                    };
                    {
                        let mut state = work.state.lock().unwrap_or_else(|e| e.into_inner());
                        if *state != WorkState::Queued || work.control.check().is_err() {
                            *state = WorkState::Cancelled;
                            let _ =
                                work.reply
                                    .send(work.control.check().and(Err(DomainError::new(
                                        "cancelled",
                                        "The queued request was cancelled.",
                                    ))));
                            continue;
                        }
                        *state = WorkState::Started;
                    }
                    let (history_limit, response_limit) = work
                        .limits
                        .unwrap_or((identity.history_limit, identity.response_limit));
                    let request_identity = StoreIdentity {
                        root: identity.root.clone(),
                        local: identity.local.clone(),
                        user_id: identity.user_id.clone(),
                        machine_id: identity.machine_id.clone(),
                        binding_id: identity.binding_id.clone(),
                        history_limit,
                        response_limit,
                        config_file: identity.config_file.clone(),
                    };
                    let mut ctx = LaneCtx {
                        identity: &request_identity,
                        source: &store,
                        control: &mut writer,
                        request: &work.control,
                    };
                    let (result, wake) = execute(&mut ctx, work.command);
                    if wake {
                        let callback = actor.wake.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        if let Some(callback) = callback {
                            callback();
                        }
                    }
                    let _ = work.reply.send(result);
                }
                actor.closed.store(true, Ordering::Release);
            })
            .map_err(|_| unavailable())?;
        Ok(Self {
            sender: LaneSender { shared },
            thread: Mutex::new(Some(thread)),
        })
    }
    pub fn sender(&self) -> LaneSender {
        self.sender.clone()
    }
    pub fn set_waker(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self
            .sender
            .shared
            .wake
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(wake);
    }
    pub fn close(&self) {
        let _ = self.close_checked();
    }
    pub fn close_checked(&self) -> crate::ports::RuntimeResult<()> {
        if !self.sender.shared.closed.swap(true, Ordering::AcqRel) {
            let _ = self.sender.shared.sender.send(Message::Stop);
        }
        if let Some(thread) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            thread.join().map_err(|_| {
                crate::ports::RuntimeError::Unavailable(
                    "Mutation lane terminated unexpectedly.".into(),
                )
            })?;
        }
        Ok(())
    }
}
impl Drop for MutationLane {
    fn drop(&mut self) {
        self.close();
    }
}

impl LaneSender {
    pub fn call(&self, command: LaneCommand, control: &RequestControl) -> Result<LaneReply> {
        self.call_with_limits(command, control, None)
    }
    pub(crate) fn call_with_limits(
        &self,
        command: LaneCommand,
        control: &RequestControl,
        limits: Option<(usize, usize)>,
    ) -> Result<LaneReply> {
        control.check()?;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let state = Arc::new(Mutex::new(WorkState::Queued));
        let (reply, receiver) = mpsc::channel();
        let work = Work {
            command,
            control: control.clone(),
            limits,
            state: state.clone(),
            reply,
        };
        match self.shared.sender.try_send(Message::Work(work)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(DomainError::new(
                    "capacity",
                    "The store mutation queue is full.",
                ))
            }
            Err(TrySendError::Disconnected(_)) => return Err(unavailable()),
        }
        loop {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(unavailable()),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Err(error) = control.check() {
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        if *state == WorkState::Queued {
                            *state = WorkState::Cancelled;
                            return Err(error);
                        }
                    }
                }
            }
        }
    }
}

fn execute(ctx: &mut LaneCtx<'_>, command: LaneCommand) -> (Result<LaneReply>, bool) {
    match command {
        LaneCommand::Apply(prepared) => (
            agentlaw_flows::apply::apply(ctx, prepared).map(LaneReply::Applied),
            true,
        ),
        LaneCommand::Recover => {
            let original = ctx.request.clone();
            let recovery = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
                original.phase(phase)
            });
            let mut recovery_ctx = LaneCtx {
                identity: ctx.identity,
                source: ctx.source,
                control: ctx.control,
                request: &recovery,
            };
            (
                agentlaw_flows::recovery::recover(&mut recovery_ctx).map(|_| LaneReply::Done),
                true,
            )
        }
        LaneCommand::SetExactGeneration {
            generation_id,
            exact_file,
        } => (
            ctx.control
                .set_exact_generation(&generation_id, &exact_file)
                .map(|_| LaneReply::Done),
            false,
        ),
        LaneCommand::SyncPersist(row) => (
            ctx.control
                .sync_persist(&row.id, &row.source, &row.state_json, row.active)
                .map(|_| LaneReply::Done),
            false,
        ),
        LaneCommand::SyncReserve {
            id,
            operation,
            digest,
        } => (
            ctx.control
                .sync_reserve(&id, &operation, &digest)
                .map(LaneReply::Reserved),
            false,
        ),
        LaneCommand::SyncComplete { id, response_json } => (
            ctx.control
                .sync_complete(&id, &response_json)
                .map(|_| LaneReply::Done),
            false,
        ),
        LaneCommand::PublishSyncOverlay(input) => {
            let result = (|| {
                ctx.request.check()?;
                let baseline = Store::open_read_only_with_coordination(
                    &input.baseline_root,
                    &input.baseline_local,
                    ctx.source.coordination_root(),
                )
                .map_err(storage)?;
                let resolved = Store::open_read_only_with_coordination(
                    &input.resolved_root,
                    &input.resolved_local,
                    ctx.source.coordination_root(),
                )
                .map_err(storage)?;
                ctx.source
                    .publish_sync_overlay_control(
                        &input.publication_id,
                        &baseline,
                        &resolved,
                        &input.guard,
                        None,
                        Some(&ctx.request.cancel),
                    )
                    .map(LaneReply::Published)
                    .map_err(storage)
            })();
            (result, true)
        }
        LaneCommand::ImportReceipt(operation_id) => (
            ctx.source
                .sync_receipt(&operation_id)
                .map(LaneReply::ImportReceipt)
                .map_err(storage),
            false,
        ),
        LaneCommand::ImportChoiceDecided(operation_id) => (
            ctx.source
                .import_choice_decided(&operation_id)
                .map(LaneReply::Reserved)
                .map_err(storage),
            false,
        ),
        LaneCommand::ImportChoices {
            operation_id,
            choices,
        } => (
            ctx.source
                .choose_import_conflicts(&operation_id, &choices, true)
                .map(LaneReply::Published)
                .map_err(storage),
            true,
        ),
        LaneCommand::PublishImport(input) => {
            let result = (|| {
                ctx.request.check()?;
                let staged = Store::open_read_only_with_coordination(
                    &input.staged_root,
                    &input.staged_local,
                    ctx.source.coordination_root(),
                )
                .map_err(storage)?;
                ctx.source
                    .publish_imported_source_at(
                        &input.operation_id,
                        &staged,
                        input.expected_generation,
                        input.staged_generation,
                    )
                    .map(LaneReply::Published)
                    .map_err(storage)
            })();
            (result, true)
        }
    }
}
