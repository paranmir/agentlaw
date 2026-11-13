//! Wire frames, request classification and in-flight identifiers.

use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::PathBuf;

use agentlaw_contracts::{
    wire::ValidatedRequest, ConnectIntent, ConnectRequest, HistoryRequest, PendingAction,
    RecallRequest, RememberRequest, Request, SyncRequest,
};

/// One wire attempt. A new UUIDv4 per attempt, distinct from the MCP JSON-RPC id,
/// the source operation_id and the sync request_id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId([u8; 16]);

impl RequestId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().into_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

/// Pins one store selection. It is not a permission to open an arbitrary path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingRef([u8; 32]);

impl BindingRef {
    pub fn new(key: &agentlaw_storage::BindingKey, user_id: &str) -> Self {
        Self(binding_digest(
            key.store_identity(),
            key.local_identity(),
            user_id,
        ))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn binding_digest(store: &str, local: &str, user_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for value in [store, local, user_id] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.finalize().into()
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}

fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N], &'static str> {
    if value.len() != N * 2 {
        return Err("invalid hex length");
    }
    let mut result = [0; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let digit = |byte| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let high = digit(pair[0]).ok_or("invalid lowercase hex")?;
        let low = digit(pair[1]).ok_or("invalid lowercase hex")?;
        result[index] = high << 4 | low;
    }
    Ok(result)
}

macro_rules! hex_serde {
    ($type:ty) => {
        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&encode_hex(&self.0))
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                decode_hex(&value).map(Self).map_err(D::Error::custom)
            }
        }
    };
}

hex_serde!(RequestId);
hex_serde!(BindingRef);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CallTarget {
    Store(BindingRef),
    Setup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RequestKind {
    Query,
    Command,
    Setup,
}

#[derive(Debug, Clone)]
pub enum Classified {
    Query(QueryRequest),
    Command(CommandRequest),
    Setup(SetupRequest),
}

#[derive(Debug, Clone)]
pub enum QueryRequest {
    Recall(RecallRequest),
    History(HistoryRequest),
    PendingInspect(RememberRequest),
    Discover(ConnectRequest),
}

#[derive(Debug, Clone)]
pub enum CommandRequest {
    Remember(RememberRequest),
    Procedure(RememberRequest),
    Connect(ConnectRequest),
    Sync(SyncRequest),
}

#[derive(Debug, Clone)]
pub enum SetupRequest {
    ConnectStore(ConnectRequest),
    Command(SetupCommand),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SetupCommand {
    StoreConnect { path: PathBuf, create: bool },
    ConfigSet { key: String, value: String },
    MachineName { value: String },
}

pub fn encode_setup(command: &SetupCommand) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"setup_command": command}))
        .expect("setup command is serializable")
}

pub fn decode_body(kind: RequestKind, body: &[u8]) -> Result<Classified, RejectReason> {
    let text = std::str::from_utf8(body).map_err(|_| {
        RejectReason::InvalidBody(agentlaw_contracts::DomainError::new(
            "invalid_input",
            "Invalid JSON, duplicate object key, or invalid Unicode.",
        ))
    })?;
    let value =
        agentlaw_contracts::validation::decode_unique(text).map_err(RejectReason::InvalidBody)?;
    let classified = if let Some(object) = value
        .as_object()
        .filter(|object| object.len() == 1 && object.contains_key("setup_command"))
    {
        let command = serde_json::from_value(object["setup_command"].clone()).map_err(|_| {
            RejectReason::InvalidBody(agentlaw_contracts::DomainError::new(
                "invalid_input",
                "Setup command has invalid fields.",
            ))
        })?;
        Classified::Setup(SetupRequest::Command(command))
    } else {
        classify(agentlaw_contracts::wire::decode_wire(body).map_err(RejectReason::InvalidBody)?)
    };
    if classified.kind() != kind {
        return Err(RejectReason::KindMismatch);
    }
    Ok(classified)
}

pub fn request_kind(request: &ValidatedRequest) -> RequestKind {
    match request.request() {
        Request::Recall(_) | Request::History(_) => RequestKind::Query,
        Request::RememberThis(request) => {
            if request.pending_action == Some(PendingAction::Inspect) {
                RequestKind::Query
            } else {
                RequestKind::Command
            }
        }
        Request::ConnectProjectMemory(request) => {
            if request.memory_store_path.is_some() {
                RequestKind::Setup
            } else if matches!(request.intent, None | Some(ConnectIntent::Discover))
                && request.restore_context != Some(true)
            {
                RequestKind::Query
            } else {
                RequestKind::Command
            }
        }
        Request::Sync(_) => RequestKind::Command,
    }
}

