use super::*;

#[test]
fn store_names_must_be_single_components() {
    let fixture = TestInstallation::new();
    let absolute = fixture
        .root
        .path()
        .join("absolute")
        .to_string_lossy()
        .into_owned();
    for name in [absolute.as_str(), "a/b", "..", "."] {
        assert!(std::panic::catch_unwind(|| fixture.store_path(name)).is_err());
        assert!(fixture.select_store(name).is_err());
    }
    assert!(fixture.store_path("a").starts_with(fixture.root.path()));
}

#[test]
fn installation_selection_is_isolated_and_stable() {
    let fixture = TestInstallation::new();
    assert!(fixture.config().unwrap().is_none());
    fixture.select_store("a").unwrap();
    assert!(fixture.store_path("a").join("format.md").exists());
    assert_eq!(
        fixture.config().unwrap().unwrap().memory_store_path,
        fs::canonicalize(fixture.store_path("a")).unwrap()
    );
    let a = fixture.binding_ref().unwrap();
    assert_eq!(a, fixture.binding_ref().unwrap());
    fixture.select_store("b").unwrap();
    assert_ne!(a, fixture.binding_ref().unwrap());
    assert!(fixture
        .config()
        .unwrap()
        .unwrap()
        .memory_store_path
        .starts_with(fs::canonicalize(fixture.root.path()).unwrap()));
    fixture.select_store("a").unwrap();
    assert_eq!(a, fixture.binding_ref().unwrap());
    assert!(fixture.select_store("../escape").is_err());
}
