use super::*;

#[test]
fn closing_discards_outstanding_work_and_allows_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("close", vec![])],
        )
        .unwrap();
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let indexer =
        SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    let stale = wait_work(&indexer);
    indexer.close_binding("binding");
    assert!(indexer.progress("binding").is_none());
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.finish(stale.id, Ok(IndexResult::Vector(vec![1.0, 0.0])));
    assert_eq!(drive(&indexer, receipt.generation), 1);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}

#[cfg(windows)]
#[test]
fn normal_close_and_shutdown_checkpoint_and_release_file_handles() {
    use std::os::windows::fs::OpenOptionsExt;
    for close_only in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
        let receipt = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![mutation("checkpoint", vec![])],
            )
            .unwrap();
        let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
        config.dimension = 2;
        let indexer =
            SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
        indexer
            .open_binding("binding", store.owned_published_reader())
            .unwrap();
        indexer.model_ready("model");
        drive(&indexer, receipt.generation);
        let path = {
            let state = indexer.inner.state.lock().unwrap();
            state.schedule.lanes[&LaneKey::current("binding")]
                .active
                .as_ref()
                .unwrap()
                .path
                .clone()
        };
        let mut wal_name = path.as_os_str().to_owned();
        wal_name.push("-wal");
        let wal = std::path::PathBuf::from(wal_name);
        let db_before = std::fs::read(&path).unwrap();
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        if close_only {
            indexer.close_binding("binding");
        } else {
            assert!(indexer.shutdown(Duration::from_secs(5)));
        }
        let exclusive = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        drop(exclusive);
        assert_ne!(std::fs::read(&path).unwrap(), db_before);
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
        assert!(indexer.shutdown(Duration::from_secs(5)));
    }
}

#[cfg(windows)]
#[test]
fn close_and_shutdown_wait_for_inflight_read_view() {
    use std::os::windows::fs::OpenOptionsExt;
    for close_only in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
        let receipt = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![mutation("read", vec![])],
            )
            .unwrap();
        let target = store
            .owned_published_reader()
            .read_snapshot()
            .unwrap()
            .position()
            .clone();
        let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
        config.dimension = 2;
        let indexer =
            SemanticIndexer::start(config, ModelRequestIds::new(), HealthSink::default()).unwrap();
        indexer
            .open_binding("binding", store.owned_published_reader())
            .unwrap();
        indexer.model_ready("model");
        drive(&indexer, receipt.generation);
        let path = {
            let state = indexer.inner.state.lock().unwrap();
            state.schedule.lanes[&LaneKey::current("binding")]
                .active
                .as_ref()
                .unwrap()
                .path
                .clone()
        };
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let hook_barrier = barrier.clone();
        *indexer.inner.readview_hook.lock().unwrap() = Some(Arc::new(move || {
            hook_barrier.wait();
            hook_barrier.wait();
        }));
        let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
        let query_thread = std::thread::spawn(move || {
            let vector = QueryVector {
                model: "model".into(),
                values: Arc::from([1.0, 0.0]),
            };
            let scopes = vec!["user".into()];
            search.search_available(
                SemanticQuery {
                    binding: "binding",
                    scopes: &scopes,
                    text: "read",
                    vector: Some(&vector),
                    limit: 10,
                    source: &target,
                },
                &RequestControl::default(),
            )
        });
        barrier.wait();
        let closer = indexer.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let close_thread = std::thread::spawn(move || {
            if close_only {
                closer.close_binding("binding");
                done_tx.send(true).unwrap();
            } else {
                done_tx
                    .send(closer.shutdown(Duration::from_secs(5)))
                    .unwrap();
            }
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        barrier.wait();
        assert!(query_thread.join().unwrap().is_err());
        assert!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        close_thread.join().unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
            .unwrap();
        assert!(indexer.shutdown(Duration::from_secs(5)));
    }
}

