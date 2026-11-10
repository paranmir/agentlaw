use super::*;
use crate::{codec, Store};

#[test]
fn canonical_paths_define_the_key() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let other = temp.path().join("other");
    for path in [&root, &local, &other] {
        fs::create_dir_all(path.join("sub")).unwrap();
    }
    let key = binding_identity(&root, &local).unwrap();
    assert_eq!(
        key,
        binding_identity(&root.join("sub/.."), &local.join("sub/..")).unwrap()
    );
    assert_ne!(key, binding_identity(&root, &other).unwrap());
}

#[cfg(windows)]
#[test]
fn case_variants_define_the_same_key() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Source");
    let local = temp.path().join("Local");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&local).unwrap();
    let key = binding_identity(&root, &local).unwrap();
    assert_eq!(
        key,
        binding_identity(&temp.path().join("SOURCE"), &temp.path().join("LOCAL")).unwrap()
    );
}

#[test]
fn missing_paths_are_not_created() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing");
    assert!(binding_identity(temp.path(), &missing).is_err());
    assert!(!missing.exists());
    assert!(binding_identity(&missing, temp.path()).is_err());
    assert!(!missing.exists());
}

#[test]
fn key_matches_persisted_binding() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    let key = binding_identity(&root, &local).unwrap();
    let dir = store
        .coordination_root()
        .join(codec::digest(key.store_identity().as_bytes()));
    assert!(dir.is_dir());
    let binding: String = store.load(&dir.join("binding.json")).unwrap();
    assert_eq!(binding, key.local_identity());
}
