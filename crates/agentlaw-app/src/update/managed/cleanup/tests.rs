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
        "state/worker/model.lock",
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

#[cfg(windows)]
#[test]
fn runtime_start_retries_recorded_operating_file_deletions_without_fresh_inventory() {
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
    let report = execute_plan_with(&root, &plan.id, None, &recorder, false, true).unwrap();
    assert!(
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["path"] == json!(busy) && item["phase"] == "cleanup_deferred"),
        "{report}"
    );
    let update = completed_update(&root, &plan.id);
    save(&update).unwrap();
    let added_later = write_fixture(&root, "state/worker/component-added-after-update.json");
    *recorder.busy.borrow_mut() = None;
    super::super::check_frontend_start(&root.join("state")).unwrap();
    assert!(
        !busy.exists(),
        "runtime start must retry the recorded sharing violation"
    );
    assert_eq!(
        fs::read(&added_later).unwrap(),
        b"fixture",
        "startup must not inventory newly matching paths"
    );
    let saved = cleanup_load(&root, &plan.id).unwrap();
    assert!(saved
        .items
        .iter()
        .any(|item| item.source == busy && item.phase == "removed"));
    assert!(!saved.items.iter().any(|item| item.source == added_later));
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

fn managed_cleanup_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".agentlaw-layout"),
        b"agentlaw-managed-layout-v1\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("state")).unwrap();
    dir
}

fn verified_finalizing_update(root: &Path) -> Plan {
    let mut update = completed_update(root, "not-yet-inventoried");
    update.phase = "finalizing".into();
    update.cleanup_plan_id = None;
    update.cleanup_completed = false;
    let bytes = b"verified candidate fixture";
    let hash = format!("{:x}", Sha256::digest(bytes));
    update.bundle_hashes = Some(BTreeMap::from([(binary_name().into(), hash)]));
    let candidate = candidate_executable(&update).unwrap();
    fs::create_dir_all(candidate.parent().unwrap()).unwrap();
    fs::write(candidate, bytes).unwrap();
    // A real finalizing update still owns its previous bundle and staged assets.
    let previous = previous_bundle(&update);
    fs::create_dir_all(&previous).unwrap();
    for name in [binary_name(), worker_binary_name(), "LICENSE.agentlaw"] {
        let path = previous.join(name);
        fs::write(&path, b"previous bundle fixture").unwrap();
        update
            .bin_before_hashes
            .insert(name.into(), digest(&path).unwrap());
    }
    update.bin_before = update.bin_before_hashes[binary_name()].clone();
    let staged = stage(root, &update.id);
    fs::create_dir_all(&staged).unwrap();
    let archive = staged.join(&update.archive);
    fs::write(&archive, b"pinned release archive fixture").unwrap();
    update.archive_digest = digest(&archive).unwrap();
    let launcher = staged.join("launcher.asset");
    fs::write(&launcher, b"pinned launcher fixture").unwrap();
    update.launcher_digest = Some(digest(&launcher).unwrap());
    save(&update).unwrap();
    update
}

