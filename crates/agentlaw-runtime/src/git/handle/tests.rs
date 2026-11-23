use crate::store::service::StoreService;
use crate::test_support::store_service::{deps, fixture, id};
use agentlaw_flows::RequestControl;
use agentlaw_storage::Store;
use serde_json::json;
#[test]
fn git_import_handle_rejects_stale_generation_and_preserves_source() {
    use crate::store::lane::ImportPublishInput;
    use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
    let (dir, identity) = fixture();
    let store = Store::open_with_coordination(
        &identity.root,
        identity.local.join("canonical"),
        dir.path().join("coordination"),
    )
    .unwrap();
    let before = store.generation().unwrap();
    let staged_root = dir.path().join("staged");
    let staged_local = dir.path().join("staged-local");
    let staged =
        Store::open_with_coordination(&staged_root, &staged_local, store.coordination_root())
            .unwrap();
    let staged_generation = staged.generation().unwrap();
    let entity = id();
    let unit = CurrentUnit {
        entity_id: entity.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":id()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: "preserved local change".into(),
            }],
        },
    };
    store
        .publish(
            &id(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "test".into(),
            }],
        )
        .unwrap();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let result = service.git_handle().publish_import(
        ImportPublishInput {
            operation_id: id(),
            staged_root,
            staged_local,
            expected_generation: before,
            staged_generation,
        },
        &RequestControl::default(),
    );
    assert!(matches!(result, Err(e) if e.code == "sync_inputs_changed"));
    assert!(
        format!("{:?}", store.read_current(&entity).unwrap()).contains("preserved local change")
    );
}

