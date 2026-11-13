//! Runtime-backed frontend adapter.
use agentlaw_contracts::{wire, DomainError, Request, RequestControl, Result};
use agentlaw_runtime::{
    client::{AttachLimits, Client, ClientError},
    ports::RuntimeError,
    protocol::{self, BindingRef, CallTarget, Classified, RejectReason},
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{mpsc, OnceLock},
};

static BUILD: OnceLock<std::result::Result<String, String>> = OnceLock::new();

pub struct RuntimeBackend {
    state: PathBuf,
    build: String,
    launcher: crate::runtime_launcher::Launcher,
    client: Option<Client>,
    pending: Option<mpsc::Receiver<std::result::Result<Client, ClientError>>>,
}

impl RuntimeBackend {
    pub fn new(state: PathBuf, background: bool) -> Result<Self> {
        let executable = std::env::current_exe()
            .map_err(|e| DomainError::new("invalid_installation", e.to_string()))?;
        let build = BUILD
            .get_or_init(|| {
                agentlaw_runtime::installation::build_identity(&executable)
                    .map_err(|e| e.to_string())
            })
            .clone()
            .map_err(|e| DomainError::new("invalid_installation", e))?;
        let launcher = crate::runtime_launcher::Launcher {
            executable: executable.clone(),
            state: state.clone(),
        };
        let pending = if background {
            let (send, receive) = mpsc::channel();
            let launch = crate::runtime_launcher::Launcher {
                executable,
                state: state.clone(),
            };
            let path = state.clone();
            let identity = build.clone();
            std::thread::spawn(move || {
                let _ = send.send(Client::attach(
                    &path,
                    &identity,
                    &launch,
                    AttachLimits::default(),
                ));
            });
            Some(receive)
        } else {
            None
        };
        Ok(Self {
            state,
            build,
            launcher,
            client: None,
            pending,
        })
    }
    fn client(&mut self) -> std::result::Result<&Client, ClientError> {
        if self.client.is_none() {
            let client = if let Some(pending) = self.pending.take() {
                pending
                    .recv()
                    .map_err(|e| ClientError::Runtime(RuntimeError::Unavailable(e.to_string())))??
            } else {
                Client::attach(
                    &self.state,
                    &self.build,
                    &self.launcher,
                    AttachLimits::default(),
                )?
            };
            self.client = Some(client);
        }
        Ok(self.client.as_ref().expect("client initialized"))
    }
    pub fn setup(
        &mut self,
        command: protocol::SetupCommand,
        control: &RequestControl,
    ) -> Result<Value> {
        for attempt in 0..2 {
            let result = self
                .client()
                .and_then(|client| client.setup_once(command.clone(), control));
            match result {
                Ok(value) => return Ok(value),
                Err(ClientError::BeforeBody(_))
                | Err(ClientError::Runtime(RuntimeError::Unavailable(_)))
                    if attempt == 0 && self.client.is_some() =>
                {
                    self.client = None
                }
                Err(ClientError::Rejected(reason)) if reason.is_transient() && attempt == 0 => {}
                Err(failure) => {
                    self.client = None;
                    return Err(error(failure, false));
                }
            }
        }
        unreachable!()
    }
    pub fn config_get_if_running(&self, key: &str) -> Result<Option<Value>> {
        match agentlaw_runtime::client::probe(
            &self.state,
            &self.build,
            std::time::Duration::from_secs(5),
        ) {
            agentlaw_runtime::client::Probe::Running(_) => {
                let client =
                    Client::connect(&self.state, &self.build).map_err(|e| error(e, false))?;
                client
                    .config_get(key)
                    .map(Some)
                    .map_err(|e| error(e, false))
            }
            agentlaw_runtime::client::Probe::NotRunning { .. } => Ok(None),
            agentlaw_runtime::client::Probe::Unresponsive => Err(error(
                ClientError::Runtime(RuntimeError::Unresponsive),
                false,
            )),
            agentlaw_runtime::client::Probe::Mismatch(reply) => Err(error(
                ClientError::Runtime(RuntimeError::Protocol(format!("hello: {reply:?}"))),
                false,
            )),
        }
    }
}

