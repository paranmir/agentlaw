use super::*;
use std::cell::RefCell;
use std::collections::BTreeSet;

fn completed_update(root: &Path, cleanup_id: &str) -> Plan {
    Plan {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.to_path_buf(),
        tag: "v1.0.0".into(),
        archive: target_archive().unwrap().to_owned(),
        archive_digest: "0".repeat(64),
        bin_before: String::new(),
        bin_before_hashes: BTreeMap::new(),
        registrations: Vec::new(),
        phase: "completed".into(),
        bundle_hashes: None,
        launcher_digest: None,
        previous_bundle: None,
        activation_verified: true,
        recovery_obligations_closed: true,
        cleanup_plan_id: Some(cleanup_id.into()),
        cleanup_completed: true,
    }
}

fn cleanup_for_records(
    root: &Path,
    id: String,
    entries: Vec<(&str, PathBuf, &str)>,
) -> CleanupPlan {
    let items = entries
        .into_iter()
        .enumerate()
        .map(|(index, (kind, source, phase))| CleanupItem {
            kind: kind.into(),
            holding: root
                .join(".cleanup-holding")
                .join(&id)
                .join(index.to_string()),
            manifest: complete_manifest(&source).unwrap(),
            source,
            phase: phase.into(),
            removed_entries: BTreeSet::new(),
            entry_intent: None,
            retrying: false,
        })
        .collect();
    CleanupPlan {
        id,
        root: root.to_path_buf(),
        marker_sha256: digest(&root.join(".agentlaw-layout")).unwrap(),
        permanent_internal_removal_approved: true,
        items,
    }
}

fn add_completed_artifacts(plan: &mut CleanupPlan, update: &Plan) {
    for (kind, source) in [
        ("previous", previous_bundle(update)),
        ("stage", stage(&plan.root, &update.id)),
    ] {
        let index = plan.items.len();
        plan.items.push(CleanupItem {
            kind: kind.into(),
            source,
            holding: plan
                .root
                .join(".cleanup-holding")
                .join(&plan.id)
                .join(index.to_string()),
            manifest: vec![],
            phase: "removed".into(),
            removed_entries: BTreeSet::new(),
            entry_intent: None,
            retrying: false,
        });
    }
}

struct InterruptedRemoval {
    remove_first: bool,
}

struct CrashAt(CleanupCheckpoint);
impl Removal for CrashAt {
    fn remove(&self, path: &Path, directory: bool) -> std::io::Result<()> {
        FilesystemRemoval.remove(path, directory)
    }
    fn checkpoint(&self, checkpoint: CleanupCheckpoint) {
        if checkpoint == self.0 {
            panic!("injected interruption after checkpoint");
        }
    }
}

fn interrupted_checkpoint_recovers(checkpoint: CleanupCheckpoint) {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let record = root
        .join("state")
        .join(format!("install-completed-{}.json", uuid::Uuid::new_v4()));
    fs::create_dir_all(record.parent().unwrap()).unwrap();
    fs::write(&record, br#"{"targets":[]}"#).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let update = completed_update(&root, &id);
    let mut plan = cleanup_for_records(
        &root,
        id,
        vec![("old_install", record.clone(), "cleanup_deferred")],
    );
    if checkpoint == CleanupCheckpoint::ManifestPinned {
        plan.items[0].manifest.clear();
    }
    add_completed_artifacts(&mut plan, &update);
    cleanup_save(&plan).unwrap();
    save(&update).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        retry_pending_with(&root, &CrashAt(checkpoint), false);
    }))
    .is_err());
    let interrupted = cleanup_load(&root, &plan.id).unwrap();
    let item = &interrupted.items[0];
    assert!(item.retrying);
    assert!(item.entry_intent.is_none());
    match checkpoint {
        CleanupCheckpoint::EntryRecorded => {
            assert_eq!(item.phase, "removing");
            assert!(item.removed_entries.contains(&PathBuf::new()));
            assert!(!record.exists());
        }
        CleanupCheckpoint::ManifestPinned => {
            assert_eq!(item.phase, "approved");
            assert!(!item.manifest.is_empty());
            assert!(record.exists());
        }
    }
    retry_pending_with(&root, &FilesystemRemoval, false);
    assert!(!record.exists());
    assert_eq!(
        cleanup_load(&root, &plan.id).unwrap().items[0].phase,
        "removed"
    );
    assert!(cleanup_complete(&update).unwrap());
    assert!(unfinished_plan(&root).unwrap().is_none());
    assert!(!previous_records(&root, &uuid::Uuid::new_v4().to_string())
        .unwrap()
        .is_empty());
}