#[test]
fn verified_update_removes_previous_operating_files_and_preserves_authoritative_and_live_data() {
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (_, current_index) = real_binding_index_fixture(&root);
    let mut obsolete = Vec::new();
    for binding in ["state/runtime", "state/bindings/binding"] {
        for name in [
            "bootstrap.sqlite",
            "background-status.sqlite",
            "exact-maintenance.sqlite",
            "history-search/a-jobs.sqlite",
            "response-artifacts/response-registry.sqlite",
        ] {
            for suffix in ["", "-wal", "-shm"] {
                obsolete.push(write_fixture(&root, &format!("{binding}/{name}{suffix}")));
            }
        }
        for name in ["canonical/acquisitions", "canonical/history-cache"] {
            let directory = root.join(binding).join(name);
            write_fixture(&root, &format!("{binding}/{name}/owned"));
            obsolete.push(directory);
        }
    }
    for base in [
        "state/worker/broker.sqlite",
        "state/worker/indices/legacy/lexical.sqlite",
        "state/artifacts/response-registry.sqlite",
    ] {
        for suffix in ["", "-wal", "-shm"] {
            obsolete.push(write_fixture(&root, &format!("{base}{suffix}")));
        }
    }
    for path in [
        "state/worker/daemon.lock",
        "state/worker/launch.lock",
        "state/worker/endpoint.json",
        "state/worker/indices/legacy/writer.lock",
        "state/worker/component-recall.json",
        "state/update-check.json",
        "state/update-check.lock",
    ] {
        obsolete.push(write_fixture(&root, path));
    }
    for path in [
        "state/worker/source-ingress",
        "state/worker/indices/legacy/generations",
    ] {
        write_fixture(&root, &format!("{path}/owned"));
        obsolete.push(root.join(path));
    }
    let mut protected = vec![
        "state/worker/model.lock",
        "state/worker/execution-selection.json",
        "source/format.md",
        "state/runtime/control.sqlite",
        "state/runtime/pending.sqlite",
        "state/runtime/canonical/recovery/acquisitions/record",
        "state/runtime/git/imports/live/acquisitions/record",
        "state/machine.json",
        "models/model.onnx",
    ]
    .into_iter()
    .map(|relative| {
        let path = root.join(relative);
        if path.exists() {
            path
        } else {
            write_fixture(&root, relative)
        }
    })
    .collect::<Vec<_>>();
    protected.push(current_index.join("vector.sqlite"));
    // A model liveness lock is an empty file, unlike obsolete status payloads.
    fs::write(&protected[0], b"").unwrap();
    let before: Vec<_> = protected
        .iter()
        .map(|p| (p.clone(), fs::read(p).unwrap()))
        .collect();
    let source_before = cleanup_fixture_tree(&root.join("source"));
    let recovery_before = cleanup_fixture_tree(&root.join("state/runtime/canonical/recovery"));
    let mut update = verified_finalizing_update(&root);
    let mut unverified = update.clone();
    unverified.activation_verified = false;
    assert_eq!(
        cleanup_update_plan(&mut unverified).unwrap_err().code,
        "cleanup_plan_state"
    );
    assert!(
        obsolete.iter().all(|p| p.exists()),
        "cleanup ran before candidate verification"
    );
    cleanup_update_plan(&mut update).unwrap();
    assert_eq!(
        update.phase, "finalizing",
        "cleanup must happen before update completion"
    );
    for (path, bytes) in before {
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes,
            "protected data changed: {}",
            path.display()
        );
    }
    assert_eq!(
        cleanup_fixture_tree(&root.join("source")),
        source_before,
        "published source bytes changed"
    );
    assert_eq!(
        cleanup_fixture_tree(&root.join("state/runtime/canonical/recovery")),
        recovery_before,
        "publication recovery bytes changed"
    );
    let remaining: Vec<_> = obsolete.iter().filter(|p| p.exists()).collect();
    assert!(
        remaining.is_empty(),
        "verified update left previous-version operating files: {remaining:?}"
    );
    assert!(cleanup_complete(&update).unwrap());
}

