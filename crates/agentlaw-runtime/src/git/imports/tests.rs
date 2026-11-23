use super::*;
use fs2::FileExt;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    sync::{mpsc, Arc, Barrier},
    thread,
    time::Duration,
};

fn record(local: &Path, id: &str, filename: &str, value: serde_json::Value) {
    let folder = local.join("imports").join(id);
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join(filename), serde_json::to_vec(&value).unwrap()).unwrap();
}

fn review(id: &str) -> serde_json::Value {
    json!({"active_root":"fixture-source","review":{"import_ref":id,"incoming_commit":"1111111111111111111111111111111111111111","reviewed_tree":"2222222222222222222222222222222222222222","active_head":null,"source_generation":1,"status":"review_required","staged_path":"stage","active_source_unchanged":true,"canonical_publish_supported":true}})
}

fn entries(root: &Path) -> BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    fn visit(
        root: &Path,
        current: &Path,
        result: &mut BTreeMap<std::path::PathBuf, Option<Vec<u8>>>,
    ) {
        for entry in fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                result.insert(path.strip_prefix(root).unwrap().to_owned(), None);
                visit(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    Some(fs::read(path).unwrap()),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn import_list_distinguishes_live_completed_handoff_review_and_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("git");
    fs::create_dir(&local).unwrap();
    let ids: Vec<String> = (0..5).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    record(
        &local,
        &ids[1],
        "completed.json",
        json!({"status":"published_and_committed","publication":{"operation_id":uuid::Uuid::new_v4().to_string(),"generation":1,"references":[],"durability":"FileSyncedProcessCrashProtocolPowerLossUnverified"},"commit_oid":"1111111111111111111111111111111111111111","pushed":false,"active_index_preserved":true,"unrelated_staging_preserved":true,"canonical_index_updated":true}),
    );
    record(&local, &ids[2], "review.json", review(&ids[2]));
    record(
        &local,
        &ids[2],
        "handoff.json",
        json!({"operation_id":uuid::Uuid::new_v4().to_string(),"resolution":{"import_ref":ids[2],"resolution_token":"token","reviewed_tree":"2222222222222222222222222222222222222222","staged_generation":1,"conflicting_memory_ids":[],"ready_for_user_confirmation":true},"commit_oid":"1111111111111111111111111111111111111111","reference":"refs/heads/main","expected_head":null,"index_plan":{"index":"index","candidate":"candidate","expected":null,"desired":"digest"}}),
    );
    record(&local, &ids[3], "review.json", review(&ids[3]));
    let incomplete = local.join("imports").join(&ids[4]);
    fs::create_dir_all(&incomplete).unwrap();
    fs::write(incomplete.join("review.json"), b"{broken").unwrap();
    let progress = ProgressRegistry::default();
    progress
        .update(&ids[0], "preparing", 4, "objects", Duration::from_secs(2))
        .unwrap();
    let listed = list(&local, &progress).unwrap();
    let found: BTreeMap<_, _> = listed
        .into_iter()
        .map(|item| (item.import_ref, item.status))
        .collect();
    assert_eq!(found.get(&ids[0]), Some(&ImportStatus::InProgress));
    assert_eq!(found.get(&ids[1]), Some(&ImportStatus::Completed));
    assert_eq!(found.get(&ids[2]), Some(&ImportStatus::HandoffPending));
    assert_eq!(found.get(&ids[3]), Some(&ImportStatus::ReviewWaiting));
    assert_eq!(found.get(&ids[4]), Some(&ImportStatus::Incomplete));
}

#[test]
fn import_list_does_not_wait_for_git_lane() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("git");
    fs::create_dir(&local).unwrap();
    let lane = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(local.join("git-lane.lock"))
        .unwrap();
    lane.lock_exclusive().unwrap();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        tx.send(list(&local, &ProgressRegistry::default()).is_ok())
            .unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(3));
    FileExt::unlock(&lane).unwrap();
    worker.join().unwrap();
    assert!(result.expect("list waited for Git lane"));
}

#[test]
fn progress_updates_at_barrier_leave_state_directory_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::write(state.join("sentinel"), b"preserve").unwrap();
    let before = entries(&state);
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker = thread::spawn(move || {
        worker_barrier.wait();
        worker_barrier.wait();
    });
    barrier.wait();
    let progress = ProgressRegistry::default();
    for n in 0..5 {
        progress
            .update("import", "receiving", n, "objects", Duration::from_secs(n))
            .unwrap();
    }
    assert_eq!(entries(&state), before);
    barrier.wait();
    worker.join().unwrap();
}