#[test]
fn retry_recovers_after_recording_removed_entry_without_intent() {
    interrupted_checkpoint_recovers(CleanupCheckpoint::EntryRecorded);
}

#[test]
fn retry_recovers_after_repinning_manifest() {
    interrupted_checkpoint_recovers(CleanupCheckpoint::ManifestPinned);
}
impl Removal for InterruptedRemoval {
    fn remove(&self, path: &Path, _: bool) -> std::io::Result<()> {
        if self.remove_first {
            fs::remove_file(path)?;
        }
        panic!("injected interruption after persisted intent");
    }
}

#[test]
fn interrupted_retry_recovers_intent_before_following_update() {
    for remove_first in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        fs::write(
            root.join(".agentlaw-layout"),
            b"agentlaw-managed-layout-v1\n",
        )
        .unwrap();
        let record = root
            .join("state")
            .join(format!("install-completed-{}.json", uuid::Uuid::new_v4()));
        fs::create_dir_all(record.parent().unwrap()).unwrap();
        fs::write(&record, br#"{"targets":[]}"#).unwrap();
        let cleanup_id = uuid::Uuid::new_v4().to_string();
        let update = completed_update(&root, &cleanup_id);
        let mut plan = cleanup_for_records(
            &root,
            cleanup_id,
            vec![("old_install", record.clone(), "cleanup_deferred")],
        );
        add_completed_artifacts(&mut plan, &update);
        cleanup_save(&plan).unwrap();
        save(&update).unwrap();
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            retry_pending_with(&root, &InterruptedRemoval { remove_first }, false);
        }));
        assert!(interrupted.is_err());
        let mut saved = cleanup_load(&root, &plan.id).unwrap();
        assert_eq!(saved.items[0].phase, "removing");
        assert!(saved.items[0].retrying && saved.items[0].entry_intent.is_some());
        if remove_first {
            saved.items[0].retrying = false;
            cleanup_save(&saved).unwrap();
        }
        retry_pending_with(&root, &FilesystemRemoval, false);
        assert!(!record.exists());
        assert_eq!(
            cleanup_load(&root, &plan.id).unwrap().items[0].phase,
            "removed"
        );
        assert!(cleanup_complete(&update).unwrap());
        assert!(unfinished_plan(&root).unwrap().is_none());
        assert!(!previous_records(&root, &uuid::Uuid::new_v4().to_string())
            .unwrap()
            .is_empty());
    }
}