fn real_binding_index_fixture(root: &Path) -> (PathBuf, PathBuf) {
    use agentlaw_runtime::{
        ports::HealthSink,
        semantic::{SemanticConfig, SemanticIndexer, INDEX_CONFIG_VERSION},
    };
    use agentlaw_worker::supervisor::{IndexWorkPort, ModelRequestIds};
    let source = root.join("source");
    let local = root.join("state/runtime");
    let store = agentlaw_storage::Store::open(&source, local.join("canonical")).unwrap();
    let machine =
        agentlaw_runtime::installation::machine::load_or_create(&root.join("state")).unwrap();
    let identity = agentlaw_flows::store::StoreIdentity {
        root: source.clone(),
        local: local.clone(),
        user_id: "fixture-user".into(),
        machine_id: machine.machine_id,
        binding_id: uuid::Uuid::new_v4().to_string(),
        history_limit: 8192,
        response_limit: 65536,
        config_file: None,
    };
    let mut control = agentlaw_flows::control::ControlWriter::open(&local).unwrap();
    control.bind_identity(&identity).unwrap();
    control.finish_binding_initialization(&identity).unwrap();
    fs::write(
        root.join("state/config.json"),
        serde_json::to_vec(&config::Config {
            memory_store_path: source,
            user_id: identity.user_id.clone(),
            runtime_path: None,
            history_response_limit_bytes: 8192,
            response_limit_bytes: 65536,
        })
        .unwrap(),
    )
    .unwrap();
    drop(control);
    let current_binding = identity.binding_id.clone();
    let ids = ModelRequestIds::new();
    let health = HealthSink::default();
    let preparation_indexer = SemanticIndexer::start(
        SemanticConfig::new(
            root.join("state/worker/indices"),
            INDEX_CONFIG_VERSION.into(),
        ),
        ids,
        health.clone(),
    )
    .unwrap();
    let service = agentlaw_runtime::store::service::StoreService::open(
        identity,
        agentlaw_runtime::store::service::StoreDeps {
            health,
            semantic: std::sync::Arc::new(agentlaw_runtime::semantic::SemanticSearch::new(
                preparation_indexer.clone(),
                agentlaw_runtime::model::ModelHandle::new(),
            )),
            indexer: preparation_indexer.clone(),
            projection: agentlaw_runtime::projection::ProjectionConfig::new(),
            coordination: store.coordination_root().to_path_buf(),
            state: root.join("state"),
        },
        &agentlaw_contracts::RequestControl::default(),
    )
    .unwrap();
    let request = || {
        serde_json::from_value(json!({"memories":[{
        "operation":"create","what_to_remember":"Retained source and pending proposals survive operating-file cleanup.",
        "evidence":"Confirmed fixture observation","applies_to":["user"]}]})).unwrap()
    };
    let saved = service
        .command(
            agentlaw_runtime::protocol::CommandRequest::Remember(request()),
            &agentlaw_contracts::RequestControl::default(),
        )
        .unwrap();
    assert_eq!(saved["status"], "remembered");
    let pending = service
        .command(
            agentlaw_runtime::protocol::CommandRequest::Remember(request()),
            &agentlaw_contracts::RequestControl::default(),
        )
        .unwrap();
    assert_eq!(pending["status"], "resolution_required");
    let inspected = service.query(agentlaw_runtime::protocol::QueryRequest::PendingInspect(
        serde_json::from_value(json!({"pending_action":"inspect","pending_batch_ref":pending["pending_batch_ref"]})).unwrap()),
        &agentlaw_contracts::RequestControl::default()).unwrap();
    assert_eq!(inspected["status"], "pending");
    service.close();
    assert!(preparation_indexer.shutdown(Duration::from_secs(10)));
    drop(service);
    let indices = root.join("state/worker/indices");
    let mut known = BTreeSet::new();
    let mut paths = Vec::new();
    // A prior binding no longer exists in any retained control store; both files
    // carry real vector-file identities, not directory-name guesses.
    for binding in [uuid::Uuid::new_v4().to_string(), current_binding] {
        let indexer = SemanticIndexer::start(
            SemanticConfig::new(indices.clone(), INDEX_CONFIG_VERSION.into()),
            ModelRequestIds::new(),
            HealthSink::default(),
        )
        .unwrap();
        indexer
            .open_binding(&binding, store.owned_published_reader())
            .unwrap();
        indexer.model_ready("fixture-model");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !indexer
            .progress(&binding)
            .is_some_and(|progress| progress.complete)
        {
            if let Some(work) = indexer.next_work("fixture-model") {
                let mut vector = vec![0.0; 256];
                vector[0] = 1.0;
                indexer.finish(
                    work.id,
                    Ok(agentlaw_worker::supervisor::IndexResult::Vector(vector)),
                );
            }
            assert!(
                Instant::now() < deadline,
                "semantic fixture index did not complete: {:?}",
                indexer.progress(&binding)
            );
            std::thread::yield_now();
        }
        assert!(indexer.shutdown(Duration::from_secs(10)));
        let found = fs::read_dir(&indices)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| !known.contains(path))
            .unwrap();
        assert!(found.join("vector.sqlite").is_file());
        known.insert(found.clone());
        paths.push(found);
    }
    (paths.remove(0), paths.remove(0))
}

#[test]
fn obsolete_owned_index_directory_is_removed_while_current_and_unrecognized_directories_survive() {
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (obsolete, current) = real_binding_index_fixture(&root);
    let unknown = write_fixture(&root, "state/worker/indices/unrecognized/vector.sqlite");
    let note = write_fixture(&root, "state/worker/indices/unrecognized/notes.txt");
    let current_bytes = fs::read(current.join("vector.sqlite")).unwrap();
    let plan = fixture_plan(&root);
    cleanup_save(&plan).unwrap();
    let recorder = RecordingRemoval {
        paths: RefCell::new(Vec::new()),
        busy: RefCell::new(None),
    };
    execute_plan_with(&root, &plan.id, None, &recorder, false, true).unwrap();
    assert_eq!(
        fs::read(current.join("vector.sqlite")).unwrap(),
        current_bytes
    );
    assert_eq!(fs::read(unknown).unwrap(), b"fixture");
    assert_eq!(fs::read(note).unwrap(), b"fixture");
    assert!(
        !obsolete.exists(),
        "a genuine index for a retired binding was not removed: {}",
        obsolete.display()
    );
}

