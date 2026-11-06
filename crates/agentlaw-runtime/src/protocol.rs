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
    StoreConnect { path: PathBuf, create: bool },
    ConfigSet { key: String, value: String },
}

pub fn classify(request: ValidatedRequest) -> Classified {
    match request.into_request() {
        Request::Recall(request) => Classified::Query(QueryRequest::Recall(request)),
        Request::History(request) => Classified::Query(QueryRequest::History(request)),
        Request::RememberThis(request) => {
            if request.pending_action == Some(PendingAction::Inspect) {
                Classified::Query(QueryRequest::PendingInspect(request))
            } else if request.procedure.is_some() {
                Classified::Command(CommandRequest::Procedure(request))
            } else {
                Classified::Command(CommandRequest::Remember(request))
            }
        }
        Request::ConnectProjectMemory(request) => {
            if request.memory_store_path.is_some() {
                Classified::Setup(SetupRequest::ConnectStore(request))
            } else if matches!(request.intent, None | Some(ConnectIntent::Discover))
                && request.restore_context != Some(true)
            {
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
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::Cursor;

    struct CountingReader<R> {
        inner: R,
        consumed: usize,
    }

    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let count = self.inner.read(buf)?;
            self.consumed += count;
            Ok(count)
        }
    }

    fn domain_error() -> agentlaw_contracts::DomainError {
        agentlaw_contracts::DomainError {
            code: "invalid".into(),
            message: "bad body".into(),
            retryable: false,
            details: Some(json!({"field": "value"})),
        }
    }

    fn round_trip<T>(value: T)
    where
        T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug + PartialEq,
    {
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(serde_json::from_slice::<T>(&bytes).unwrap(), value);
    }

    #[test]
    fn frames_round_trip_and_enforce_limits() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, b"hello", 5).unwrap();
        assert_eq!(&bytes[..4], &[0, 0, 0, 5]);
        write_frame(&mut bytes, b"world", 5).unwrap();
        write_frame(&mut bytes, b"", 0).unwrap();
        let mut input = Cursor::new(bytes);
        assert_eq!(read_frame(&mut input, 5).unwrap(), b"hello");
        assert_eq!(read_frame(&mut input, 5).unwrap(), b"world");
        assert_eq!(read_frame(&mut input, 0).unwrap(), b"");

        let mut out = Vec::new();
        assert_eq!(
            write_frame(&mut out, b"12345", 4).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(out.is_empty());
        write_frame(&mut out, b"1234", 4).unwrap();
        let mut input = CountingReader {
            inner: Cursor::new([0, 0, 0, 5, 1, 2, 3]),
            consumed: 0,
        };
        assert_eq!(
            read_frame(&mut input, 4).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(input.consumed, 4);
    }

    #[test]
    fn truncated_frames_fail() {
        let mut input = Cursor::new([0x10, 0, 0, 0, 1, 2, 3]);
        assert_eq!(
            read_frame(&mut input, MAX_RESULT_FRAME_BYTES)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            read_frame(&mut Cursor::new([0, 0]), 10).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn wire_messages_round_trip() {
        let id = RequestId::new();
        let error = domain_error();
        for value in [
            HelloReply::Accepted,
            HelloReply::ProtocolMismatch,
            HelloReply::BuildMismatch,
            HelloReply::NotAuthorized,
            HelloReply::Stopping,
        ] {
            round_trip(value);
        }
        let reasons = [
            RejectReason::Busy,
            RejectReason::Stopping,
            RejectReason::BodyTooLarge,
            RejectReason::KindMismatch,
            RejectReason::InvalidBody(error.clone()),
            RejectReason::BindingChanged,
            RejectReason::ConnectionRequired,
            RejectReason::ConfigInvalid("config".into()),
            RejectReason::StoreUnavailable("store".into()),
        ];
        for (i, reason) in reasons.iter().enumerate() {
            assert_eq!(reason.is_transient(), i < 2);
            round_trip(reason.clone());
        }
        for value in [
            ControlRequest::Health,
            ControlRequest::Cancel(id),
            ControlRequest::Lease {
                client: "client".into(),
            },
            ControlRequest::ResolveBinding,
            ControlRequest::Shutdown {
                reason: "reason".into(),
            },
        ] {
            round_trip(value);
        }
        let health = crate::ports::HealthSnapshot {
            components: vec![],
            activity: Default::default(),
        };
        for value in [
            ControlReply::Health(health),
            ControlReply::CancellationRequested,
            ControlReply::NotActive,
            ControlReply::LeaseRenewed,
            ControlReply::Binding(BindingRef([1; 32])),
            ControlReply::BindingError(BindingError::ConnectionRequired),
            ControlReply::BindingError(BindingError::ConfigInvalid("bad".into())),
            ControlReply::BindingError(BindingError::BindingChanged),
            ControlReply::ShutdownAccepted,
        ] {
            round_trip(value);
        }
        for value in [
            DataFrame::Proceed { id },
            DataFrame::Rejected {
                id,
                reason: RejectReason::Busy,
            },
            DataFrame::RejectedUnread {
                reason: RejectReason::Busy,
            },
            DataFrame::Progress {
                id,
                phase: "phase".into(),
            },
            DataFrame::Completed {
                id,
                result: json!({"nested": ["안녕", false]}),
            },
            DataFrame::Failed { id, error },
        ] {
            round_trip(value);
        }
        round_trip(Hello {
            protocol: PROTOCOL_VERSION,
            installation_build: "build".into(),
            secret: "secret".into(),
        });
    }

    #[test]
    fn busy_rejection_fits_small_frame() {
        let payload = serde_json::to_vec(&DataFrame::RejectedUnread {
            reason: RejectReason::Busy,
        })
        .unwrap();
        let mut frame = Vec::new();
        write_frame(&mut frame, &payload, MAX_CONTROL_FRAME_BYTES).unwrap();
        assert!(frame.len() <= 64);
    }

    #[test]
    fn large_health_reply_fits_limit() {
        use crate::ports::{
            ComponentId, ComponentKind, ComponentState, ComponentStatus, HealthSnapshot,
        };
        let kinds = [
            ComponentKind::Host,
            ComponentKind::Setup,
            ComponentKind::Store,
            ComponentKind::Control,
            ComponentKind::Exact,
            ComponentKind::Semantic,
            ComponentKind::Model,
            ComponentKind::Git,
        ];
        let mut components = Vec::new();
        for binding in 0..64 {
            for kind in kinds {
                components.push((
                    ComponentId {
                        binding: Some(format!("binding-{binding}")),
                        kind,
                    },
                    ComponentStatus {
                        state: ComponentState::Degraded,
                        observed_at: std::time::SystemTime::UNIX_EPOCH,
                        reason: Some("x".repeat(256)),
                    },
                ));
            }
        }
        let reply = ControlReply::Health(HealthSnapshot {
            components,
            activity: Default::default(),
        });
        assert!(serde_json::to_vec(&reply).unwrap().len() <= MAX_CONTROL_REPLY_BYTES as usize);
        round_trip(reply);
    }

    fn public_input(mut input: Value) -> Value {
        let object = input.as_object_mut().unwrap();
        let action = object.remove("action").unwrap();
        let name = action.as_str().unwrap().to_owned();
        json!({"action": action, name: input})
    }

    #[test]
    fn classify_valid_requests() {
        let examples: Value = serde_json::from_str(include_str!(
            "../../../docs/design/contracts/agentlaw-input.examples.json"
        ))
        .unwrap();
        let example = |id: &str| {
            examples["examples"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == id)
                .unwrap()["input"]
                .clone()
        };
        let cases = [
            (example("recall-query"), "recall", RequestKind::Query),
            (example("history-recent"), "history", RequestKind::Query),
            (example("pending-inspect"), "inspect", RequestKind::Query),
            (
                example("pending-judgment"),
                "remember",
                RequestKind::Command,
            ),
            (example("pending-discard"), "remember", RequestKind::Command),
            (example("remember-create"), "remember", RequestKind::Command),
            (
                example("procedure-authoring"),
                "procedure",
                RequestKind::Command,
            ),
            (example("connect-discover"), "discover", RequestKind::Query),
            (
                json!({"action":"connect_project_memory", "project_path":"C:/work/app", "intent":"discover"}),
                "discover",
                RequestKind::Query,
            ),
            (
                json!({"action":"connect_project_memory", "project_path":"C:/work/app", "restore_context":true, "recall_for":"project context"}),
                "connect",
                RequestKind::Command,
            ),
            (
                json!({"action":"connect_project_memory", "project_path":"C:/work/app", "intent":"discover", "restore_context":true, "recall_for":"project context"}),
                "connect",
                RequestKind::Command,
            ),
            (example("connect-existing"), "connect", RequestKind::Command),
            (example("connect-create"), "connect", RequestKind::Command),
            (
                json!({"action":"connect_project_memory", "project_path":"C:/work/app", "memory_store_path":"C:/work/store"}),
                "store",
                RequestKind::Setup,
            ),
            (example("sync-status"), "sync", RequestKind::Command),
            (
                example("sync-start-registered-policy"),
                "sync",
                RequestKind::Command,
            ),
        ];
        for (input, variant, kind) in cases {
            let request = ValidatedRequest::parse_public(&public_input(input).to_string()).unwrap();
            let classified = classify(request);
            assert_eq!(classified.kind(), kind, "{variant}");
            assert!(
                matches!(
                    (&classified, variant),
                    (Classified::Query(QueryRequest::Recall(_)), "recall")
                        | (Classified::Query(QueryRequest::History(_)), "history")
                        | (
                            Classified::Query(QueryRequest::PendingInspect(_)),
                            "inspect"
                        )
                        | (Classified::Query(QueryRequest::Discover(_)), "discover")
                        | (Classified::Command(CommandRequest::Remember(_)), "remember")
                        | (
                            Classified::Command(CommandRequest::Procedure(_)),
                            "procedure"
                        )
                        | (Classified::Command(CommandRequest::Connect(_)), "connect")
                        | (Classified::Command(CommandRequest::Sync(_)), "sync")
                        | (Classified::Setup(SetupRequest::ConnectStore(_)), "store")
                ),
                "unexpected variant for {variant}: {classified:?}"
            );
        }
    }

    #[test]
    fn valid_examples_only_allow_safe_queries() {
        let examples: Value = serde_json::from_str(include_str!(
            "../../../docs/design/contracts/agentlaw-input.examples.json"
        ))
        .unwrap();
        for example in examples["examples"].as_array().unwrap() {
            if example["valid"] != true {
                continue;
            }
            let id = example["id"].as_str().unwrap();
            let input = example["input"].clone();
            let action = input["action"].as_str().unwrap();
            let query_allowed = action == "recall"
                || action == "history"
                || (action == "remember_this" && input["pending_action"] == "inspect")
                || (action == "connect_project_memory"
                    && input.get("memory_store_path").is_none()
                    && input["restore_context"] != true
                    && (input.get("intent").is_none() || input["intent"] == "discover"));
            let request = ValidatedRequest::parse_public(&public_input(input).to_string())
                .unwrap_or_else(|error| panic!("{id}: {error:?}"));
            assert_eq!(
                classify(request).kind() == RequestKind::Query,
                query_allowed,
                "{id}"
            );
        }
    }

    #[test]
    fn binding_digest_known_answers() {
        for (store, local, user, expected) in [
            (
                "c:\\store",
                "c:\\local",
                "personal",
                "4cb1afaac08dcac1bbd7daa8e7f2e4b77718565ad276dfbe4cffdcdfdd2385eb",
            ),
            (
                "a",
                "bc",
                "u",
                "4c0f0ecb2bd9a8a15d26fcea1af67ee2e674661930892806419cc25be2dfe792",
            ),
            (
                "ab",
                "c",
                "u",
                "cbc366cbcacfb65a139fbd452f3a5e2cd0d1484feccc259cf91dc6ca9a8be3c0",
            ),
        ] {
            assert_eq!(encode_hex(&binding_digest(store, local, user)), expected);
        }
    }

    #[test]
    fn binding_ref_tracks_key_and_user() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("local");
        let other = temp.path().join("other");
        std::fs::create_dir(&local).unwrap();
        std::fs::create_dir(&other).unwrap();
        let key = agentlaw_storage::binding_identity(temp.path(), &local).unwrap();
        let other_key = agentlaw_storage::binding_identity(temp.path(), &other).unwrap();
        let reference = BindingRef::new(&key, "personal");
        assert_eq!(reference, BindingRef::new(&key, "personal"));
        assert_ne!(reference, BindingRef::new(&key, "other"));
        assert_ne!(reference, BindingRef::new(&other_key, "personal"));
    }

    #[test]
    fn header_json_round_trip() {
        for target in [CallTarget::Store(BindingRef([42; 32])), CallTarget::Setup] {
            let header = CallHeader {
                id: RequestId::new(),
                target,
                kind: RequestKind::Query,
                body_len: 123,
            };
            let json = serde_json::to_value(header).unwrap();
            let id = json["id"].as_str().unwrap();
            assert_eq!(id.len(), 32);
            assert!(id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
            assert_eq!(serde_json::from_value::<CallHeader>(json).unwrap(), header);
        }
    }

    #[test]
    fn malformed_identifiers_are_rejected() {
        for len in [32, 64] {
            for value in [
                "a".repeat(len - 1),
                format!("A{}", "a".repeat(len - 1)),
                format!("g{}", "a".repeat(len - 1)),
            ] {
                let json = format!("\"{value}\"");
                if len == 32 {
                    assert!(serde_json::from_str::<RequestId>(&json).is_err());
                } else {
                    assert!(serde_json::from_str::<BindingRef>(&json).is_err());
                }
            }
        }
    }

    #[test]
    fn request_ids_are_uuid_v4() {
        let first = RequestId::new();
        let second = RequestId::new();
        assert_ne!(first, second);
        assert_eq!(first.as_bytes()[6] & 0xf0, 0x40);
        assert_eq!(first.as_bytes()[8] & 0xc0, 0x80);
    }
}