#[test]
fn later_update_does_not_reselect_paths_tracked_by_deferred_plan() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let a_cleanup_id = uuid::Uuid::new_v4().to_string();
    let a = completed_update(&root, &a_cleanup_id);
    let mut a_cleanup = cleanup_for_records(&root, a_cleanup_id, vec![]);
    add_completed_artifacts(&mut a_cleanup, &a);
    cleanup_save(&a_cleanup).unwrap();
    save(&a).unwrap();
    let a_path = plan_path(&root, &a.id).unwrap();
    let a_cleanup_path = cleanup_plan_path(&root, &a_cleanup.id).unwrap();
    let b_cleanup_id = uuid::Uuid::new_v4().to_string();
    let b = completed_update(&root, &b_cleanup_id);
    let mut b_cleanup = cleanup_for_records(
        &root,
        b_cleanup_id,
        vec![
            ("old_update", a_path.clone(), "cleanup_deferred"),
            ("old_cleanup", a_cleanup_path.clone(), "cleanup_deferred"),
        ],
    );
    add_completed_artifacts(&mut b_cleanup, &b);
    cleanup_save(&b_cleanup).unwrap();
    save(&b).unwrap();
    let c_cleanup_id = uuid::Uuid::new_v4().to_string();
    let c = completed_update(&root, &c_cleanup_id);
    let selected = previous_records(&root, &c.id).unwrap();
    assert!(!selected
        .iter()
        .any(|(_, path)| path == &a_path || path == &a_cleanup_path));
    assert!(a_path.exists() && a_cleanup_path.exists());
    let mut c_cleanup = cleanup_for_records(
        &root,
        c_cleanup_id,
        selected
            .into_iter()
            .map(|(kind, path)| (kind, path, "approved"))
            .collect(),
    );
    add_completed_artifacts(&mut c_cleanup, &c);
    cleanup_save(&c_cleanup).unwrap();
    save(&c).unwrap();
    retry_pending_with(&root, &FilesystemRemoval, false);
    assert!(!a_path.exists() && !a_cleanup_path.exists());
    assert!(cleanup_complete(&b).unwrap());
    assert!(cleanup_complete(&c).unwrap());
    assert!(unfinished_plan(&root).unwrap().is_none());
}

struct BusyInspection;
impl Removal for BusyInspection {
    fn remove(&self, _: &Path, _: bool) -> std::io::Result<()> {
        panic!("inspection must stop deletion")
    }
    fn inspect(&self, _: &Path, _: bool) -> Result<Vec<ManifestEntry>> {
        Err(error("cleanup_deferred", "An item is in use."))
    }
}

#[test]
fn sharing_violation_during_manifest_recheck_is_deferred() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let record = root
        .join("state")
        .join(format!("install-completed-{}.json", uuid::Uuid::new_v4()));
    fs::create_dir_all(record.parent().unwrap()).unwrap();
    fs::write(&record, br#"{"targets":[]}"#).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let current = uuid::Uuid::new_v4().to_string();
    let plan = cleanup_for_records(&root, id, vec![("old_install", record.clone(), "approved")]);
    cleanup_save(&plan).unwrap();
    execute_plan_with(
        &root,
        &plan.id,
        Some(&current),
        &BusyInspection,
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        cleanup_load(&root, &plan.id).unwrap().items[0].phase,
        "cleanup_deferred"
    );
    assert!(record.exists());
}

#[test]
fn manifest_pins_object_identity_across_same_volume_rename() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let holding = root.path().join("holding");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("owned.txt"), b"owned").unwrap();
    let pinned = complete_manifest(&source).unwrap();
    fs::rename(&source, &holding).unwrap();
    assert!(complete_manifest_at(&holding, &pinned).unwrap());
    fs::write(holding.join("owned.txt"), b"changed").unwrap();
    assert!(!complete_manifest_at(&holding, &pinned).unwrap());
}

struct Denied(RefCell<Vec<PathBuf>>);

impl Removal for Denied {
    fn remove(&self, path: &Path, _: bool) -> std::io::Result<()> {
        self.0.borrow_mut().push(path.to_path_buf());
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    }
}

#[test]
fn unconfirmed_removal_remains_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("state/cleanup-plans")).unwrap();
    let source = root.join("owned");
    fs::write(&source, b"owned").unwrap();
    let mut plan = CleanupPlan {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.to_path_buf(),
        marker_sha256: String::new(),
        permanent_internal_removal_approved: true,
        items: vec![CleanupItem {
            kind: "test".into(),
            source: source.clone(),
            holding: root.join("unused"),
            manifest: complete_manifest(&source).unwrap(),
            phase: "approved".into(),
            removed_entries: BTreeSet::new(),
            entry_intent: None,
            retrying: false,
        }],
    };
    let removal = Denied(RefCell::new(Vec::new()));
    let result = remove_item(&mut plan, 0, &removal).unwrap_err();
    assert_eq!(result.code, "cleanup_unknown");
    assert_eq!(&*removal.0.borrow(), &[source.clone()]);
    assert!(source.exists());
    assert_ne!(plan.items[0].phase, "removed");
}

