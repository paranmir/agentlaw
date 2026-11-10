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
    indexer.finish(stale.id, Ok(vec![1.0, 0.0]));
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
