use crate::{
    model::ModelHandle,
    ports::HealthSink,
    projection::ProjectionConfig,
    semantic::{SemanticConfig, SemanticIndexer, SemanticSearch},
    store::service::StoreDeps,
};
use agentlaw_flows::runtime::StoreIdentity;
use agentlaw_storage::Store;
use agentlaw_worker::supervisor::ModelRequestIds;
use std::{path::Path, sync::Arc};
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub(crate) struct TestGitHandle {
    pub handle: crate::git::handle::GitStoreHandle,
    _lane: crate::store::lane::MutationLane,
}
pub(crate) fn git_handle(store: &Store) -> TestGitHandle {
    let local = store.local_root();
    let binding = if local.file_name().is_some_and(|name| name == "canonical") {
        local.parent().unwrap_or(local)
    } else {
        local
    };
    git_handle_in(store, binding)
}
pub(crate) fn git_handle_in(store: &Store, binding_local: &Path) -> TestGitHandle {
    let root = store
        .with_source_read(|root, _| Ok(root.to_owned()))
        .unwrap();
    let local = binding_local.to_owned();
    let coordination = store.coordination_root().to_owned();
    let identity = StoreIdentity {
        root: root.clone(),
        local: local.clone(),
        user_id: "user".into(),
        machine_id: id(),
        binding_id: id(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: None,
    };
    let writer = agentlaw_flows::control::ControlWriter::open(&local).unwrap();
    let lane_store =
        Store::open_with_coordination(&root, store.local_root(), &coordination).unwrap();
    let lane = crate::store::lane::MutationLane::start(identity, lane_store, writer).unwrap();
    let handle = crate::git::handle::GitStoreHandle {
        root,
        local,
        coordination,
        source: store.owned_published_reader(),
        lane: lane.sender(),
    };
    TestGitHandle {
        handle,
        _lane: lane,
    }
}
pub fn fixture() -> (tempfile::TempDir, StoreIdentity) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    std::fs::create_dir_all(&local).unwrap();
    drop(Store::open(&root, local.join("canonical")).unwrap());
    let identity = StoreIdentity {
        root,
        local,
        user_id: id(),
        machine_id: id(),
        binding_id: id(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    (dir, identity)
}
pub fn deps(dir: &Path) -> StoreDeps {
    let health = HealthSink::default();
    let indexer = SemanticIndexer::start(
        SemanticConfig::new(dir.join("semantic"), "test".into()),
        ModelRequestIds::new(),
        health.clone(),
    )
    .unwrap();
    let semantic = Arc::new(SemanticSearch::new(indexer.clone(), ModelHandle::new()));
    StoreDeps {
        health,
        semantic,
        indexer,
        projection: ProjectionConfig::new(),
        coordination: dir.join("coordination"),
        state: dir.join("state"),
    }
}
