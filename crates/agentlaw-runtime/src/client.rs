//! Frontend client for the installation runtime.
use crate::{
    ports::{HealthSnapshot, RuntimeError},
    protocol::*,
    transport::{self, Endpoint},
};
use agentlaw_contracts::{
    wire::{self, ValidatedRequest},
    DomainError, RequestControl,
};
use serde_json::Value;
use std::{
    io::{self, Write},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum ClientError {
    Runtime(RuntimeError),
    Rejected(RejectReason),
    MutationOutcomeUnknown,
    Domain(DomainError),
}
fn unavailable(error: impl std::fmt::Display) -> ClientError {
    ClientError::Runtime(RuntimeError::Unavailable(error.to_string()))
}
struct Inner {
    state: PathBuf,
    endpoint: Endpoint,
    client: String,
    build: String,
}
pub struct Client {
    inner: Arc<Inner>,
}

pub enum ShutdownState {
    NotRunning,
    Stopping,
}

pub fn shutdown_installed(_state: &Path, _reason: &str) -> Result<ShutdownState, ClientError> {
    Err(unavailable("shutdown_installed is not implemented"))
}

pub struct AttachLimits {
    pub start_wait: Duration,
    pub hello_timeout: Duration,
}

impl Default for AttachLimits {
    fn default() -> Self {
        Self {
            start_wait: Duration::from_secs(30),
            hello_timeout: Duration::from_secs(5),
        }
    }
}

pub enum CandidateExit {
    AlreadyOwned,
    StartBlocked(DomainError),
    Failed(String),
}

pub trait RuntimeLauncher: Send + Sync {
    fn launch(&self) -> io::Result<Box<dyn RuntimeCandidate>>;
}

pub trait RuntimeCandidate: Send {
    fn try_exit(&mut self) -> io::Result<Option<CandidateExit>>;
}

pub enum Probe {
    NotRunning { stale_endpoint: bool },
    Running(HealthSnapshot),
    Unresponsive,
    Mismatch(HelloReply),
}

fn hello(
    endpoint: &Endpoint,
    build: &str,
    timeout: Duration,
    connect_timeout: Duration,
) -> io::Result<(TcpStream, HelloReply)> {
    if !endpoint.control.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "non-loopback endpoint",
        ));
    }
    let mut stream = TcpStream::connect_timeout(
        &endpoint.control,
        connect_timeout.max(Duration::from_millis(1)),
    )?;
    let timeout = timeout.max(Duration::from_millis(1));
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    transport::send(
        &mut stream,
        &Hello {
            protocol: PROTOCOL_VERSION,
            installation_build: build.to_owned(),
            secret: endpoint.secret.clone(),
        },
        MAX_CONTROL_FRAME_BYTES,
    )?;
    let reply = transport::receive(&mut stream, MAX_CONTROL_REPLY_BYTES)?;
    Ok((stream, reply))
}

pub fn probe(state: &Path, installation_build: &str, hello_timeout: Duration) -> Probe {
    let endpoint = match transport::read_endpoint(state) {
        Ok(endpoint) => endpoint,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Probe::NotRunning {
                stale_endpoint: false,
            }
        }
        Err(_) => return Probe::Unresponsive,
    };
    match hello(
        &endpoint,
        installation_build,
        hello_timeout,
        Duration::from_secs(3),
    ) {
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => Probe::NotRunning {
            stale_endpoint: true,
        },
        Err(_) => Probe::Unresponsive,
        Ok((mut stream, HelloReply::Accepted)) => {
            if transport::send(
                &mut stream,
                &ControlRequest::Health,
                MAX_CONTROL_FRAME_BYTES,
            )
            .is_err()
            {
                return Probe::Unresponsive;
            }
            match transport::receive(&mut stream, MAX_CONTROL_REPLY_BYTES) {
                Ok(ControlReply::Health(snapshot)) => Probe::Running(snapshot),
                _ => Probe::Unresponsive,
            }
        }
        Ok((_, reply @ (HelloReply::ProtocolMismatch | HelloReply::BuildMismatch))) => {
            Probe::Mismatch(reply)
        }
        Ok(_) => Probe::Unresponsive,
    }
}

impl Client {
    pub fn setup(
        &self,
        _command: SetupCommand,
        _control: &RequestControl,
    ) -> Result<Value, ClientError> {
        Err(unavailable("setup is not implemented"))
    }

