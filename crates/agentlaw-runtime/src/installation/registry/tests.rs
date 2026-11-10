use super::*;
use std::fs;

fn snapshot(base: &std::path::Path, name: &str) -> Arc<ConfigSnapshot> {
    let root = base.join(name);
    fs::create_dir_all(&root).unwrap();
    let key = agentlaw_storage::binding_identity(&root, &root).unwrap();
    let config = Config {
        memory_store_path: root,
        user_id: "user".into(),
        history_response_limit_bytes: 8192,
        runtime_path: None,
        response_limit_bytes: 65536,
    };
    let reference = BindingRef::new(&key, &config.user_id);
    Arc::new(ConfigSnapshot {
        config,
        key,
        reference,
    })
}

#[test]
fn issued_binding_survives_selection_and_lru_evicts_oldest() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ConfigRegistry::new();
    let a = snapshot(dir.path(), "a");
    let b = snapshot(dir.path(), "b");
    registry.publish(ConfigState::Ready(a.clone()));
    registry.current();
    registry.publish(ConfigState::Ready(b.clone()));
    registry.current();
    assert_eq!(registry.resolve(&a.reference).unwrap().key, a.key);
    for i in 0..64 {
        registry.publish(ConfigState::Ready(snapshot(
            dir.path(),
            &format!("store-{i}"),
        )));
        registry.current();
    }
    assert!(matches!(
        registry.resolve(&a.reference),
        Err(BindingError::BindingChanged)
    ));
    assert!(matches!(
        registry.resolve(&b.reference),
        Err(BindingError::BindingChanged)
    ));
}

#[test]
fn restart_accepts_only_current_selection() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ConfigRegistry::new();
    let a = snapshot(dir.path(), "a");
    let b = snapshot(dir.path(), "b");
    registry.publish(ConfigState::Ready(b.clone()));
    assert!(matches!(
        registry.resolve(&a.reference),
        Err(BindingError::BindingChanged)
    ));
    assert_eq!(registry.resolve(&b.reference).unwrap().key, b.key);
}
