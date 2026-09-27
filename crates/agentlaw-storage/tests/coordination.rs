use agentlaw_storage::{Error, Store};
use std::fs;

fn roots(temp: &tempfile::TempDir, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let source = temp.path().join(format!("{name}-source"));
    let local = temp.path().join(format!("{name}-local"));
    (source, local)
}

#[test]
fn explicit_registry_is_used_and_fences_a_source_to_one_local_directory() {
    let temp = tempfile::tempdir().unwrap();
    let (source, local) = roots(&temp, "primary");
    let coordination = temp
        .path()
        .join("selected-install")
        .join("state")
        .join("source-coordination");

    let store = Store::open_with_coordination(&source, &local, &coordination).unwrap();
    assert_eq!(store.coordination_root(), coordination);
    assert!(
        coordination.is_dir(),
        "the supplied registry root should be created"
    );
    assert!(
        fs::read_dir(&coordination).unwrap().next().is_some(),
        "the source binding should be recorded under the supplied registry"
    );

    // The old two-argument wrapper derives its own isolated registry. A second
    // installation-style local directory must instead consult the explicitly
    // shared registry and fail closed for this source.
    let (_same_source, other_local) = roots(&temp, "secondary");
    assert!(matches!(
        Store::open_with_coordination(&source, &other_local, &coordination),
        Err(Error::LocalBindingMismatch)
    ));

    drop(store);
    let reopened = Store::open_with_coordination(&source, &local, &coordination).unwrap();
    assert_eq!(reopened.coordination_root(), coordination);
}

#[test]
fn readonly_open_uses_existing_registry_and_does_not_create_a_missing_one() {
    let temp = tempfile::tempdir().unwrap();
    let (source, local) = roots(&temp, "readonly");
    let coordination = temp.path().join("managed").join("source-coordination");
    Store::open_with_coordination(&source, &local, &coordination).unwrap();

    let readonly = Store::open_read_only_with_coordination(&source, &local, &coordination).unwrap();
    assert_eq!(readonly.coordination_root(), coordination);

    let missing_registry = temp.path().join("missing-registry");
    assert!(Store::open_read_only_with_coordination(&source, &local, &missing_registry).is_err());
    assert!(
        !missing_registry.exists(),
        "read-only binding lookup must not create the registry or a binding record"
    );
}

#[test]
fn explicit_attachment_and_readonly_reopen_share_the_selected_registry() {
    let temp = tempfile::tempdir().unwrap();
    let (source, initial_local) = roots(&temp, "portable");
    let initial_coordination = temp.path().join("initial-install").join("coordination");
    Store::open_with_coordination(&source, &initial_local, &initial_coordination).unwrap();

    // Attaching to an already initialized source uses the registry supplied by
    // the application for this installation, not a profile-derived location.
    let attached_local = temp.path().join("attached-local");
    let attached_coordination = temp.path().join("attached-install").join("coordination");
    let attached =
        Store::attach_existing_with_coordination(&source, &attached_local, &attached_coordination)
            .unwrap();
    assert_eq!(attached.coordination_root(), attached_coordination);
    assert!(attached_coordination.is_dir());

    let reopened =
        Store::open_read_only_with_coordination(&source, &attached_local, &attached_coordination)
            .unwrap();
    assert_eq!(reopened.coordination_root(), attached_coordination);
}
