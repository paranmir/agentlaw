use super::*;
use serde_json::{json, Value};
use std::io::Cursor;

#[test]
fn share_and_sync_admin_commands_round_trip_with_documented_wire_keys() {
    let commands = [
        (
            AdminCommand::ShareInspect {
                remote: "origin".into(),
                target_ref: "refs/heads/main".into(),
            },
            json!({"share_inspect":{"remote":"origin","target_ref":"refs/heads/main"}}),
        ),
        (
            AdminCommand::SharePush {
                review: "review".into(),
                allow_sensitive: false,
                user_confirmed: true,
            },
            json!({"share_push":{"review":"review","allow_sensitive":false,"user_confirmed":true}}),
        ),
        (
            AdminCommand::ShareFetch {
                remote: "origin".into(),
            },
            json!({"share_fetch":{"remote":"origin"}}),
        ),
        (
            AdminCommand::ImportPrepare {
                commit: "commit".into(),
            },
            json!({"import_prepare":{"commit":"commit"}}),
        ),
        (AdminCommand::ImportList, json!({"import_list":{}})),
        (
            AdminCommand::ImportInspect {
                import_ref: "ref".into(),
            },
            json!({"import_inspect":{"import_ref":"ref"}}),
        ),
        (
            AdminCommand::ImportCall {
                import_ref: "ref".into(),
                request: json!({"action":"recall","recall":{"recall_for":"fixture"}}),
            },
            json!({"import_call":{"import_ref":"ref","request":{"action":"recall","recall":{"recall_for":"fixture"}}}}),
        ),
        (
            AdminCommand::ImportResolve {
                import_ref: "ref".into(),
                choices: None,
                user_confirmed: true,
            },
            json!({"import_resolve":{"import_ref":"ref","choices":null,"user_confirmed":true}}),
        ),
        (
            AdminCommand::ImportPublish {
                import_ref: "ref".into(),
                resolution: "token".into(),
                user_confirmed: true,
            },
            json!({"import_publish":{"import_ref":"ref","resolution":"token","user_confirmed":true}}),
        ),
        (
            AdminCommand::SyncAcceptFindings {
                operation: "op".into(),
                candidate: "candidate".into(),
                findings_digest: "digest".into(),
                confirm_sharing: true,
            },
            json!({"sync_accept_findings":{"operation":"op","candidate":"candidate","findings_digest":"digest","confirm_sharing":true}}),
        ),
        (
            AdminCommand::SyncPolicyPropose {
                remote: "origin".into(),
                target_ref: "refs/heads/main".into(),
            },
            json!({"sync_policy_propose":{"remote":"origin","target_ref":"refs/heads/main"}}),
        ),
        (
            AdminCommand::SyncPolicyConfigure {
                record: json!({"enabled":true}),
                confirm_delegation: true,
            },
            json!({"sync_policy_configure":{"record":{"enabled":true},"confirm_delegation":true}}),
        ),
    ];
    for (command, expected) in commands {
        let body = encode_admin(&command).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"admin_command":expected})
        );
        assert!(matches!(
            decode_body(RequestKind::Admin, &body),
            Ok(Classified::Admin(_))
        ));
    }
}

#[test]
fn import_list_requires_an_empty_object_and_rejects_duplicate_or_unknown_fields() {
    assert!(matches!(
        decode_body(
            RequestKind::Admin,
            br#"{"admin_command":{"import_list":{}}}"#
        ),
        Ok(Classified::Admin(AdminCommand::ImportList))
    ));
    for body in [
        &br#"{"admin_command":{"import_list":null}}"#[..],
        &br#"{"admin_command":{"import_list":{"extra":true}}}"#[..],
        &br#"{"admin_command":{"import_list":{},"import_list":{}}}"#[..],
        &br#"{"admin_command":{"share_fetch":{"remote":"a","remote":"b"}}}"#[..],
        &br#"{"admin_command":{"share_fetch":{"remote":"a","extra":true}}}"#[..],
    ] {
        assert!(matches!(
            decode_body(RequestKind::Admin, body),
            Err(RejectReason::InvalidBody(_))
        ));
    }
}

#[test]
fn import_call_rejects_store_switch_and_sync_requests() {
    // Existing codes: a store switch is not an import-review request, and sync
    // needs the installation's sync adapter rather than a review workspace.
    for (request, code) in [
        (
            json!({"action":"connect_project_memory","connect_project_memory":{"project_path":"C:/work/project","memory_store_path":"C:/other","intent":"discover"}}),
            "invalid_import_request",
        ),
        (
            json!({"action":"sync","sync":{"command":"status","operation_id":"6f1d2c3b-9a8e-4f70-8d61-2b5c4e7a9f10"}}),
            "sync_requires_app_adapter",
        ),
    ] {
        let body = serde_json::to_vec(
            &json!({"admin_command":{"import_call":{"import_ref":"ref","request":request}}}),
        )
        .unwrap();
        assert!(
            matches!(decode_body(RequestKind::Admin, &body), Err(RejectReason::InvalidBody(ref error)) if error.code == code),
            "{code}"
        );
    }
}

#[test]
fn admin_body_rejects_wrong_kind_and_invalid_fields() {
    let valid = br#"{"admin_command":{"repair":{}}}"#;
    assert!(matches!(
        decode_body(RequestKind::Admin, valid),
        Ok(Classified::Admin(_))
    ));
    assert!(matches!(
        decode_body(RequestKind::Query, valid),
        Err(RejectReason::KindMismatch)
    ));
    let query = br#"{"action":"recall","recall":{"memory_ids":["id"]}}"#;
    assert!(matches!(
        decode_body(RequestKind::Admin, query),
        Err(RejectReason::KindMismatch)
    ));
    for body in [
        &br#"{"admin_command":{"repair":{"extra":1}}}"#[..],
        &br#"{"admin_command":{"repair":{},"repair":{}}}"#[..],
    ] {
        assert!(matches!(
            decode_body(RequestKind::Admin, body),
            Err(RejectReason::InvalidBody(_))
        ));
    }
}

#[test]
fn setup_body_rejects_incorrect_kind_and_invalid_fields() {
    let valid = br#"{"setup_command":{"config_set":{"key":"x","value":"y"}}}"#;
    for kind in [RequestKind::Query, RequestKind::Command] {
        assert!(matches!(
            decode_body(kind, valid),
            Err(RejectReason::KindMismatch)
        ));
    }
    for body in [
        &br#"{"setup_command":{"config_set":{"key":"x","value":"y","extra":1}}}"#[..],
        &br#"{"setup_command":{"config_set":{"key":"x","key":"z","value":"y"}}}"#[..],
    ] {
        assert!(matches!(
            decode_body(RequestKind::Setup, body),
            Err(RejectReason::InvalidBody(_))
        ));
    }
}

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
        next_action: None,
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
    let frame = serde_json::to_value(DataFrame::Failed {
        id,
        error: error.clone(),
    })
    .unwrap();
    assert!(frame["Failed"]["error"].is_object(), "FR-06.4: {frame}");
    assert!(
        frame["Failed"]["error"].get("next_action").is_none(),
        "FR-06.4: {frame}"
    );
    let DataFrame::Failed { error: old, .. } = serde_json::from_value(frame).unwrap() else {
        panic!("Expected a legacy Failed frame");
    };
    assert_eq!(
        old.next_action, None,
        "FR-06.4: absent guidance must deserialize"
    );
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
        "../../../../docs/design/contracts/agentlaw-input.examples.json"
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
        "../../../../docs/design/contracts/agentlaw-input.examples.json"
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