struct RecordingRemoval {
    paths: RefCell<Vec<PathBuf>>,
    busy: RefCell<Option<PathBuf>>,
}

impl Removal for RecordingRemoval {
    fn remove(&self, path: &Path, directory: bool) -> std::io::Result<()> {
        self.paths.borrow_mut().push(path.to_path_buf());
        if self.busy.borrow().as_deref() == Some(path) {
            return Err(std::io::Error::from_raw_os_error(32));
        }
        FilesystemRemoval.remove(path, directory)
    }
}

fn write_fixture(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"fixture").unwrap();
    path
}

fn fixture_plan(root: &Path) -> CleanupPlan {
    let id = uuid::Uuid::new_v4().to_string();
    let paths = legacy::inventory(root).unwrap();
    let items = paths
        .into_iter()
        .enumerate()
        .map(|(index, source)| CleanupItem {
            kind: "legacy".into(),
            holding: root
                .join(".cleanup-holding")
                .join(&id)
                .join(index.to_string()),
            manifest: complete_manifest_with_hash(&source, false).unwrap(),
            source,
            phase: "approved".into(),
            removed_entries: BTreeSet::new(),
            entry_intent: None,
            retrying: false,
        })
        .collect();
    CleanupPlan {
        id,
        root: root.to_path_buf(),
        marker_sha256: digest(&root.join(".agentlaw-layout")).unwrap(),
        permanent_internal_removal_approved: true,
        items,
    }
}

#[test]
fn cleanup_inventory_deletes_only_named_owned_paths_and_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let sqlite = [
        "state/worker/broker.sqlite",
        "state/worker/indices/old/lexical.sqlite",
        "state/runtime/bootstrap.sqlite",
        "state/runtime/background-status.sqlite",
        "state/runtime/exact-maintenance.sqlite",
        "state/runtime/history-search/a-jobs.sqlite",
        "state/runtime/response-artifacts/response-registry.sqlite",
        "state/bindings/binding/bootstrap.sqlite",
        "state/artifacts/response-registry.sqlite",
    ];
    let mut expected = BTreeSet::new();
    for base in sqlite {
        for suffix in ["", "-wal", "-shm"] {
            expected.insert(write_fixture(&root, &format!("{base}{suffix}")));
        }
    }
    for path in [
        "state/worker/daemon.lock",
        "state/worker/launch.lock",
        "state/worker/model.lock",
        "state/worker/endpoint.json",
        "state/worker/component-recall.json",
        "state/worker/indices/old/writer.lock",
        "state/update-check.json",
        "state/update-check.lock",
    ] {
        expected.insert(write_fixture(&root, path));
    }
    let dirs = [
        "state/worker/source-ingress",
        "state/worker/indices/old/generations",
        "state/runtime/canonical/acquisitions",
        "state/runtime/canonical/history-cache",
    ];
    for dir in dirs {
        expected.insert(root.join(dir));
        expected.insert(write_fixture(&root, &format!("{dir}/owned")));
    }
    let protected = [
        "state/runtime/git/imports/live/acquisitions/file",
        "state/runtime/canonical/recovery/acquisitions/file",
        "state/worker/indices/old/vector.sqlite",
        "state/worker/indices/old/unrelated",
        "state/runtime/pending.sqlite",
        "state/machine.json",
        "models/model.bin",
    ]
    .map(|path| write_fixture(&root, path));
    let plan = fixture_plan(&root);
    assert!(plan
        .items
        .iter()
        .flat_map(|item| &item.manifest)
        .all(|entry| entry.sha256.is_none()));
    cleanup_save(&plan).unwrap();
    let recorder = RecordingRemoval {
        paths: RefCell::new(Vec::new()),
        busy: RefCell::new(None),
    };
    execute_plan_with(&root, &plan.id, None, &recorder, false, true).unwrap();
    let observed: BTreeSet<_> = recorder.paths.borrow().iter().cloned().collect();
    assert_eq!(observed, expected);
    assert!(protected.iter().all(|path| path.exists()));
    assert!(root.join("state/worker/indices/old").is_dir());
    assert!(cleanup_load(&root, &plan.id)
        .unwrap()
        .items
        .iter()
        .all(|item| item.phase == "removed"));
}