#[test]
fn model_liveness_lock_holder_fixture() {
    let Some(path) = std::env::var_os("AGENTLAW_MODEL_LOCK_FIXTURE") else {
        return;
    };
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    lock.lock_exclusive().unwrap();
    println!("model-lock-held");
    std::io::stdout().flush().unwrap();
    let mut release = String::new();
    std::io::stdin().read_line(&mut release).unwrap();
    assert_eq!(release.trim(), "exit");
}

#[test]
fn cleanup_preserves_the_model_liveness_lock_while_its_owned_child_runs_and_after_retry() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let worker = root.join("state/worker");
    fs::create_dir_all(&worker).unwrap();
    let model_lock = worker.join("model.lock");
    fs::write(&model_lock, b"").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "update::managed::cleanup::tests::model_liveness_lock_holder_fixture",
            "--nocapture",
        ])
        .env("AGENTLAW_MODEL_LOCK_FIXTURE", &model_lock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = ChildGuard(command.spawn().unwrap());
    let output = child.0.stdout.take().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            if line.unwrap().contains("model-lock-held") {
                let _ = ready_tx.send(());
            }
        }
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let plan = fixture_plan(&root);
    cleanup_save(&plan).unwrap();
    let recorder = RecordingRemoval {
        paths: RefCell::new(Vec::new()),
        busy: RefCell::new(None),
    };
    execute_plan_with(&root, &plan.id, None, &recorder, false, true).unwrap();
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "cleanup terminated the model child"
    );
    assert_eq!(fs::metadata(&model_lock).unwrap().len(), 0);
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&model_lock)
        .unwrap();
    assert!(
        probe.try_lock_exclusive().is_err(),
        "model liveness lock was replaced"
    );
    drop(probe);
    writeln!(child.0.stdin.take().unwrap(), "exit").unwrap();
    assert!(child.0.wait().unwrap().success());
    execute_plan_with(&root, &plan.id, None, &recorder, false, true).unwrap();
    assert!(
        model_lock.exists(),
        "cleanup retry removed the live model liveness-lock path"
    );
    assert!(
        !recorder.paths.borrow().contains(&model_lock),
        "model.lock was submitted for deletion"
    );
}

fn cleanup_fixture_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, current: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn legacy_daemon_index_fixture(root: &Path) -> PathBuf {
    let _ = real_binding_index_fixture(root);
    let local = root.join("state/runtime");
    let binding = agentlaw_flows::control::ControlReader::open(&local)
        .unwrap()
        .binding_id()
        .unwrap();
    let store =
        agentlaw_storage::Store::open(root.join("source"), local.join("canonical")).unwrap();
    let epoch = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .epoch
        .clone();
    let model = format!("{:x}", Sha256::digest(b"retired daemon model"));
    let config = format!("{:x}", Sha256::digest(b"retired daemon configuration"));
    // The retired daemon hashed a JSON tuple, not the current length-prefixed identity.
    let key = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                binding.as_str(),
                model.as_str(),
                config.as_str(),
                epoch.as_str(),
            ))
            .unwrap()
        )
    );
    let directory = root.join("state/worker/indices").join(key);
    fs::create_dir_all(&directory).unwrap();
    let db = rusqlite::Connection::open(directory.join("vector.sqlite")).unwrap();
    // Frozen schema from the old ExactVectorIndex, including its ANN tables.
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
         CREATE TABLE vector_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
             generation INTEGER NOT NULL,commit_id INTEGER NOT NULL,ack INTEGER NOT NULL,
             model TEXT NOT NULL,config TEXT NOT NULL,dimension INTEGER NOT NULL);
         CREATE TABLE vectors(memory TEXT NOT NULL,change_id TEXT NOT NULL,section TEXT NOT NULL,
             scope TEXT NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(memory,change_id,section)) WITHOUT ROWID;
         CREATE TABLE vector_buckets(table_id INTEGER NOT NULL,bucket INTEGER NOT NULL,scope TEXT NOT NULL,
             memory TEXT NOT NULL,change_id TEXT NOT NULL,section TEXT NOT NULL,
             PRIMARY KEY(table_id,bucket,scope,memory,change_id,section)) WITHOUT ROWID;
         CREATE INDEX bucket_memory ON vector_buckets(memory);
         CREATE TABLE ann_profile(singleton INTEGER PRIMARY KEY,version INTEGER);
         INSERT INTO ann_profile VALUES(1,1);"
    ).unwrap();
    db.execute(
        "INSERT INTO vector_meta VALUES(1,1,0,0,?1,?2,256)",
        rusqlite::params![model, config],
    )
    .unwrap();
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop(db);
    directory
}

