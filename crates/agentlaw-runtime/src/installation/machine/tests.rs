use super::*;
#[test]
fn naming_and_reload_never_change_identity() {
    let root = tempfile::tempdir().unwrap();
    let first = load_or_create(root.path()).unwrap();
    assert!(first.display_name.is_none());
    let named = name(root.path(), "개발 노트북").unwrap();
    assert_eq!(named.machine_id, first.machine_id);
    assert_eq!(
        load_or_create(root.path()).unwrap().display_name,
        named.display_name
    );
    fs::write(root.path().join("machine.json"), "{}").unwrap();
    assert!(load_or_create(root.path()).is_err());
}

#[test]
fn selected_store_never_regenerates_missing_machine_identity() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("config.json"), b"selected").unwrap();
    assert!(load_or_create(root.path()).is_err());
    assert!(!root.path().join("machine.json").exists());
}