#[test]
fn frontend_start_and_update_preserve_legacy_inventory_until_transition() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let broker = write_fixture(&root, "state/worker/broker.sqlite");
    let endpoint = write_fixture(&root, "state/worker/endpoint.json");
    let ingress = write_fixture(&root, "state/worker/source-ingress/current");
    let mut plan = fixture_plan(&root);
    for item in &mut plan.items {
        item.phase = "cleanup_deferred".into();
    }
    cleanup_save(&plan).unwrap();
    let update = Plan {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.clone(),
        tag: "v1.0.0".into(),
        archive: target_archive().unwrap().to_owned(),
        archive_digest: "0".repeat(64),
        bin_before: String::new(),
        bin_before_hashes: BTreeMap::new(),
        registrations: Vec::new(),
        phase: "completed".into(),
        bundle_hashes: None,
        launcher_digest: None,
        previous_bundle: None,
        activation_verified: true,
        recovery_obligations_closed: true,
        cleanup_plan_id: Some(plan.id.clone()),
        cleanup_completed: true,
    };
    save(&update).unwrap();
    super::super::check_frontend_start(&root.join("state")).unwrap();
    execute_plan_locked(&root, &plan.id, Some(&update.id)).unwrap();
    assert!(broker.exists() && endpoint.exists() && ingress.exists());
    assert!(cleanup_load(&root, &plan.id)
        .unwrap()
        .items
        .iter()
        .all(|item| item.phase == "cleanup_deferred"));
}

