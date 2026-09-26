use agentlaw_worker::{EmbeddingError, ProcessRuntime, RuntimeConfig, SemanticAvailability};
#[path = "../../../tests/support/owned_process.rs"]
mod owned_process;
use owned_process::OwnedProcess as StopChild;
use std::{
    process::{Command, Stdio},
    time::Duration,
};
#[test]
fn real_process_reuses_daemon_and_missing_assets_are_explicit() {
    let dir = std::env::temp_dir().join(format!("agentlaw-ipc-{}", uuid::Uuid::new_v4()));
    let exe = std::path::PathBuf::from(env!("CARGO_BIN_EXE_agentlaw-worker"));
    let mut command = Command::new(&exe);
    command
        .arg("worker-daemon")
        .arg("--state-dir")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let _child = StopChild::new(command.spawn().unwrap());
    for _ in 0..100 {
        if dir.join("endpoint.json").is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let config = RuntimeConfig {
        state_dir: dir.clone(),
        executable: exe,
        model: None,
    };
    let first = ProcessRuntime::attach(&config).unwrap();
    let second = ProcessRuntime::attach(&config).unwrap();
    for _ in 0..100 {
        if second.availability().unwrap() != SemanticAvailability::Loading {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        second.embed("not a synthetic embedding"),
        Err(EmbeddingError::Unavailable(_))
    ));
    drop(first);
    assert_eq!(second.availability().unwrap(), SemanticAvailability::Failed);
    use agentlaw_worker::derived::*;
    let context = DerivedContext {
        repository_id: "repo".into(),
        model_digest: "model".into(),
        config_digest: "config".into(),
        initial_basis: SourcePosition {
            epoch: "epoch".into(),
            sequence: 0,
        },
    };
    let page = PublishedPage {
        basis: context.initial_basis.clone(),
        covered_through: SourcePosition {
            epoch: "epoch".into(),
            sequence: 1,
        },
        source_position: SourcePosition {
            epoch: "epoch".into(),
            sequence: 1,
        },
        coverage_complete: true,
        batches: vec![PublishedBatch {
            sequence: 1,
            documents: vec![DerivedDocument {
                memory_id: "memory".into(),
                change_id: "revision".into(),
                scope: "user".into(),
                body: Some("durable lexical publication".into()),
                section: "body".into(),
            }],
        }],
    };
    let mut cloned_context = context.clone();
    cloned_context.repository_id = "fresh-clone".into();
    assert_eq!(
        second.bootstrap_status(&cloned_context).unwrap(),
        (0, false)
    );
    let staged = second
        .stage_source_body(&mut std::io::Cursor::new(
            b"existing cloned memory inventory",
        ))
        .unwrap();
    let initial = agentlaw_worker::spool::PublishedSpoolPage {
        page: PublishedPage {
            basis: cloned_context.initial_basis.clone(),
            covered_through: cloned_context.initial_basis.clone(),
            source_position: cloned_context.initial_basis.clone(),
            coverage_complete: true,
            batches: vec![PublishedBatch {
                sequence: 0,
                documents: vec![DerivedDocument {
                    memory_id: "existing-clone-id".into(),
                    change_id: "immutable-version".into(),
                    scope: "user".into(),
                    section: "body".into(),
                    body: Some(String::new()),
                }],
            }],
        },
        bodies: vec![agentlaw_worker::spool::SpoolBodyRef {
            batch_index: 0,
            document_index: 0,
            body: staged,
        }],
    };
    second
        .bootstrap_spooled(&cloned_context, 0, true, &initial)
        .unwrap();
    assert_eq!(second.bootstrap_status(&cloned_context).unwrap(), (1, true));
    // Receipt replay succeeds after the accepted ingress file has already been reclaimed.
    second
        .bootstrap_spooled(&cloned_context, 0, true, &initial)
        .unwrap();
    let cloned = second
        .search_index(
            &cloned_context,
            "cloned",
            &["user".into()],
            10,
            None,
            cloned_context.initial_basis.clone(),
        )
        .unwrap();
    assert_eq!(cloned.lexical[0].memory_id, "existing-clone-id");
    assert_eq!(cloned.lexical_stamp.ack, 0);
    assert!(!cloned.semantic_complete);
    second.ingest_published(&context, &page).unwrap();
    let packet = second
        .search_index(
            &context,
            "durable",
            &["user".into()],
            10,
            None,
            page.covered_through.clone(),
        )
        .unwrap();
    assert_eq!(packet.lexical[0].memory_id, "memory");
    assert_eq!(packet.lexical_stamp.ack, 1);
    assert!(!packet.semantic_complete);
    assert!(second
        .search_index(
            &context,
            "durable",
            &["project:other".into()],
            10,
            None,
            page.covered_through.clone()
        )
        .unwrap()
        .lexical
        .is_empty());
    // Replacing the child while keeping the same Client exercises stale endpoint recovery.
    drop(_child);
    let _replacement = StopChild::new(command.spawn().unwrap());
    for _ in 0..100 {
        if second.availability().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(second.availability().unwrap(), SemanticAvailability::Failed);
    assert_eq!(
        second
            .search_index(
                &context,
                "durable",
                &["user".into()],
                10,
                None,
                page.covered_through
            )
            .unwrap()
            .lexical
            .len(),
        1
    );
    use std::io::Read;
    let mut large = std::io::repeat(b'x')
        .take(6 * 1024 * 1024)
        .chain(std::io::Cursor::new(b" needle"));
    let body = second.stage_source_body(&mut large).unwrap();
    let required = SourcePosition {
        epoch: "epoch".into(),
        sequence: 2,
    };
    let spool_page = agentlaw_worker::spool::PublishedSpoolPage {
        page: PublishedPage {
            basis: SourcePosition {
                epoch: "epoch".into(),
                sequence: 1,
            },
            covered_through: required.clone(),
            source_position: required.clone(),
            coverage_complete: true,
            batches: vec![PublishedBatch {
                sequence: 2,
                documents: vec![DerivedDocument {
                    memory_id: "memory".into(),
                    change_id: "v2".into(),
                    scope: "user".into(),
                    body: Some(String::new()),
                    section: "body".into(),
                }],
            }],
        },
        bodies: vec![agentlaw_worker::spool::SpoolBodyRef {
            batch_index: 0,
            document_index: 0,
            body,
        }],
    };
    second.ingest_spooled(&context, &spool_page).unwrap();
    assert!(agentlaw_worker::spool::open_body(&dir, &spool_page.bodies[0].body).is_err());
    assert_eq!(
        second.ingest_spooled(&context, &spool_page).unwrap(),
        required
    );
    assert_eq!(
        second
            .search_index(
                &context,
                "needle",
                &["user".into()],
                10,
                None,
                required.clone()
            )
            .unwrap()
            .lexical[0]
            .change_id,
        "v2"
    );
    let index = agentlaw_worker::indexing::directory(&dir, &context)
        .unwrap()
        .join("lexical.sqlite");
    std::fs::write(&index, b"corrupt derived index fixture").unwrap();
    assert!(second
        .search_index(
            &context,
            "needle",
            &["user".into()],
            10,
            None,
            required.clone()
        )
        .is_err());
    let repair = second.repair_index(&context).unwrap();
    assert_eq!(repair.lexical, required);
    assert_eq!(repair.vector.sequence, 0);
    assert_eq!(
        second
            .search_index(&context, "needle", &["user".into()], 10, None, required)
            .unwrap()
            .lexical
            .len(),
        1
    );
    drop(second);
    drop(_replacement);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires explicitly provisioned local Granite and ONNX artifacts"]
fn real_onnx_model_over_daemon_ipc() {
    let model = agentlaw_worker::ModelAssets {
        onnx_model: std::env::var_os("AGENTLAW_SMOKE_MODEL")
            .expect("model path")
            .into(),
        tokenizer_json: std::env::var_os("AGENTLAW_SMOKE_TOKENIZER")
            .expect("tokenizer path")
            .into(),
        runtime_library: std::env::var_os("AGENTLAW_SMOKE_ORT")
            .expect("ORT library path")
            .into(),
    };
    let dir = std::env::temp_dir().join(format!("agentlaw-real-ipc-{}", uuid::Uuid::new_v4()));
    let exe = std::path::PathBuf::from(env!("CARGO_BIN_EXE_agentlaw-worker"));
    let mut command = Command::new(&exe);
    command
        .args(["worker-daemon", "--state-dir"])
        .arg(&dir)
        .arg("--model")
        .arg(&model.onnx_model)
        .arg("--tokenizer")
        .arg(&model.tokenizer_json)
        .arg("--ort-library")
        .arg(&model.runtime_library)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let child = StopChild::new(command.spawn().unwrap());
    for _ in 0..200 {
        if dir.join("endpoint.json").is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let config = RuntimeConfig {
        state_dir: dir.clone(),
        executable: exe,
        model: Some(model),
    };
    let client = ProcessRuntime::attach(&config).unwrap();
    for _ in 0..1200 {
        match client.availability().unwrap() {
            SemanticAvailability::Ready => break,
            SemanticAvailability::Loading => std::thread::sleep(Duration::from_millis(50)),
            other => panic!("unexpected actual model state {other:?}"),
        }
    }
    assert_eq!(client.availability().unwrap(), SemanticAvailability::Ready);
    assert!(client
        .model_digest()
        .unwrap()
        .starts_with("granite-r2-256:"));
    let first = client.embed("원자적으로 기억을 저장한다").unwrap();
    let second = client.embed("원자적으로 기억을 저장한다").unwrap();
    assert_eq!(first.len(), 256);
    assert_eq!(first, second);
    assert!(first.iter().all(|v| v.is_finite()));
    let norm: f64 = first.iter().map(|v| (*v as f64).powi(2)).sum();
    assert!((norm - 1.0).abs() < 1e-5);
    use agentlaw_worker::derived::*;
    let context = DerivedContext {
        repository_id: "real-model-source".into(),
        model_digest: client.model_digest().unwrap(),
        config_digest: "section-v1".into(),
        initial_basis: SourcePosition {
            epoch: "actual".into(),
            sequence: 0,
        },
    };
    let required = SourcePosition {
        epoch: "actual".into(),
        sequence: 1,
    };
    client
        .ingest_published(
            &context,
            &PublishedPage {
                basis: context.initial_basis.clone(),
                covered_through: required.clone(),
                source_position: required.clone(),
                coverage_complete: true,
                batches: vec![PublishedBatch {
                    sequence: 1,
                    documents: vec![DerivedDocument {
                        memory_id: "real".into(),
                        change_id: "v1".into(),
                        scope: "user".into(),
                        section: "body".into(),
                        body: Some("원자적으로 기억을 저장한다".into()),
                    }],
                }],
            },
        )
        .unwrap();
    let packet = client
        .search_index(
            &context,
            "기억",
            &["user".into()],
            10,
            Some(&first),
            required,
        )
        .unwrap();
    assert!(packet.semantic_complete);
    assert_eq!(packet.vector[0].memory_id, "real");
    assert!(packet.vector[0].score > 0.99999);
    assert_eq!(packet.vector_stamp.unwrap().ack, 1);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let start = std::time::Instant::now();
        let result = client.search_index_cancellable(
            &context,
            "missing",
            &["user".into()],
            10,
            Some(&first),
            SourcePosition {
                epoch: "actual".into(),
                sequence: 2,
            },
            &cancel,
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    });
    drop(client);
    drop(child);
    std::fs::remove_dir_all(dir).unwrap();
}