#[test]
fn import_handle_publishes_staged_source_and_returns_receipt() {
    use crate::store::lane::ImportPublishInput;
    use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
    let (dir, identity) = fixture();
    let source = Store::open_with_coordination(
        &identity.root,
        identity.local.join("canonical"),
        dir.path().join("coordination"),
    )
    .unwrap();
    let before = source.generation().unwrap();
    let staged_root = dir.path().join("staged");
    let staged_local = dir.path().join("staged-local");
    let staged =
        Store::open_with_coordination(&staged_root, &staged_local, source.coordination_root())
            .unwrap();
    let entity = id();
    staged.publish(&id(), vec![Mutation { unit: CurrentUnit { entity_id: entity.clone(), entity_type: "memory".into(), state: UnitState::Live { heads: vec![Head { metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":id()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}), body: "imported".into() }] } }, expected_versions: vec![], evidence: "test".into() }]).unwrap();
    let staged_generation = staged.generation().unwrap();
    let operation_id = id();
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let receipt = service
        .git_handle()
        .publish_import(
            ImportPublishInput {
                operation_id: operation_id.clone(),
                staged_root,
                staged_local,
                expected_generation: before,
                staged_generation,
            },
            &RequestControl::default(),
        )
        .unwrap();
    assert_eq!(receipt.operation_id, operation_id);
    assert_eq!(
        source.read_current(&entity).unwrap().heads()[0].body,
        "imported"
    );
}

#[test]
fn sync_overlay_handle_rejects_changed_captured_inputs() {
    use crate::store::lane::SyncOverlayInput;
    use agentlaw_storage::{sync::CaptureLimits, CurrentUnit, Head, Mutation, UnitState};
    let (dir, identity) = fixture();
    let source = Store::open_with_coordination(
        &identity.root,
        identity.local.join("canonical"),
        dir.path().join("coordination"),
    )
    .unwrap();
    let entity = id();
    let write = |store: &Store, body: &str| {
        let versions = store
            .read_current(&entity)
            .ok()
            .map(|u| {
                u.references()
                    .unwrap()
                    .into_iter()
                    .map(|v| v.observed_version)
                    .collect()
            })
            .unwrap_or_default();
        let unit = CurrentUnit {
            entity_id: entity.clone(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":id()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: body.into(),
                }],
            },
        };
        store
            .publish(
                &id(),
                vec![Mutation {
                    unit,
                    expected_versions: versions,
                    evidence: "test".into(),
                }],
            )
            .unwrap();
    };
    write(&source, "base");
    let baseline_root = dir.path().join("baseline");
    let baseline_local = dir.path().join("baseline-local");
    let captured = source
        .capture_sync(&baseline_root, &CaptureLimits::default(), None)
        .unwrap();
    let baseline = Store::attach_existing_with_coordination(
        &baseline_root,
        &baseline_local,
        source.coordination_root(),
    )
    .unwrap();
    let incoming_root = dir.path().join("incoming");
    source
        .capture_sync(&incoming_root, &CaptureLimits::default(), None)
        .unwrap();
    let incoming = Store::attach_existing_with_coordination(
        &incoming_root,
        dir.path().join("incoming-local"),
        source.coordination_root(),
    )
    .unwrap();
    write(&incoming, "incoming");
    let resolved_root = dir.path().join("resolved");
    let resolved_local = dir.path().join("resolved-local");
    let resolved = baseline
        .prepare_sync_union(&incoming, &resolved_root, &resolved_local)
        .unwrap();
    let guard = source
        .prepare_overlay_guard(&captured, &baseline, &resolved)
        .unwrap();
    write(&source, "new local input");
    let service =
        StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
    let result = service.git_handle().publish_sync_overlay(
        SyncOverlayInput {
            publication_id: id(),
            baseline_root,
            baseline_local,
            resolved_root,
            resolved_local,
            guard,
        },
        &RequestControl::default(),
    );
    assert!(matches!(result, Err(e) if e.code == "sync_inputs_changed"));
    assert_eq!(
        source.read_current(&entity).unwrap().heads()[0].body,
        "new local input"
    );
}

#[test]
fn cancelled_queued_git_mutations_leave_no_effects() {
    use crate::store::lane::{ImportPublishInput, LaneCommand, SyncOperationRow, SyncOverlayInput};
    use agentlaw_storage::{sync::CaptureLimits, CurrentUnit, Head, Mutation, UnitState};
    use std::{
        sync::{atomic::Ordering, mpsc},
        thread,
        time::Duration,
    };

    for kind in ["persist", "reserve", "complete", "overlay", "import"] {
        let (dir, identity) = fixture();
        let source = Store::open_with_coordination(
            &identity.root,
            identity.local.join("canonical"),
            dir.path().join("coordination"),
        )
        .unwrap();
        let source_local = identity.local.clone();
        let baseline_root = dir.path().join("base");
        let baseline_local = dir.path().join("base-local");
        let capture = source
            .capture_sync(&baseline_root, &CaptureLimits::default(), None)
            .unwrap();
        let baseline = Store::attach_existing_with_coordination(
            &baseline_root,
            &baseline_local,
            source.coordination_root(),
        )
        .unwrap();
        let staged_root = dir.path().join("staged");
        let staged_local = dir.path().join("staged-local");
        source
            .capture_sync(&staged_root, &CaptureLimits::default(), None)
            .unwrap();
        let staged = Store::attach_existing_with_coordination(
            &staged_root,
            &staged_local,
            source.coordination_root(),
        )
        .unwrap();
        let entity = id();
        staged.publish(&id(), vec![Mutation { unit: CurrentUnit { entity_id: entity.clone(), entity_type: "memory".into(), state: UnitState::Live { heads: vec![Head { metadata: json!({"change_id":id(),"applicability":{"scope":"user"},"origin":{"machine_id":id()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}), body: "only staged".into() }] } }, expected_versions: vec![], evidence: "test".into() }]).unwrap();
        let staged_generation = staged.generation().unwrap();
        let generation = source.generation().unwrap();
        let guard = source
            .prepare_overlay_guard(&capture, &baseline, &staged)
            .unwrap();
        let publication_id = id();
        let service =
            StoreService::open(identity, deps(dir.path()), &RequestControl::default()).unwrap();
        service
            .lane_sender()
            .call(
                LaneCommand::SyncReserve {
                    id: "completion".into(),
                    operation: "op".into(),
                    digest: "digest".into(),
                },
                &RequestControl::default(),
            )
            .unwrap();
        let lock = rusqlite::Connection::open(source_local.join("control.sqlite")).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let sender = service.lane_sender();
        let (first_tx, first_rx) = mpsc::channel();
        let first = thread::spawn(move || {
            first_tx
                .send(sender.call(
                    LaneCommand::SyncPersist(SyncOperationRow {
                        id: "lane-blocker".into(),
                        source: "block".into(),
                        state_json: "{}".into(),
                        active: false,
                    }),
                    &RequestControl::default(),
                ))
                .ok();
        });
        assert!(matches!(
            first_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let handle = service.git_handle();
        let cancel = RequestControl::default();
        let worker_control = cancel.clone();
        let publication = publication_id.clone();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = match kind {
                "persist" => handle.sync_control().persist(
                    SyncOperationRow {
                        id: "queued".into(),
                        source: "queued".into(),
                        state_json: "{}".into(),
                        active: false,
                    },
                    &worker_control,
                ),
                "reserve" => handle
                    .sync_control()
                    .reserve("queued", "op", "digest", &worker_control)
                    .map(|_| ()),
                "complete" => handle.sync_control().complete(
                    "completion",
                    "{\"completed\":true}".into(),
                    &worker_control,
                ),
                "overlay" => handle
                    .publish_sync_overlay(
                        SyncOverlayInput {
                            publication_id: publication,
                            baseline_root,
                            baseline_local,
                            resolved_root: staged_root,
                            resolved_local: staged_local,
                            guard,
                        },
                        &worker_control,
                    )
                    .map(|_| ()),
                "import" => handle
                    .publish_import(
                        ImportPublishInput {
                            operation_id: publication,
                            staged_root,
                            staged_local,
                            expected_generation: generation,
                            staged_generation,
                        },
                        &worker_control,
                    )
                    .map(|_| ()),
                _ => unreachable!(),
            };
            tx.send(result).ok();
        });
        let waiting = rx.recv_timeout(Duration::from_millis(200));
        cancel.cancel.store(true, Ordering::Release);
        let cancelled = if matches!(waiting, Err(mpsc::RecvTimeoutError::Timeout)) {
            rx.recv_timeout(Duration::from_secs(3))
        } else {
            waiting
        };
        lock.execute_batch("COMMIT").unwrap();
        let _ = first_rx.recv_timeout(Duration::from_secs(5));
        first.join().unwrap();
        let joined = worker.join();
        assert!(joined.is_ok(), "{kind} worker panicked");
        assert!(
            matches!(cancelled, Ok(Err(ref e)) if e.code == "cancelled"),
            "{kind} did not remain queued and cancel"
        );
        let reader = agentlaw_flows::control::ControlReader::open(&source_local).unwrap();
        assert!(reader.sync_operation("queued").unwrap().is_none());
        assert!(reader.sync_request("queued").unwrap().is_none());
        assert_eq!(reader.sync_request("completion").unwrap().unwrap().2, None);
        assert_eq!(source.generation().unwrap(), generation, "{kind}");
        assert!(
            source.read_current(&entity).is_err(),
            "{kind} published staged content"
        );
        assert!(source.sync_receipt(&publication_id).unwrap().is_none());
    }
}