    pub fn config_get(&self, _key: &str) -> Result<Value, ClientError> {
        Err(unavailable("config_get is not implemented"))
    }
    pub fn attach(
        state: &Path,
        installation_build: &str,
        launcher: &dyn RuntimeLauncher,
        limits: AttachLimits,
    ) -> Result<Client, ClientError> {
        let deadline = Instant::now() + limits.start_wait;
        let mut candidate: Option<Box<dyn RuntimeCandidate>> = None;
        let mut failed_endpoint: Option<(u32, std::net::SocketAddr, String)> = None;
        loop {
            if Instant::now() >= deadline {
                return Err(ClientError::Runtime(RuntimeError::Unresponsive));
            }
            if let Ok(endpoint) = transport::read_endpoint(state) {
                let identity = (endpoint.pid, endpoint.control, endpoint.secret.clone());
                if failed_endpoint.as_ref() != Some(&identity) {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    match hello(
                        &endpoint,
                        installation_build,
                        limits.hello_timeout.min(remaining),
                        Duration::from_secs(3).min(remaining),
                    ) {
                        Ok((_, HelloReply::Accepted)) => {
                            if let Ok(client) = Self::connect(state, installation_build) {
                                return Ok(client);
                            }
                        }
                        Ok((
                            _,
                            reply @ (HelloReply::BuildMismatch | HelloReply::ProtocolMismatch),
                        )) => {
                            return Err(ClientError::Runtime(RuntimeError::Protocol(format!(
                                "hello: {reply:?}"
                            ))));
                        }
                        Err(ref error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::ConnectionRefused
                                    | io::ErrorKind::TimedOut
                                    | io::ErrorKind::WouldBlock
                            ) =>
                        {
                            failed_endpoint = Some(identity);
                        }
                        _ => {}
                    }
                }
            }
            if let Some(active) = candidate.as_mut() {
                match active.try_exit().map_err(unavailable)? {
                    Some(CandidateExit::AlreadyOwned) => candidate = None,
                    Some(CandidateExit::StartBlocked(error)) => {
                        return Err(ClientError::Domain(error))
                    }
                    Some(CandidateExit::Failed(reason)) => return Err(unavailable(reason)),
                    None => {}
                }
            } else {
                candidate = Some(launcher.launch().map_err(unavailable)?);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn connect(state: &Path, installation_build: &str) -> Result<Self, ClientError> {
        let mut endpoint = transport::read_endpoint(state).map_err(unavailable)?;
        let client = uuid::Uuid::new_v4().to_string();
        for attempt in 0..2 {
            match control_request(
                &endpoint,
                installation_build,
                &ControlRequest::Lease {
                    client: client.clone(),
                },
            ) {
                Ok(ControlReply::LeaseRenewed) => break,
                Ok(_) => return Err(unavailable("invalid lease reply")),
                Err(e) if attempt == 0 && e.kind() != io::ErrorKind::PermissionDenied => {
                    endpoint = transport::read_endpoint(state).map_err(unavailable)?;
                }
                Err(e) => return Err(unavailable(e)),
            }
        }
        let inner = Arc::new(Inner {
            state: state.to_owned(),
            endpoint,
            client,
            build: installation_build.to_owned(),
        });
        let weak = Arc::downgrade(&inner);
        thread::spawn(move || loop {
            thread::park_timeout(Duration::from_secs(10));
            let Some(inner) = weak.upgrade() else {
                break;
            };
            let _ = control_request(
                &inner.endpoint,
                &inner.build,
                &ControlRequest::Lease {
                    client: inner.client.clone(),
                },
            );
        });
        Ok(Self { inner })
    }
    pub fn resolve_binding(&self) -> Result<BindingRef, ClientError> {
        match self.control(ControlRequest::ResolveBinding)? {
            ControlReply::Binding(b) => Ok(b),
            ControlReply::BindingError(e) => Err(ClientError::Rejected(match e {
                BindingError::BindingChanged => RejectReason::BindingChanged,
                BindingError::ConnectionRequired => RejectReason::ConnectionRequired,
                BindingError::ConfigInvalid(s) => RejectReason::ConfigInvalid(s),
            })),
            _ => Err(unavailable("invalid binding reply")),
        }
    }
    pub fn health(&self) -> Result<HealthSnapshot, ClientError> {
        match self.control(ControlRequest::Health)? {
            ControlReply::Health(h) => Ok(h),
            _ => Err(unavailable("invalid health reply")),
        }
    }
    pub fn cancel(&self, id: RequestId) -> Result<ControlReply, ClientError> {
        self.control(ControlRequest::Cancel(id))
    }
    pub fn shutdown(&self, reason: &str) -> Result<ControlReply, ClientError> {
        self.control(ControlRequest::Shutdown {
            reason: reason.into(),
        })
    }
    fn control(&self, request: ControlRequest) -> Result<ControlReply, ClientError> {
        control_request(&self.inner.endpoint, &self.inner.build, &request).map_err(unavailable)
    }
    pub fn call(
        &self,
        target: CallTarget,
        request: ValidatedRequest,
        control: &RequestControl,
    ) -> Result<Value, ClientError> {
        let kind = request_kind(&request);
        let body = wire::encode_wire(request.request()).map_err(ClientError::Domain)?;
        if body.len() > MAX_WIRE_BODY_BYTES as usize {
            return Err(ClientError::Rejected(RejectReason::BodyTooLarge));
        }
        for attempt in 0..2 {
            if control.cancel.load(Ordering::Acquire) {
                return Err(ClientError::Domain(DomainError::new(
                    "cancelled",
                    "The request was cancelled.",
                )));
            }
            let endpoint = if attempt == 0 {
                self.inner.endpoint.clone()
            } else {
                transport::read_endpoint(&self.inner.state).map_err(unavailable)?
            };
            let mut stream = match transport::connect(&endpoint, endpoint.data, &self.inner.build) {
                Ok(s) => s,
                Err(e) => {
                    if attempt == 0 && e.kind() != io::ErrorKind::PermissionDenied {
                        continue;
                    }
                    return Err(unavailable(e));
                }
            };
            let id = RequestId::new();
            let header = CallHeader {
                id,
                target,
                kind,
                body_len: body.len() as u32,
            };
            if let Err(e) = transport::send(&mut stream, &header, MAX_CONTROL_FRAME_BYTES) {
                if attempt == 0 {
                    continue;
                }
                return Err(unavailable(e));
            }
            let mut reader = stream.try_clone().map_err(unavailable)?;
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || loop {
                let frame = transport::receive::<DataFrame>(&mut reader, MAX_RESULT_FRAME_BYTES);
                if matches!(frame, Ok(DataFrame::Proceed { .. })) {
                    if let Err(error) = reader.set_read_timeout(None) {
                        let _ = tx.send(Err(error));
                        break;
                    }
                }
                let done = frame.is_err()
                    || matches!(
                        frame,
                        Ok(DataFrame::Completed { .. }
                            | DataFrame::Failed { .. }
                            | DataFrame::Rejected { .. }
                            | DataFrame::RejectedUnread { .. })
                    );
                if tx.send(frame).is_err() || done {
                    break;
                }
            });
            let mut proceeded = false;
            let mut sent_cancel = false;
            loop {
                if control.cancel.load(Ordering::Acquire) {
                    if !proceeded {
                        let _ = stream.shutdown(Shutdown::Both);
                        return Err(ClientError::Domain(DomainError::new(
                            "cancelled",
                            "The request was cancelled.",
                        )));
                    }
                    if !sent_cancel {
                        let _ = self.cancel(id);
                        sent_cancel = true;
                    }
                }
                let frame = match rx.recv_timeout(Duration::from_millis(30)) {
                    Ok(Ok(frame)) => frame,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Ok(Err(e)) => {
                        if proceeded && kind != RequestKind::Query {
                            return Err(ClientError::MutationOutcomeUnknown);
                        }
                        if attempt == 0 {
                            break;
                        }
                        return Err(unavailable(e));
                    }
                    Err(_) => {
                        if proceeded && kind != RequestKind::Query {
                            return Err(ClientError::MutationOutcomeUnknown);
                        }
                        if attempt == 0 {
                            break;
                        }
                        return Err(unavailable("connection closed"));
                    }
                };
                match frame {
                    DataFrame::Proceed { id: got } if got == id && !proceeded => {
                        proceeded = true;
                        if control.cancel.load(Ordering::Acquire) {
                            let _ = self.cancel(id);
                            let _ = stream.shutdown(Shutdown::Both);
                            return Err(ClientError::Domain(DomainError::new(
                                "cancelled",
                                "The request was cancelled.",
                            )));
                        }
                        if let Err(e) = stream.write_all(&body) {
                            if kind != RequestKind::Query {
                                return Err(ClientError::MutationOutcomeUnknown);
                            }
                            if attempt == 0 {
                                break;
                            }
                            return Err(unavailable(e));
                        }
                    }
                    DataFrame::Rejected { id: got, reason } if got == id => {
                        if reason.is_transient() && attempt == 0 {
                            break;
                        }
                        return Err(ClientError::Rejected(reason));
                    }
                    DataFrame::RejectedUnread { reason } => {
                        if reason.is_transient() && attempt == 0 {
                            break;
                        }
                        return Err(ClientError::Rejected(reason));
                    }
                    DataFrame::Progress { id: got, phase } if got == id && proceeded => {
                        control.phase(&phase)
                    }
                    DataFrame::Completed { id: got, result } if got == id && proceeded => {
                        return Ok(result)
                    }
                    DataFrame::Failed { id: got, error } if got == id && proceeded => {
                        return Err(ClientError::Domain(error))
                    }
                    _ => {
                        let _ = stream.shutdown(Shutdown::Both);
                        return Err(ClientError::Runtime(RuntimeError::Protocol(
                            "unexpected data frame".into(),
                        )));
                    }
                }
            }
        }
        Err(unavailable("retry exhausted"))
    }
}
fn control_request(
    endpoint: &Endpoint,
    build: &str,
    request: &ControlRequest,
) -> io::Result<ControlReply> {
    let mut stream = transport::connect(endpoint, endpoint.control, build)?;
    transport::send(&mut stream, request, MAX_CONTROL_FRAME_BYTES)?;
    transport::receive(&mut stream, MAX_CONTROL_REPLY_BYTES)
}
#[cfg(test)]
mod tests;