#[test]
fn verified_update_retires_the_daemon_vector_format_and_preserves_unproven_ownership() {
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let legacy = legacy_daemon_index_fixture(&root);
    let unknown = root.join("state/worker/indices").join("0".repeat(64));
    assert_ne!(legacy, unknown);
    fs::create_dir_all(&unknown).unwrap();
    fs::copy(legacy.join("vector.sqlite"), unknown.join("vector.sqlite")).unwrap();
    fs::write(
        unknown.join("user-notes.txt"),
        b"not established as product-owned",
    )
    .unwrap();
    let unknown_before = cleanup_fixture_tree(&unknown);
    let mut update = verified_finalizing_update(&root);
    cleanup_update_plan(&mut update).unwrap();
    assert_eq!(
        cleanup_fixture_tree(&unknown),
        unknown_before,
        "unproven index ownership must be preserved"
    );
    assert!(
        !legacy.exists(),
        "the real retired daemon format survived a verified update: {}",
        legacy.display()
    );
}

#[cfg(windows)]
fn partially_removed_index_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf, CleanupPlan, Plan) {
    use std::os::windows::fs::OpenOptionsExt;
    let (obsolete, current) = real_binding_index_fixture(root);
    let first_id = "11111111-1111-4111-8111-111111111111";
    let second_id = "22222222-2222-4222-8222-222222222222";
    let first = obsolete.join(format!("vector-{first_id}.sqlite"));
    let second = obsolete.join(format!("vector-{second_id}.sqlite"));
    for (path, generation) in [(&first, first_id), (&second, second_id)] {
        fs::copy(obsolete.join("vector.sqlite"), path).unwrap();
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute(
            "UPDATE binding_vector_meta SET generation=?1 WHERE singleton=1",
            [generation],
        )
        .unwrap();
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    }
    let wal = obsolete.join(format!("vector-{first_id}.sqlite-wal"));
    fs::write(&wal, []).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(3)
        .open(&wal)
        .unwrap();
    let plan = fixture_plan(root);
    assert!(
        plan.items.iter().any(|item| item.source == obsolete),
        "fixture must begin as an owned retired directory"
    );
    cleanup_save(&plan).unwrap();
    let recorder = RecordingRemoval {
        paths: RefCell::new(Vec::new()),
        busy: RefCell::new(None),
    };
    execute_plan_with(root, &plan.id, None, &recorder, false, true).unwrap();
    assert!(
        !first.exists(),
        "first pass must really remove the database before its held WAL"
    );
    assert!(wal.exists());
    assert!(
        second.exists(),
        "the other generation must survive the sharing violation"
    );
    let mut saved = cleanup_load(root, &plan.id).unwrap();
    assert!(saved
        .items
        .iter()
        .any(|item| item.source == obsolete && item.phase == "cleanup_deferred"));
    let update = completed_update(root, &plan.id);
    add_completed_artifacts(&mut saved, &update);
    cleanup_save(&saved).unwrap();
    save(&update).unwrap();
    drop(held);
    (obsolete, wal, current, saved, update)
}

#[cfg(windows)]
#[test]
fn partial_index_deletion_retry_removes_orphan_sidecars_and_finishes_the_recorded_item() {
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (obsolete, wal, current, plan, _) = partially_removed_index_fixture(&root);
    let current_before = cleanup_fixture_tree(&current);
    retry_pending(&root);
    assert_eq!(cleanup_fixture_tree(&current), current_before);
    let saved = cleanup_load(&root, &plan.id).unwrap();
    let item = saved
        .items
        .iter()
        .find(|item| item.source == obsolete)
        .unwrap();
    assert!(
        !wal.exists() && !obsolete.exists(),
        "retry stranded an orphan sidecar or retired directory; phase={}",
        item.phase
    );
    assert_eq!(
        item.phase, "removed",
        "retry must finish rather than block a partially removed owned directory"
    );
}

