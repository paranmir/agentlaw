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