fn has_reference(request: &Request) -> bool {
    match request {
        Request::RememberThis(r) => r.pending_batch_ref.is_some() || r.authoring_ref.is_some(),
        Request::Sync(r) => r.operation_id.is_some() || r.request_id.is_some(),
        _ => false,
    }
}
fn error(error: ClientError, has_reference: bool) -> DomainError {
    match error {
        ClientError::Domain(error) => error,
        ClientError::MutationOutcomeUnknown => DomainError::new("execution_outcome_unknown", if has_reference {
            "The connection ended before the operation's result was received. It may have completed. Use the existing reference to inspect or resume that work; do not repeat an unconfirmed write."
        } else {
            "The connection ended before the operation's result was received. It may have completed, and no execution reference is available to determine its outcome. Do not repeat this write automatically."
        }),
        ClientError::Runtime(RuntimeError::Unresponsive) => DomainError::new("runtime_unresponsive", "The Agentlaw runtime process is running but not responding, so this operation was not executed. You must tell the user in their language that Agentlaw memory is unavailable because its runtime is unresponsive, and that ending the Agentlaw runtime process lets the next call start a new one. Do not retry automatically and do not end the process yourself."),
        ClientError::Runtime(RuntimeError::Protocol(reason)) => DomainError::new("runtime_protocol_mismatch", format!("The runtime does not match this installation. Restart the harness. {reason}")),
        ClientError::Rejected(RejectReason::InvalidBody(error)) => error,
        ClientError::Rejected(RejectReason::ConnectionRequired) => DomainError::new("memory_store_connection_required", "No memory store is connected. Connect a memory store before continuing."),
        ClientError::Rejected(RejectReason::StoreUnavailable(reason)) => {
            if let Some((code, message)) = reason.split_once(": ") {
                if code.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    return DomainError::new(code, message);
                }
            }
            DomainError::new("store_unavailable", reason)
        }
        ClientError::Rejected(reason) => DomainError::new("runtime_rejected", format!("Runtime rejected the request: {reason:?}")),
        ClientError::BeforeBody(reason) | ClientError::AfterBody(reason) => DomainError::new("runtime_unavailable", reason),
        ClientError::Runtime(error) => DomainError::new("runtime_unavailable", error.to_string()),
    }
}

impl crate::Backend for RuntimeBackend {
    fn call(&mut self, request: Request) -> Result<Value> {
        self.call_with_control(request, RequestControl::default())
    }
    fn call_with_control(&mut self, request: Request, control: RequestControl) -> Result<Value> {
        let bytes = wire::encode_wire(&request)?;
        let validated = wire::decode_wire(&bytes)?;
        let classified = protocol::classify(validated.clone());
        let is_setup = matches!(classified, Classified::Setup(_));
        let is_query = matches!(classified, Classified::Query(_));
        let has_reference = has_reference(&request);
        let mut binding: Option<BindingRef> = None;
        for attempt in 0..2 {
            let result = (|| {
                let client = self.client()?;
                let target = if is_setup {
                    CallTarget::Setup
                } else {
                    let reference = match binding {
                        Some(reference) => reference,
                        None => {
                            let reference = client.resolve_binding()?;
                            binding = Some(reference);
                            reference
                        }
                    };
                    CallTarget::Store(reference)
                };
                client.call_once(target, validated.clone(), &control)
            })();
            match result {
                Ok(value) => return Ok(value),
                Err(ClientError::BeforeBody(_))
                | Err(ClientError::Runtime(RuntimeError::Unavailable(_)))
                    if attempt == 0 && self.client.is_some() =>
                {
                    self.client = None;
                }
                Err(ClientError::AfterBody(_)) if is_query && attempt == 0 => {
                    self.client = None;
                }
                Err(ClientError::Rejected(reason)) if reason.is_transient() && attempt == 0 => {}
                Err(failure) => {
                    if matches!(
                        failure,
                        ClientError::MutationOutcomeUnknown
                            | ClientError::Runtime(RuntimeError::Unavailable(_))
                            | ClientError::Runtime(RuntimeError::Unresponsive)
                    ) {
                        self.client = None;
                    }
                    return Err(error(failure, has_reference));
                }
            }
        }
        unreachable!()
    }
}
