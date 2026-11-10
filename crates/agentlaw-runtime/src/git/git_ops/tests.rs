use super::*;
use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
use serde_json::json;
fn fixture() -> (tempfile::TempDir, PathBuf, Store, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let root = base.join("source");
    let local = base.join("local");
    let git_control = base.join("git-control");
    let s = Store::open(&root, &local).unwrap();
    run(&root, &["init", "--initial-branch=main"]).unwrap();
    run(&root, &["config", "user.name", "Agentlaw Test"]).unwrap();
    run(&root, &["config", "user.email", "test@example.invalid"]).unwrap();
    (temp, root, s, git_control)
}
fn publish(s: &Store, body: &str) {
    s.publish(&uuid::Uuid::new_v4().to_string(),vec![Mutation{unit:CurrentUnit{entity_id:uuid::Uuid::new_v4().to_string(),entity_type:"memory".into(),state:UnitState::Live{heads:vec![Head{metadata:json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),body:body.into()}]}},expected_versions:vec![],evidence:"test".into()}]).unwrap();
}
#[test]
fn canonical_user_staging_is_not_overwritten_and_ref_gap_resumes() {
    let (_temp, root, source, local) = fixture();
    publish(&source, "base");
    let before = continuity_save(&source, &local, "base").unwrap();
    publish(&source, "second");
    run(&root, &["add", "current"]).unwrap();
    let index = run(&root, &["ls-files", "--stage"]).unwrap();
    assert_eq!(
        continuity_save(&source, &local, "must refuse")
            .unwrap_err()
            .code,
        "canonical_user_staging_present"
    );
    assert_eq!(run(&root, &["ls-files", "--stage"]).unwrap(), index);
    assert_eq!(optional_head(&root).unwrap().unwrap(), before.commit_oid);
    // Explicit test-only unstage restores the user's chosen baseline.
    run(&root, &["reset", "HEAD", "--", "current"]).unwrap();
    let tree = staged_tree(&root, &root, &local).unwrap();
    let plan = prepare_index_plan(&root, &local, &tree).unwrap();
    let commit = text(
        &root,
        &[
            "commit-tree",
            &tree,
            "-p",
            &before.commit_oid,
            "-m",
            "test crash gap",
        ],
    )
    .unwrap();
    run(
        &root,
        &["update-ref", "refs/heads/main", &commit, &before.commit_oid],
    )
    .unwrap();
    apply_ref_index(
        &root,
        "refs/heads/main",
        Some(&before.commit_oid),
        &commit,
        &plan,
    )
    .unwrap();
    assert!(run(
        &root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "--",
            "current",
            "history"
        ]
    )
    .unwrap()
    .is_empty());
}
#[test]
fn private_index_and_no_duplicate_empty_commit() {
    let (_temp, root, s, local) = fixture();
    publish(&s, "remember");
    fs::write(root.join("unrelated.txt"), "user staging").unwrap();
    run(&root, &["add", "unrelated.txt"]).unwrap();
    let before = run(&root, &["ls-files", "--stage", "--", "unrelated.txt"]).unwrap();
    let a = continuity_save(&s, &local, "test canonical snapshot").unwrap();
    let b = continuity_save(&s, &local, "no new changes").unwrap();
    assert_eq!(a.commit_oid, b.commit_oid);
    assert!(!a.pushed);
    assert_eq!(
        run(&root, &["ls-files", "--stage", "--", "unrelated.txt"]).unwrap(),
        before
    );
    assert!(run(
        &root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "--",
            "current",
            "history",
            "catalog",
            "format.md",
            ".gitattributes"
        ]
    )
    .unwrap()
    .is_empty());
    assert!(
        !text(&root, &["ls-tree", "-r", "--name-only", &a.commit_oid])
            .unwrap()
            .contains("unrelated.txt")
    );
}
#[test]
fn stale_owner_sidecar_does_not_delete_later_full_index_lock() {
    let (_temp, root, source, local) = fixture();
    publish(&source, "base");
    let before = continuity_save(&source, &local, "base").unwrap();
    publish(&source, "second");
    let tree = staged_tree(&root, &root, &local).unwrap();
    let plan = prepare_index_plan(&root, &local, &tree).unwrap();
    let commit = text(
        &root,
        &[
            "commit-tree",
            &tree,
            "-p",
            &before.commit_oid,
            "-m",
            "handoff",
        ],
    )
    .unwrap();
    apply_ref_index(
        &root,
        "refs/heads/main",
        Some(&before.commit_oid),
        &commit,
        &plan,
    )
    .unwrap();
    let owner = plan.index.with_extension("lock.agentlaw-owner");
    assert!(!owner.exists());
    // Reproduce a crash after index installation but before owner cleanup,
    // followed by later user staging and an unrelated full index lock.
    let marker = format!(
        "agentlaw-index-handoff-v2\n{}\n{commit}\n{}\n",
        plan.owner_token, plan.desired
    );
    save_local_json(&owner, &marker).unwrap();
    fs::write(root.join("later-user.txt"), "later staging").unwrap();
    run(&root, &["add", "later-user.txt"]).unwrap();
    let staged = run(&root, &["ls-files", "--stage"]).unwrap();
    let foreign = fs::read(&plan.candidate).unwrap();
    let lock = plan.index.with_extension("lock");
    fs::write(&lock, &foreign).unwrap();
    assert_eq!(
        apply_ref_index(
            &root,
            "refs/heads/main",
            Some(&before.commit_oid),
            &commit,
            &plan
        )
        .unwrap_err()
        .code,
        "index_handoff_pending"
    );
    assert_eq!(fs::read(&lock).unwrap(), foreign);
    assert_eq!(run(&root, &["ls-files", "--stage"]).unwrap(), staged);
}
#[test]
fn scan_history_masks_values_and_endpoint_change_invalidates_review() {
    let (_temp, root, s, local) = fixture();
    publish(&s, "ghp_test_sensitive_placeholder");
    continuity_save(&s, &local, "sensitive ancestor").unwrap();
    run(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/memory.git",
        ],
    )
    .unwrap();
    let r = inspect_share(&s, &local, "origin", "refs/heads/main").unwrap();
    assert!(!r.findings.is_empty());
    assert!(!serde_json::to_string(&r)
        .unwrap()
        .contains("sensitive_placeholder"));
    assert_eq!(
        push_review(&s, &local, &r.review_ref, false)
            .unwrap_err()
            .code,
        "sharing_choice_required"
    );
    run(
        &root,
        &[
            "remote",
            "set-url",
            "origin",
            "https://changed.invalid/memory.git",
        ],
    )
    .unwrap();
    assert_eq!(
        push_review(&s, &local, &r.review_ref, true)
            .unwrap_err()
            .code,
        "sharing_review_stale"
    );
}
#[test]
fn approved_import_publication_and_handoff_retry() {
    let (_temp, root, source, local) = fixture();
    publish(&source, "original");
    let saved = continuity_save(&source, &local, "base").unwrap();
    fs::write(
        root.join("committed-note.txt"),
        "retain unrelated committed file",
    )
    .unwrap();
    run(&root, &["add", "committed-note.txt"]).unwrap();
    run(&root, &["commit", "-m", "test user tracked note"]).unwrap();
    let review = prepare_import(&source, &local, &saved.commit_oid).unwrap();
    let context = get_import_stage(&source, &local, &review.import_ref).unwrap();
    let staged = Store::open_with_coordination(
        &context.store_path,
        context.runtime_path.join("canonical"),
        source.coordination_root(),
    )
    .unwrap();
    publish(&staged, "explicitly approved staged addition");
    let resolution = resolve_import(&source, &local, &review.import_ref).unwrap();
    assert!(resolution.ready_for_user_confirmation);
    assert!(publish_import(
        &source,
        &local,
        &review.import_ref,
        &resolution.resolution_token,
        false
    )
    .is_err());
    fs::write(root.join("unrelated.txt"), "user staging survives import").unwrap();
    run(&root, &["add", "unrelated.txt"]).unwrap();
    let before = run(&root, &["ls-files", "--stage", "--", "unrelated.txt"]).unwrap();
    let receipt = publish_import(
        &source,
        &local,
        &review.import_ref,
        &resolution.resolution_token,
        true,
    )
    .unwrap();
    assert_eq!(receipt.status, "published_and_committed");
    assert_eq!(source.snapshot().unwrap().1.len(), 2);
    assert_eq!(optional_head(&root).unwrap().unwrap(), receipt.commit_oid);
    assert_eq!(
        text(
            &root,
            &[
                "show",
                &format!("{}:committed-note.txt", receipt.commit_oid)
            ]
        )
        .unwrap(),
        "retain unrelated committed file"
    );
    assert_eq!(
        run(&root, &["ls-files", "--stage", "--", "unrelated.txt"]).unwrap(),
        before
    );
    assert!(run(
        &root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "--",
            "current",
            "history",
            "catalog",
            "format.md",
            ".gitattributes"
        ]
    )
    .unwrap()
    .is_empty());
    let replay = publish_import(
        &source,
        &local,
        &review.import_ref,
        &resolution.resolution_token,
        true,
    )
    .unwrap();
    assert_eq!(receipt.commit_oid, replay.commit_oid);
    assert_eq!(
        receipt.publication.generation,
        replay.publication.generation
    );
}
#[test]
fn import_prepare_is_isolated_and_source_change_requires_review() {
    let (_temp, root, s, local) = fixture();
    publish(&s, "before import");
    let saved = continuity_save(&s, &local, "source commit").unwrap();
    let generation = s.generation().unwrap();
    let prepared = prepare_import(&s, &local, &saved.commit_oid).unwrap();
    assert_eq!(prepared.status, "review_required");
    assert!(prepared.canonical_publish_supported);
    assert_eq!(s.generation().unwrap(), generation);
    assert_eq!(optional_head(&root).unwrap().unwrap(), saved.commit_oid);
    assert!(prepared.staged_path.join("format.md").is_file());
    publish(&s, "new active memory");
    assert_eq!(
        inspect_import(&s, &local, &prepared.import_ref)
            .unwrap()
            .status,
        "review_required_source_changed"
    );
}
