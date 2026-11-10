use super::*;

#[test]
fn selection_cache_does_not_rewrite_unchanged_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("selection.json");
    let cache = SelectionCache::open(path.clone());
    let selection = crate::execution::Selection {
        cacheable: true,
        backend: crate::execution::ExecutionBackend::Cpu,
        cpu_ns: vec![1, 2, 3],
        gpu_ns: vec![],
        reason: "test".into(),
        host_resident_bytes: None,
        gpu_dedicated_bytes: None,
        gpu_shared_bytes: None,
    };
    cache.save("fingerprint".into(), selection.clone());
    cache.flush();
    assert!(path.exists());
    std::fs::remove_file(&path).unwrap();
    cache.save("fingerprint".into(), selection);
    cache.flush();
    assert!(!path.exists(), "unchanged content was written twice");
}
