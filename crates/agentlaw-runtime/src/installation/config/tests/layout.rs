use super::*;

#[test]
fn managed_layout_requires_its_exact_marker_and_state_child() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let state = root.join("state");
    assert_eq!(managed_install_root(&state).unwrap(), None);
    assert_eq!(
        model_artifact_root(&state).unwrap(),
        state.join("artifacts")
    );
    fs::write(root.join(MANAGED_LAYOUT_MARKER), "unknown-layout\n").unwrap();
    assert!(managed_install_root(&state).is_err());
    fs::write(
        root.join(MANAGED_LAYOUT_MARKER),
        format!("{MANAGED_LAYOUT_VERSION}\n"),
    )
    .unwrap();
    assert_eq!(
        managed_install_root(&state).unwrap(),
        Some(root.to_path_buf())
    );
    fs::write(&state, "not a directory").unwrap();
    assert!(managed_install_root(&state).is_err());
    fs::remove_file(&state).unwrap();
    assert_eq!(model_artifact_root(&state).unwrap(), root.join("models"));
    assert_eq!(managed_install_root(&root.join("other")).unwrap(), None);
}