#[test]
fn rebuilds_v1_current_and_history_before_reclaiming_legacy_files() {
    use agentlaw_flows::semantic::{HistoryIngestHint, HistorySemanticPort, OfferResult};
    use agentlaw_search::vector_file::{
        CurrentHeads, InventoryCursor, VectorFileBatch, VectorFileWriter,
    };
    use agentlaw_search::VectorRecord;
    use agentlaw_worker::supervisor::IndexTask;

    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let receipt = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation("original body", vec![])],
        )
        .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let change = &receipt.references[0].observed_version;
    let mut old_config = SemanticConfig::new(temp.path().join("indices"), "v1".into());
    old_config.dimension = 2;
    let old_paths: Vec<_> = [
        (VectorFileKind::Current, ID.to_owned(), "body:0", "user"),
        (
            VectorFileKind::History { subject: ID.into() },
            format!("change:{change}"),
            "history:0",
            "history",
        ),
    ]
    .into_iter()
    .map(|(kind, memory, section, scope)| {
        let identity = VectorFileIdentity {
            binding: "binding".into(),
            kind,
            epoch: position.epoch.clone(),
            model_digest: "model".into(),
            config: "v1".into(),
            generation: "initial".into(),
            dimension: 2,
        };
        let path = super::super::identity::path(&old_config, &identity);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut writer = VectorFileWriter::open(&path, &identity).unwrap();
        writer
            .commit(VectorFileBatch {
                heads: vec![CurrentHeads {
                    memory_id: memory.clone(),
                    change_ids: vec![change.clone()],
                }],
                records: vec![VectorRecord {
                    memory_id: memory.clone(),
                    change_id: change.clone(),
                    section: section.into(),
                    scope: scope.into(),
                    vector: vec![0.0, 1.0],
                }],
                ack: Some(SourcePosition {
                    epoch: position.epoch.clone(),
                    sequence: position.sequence as i64,
                }),
                inventory: (section == "body:0").then(|| InventoryCursor {
                    start: SourcePosition {
                        epoch: position.epoch.clone(),
                        sequence: position.sequence as i64,
                    },
                    completed_identity: format!("memory/{memory}"),
                }),
                complete: true,
                completed_changes: if section == "history:0" {
                    vec![(memory.clone(), change.clone())]
                } else {
                    vec![]
                },
                ..Default::default()
            })
            .unwrap();
        assert!(writer.contains(&memory, change, section).unwrap());
        writer.checkpoint().unwrap();
        drop(writer);
        let view = VectorFileReadView::open(&path, &identity).unwrap();
        assert!(view.progress().complete);
        assert_eq!(
            view.progress().ack.as_ref().unwrap().sequence,
            position.sequence as i64
        );
        assert_eq!(
            view.progress()
                .inventory
                .as_ref()
                .map(|cursor| cursor.completed_identity.as_str()),
            (section == "body:0")
                .then(|| format!("memory/{memory}"))
                .as_deref()
        );
        path
    })
    .collect();

    assert_eq!(INDEX_CONFIG_VERSION, "v2");
    let mut config = old_config;
    config.config_id = INDEX_CONFIG_VERSION.into();
    let indexer = SemanticIndexer::start(
        config.clone(),
        ModelRequestIds::new(),
        HealthSink::default(),
    )
    .unwrap();
    indexer.open_binding("binding", source).unwrap();
    indexer.model_ready("model");
    let search = SemanticSearch::new(indexer.clone(), crate::model::ModelHandle::new());
    let hint = HistoryIngestHint {
        binding: "binding".into(),
        subject: ID.into(),
        through: position.clone(),
    };
    assert_eq!(
        HistorySemanticPort::offer(&search, hint.clone()),
        OfferResult::Queued
    );
    let first = wait_work(&indexer);
    let vector = QueryVector {
        model: "model".into(),
        values: Arc::from([1.0, 0.0]),
    };
    let result = SemanticPort::search_available(
        &search,
        SemanticQuery {
            binding: "binding",
            scopes: &["user".into()],
            text: "original body",
            vector: Some(&vector),
            limit: 10,
            source: &position,
        },
        &RequestControl::default(),
    )
    .unwrap();
    assert!(matches!(result.coverage, SemanticCoverage::Partial { .. }));
    assert!(result.hits.is_empty());
    assert!(old_paths.iter().all(|path| path.is_file()));
    assert!(indexer.progress("binding").unwrap().ack.is_none());

    let mut next = Some(first);
    let mut split_count = 0;
    let mut embed_count = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(work) = next.take().or_else(|| indexer.next_work("model")) {
            let result = match work.task {
                IndexTask::Split(text) => {
                    split_count += 1;
                    assert!(text.starts_with("original body"));
                    IndexResult::Pieces(vec![(0, text.len() as u32)])
                }
                IndexTask::Embed(_) => {
                    embed_count += 1;
                    IndexResult::Vector(vec![1.0, 0.0])
                }
            };
            indexer.finish(work.id, Ok(result));
        }
        if indexer.progress("binding").is_some_and(|progress| {
            progress.complete
                && progress
                    .ack
                    .is_some_and(|ack| ack.sequence == position.sequence as i64)
        }) && HistorySemanticPort::offer(&search, hint.clone()) == OfferResult::Accepted
        {
            break;
        }
        assert!(Instant::now() < deadline, "v2 rebuild did not complete");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!((split_count, embed_count), (2, 2));
    for (kind, section) in [
        (VectorFileKind::Current, "body:0:0"),
        (
            VectorFileKind::History { subject: ID.into() },
            "history:0:0",
        ),
    ] {
        let identity = VectorFileIdentity {
            binding: "binding".into(),
            kind,
            epoch: position.epoch.clone(),
            model_digest: "model".into(),
            config: "v2".into(),
            generation: "initial".into(),
            dimension: 2,
        };
        let path = super::super::identity::path(&config, &identity);
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            conn.query_row("SELECT section FROM vectors", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            section
        );
        let vector = conn
            .query_row("SELECT vector FROM vectors", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap();
        assert_eq!(
            vector,
            [1.0_f32, 0.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect::<Vec<_>>()
        );
        let view = VectorFileReadView::open(path, &identity).unwrap();
        assert_eq!(
            view.progress().ack.as_ref().unwrap().sequence,
            position.sequence as i64
        );
    }
    while old_paths.iter().any(|path| path.exists()) {
        assert!(Instant::now() < deadline, "v1 files were not reclaimed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(indexer.shutdown(Duration::from_secs(5)));
}