#[cfg(windows)]
#[test]
fn partial_index_retry_preserves_a_surviving_database_restored_from_another_binding() {
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (obsolete, _, current, plan, _) = partially_removed_index_fixture(&root);
    let surviving = obsolete.join("vector-22222222-2222-4222-8222-222222222222.sqlite");
    // Restoring a backup from another binding while cleanup was deferred changes ownership.
    fs::copy(current.join("vector.sqlite"), &surviving).unwrap();
    let restored_bytes = fs::read(&surviving).unwrap();
    retry_pending(&root);
    assert_eq!(
        fs::read(&surviving).unwrap(),
        restored_bytes,
        "retry removed a surviving database whose identity no longer belongs to this directory"
    );
    let saved = cleanup_load(&root, &plan.id).unwrap();
    assert_ne!(
        saved
            .items
            .iter()
            .find(|item| item.source == obsolete)
            .unwrap()
            .phase,
        "removed"
    );
}

#[cfg(windows)]
#[test]
fn partial_index_retry_preserves_unrecognized_content_added_while_deletion_was_deferred() {
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (obsolete, _, _, _, _) = partially_removed_index_fixture(&root);
    let note = obsolete.join("user-added-after-update.txt");
    fs::write(&note, b"new unrecognized content").unwrap();
    retry_pending(&root);
    assert_eq!(fs::read(note).unwrap(), b"new unrecognized content");
}

#[cfg(windows)]
fn recorded_startup_retry_fixture(root: &Path) -> (PathBuf, CleanupPlan) {
    let path = write_fixture(root, "state/worker/component-recorded-retry.json");
    let plan = fixture_plan(root);
    cleanup_save(&plan).unwrap();
    let recorder = RecordingRemoval {
        paths: RefCell::new(Vec::new()),
        busy: RefCell::new(Some(path.clone())),
    };
    execute_plan_with(root, &plan.id, None, &recorder, false, true).unwrap();
    let mut saved = cleanup_load(root, &plan.id).unwrap();
    assert!(saved
        .items
        .iter()
        .any(|item| item.source == path && item.phase == "cleanup_deferred"));
    let update = completed_update(root, &plan.id);
    add_completed_artifacts(&mut saved, &update);
    cleanup_save(&saved).unwrap();
    save(&update).unwrap();
    (path, saved)
}

#[cfg(windows)]
#[test]
fn startup_retry_excludes_runtime_admission_until_recorded_removal_finishes() {
    struct PausedRemoval {
        entered: std::sync::mpsc::SyncSender<()>,
        proceed: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }
    impl Removal for PausedRemoval {
        fn remove(&self, path: &Path, directory: bool) -> std::io::Result<()> {
            if let Some(proceed) = self.proceed.lock().unwrap().take() {
                self.entered.send(()).unwrap();
                proceed.recv_timeout(Duration::from_secs(30)).unwrap();
            }
            FilesystemRemoval.remove(path, directory)
        }
    }
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (path, _) = recorded_startup_retry_fixture(&root);
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let retry_root = root.clone();
    let retry = std::thread::spawn(move || {
        retry_pending_with(
            &retry_root,
            &PausedRemoval {
                entered: entered_tx,
                proceed: std::sync::Mutex::new(Some(release_rx)),
            },
            true,
        );
    });
    entered_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(
        path.exists(),
        "removal gate must pause before the file is removed"
    );
    let owner =
        agentlaw_runtime::installation::owner::OwnerLock::try_acquire(&root.join("state")).unwrap();
    let start_allowed = owner.is_some()
        && agentlaw_runtime::installation::gate::check_start(&root.join("state")).is_ok();
    drop(owner);
    release_tx.send(()).unwrap();
    retry.join().unwrap();
    assert!(
        !start_allowed,
        "runtime owner/start gate admitted a runtime while retry removal was paused"
    );
    assert!(
        !path.exists(),
        "recorded removal did not finish after its gate was released"
    );
}

#[cfg(windows)]
#[test]
fn startup_retry_defers_for_a_live_runtime_owner_and_removes_the_file_after_owner_exit() {
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (path, plan) = recorded_startup_retry_fixture(&root);
    let owner = agentlaw_runtime::installation::owner::OwnerLock::try_acquire(&root.join("state"))
        .unwrap()
        .unwrap();
    let before = fs::read(&path).unwrap();
    let recorder = RecordingRemoval {
        paths: RefCell::new(Vec::new()),
        busy: RefCell::new(None),
    };
    retry_pending_with(&root, &recorder, true);
    let still_present = fs::read(&path).ok();
    let attempted_while_live = recorder.paths.borrow().contains(&path);
    drop(owner);
    // Always release the owner before reporting a failure, so cleanup cannot strand a test lock.
    assert_eq!(
        still_present,
        Some(before),
        "retry deleted data while a live runtime owned the installation"
    );
    assert!(
        !attempted_while_live,
        "retry attempted removal without lifecycle exclusion"
    );
    retry_pending(&root);
    assert!(
        !path.exists(),
        "retry was not attempted after the runtime owner exited"
    );
    assert!(cleanup_load(&root, &plan.id)
        .unwrap()
        .items
        .iter()
        .any(|item| item.source == path && item.phase == "removed"));
}