#[test]
fn next_update_removes_completed_records_and_preserves_current_and_pending() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("state/update-plans")).unwrap();
    let previous_cleanup = fixture_plan(&root);
    cleanup_save(&previous_cleanup).unwrap();
    let old = Plan {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.clone(),
        tag: "v1.0.0".into(),
        archive: target_archive().unwrap().to_owned(),
        archive_digest: "0".repeat(64),
        bin_before: String::new(),
        bin_before_hashes: BTreeMap::new(),
        registrations: Vec::new(),
        phase: "completed".into(),
        bundle_hashes: None,
        launcher_digest: None,
        previous_bundle: None,
        activation_verified: true,
        recovery_obligations_closed: true,
        cleanup_plan_id: Some(previous_cleanup.id.clone()),
        cleanup_completed: true,
    };
    save(&old).unwrap();
    let current_id = uuid::Uuid::new_v4().to_string();
    let mut current = old.clone();
    current.id = current_id.clone();
    current.phase = "finalizing".into();
    current.cleanup_plan_id = None;
    current.cleanup_completed = false;
    save(&current).unwrap();
    let install = root
        .join("state")
        .join(format!("install-completed-{}.json", uuid::Uuid::new_v4()));
    fs::write(&install, br#"{"targets":[]}"#).unwrap();
    let current_install = root
        .join("state")
        .join(format!("install-completed-{}.json", uuid::Uuid::new_v4()));
    fs::write(
        &current_install,
        serde_json::to_vec(&json!({"targets":[],"update_plan_id":current_id})).unwrap(),
    )
    .unwrap();
    let pending = write_fixture(&root, "state/runtime/pending.sqlite");
    let paths = previous_records(&root, &current_id).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let items = paths
        .into_iter()
        .enumerate()
        .map(|(index, (kind, source))| CleanupItem {
            kind: kind.into(),
            holding: root
                .join(".cleanup-holding")
                .join(&id)
                .join(index.to_string()),
            manifest: complete_manifest(&source).unwrap(),
            source,
            phase: "approved".into(),
            removed_entries: BTreeSet::new(),
            entry_intent: None,
            retrying: false,
        })
        .collect();
    let plan = CleanupPlan {
        id,
        root: root.clone(),
        marker_sha256: digest(&root.join(".agentlaw-layout")).unwrap(),
        permanent_internal_removal_approved: true,
        items,
    };
    cleanup_save(&plan).unwrap();
    let removal = RecordingRemoval {
        paths: RefCell::new(Vec::new()),
        busy: RefCell::new(None),
    };
    #[cfg(windows)]
    {
        *removal.busy.borrow_mut() = Some(plan_path(&root, &old.id).unwrap());
        execute_plan_with(&root, &plan.id, Some(&current_id), &removal, false, true).unwrap();
        let deferred = cleanup_load(&root, &plan.id).unwrap();
        assert!(deferred
            .items
            .iter()
            .filter(|item| item.kind == "old_update" || item.kind == "old_cleanup")
            .all(|item| item.phase == "cleanup_deferred"));
        assert!(plan_path(&root, &old.id).unwrap().exists());
        assert!(cleanup_plan_path(&root, &previous_cleanup.id)
            .unwrap()
            .exists());
        *removal.busy.borrow_mut() = None;
    }
    execute_plan_with(&root, &plan.id, Some(&current_id), &removal, false, true).unwrap();
    assert!(!plan_path(&root, &old.id).unwrap().exists());
    assert!(!cleanup_plan_path(&root, &previous_cleanup.id)
        .unwrap()
        .exists());
    assert!(!install.exists());
    assert!(current_install.exists());
    assert!(plan_path(&root, &current_id).unwrap().exists());
    assert!(pending.exists());
    assert!(cleanup_plan_path(&root, &plan.id).unwrap().exists());
    assert!(cleanup_load(&root, &plan.id)
        .unwrap()
        .items
        .iter()
        .all(|item| item.phase == "removed"));
}

#[cfg(windows)]
#[test]
fn sharing_violation_is_retried_on_frontend_start_without_new_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::write(
        root.join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    let busy = write_fixture(&root, "state/runtime/bootstrap.sqlite");
    let plan = fixture_plan(&root);
    cleanup_save(&plan).unwrap();
    let recorder = RecordingRemoval {
        paths: RefCell::new(Vec::new()),
        busy: RefCell::new(Some(busy.clone())),
    };
    execute_plan_with(&root, &plan.id, None, &recorder, false, true).unwrap();
    assert_eq!(
        cleanup_load(&root, &plan.id).unwrap().items[0].phase,
        "cleanup_deferred"
    );
    assert!(busy.exists());
    let update = Plan {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.clone(),
        tag: "v1.0.0".into(),
        archive: target_archive().unwrap().to_owned(),
        archive_digest: "0".repeat(64),
        bin_before: String::new(),
        bin_before_hashes: BTreeMap::new(),
        registrations: Vec::new(),
        phase: "completed".into(),
        bundle_hashes: None,
        launcher_digest: None,
        previous_bundle: None,
        activation_verified: true,
        recovery_obligations_closed: true,
        cleanup_plan_id: Some(plan.id.clone()),
        cleanup_completed: true,
    };
    save(&update).unwrap();
    *recorder.busy.borrow_mut() = None;
    retry_pending_with(&root, &recorder, true);
    assert!(!busy.exists());
    assert_eq!(
        cleanup_load(&root, &plan.id).unwrap().items[0].phase,
        "removed"
    );
    assert_eq!(
        recorder
            .paths
            .borrow()
            .iter()
            .filter(|p| *p == &busy)
            .count(),
        2
    );
    assert_eq!(
        legacy::children(&root.join("state/cleanup-plans"))
            .unwrap()
            .len(),
        1
    );
}
