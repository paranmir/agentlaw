//! Control and data listeners, connection admission and reservations.
use super::{dispatch::Dispatcher, health, lifecycle::Lifecycle, mailbox::Mailbox};
use crate::{
    installation::{
        config, delivery,
        registry::{ConfigRegistry, ConfigState},
    },
    ports::{ActivityKind, HealthSink},
    protocol::*,
    transport,
};
use agentlaw_contracts::RequestControl;
use std::{
    collections::HashMap,
    io::{self, Read},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

pub struct Host {
    pub control: SocketAddr,
    pub data: SocketAddr,
    stop: Arc<(Mutex<bool>, Condvar)>,
}
impl Drop for Host {
    fn drop(&mut self) {
        *self.stop.0.lock().unwrap() = true;
        self.stop.1.notify_all();
        let _ = TcpStream::connect_timeout(&self.control, Duration::from_millis(100));
        let _ = TcpStream::connect_timeout(&self.data, Duration::from_millis(100));
    }
}
pub struct HostLimits {
    pub admission_slots: usize,
    pub body_budget_bytes: u64,
}
impl Default for HostLimits {
    fn default() -> Self {
        Self {
            admission_slots: 64,
            body_budget_bytes: 64 * 1024 * 1024,
        }
    }
}
type Active = Arc<Mutex<HashMap<RequestId, Arc<AtomicBool>>>>;
struct Context<D> {
    dispatcher: Arc<D>,
    registry: Arc<ConfigRegistry>,
    health: HealthSink,
    lifecycle: Lifecycle,
    active: Active,
    slots: Arc<AtomicUsize>,
    body_bytes: Arc<AtomicU64>,
    limits: HostLimits,
    endpoint: transport::Endpoint,
    state: PathBuf,
    stop: Arc<(Mutex<bool>, Condvar)>,
}
struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct BodyReservation {
    count: Arc<AtomicU64>,
    bytes: u64,
}
impl Drop for BodyReservation {
    fn drop(&mut self) {
        self.count.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
struct Registration(Active, RequestId);
impl Drop for Registration {
    fn drop(&mut self) {
        self.0.lock().unwrap().remove(&self.1);
    }
}
fn reserve_slot<D>(ctx: &Context<D>) -> Option<Slot> {
    ctx.slots
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < ctx.limits.admission_slots).then_some(n + 1)
        })
        .ok()
        .map(|_| Slot(ctx.slots.clone()))
}
fn reserve_body<D>(ctx: &Context<D>, bytes: u64) -> Option<BodyReservation> {
    ctx.body_bytes
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(bytes)
                .filter(|sum| *sum <= ctx.limits.body_budget_bytes)
        })
        .ok()
        .map(|_| BodyReservation {
            count: ctx.body_bytes.clone(),
            bytes,
        })
}
fn hello<D>(stream: &mut TcpStream, ctx: &Context<D>) -> io::Result<bool> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let h: Hello = transport::receive(stream, MAX_CONTROL_FRAME_BYTES)?;
    let reply = if h.protocol != PROTOCOL_VERSION {
        HelloReply::ProtocolMismatch
    } else if h.installation_build != ctx.endpoint.installation_build {
        HelloReply::BuildMismatch
    } else if h.secret != ctx.endpoint.secret {
        HelloReply::NotAuthorized
    } else {
        ctx.lifecycle.hello_reply()
    };
    transport::send(stream, &reply, MAX_CONTROL_REPLY_BYTES)?;
    Ok(reply == HelloReply::Accepted)
}
pub fn start<D: Dispatcher>(
    state: &Path,
    build: &str,
    secret: &str,
    dispatcher: Arc<D>,
    registry: Arc<ConfigRegistry>,
    health: HealthSink,
    lifecycle: Lifecycle,
    limits: HostLimits,
) -> io::Result<Host> {
    let control = TcpListener::bind("127.0.0.1:0")?;
    let data = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = transport::Endpoint {
        protocol: PROTOCOL_VERSION,
        installation_build: build.into(),
        control: control.local_addr()?,
        data: data.local_addr()?,
        secret: secret.into(),
        pid: std::process::id(),
    };
    transport::write_endpoint(state, &endpoint)?;
    let stop = Arc::new((Mutex::new(false), Condvar::new()));
    let ctx = Arc::new(Context {
        dispatcher,
        registry,
        health,
        lifecycle,
        active: Arc::new(Mutex::new(HashMap::new())),
        slots: Arc::new(AtomicUsize::new(0)),
        body_bytes: Arc::new(AtomicU64::new(0)),
        limits,
        endpoint: endpoint.clone(),
        state: state.to_owned(),
        stop: stop.clone(),
    });
    let c = ctx.clone();
    thread::spawn(move || accept_control(control, c));
    let c = ctx.clone();
    thread::spawn(move || accept_data(data, c));
    thread::spawn(move || lifecycle_timer(ctx));
    Ok(Host {
        control: endpoint.control,
        data: endpoint.data,
        stop,
    })
}
fn stopped<D>(ctx: &Context<D>) -> bool {
    *ctx.stop.0.lock().unwrap()
}
fn lifecycle_timer<D>(ctx: Arc<Context<D>>) {
    let (lock, wake) = &*ctx.stop;
    let mut stop = lock.lock().unwrap();
    while !*stop {
        let (next, _) = wake.wait_timeout(stop, Duration::from_secs(1)).unwrap();
        stop = next;
        if !*stop {
            drop(stop);
            ctx.lifecycle.poll();
            stop = lock.lock().unwrap();
        }
    }
}
fn next_connection<D>(listener: &TcpListener, ctx: &Context<D>) -> Option<TcpStream> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(_) => {
                let stop = ctx.stop.0.lock().unwrap();
                if *stop {
                    return None;
                }
                let (stop, _) = ctx
                    .stop
                    .1
                    .wait_timeout(stop, Duration::from_millis(50))
                    .unwrap();
                if *stop {
                    return None;
                }
            }
        }
    }
}
fn accept_control<D: Dispatcher>(listener: TcpListener, ctx: Arc<Context<D>>) {
    while let Some(stream) = next_connection(&listener, &ctx) {
        if stopped(&ctx) {
            break;
        }
        let ctx = ctx.clone();
        thread::spawn(move || serve_control(stream, ctx));
    }
}
fn accept_data<D: Dispatcher>(listener: TcpListener, ctx: Arc<Context<D>>) {
    while let Some(mut stream) = next_connection(&listener, &ctx) {
        if stopped(&ctx) {
            break;
        }
        if stream.set_nonblocking(false).is_err() {
            continue;
        }
        let Some(slot) = reserve_slot(&ctx) else {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
            let _ = transport::send(
                &mut stream,
                &DataFrame::RejectedUnread {
                    reason: RejectReason::Busy,
                },
                64,
            );
            continue;
        };
        let ctx = ctx.clone();
        thread::spawn(move || serve_data(stream, ctx, slot));
    }
}
fn serve_control<D: Dispatcher>(mut stream: TcpStream, ctx: Arc<Context<D>>) {
    if !hello(&mut stream, &ctx).unwrap_or(false) {
        return;
    }
    while let Ok(request) =
        transport::receive::<ControlRequest>(&mut stream, MAX_CONTROL_FRAME_BYTES)
    {
        let reply = match request {
            ControlRequest::Health => {
                ControlReply::Health(health::snapshot(&ctx.health, &ctx.lifecycle))
            }
            ControlRequest::Cancel(id) => match ctx.active.lock().unwrap().get(&id) {
                Some(flag) => {
                    flag.store(true, Ordering::Release);
                    ControlReply::CancellationRequested
                }
                None => ControlReply::NotActive,
            },
            ControlRequest::Lease { client } => {
                ctx.lifecycle.lease(client);
                ControlReply::LeaseRenewed
            }
            ControlRequest::ResolveBinding => match ctx.registry.current() {
                ConfigState::Ready(s) => ControlReply::Binding(s.reference),
                ConfigState::Missing => {
                    ControlReply::BindingError(BindingError::ConnectionRequired)
                }
                ConfigState::Invalid(s) => {
                    ControlReply::BindingError(BindingError::ConfigInvalid(s))
                }
            },
            ControlRequest::Shutdown { .. } => {
                ctx.lifecycle.stop();
                ControlReply::ShutdownAccepted
            }
            ControlRequest::ConfigGet { key } => {
                let result = match ctx.registry.current() {
                    ConfigState::Ready(snapshot) => {
                        config::get_value(&ctx.state, Some(&snapshot.config), &key)
                    }
                    ConfigState::Missing => config::get_value(&ctx.state, None, &key),
                    ConfigState::Invalid(reason) => Err(agentlaw_contracts::DomainError::new(
                        "invalid_configuration",
                        reason,
                    )),
                };
                match result {
                    Ok(value) => ControlReply::Config(value),
                    Err(error) => ControlReply::ConfigError(error),
                }
            }
        };
        if transport::send(&mut stream, &reply, MAX_CONTROL_REPLY_BYTES).is_err() {
            break;
        }
    }
}
fn reject(stream: &mut TcpStream, id: RequestId, reason: RejectReason) {
    let _ = transport::send(
        stream,
        &DataFrame::Rejected { id, reason },
        MAX_CONTROL_REPLY_BYTES,
    );
}
fn serve_data<D: Dispatcher>(mut stream: TcpStream, ctx: Arc<Context<D>>, slot: Slot) {
    if !hello(&mut stream, &ctx).unwrap_or(false) {
        return;
    }
    let Ok(header) = transport::receive::<CallHeader>(&mut stream, MAX_CONTROL_FRAME_BYTES) else {
        return;
    };
    if let Err(reason) = admit_call(&mut stream, ctx, slot, header) {
        reject(&mut stream, header.id, reason);
    }
}
fn admit_call<D: Dispatcher>(
    stream: &mut TcpStream,
    ctx: Arc<Context<D>>,
    slot: Slot,
    header: CallHeader,
) -> Result<(), RejectReason> {
    let id = header.id;
    if matches!(
        (header.kind, header.target),
        (RequestKind::Admin, CallTarget::Setup)
    ) {
        return Err(RejectReason::InvalidBody(
            agentlaw_contracts::DomainError::new(
                "invalid_input",
                "Admin commands require a store target.",
            ),
        ));
    }
    if header.body_len > MAX_WIRE_BODY_BYTES {
        return Err(RejectReason::BodyTooLarge);
    }
    if matches!(
        (header.kind, header.target),
        (RequestKind::Admin, CallTarget::Setup)
    ) {
        return Err(RejectReason::InvalidBody(
            agentlaw_contracts::DomainError::new(
                "invalid_input",
                "Admin commands require a store binding.",
            ),
        ));
    }
    if matches!(
        (header.kind, header.target),
        (RequestKind::Setup, CallTarget::Store(_))
            | (RequestKind::Query | RequestKind::Command, CallTarget::Setup)
    ) {
        return Err(RejectReason::KindMismatch);
    }
    let body_reservation =
        reserve_body(&ctx, u64::from(header.body_len)).ok_or(RejectReason::Busy)?;
    let flag = Arc::new(AtomicBool::new(false));
    {
        let mut active = ctx.active.lock().unwrap();
        if active.contains_key(&id) {
            return Err(RejectReason::Busy);
        }
        active.insert(id, flag.clone());
    }
    let registration = Registration(ctx.active.clone(), id);
    if transport::send(stream, &DataFrame::Proceed { id }, MAX_CONTROL_REPLY_BYTES).is_err() {
        return Ok(());
    }
    let mut body = vec![0; header.body_len as usize];
    if stream.read_exact(&mut body).is_err() {
        return Ok(());
    }
    let classified = decode_body(header.kind, &body)?;
    drop(body);
    let guard = ctx
        .lifecycle
        .begin(ActivityKind::Execution)
        .map_err(|e| e.stopping_reason().unwrap_or(RejectReason::Busy))?;
    let history = matches!(&classified, Classified::Query(QueryRequest::History(_)));
    let admin = matches!(&classified, Classified::Admin(command) if !matches!(command, AdminCommand::ImportCall { .. }));
    let fallback = match header.target {
        CallTarget::Store(reference) => ctx.registry.resolve(&reference).ok().map(|s| {
            if history {
                s.config
                    .response_limit_bytes
                    .min(s.config.history_response_limit_bytes)
            } else {
                s.config.response_limit_bytes
            }
        }),
        CallTarget::Setup => None,
    };
    let admitted = ctx.dispatcher.admit(&header.target, classified)?;
    let mailbox = Arc::new(Mailbox::new(id, 16));
    let output = mailbox.clone();
    thread::spawn(move || {
        run_execution(
            ctx,
            admitted,
            flag,
            output,
            id,
            fallback,
            admin,
            (guard, registration, slot, body_reservation),
        )
    });
    send_frames(stream, &mailbox);
    Ok(())
}
fn run_execution<D: Dispatcher>(
    ctx: Arc<Context<D>>,
    admitted: D::Admitted,
    flag: Arc<AtomicBool>,
    mailbox: Arc<Mailbox>,
    id: RequestId,
    fallback: Option<usize>,
    admin: bool,
    resources: (
        super::lifecycle::LifecycleGuard,
        Registration,
        Slot,
        BodyReservation,
    ),
) {
    let progress = mailbox.clone();
    let control = RequestControl::new(flag, move |phase| progress.publish_progress(phase));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.dispatcher
            .run_with_limit(admitted, &control)
            .and_then(|(value, selected)| {
                if admin {
                    return Ok(value);
                }
                delivery::adapt(
                    value,
                    &ctx.state,
                    selected
                        .or(fallback)
                        .unwrap_or(MAX_RESULT_FRAME_BYTES as usize)
                        .min(MAX_RESULT_FRAME_BYTES as usize),
                )
            })
    }));
    let terminal = match result {
        Ok(Ok(result)) => DataFrame::Completed { id, result },
        Ok(Err(error)) => DataFrame::Failed { id, error },
        Err(_) => DataFrame::Failed {
            id,
            error: agentlaw_contracts::DomainError::new(
                "internal_error",
                "The request execution failed unexpectedly.",
            ),
        },
    };
    drop(resources);
    mailbox.publish_terminal(terminal);
}
fn send_frames(stream: &mut TcpStream, mailbox: &Mailbox) {
    loop {
        let Some(frame) = mailbox.receive(Duration::from_secs(60)) else {
            continue;
        };
        let terminal = matches!(
            frame,
            DataFrame::Completed { .. } | DataFrame::Failed { .. }
        );
        if transport::send(stream, &frame, MAX_RESULT_FRAME_BYTES).is_err() || terminal {
            break;
        }
    }
}
#[cfg(test)]
mod tests;
