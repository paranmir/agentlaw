use super::*;
#[test]
fn complete_ranges_and_noise() {
    assert!(gpu_is_unambiguously_faster(&[11, 12, 13], &[1, 2, 3]));
    assert!(!gpu_is_unambiguously_faster(&[2, 12, 13], &[1, 2, 3]));
    assert!(!gpu_is_unambiguously_faster(&[2, 3], &[1, 1]));
    assert!(!gpu_is_unambiguously_faster(&[3, 4, 5], &[0, 1, 2]));
}

#[test]
fn cpu_execution_change_invalidates_device_selection_cache() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("runtime.dll");
    std::fs::write(&runtime, b"unchanged native runtime identity").unwrap();
    let model = "unchanged-model";
    let new_settings = CpuExecutionSettings {
        version: 2,
        intra: 4,
        inter: 1,
    };
    let new = fingerprint_with_cpu_settings(model, &runtime, new_settings).unwrap();
    let old = fingerprint(model, &runtime).unwrap();
    let same = fingerprint_with_cpu_settings(model, &runtime, new_settings).unwrap();
    assert_ne!(old, new);
    assert_eq!(new, same);

    let cache = temp.path().join("selection.json");
    let initial = Selection {
        cacheable: true,
        backend: ExecutionBackend::Cpu,
        cpu_ns: vec![20; 3],
        gpu_ns: vec![25; 3],
        reason: "previous CPU timing".into(),
        host_resident_bytes: None,
        gpu_dedicated_bytes: None,
        gpu_shared_bytes: None,
    };
    std::fs::write(&cache, serde_json::to_vec(&(old, initial)).unwrap()).unwrap();
    let mut comparisons = 0;
    let mut start = |key: String| {
        let saved = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
        if let Some(saved) = cached_selection(Some(saved), Some(&key)) {
            return saved;
        }
        comparisons += 1;
        let measured = Selection {
            cacheable: true,
            backend: ExecutionBackend::Cuda,
            cpu_ns: vec![20; 3],
            gpu_ns: vec![10; 3],
            reason: "new timing".into(),
            host_resident_bytes: None,
            gpu_dedicated_bytes: None,
            gpu_shared_bytes: None,
        };
        std::fs::write(&cache, serde_json::to_vec(&(key, &measured)).unwrap()).unwrap();
        measured
    };
    assert_eq!(start(new.clone()).backend, ExecutionBackend::Cuda);
    assert_eq!(start(same).backend, ExecutionBackend::Cuda);
    assert_eq!(comparisons, 1);
    assert_ne!(
        new,
        fingerprint_with_cpu_settings(
            model,
            &runtime,
            CpuExecutionSettings {
                intra: 2,
                ..new_settings
            }
        )
        .unwrap()
    );
    assert_ne!(
        new,
        fingerprint_with_cpu_settings(
            model,
            &runtime,
            CpuExecutionSettings {
                inter: 2,
                ..new_settings
            }
        )
        .unwrap()
    );
}