#[cfg(windows)]
#[path = "../../../../../../tests/support/runtime_cleanup.rs"]
mod runtime_cleanup;

#[cfg(windows)]
fn completed_retry_crash_installation(root: &Path) -> (PathBuf, CleanupPlan, Plan) {
    let _ = real_binding_index_fixture(root);
    let (path, cleanup) = recorded_startup_retry_fixture(root);
    let update_path = legacy::children(&root.join("state/update-plans"))
        .unwrap()
        .into_iter()
        .find(|path| {
            let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value["cleanup_plan_id"] == cleanup.id
        })
        .unwrap();
    let id = record_id(&update_path).unwrap();
    let mut update = load(root, id).unwrap();
    // Match a completed managed installation, including its verified native candidate.
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::copy(std::env::current_exe().unwrap(), bin.join(binary_name())).unwrap();
    fs::write(
        bin.join(worker_binary_name()),
        b"compatibility placeholder fixture",
    )
    .unwrap();
    fs::write(bin.join("LICENSE.agentlaw"), b"fixture license").unwrap();
    update.bundle_hashes = Some(bundle_hashes(&bin).unwrap());
    let candidate = candidate_executable(&update).unwrap();
    fs::create_dir_all(candidate.parent().unwrap()).unwrap();
    fs::copy(bin.join(binary_name()), candidate).unwrap();
    save(&update).unwrap();
    assert_eq!(update.phase, "completed");
    (path, cleanup, update)
}

#[cfg(windows)]
#[test]
fn startup_cleanup_retry_crash_fixture() {
    let Some(root) = std::env::var_os("AGENTLAW_STARTUP_RETRY_CRASH_ROOT") else {
        return;
    };
    struct PausedBeforeRemoval {
        root: PathBuf,
    }
    impl Removal for PausedBeforeRemoval {
        fn remove(&self, _: &Path, _: bool) -> std::io::Result<()> {
            let state = self.root.join("state");
            let excluded = agentlaw_runtime::installation::owner::owner_alive(&state).unwrap()
                || agentlaw_runtime::installation::gate::check_start(&state).is_err();
            assert!(
                excluded,
                "retry must hold lifecycle exclusion before the crash point"
            );
            fs::write(
                state.join("fixture-startup-retry-held"),
                b"held before removal",
            )
            .unwrap();
            println!("startup-cleanup-retry-held");
            std::io::stdout().flush().unwrap();
            let mut release = String::new();
            std::io::stdin().read_line(&mut release).unwrap();
            panic!("parent must kill this child before any gate destructor runs");
        }
    }
    let root = PathBuf::from(root);
    retry_pending_with(&root, &PausedBeforeRemoval { root: root.clone() }, true);
    panic!("recorded retry did not reach the crash hook");
}

