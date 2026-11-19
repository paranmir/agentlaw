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
    let path = temp.path().join("selection.json");
    let cache = SelectionCache::open(path.clone());
    let settings = CpuExecutionSettings {
        version: 2,
        intra: 4,
        inter: 1,
    };
    let hardware = || Some("fixed model, runtime, CPU and GPU identity".to_owned());
    let old = hardware().expect("fixture identity");
    let previous = Selection {
        cacheable: true,
        backend: ExecutionBackend::Cpu,
        cpu_ns: vec![20; 3],
        gpu_ns: vec![25; 3],
        reason: "prior setting".into(),
        host_resident_bytes: None,
        gpu_dedicated_bytes: None,
        gpu_shared_bytes: None,
    };
    cache.accept(old.clone(), previous.clone());
    cache.flush();
    let comparisons = std::cell::Cell::new(0);
    let start = || {
        let device = |cached: Option<&Selection>| {
            if let Some(selection) = cached {
                return selection.clone();
            }
            comparisons.set(comparisons.get() + 1);
            Selection {
                cpu_ns: vec![12; 3],
                gpu_ns: vec![25; 3],
                reason: "measured".into(),
                ..previous.clone()
            }
        };
        let (fingerprint, selection) =
            select_for_launch(hardware, Some(settings), cache.get(), device);
        if let Some(key) = &fingerprint {
            cache.accept(key.clone(), selection);
            cache.flush();
        }
        fingerprint
    };

    let new = start();
    assert_eq!(comparisons.get(), 1);
    assert!(
        new.is_some(),
        "launch did not produce a fingerprint for the injected hardware identity and CPU settings"
    );
    assert_ne!(new.as_deref(), Some(old.as_str()));
    let (stored, selection): (String, Selection) =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(Some(stored.as_str()), new.as_deref());
    assert_eq!(selection.cpu_ns, [12; 3]);
    assert_eq!(start(), new);
    assert_eq!(comparisons.get(), 1);
}