pub fn classify(request: ValidatedRequest) -> Classified {
    let kind = request_kind(&request);
    match request.into_request() {
        Request::Recall(request) => Classified::Query(QueryRequest::Recall(request)),
        Request::History(request) => Classified::Query(QueryRequest::History(request)),
        Request::RememberThis(request) => {
            if kind == RequestKind::Query {
                Classified::Query(QueryRequest::PendingInspect(request))
            } else if request.procedure.is_some() {
                Classified::Command(CommandRequest::Procedure(request))
            } else {
                Classified::Command(CommandRequest::Remember(request))
            }
        }
        Request::ConnectProjectMemory(request) => {
            if kind == RequestKind::Setup {
                Classified::Setup(SetupRequest::ConnectStore(request))
            } else if kind == RequestKind::Query {
                Classified::Query(QueryRequest::Discover(request))
            } else {
                Classified::Command(CommandRequest::Connect(request))
            }
        }
        Request::Sync(request) => Classified::Command(CommandRequest::Sync(request)),
    }
}

impl Classified {
    pub fn kind(&self) -> RequestKind {
        match self {
            Self::Query(_) => RequestKind::Query,
            Self::Command(_) => RequestKind::Command,
            Self::Setup(_) => RequestKind::Setup,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHeader {
    pub id: RequestId,
    pub target: CallTarget,
    pub kind: RequestKind,
    pub body_len: u32,
}

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_WIRE_BODY_BYTES: u32 = 16 * 1024 * 1024 + 64 * 1024;
pub const MAX_CONTROL_FRAME_BYTES: u32 = 64 * 1024;
pub const MAX_CONTROL_REPLY_BYTES: u32 = 1024 * 1024;
pub const MAX_RESULT_FRAME_BYTES: u32 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub installation_build: String,
    pub secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HelloReply {
    Accepted,
    ProtocolMismatch,
    BuildMismatch,
    NotAuthorized,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingError {
    ConnectionRequired,
    ConfigInvalid(String),
    BindingChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    Busy,
    Stopping,
    BodyTooLarge,
    KindMismatch,
    InvalidBody(agentlaw_contracts::DomainError),
    BindingChanged,
    ConnectionRequired,
    ConfigInvalid(String),
    StoreUnavailable(String),
}

impl RejectReason {
    /// Only these may be retried automatically against the same target.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Busy | Self::Stopping => true,
            Self::BodyTooLarge
            | Self::KindMismatch
            | Self::InvalidBody(_)
            | Self::BindingChanged
            | Self::ConnectionRequired
            | Self::ConfigInvalid(_)
            | Self::StoreUnavailable(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlRequest {
    Health,
    Cancel(RequestId),
    Lease { client: String },
    ResolveBinding,
    Shutdown { reason: String },
    ConfigGet { key: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlReply {
    Health(crate::ports::HealthSnapshot),
    CancellationRequested,
    NotActive,
    LeaseRenewed,
    Binding(BindingRef),
    BindingError(BindingError),
    ShutdownAccepted,
    Config(serde_json::Value),
    ConfigError(agentlaw_contracts::DomainError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DataFrame {
    Proceed {
        id: RequestId,
    },
    Rejected {
        id: RequestId,
        reason: RejectReason,
    },
    RejectedUnread {
        reason: RejectReason,
    },
    Progress {
        id: RequestId,
        phase: String,
    },
    Completed {
        id: RequestId,
        result: serde_json::Value,
    },
    Failed {
        id: RequestId,
        error: agentlaw_contracts::DomainError,
    },
}

pub fn write_frame(out: &mut impl Write, bytes: &[u8], max: u32) -> io::Result<()> {
    if bytes.len() > max as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame exceeds limit",
        ));
    }
    out.write_all(&(bytes.len() as u32).to_be_bytes())?;
    out.write_all(bytes)?;
    out.flush()
}

pub fn read_frame(input: &mut impl Read, max: u32) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header);
    if length > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds limit",
        ));
    }
    let mut buffer = Vec::new();
    input.take(u64::from(length)).read_to_end(&mut buffer)?;
    if buffer.len() != length as usize {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete frame",
        ));
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests;