#[cfg(windows)]
fn normal_frontend_recall_after_retry(root: &Path) -> Result<Value> {
    // This is the same preflight used by ordinary MCP/CLI startup.
    super::super::check_frontend_start(&root.join("state"))?;
    struct RunningHost {
        cleanup: runtime_cleanup::RuntimeCleanup,
        thread: Option<
            std::thread::JoinHandle<
                agentlaw_runtime::ports::RuntimeResult<agentlaw_runtime::host::RunExit>,
            >,
        >,
    }
    impl Drop for RunningHost {
        fn drop(&mut self) {
            self.cleanup.shutdown();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    let state = root.join("state");
    let executable = std::env::current_exe().unwrap();
    let build = agentlaw_runtime::installation::build_identity(&executable).unwrap();
    let gate_state = state.clone();
    let deps = agentlaw_runtime::ports::HostDeps {
        state: state.clone(),
        executable,
        assets: std::sync::Arc::new(|| Ok(None)),
        installation_build: build.clone(),
        start_gate: std::sync::Arc::new(move || super::super::check_frontend_start(&gate_state)),
    };
    let mut host = RunningHost {
        cleanup: runtime_cleanup::RuntimeCleanup(state.clone()),
        thread: Some(std::thread::spawn(move || {
            agentlaw_runtime::host::run(deps)
        })),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !matches!(
        agentlaw_runtime::client::probe(&state, &build, Duration::from_millis(100)),
        agentlaw_runtime::client::Probe::Running(_)
    ) {
        assert!(
            !host.thread.as_ref().unwrap().is_finished(),
            "normal runtime exited before admission"
        );
        assert!(
            Instant::now() < deadline,
            "normal runtime did not become ready"
        );
        std::thread::yield_now();
    }
    let mut frontend = crate::frontend::RuntimeBackend::new(state, false)?;
    let result = crate::call_json(
        &mut frontend,
        &json!({"action":"recall","recall":{"recall_for":"Retained source"}}).to_string(),
    );
    drop(frontend);
    host.cleanup.shutdown();
    let stopped = host.thread.take().unwrap().join().unwrap().unwrap();
    assert!(matches!(stopped, agentlaw_runtime::host::RunExit::Stopped));
    result
}

#[cfg(windows)]
#[test]
fn killed_startup_cleanup_retry_does_not_leave_a_completed_update_blocking_frontend_start() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let state = root.join("state");
    let (path, cleanup, update) = completed_retry_crash_installation(&root);
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "update::managed::cleanup::tests::startup_cleanup_retry_crash_fixture",
            "--nocapture",
        ])
        .env("AGENTLAW_STARTUP_RETRY_CRASH_ROOT", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
    let mut child = runtime_cleanup::OwnedStaticChild(command.spawn().unwrap());
    let stdout = child.0.stdout.take().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            if line.unwrap().contains("startup-cleanup-retry-held") {
                let _ = ready_tx.send(());
            }
        }
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(state.join("fixture-startup-retry-held").is_file());
    assert!(child.0.try_wait().unwrap().is_none());
    if let Ok(bytes) = fs::read(maintenance_path(&state)) {
        let marker: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(marker["plan_id"], update.id);
        assert_eq!(marker["drain"], true);
    }
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    reader.join().unwrap();
    assert!(
        agentlaw_runtime::installation::owner::OwnerLock::try_acquire(&state)
            .unwrap()
            .is_some(),
        "OS exclusion must be released when the retry process dies"
    );
    assert!(path.exists(), "the crash hook must precede actual removal");
    let result = normal_frontend_recall_after_retry(&root);
    assert!(
        result.is_ok(),
        "completed-update cleanup crash permanently blocked normal frontend startup: {result:?}"
    );
    assert!(result.unwrap().is_object());
    assert_eq!(
        fs::read(&path).unwrap(),
        b"fixture",
        "the still-held file should remain pending"
    );
    drop(held);
    // The ordinary later start must still consume the retained deletion record.
    let later = normal_frontend_recall_after_retry(&root);
    assert!(later.is_ok(), "later normal start failed: {later:?}");
    assert!(
        !path.exists(),
        "the crash lost the recorded deletion instead of retrying it later"
    );
    let saved = cleanup_load(&root, &cleanup.id).unwrap();
    assert!(saved
        .items
        .iter()
        .any(|item| item.source == path && item.phase == "removed"));
}

#[cfg(windows)]
#[test]
fn unfinished_update_maintenance_still_blocks_frontend_start_with_completed_cleanup_pending() {
    let temp = managed_cleanup_root();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (path, _, _) = completed_retry_crash_installation(&root);
    let update = verified_finalizing_update(&root);
    assert_eq!(update.phase, "finalizing");
    let candidate = candidate_executable(&update).unwrap();
    let hash = update
        .bundle_hashes
        .as_ref()
        .unwrap()
        .get(binary_name())
        .unwrap();
    let _gate = agentlaw_runtime::installation::gate::MaintenanceGate::open(
        &root.join("state"),
        &update.id,
        &candidate,
        hash,
    )
    .unwrap();
    let before = fs::read(maintenance_path(&root.join("state"))).unwrap();
    let bytes = fs::read(&path).unwrap();
    let error = normal_frontend_recall_after_retry(&root).unwrap_err();
    assert_eq!(error.code, "update_maintenance");
    assert_eq!(
        fs::read(maintenance_path(&root.join("state"))).unwrap(),
        before,
        "unfinished update gate must never be mistaken for abandoned completed-update cleanup"
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}
