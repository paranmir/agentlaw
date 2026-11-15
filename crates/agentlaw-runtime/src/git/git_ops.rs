//! Git is an explicit sharing boundary, never the completion condition of remembering.
use agentlaw_contracts::{DomainError, Result};
use agentlaw_storage::import_conflict::ImportConflict;
use agentlaw_storage::Store;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) const POLICY: &str = "agentlaw-pattern-scan-v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaveReceipt {
    pub status: String,
    pub commit_oid: String,
    pub tree_oid: String,
    pub source_generation: u64,
    pub pushed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub category: String,
    pub blob_oid: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareReview {
    pub review_ref: String,
    pub commit_oid: String,
    pub tree_oid: String,
    pub remote: String,
    pub target_ref: String,
    pub policy_version: String,
    pub findings: Vec<Finding>,
    pub includes_all_reachable_history: bool,
}
#[derive(Serialize, Deserialize)]
struct StoredReview {
    review: ShareReview,
    repo: String,
    endpoint_digest: String,
    closure_digest: String,
    #[serde(default)]
    scan: Option<super::git_scan::ScanReceipt>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushReceipt {
    pub status: String,
    pub commit_oid: String,
    pub target_ref: String,
    pub local_source_preserved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_ref: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FetchReceipt {
    pub fetch_ref: String,
    pub remote: String,
    pub fetched_refs: Vec<String>,
    pub active_source_unchanged: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportReview {
    #[serde(default)]
    pub completion: Option<ImportPublishReceipt>,
    #[serde(default)]
    pub conflicting_memory_ids: Vec<String>,
    #[serde(default)]
    pub next_action: Option<String>,
    #[serde(default)]
    pub structural_conflicts: Vec<ImportConflict>,
    pub import_ref: String,
    pub incoming_commit: String,
    pub reviewed_tree: String,
    pub active_head: Option<String>,
    pub source_generation: u64,
    pub status: String,
    pub staged_path: PathBuf,
    pub active_source_unchanged: bool,
    pub canonical_publish_supported: bool,
}
#[derive(Serialize, Deserialize)]
struct StoredImport {
    review: ImportReview,
    active_root: String,
}
pub(super) fn valid_review_record(value: serde_json::Value, reference: &str) -> bool {
    serde_json::from_value::<StoredImport>(value).is_ok_and(|record| {
        record.review.import_ref == reference
            && valid_oid(&record.review.incoming_commit)
            && valid_oid(&record.review.reviewed_tree)
            && !record.active_root.is_empty()
    })
}
pub(super) fn valid_completion_record(value: serde_json::Value) -> bool {
    serde_json::from_value::<ImportPublishReceipt>(value).is_ok_and(|record| {
        record.status == "published_and_committed"
            && valid_oid(&record.commit_oid)
            && uuid::Uuid::parse_str(&record.publication.operation_id).is_ok()
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportStageContext {
    pub store_path: PathBuf,
    pub runtime_path: PathBuf,
}
pub fn get_import_stage(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    import_ref: &str,
) -> Result<ImportStageContext> {
    let _lane = lane(local)?;
    let (dir, stored) = import_record(local, import_ref)?;
    let (root, generation) = store
        .with_source_read(|r, g| Ok((r.to_string_lossy().to_string(), g)))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    if root != stored.active_root || generation != stored.review.source_generation {
        return Err(DomainError::new(
            "stale_import_review",
            "Active source changed; prepare a fresh import review.",
        ));
    }
    if dir.join("handoff.json").exists() {
        return Err(DomainError::new(
            "import_already_confirmed",
            "Confirmed import can only resume its exact publication/handoff.",
        ));
    }
    Ok(ImportStageContext {
        store_path: stored.review.staged_path,
        runtime_path: dir.join("runtime"),
    })
}
pub fn get_import_stage_for_publish(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    import_ref: &str,
) -> Result<ImportStageContext> {
    let _lane = lane(local)?;
    let (dir, stored) = import_record(local, import_ref)?;
    let root = store
        .with_source_read(|root, _| Ok(root.to_string_lossy().to_string()))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    if root != stored.active_root {
        return Err(DomainError::new(
            "import_binding_mismatch",
            "Import belongs to another source.",
        ));
    }
    Ok(ImportStageContext {
        store_path: stored.review.staged_path,
        runtime_path: dir.join("runtime"),
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportResolution {
    #[serde(default)]
    pub structural_conflicts: Vec<ImportConflict>,
    #[serde(default)]
    pub next_action: Option<String>,
    pub import_ref: String,
    pub resolution_token: String,
    pub reviewed_tree: String,
    pub staged_generation: u64,
    pub conflicting_memory_ids: Vec<String>,
    pub ready_for_user_confirmation: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportPublishReceipt {
    pub status: String,
    pub publication: agentlaw_storage::PublishReceipt,
    pub commit_oid: String,
    pub pushed: bool,
    pub active_index_preserved: bool,
    pub unrelated_staging_preserved: bool,
    pub canonical_index_updated: bool,
}
#[derive(Serialize, Deserialize)]
struct ImportHandoff {
    operation_id: String,
    resolution: ImportResolution,
    commit_oid: String,
    reference: String,
    expected_head: Option<String>,
    index_plan: IndexPlan,
}
pub(super) fn valid_handoff_record(value: serde_json::Value, reference: &str) -> bool {
    serde_json::from_value::<ImportHandoff>(value).is_ok_and(|record| {
        record.resolution.import_ref == reference
            && !record.resolution.resolution_token.is_empty()
            && valid_oid(&record.commit_oid)
            && uuid::Uuid::parse_str(&record.operation_id).is_ok()
    })
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct IndexPlan {
    index: PathBuf,
    candidate: PathBuf,
    expected: Option<String>,
    desired: String,
    #[serde(default)]
    owner_token: String,
}
#[derive(Serialize, Deserialize)]
struct SaveHandoff {
    root: PathBuf,
    reference: String,
    parent: Option<String>,
    receipt: SaveReceipt,
    index_plan: IndexPlan,
}
pub(crate) fn file_digest(path: &Path) -> Result<Option<String>> {
    match File::open(path) {
        Ok(mut file) => {
            let mut hash = Sha256::new();
            std::io::copy(&mut file, &mut hash).map_err(|_| io_error("index hash"))?;
            Ok(Some(format!("{:x}", hash.finalize())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(io_error("index read")),
    }
}
pub(crate) fn pipe_git_index(repo: &Path, index: &Path, args: &[&str], input: &[u8]) -> Result<()> {
    let mut child = command(repo)
        .env("GIT_INDEX_FILE", git_path(index))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| io_error("index candidate command"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| io_error("index input"))?
        .write_all(input)
        .map_err(|_| io_error("index input"))?;
    if !child
        .wait()
        .map_err(|_| io_error("index candidate wait"))?
        .success()
    {
        return Err(io_error("index candidate update"));
    }
    Ok(())
}
pub(crate) fn prepare_index_plan(repo: &Path, local: &Path, tree: &str) -> Result<IndexPlan> {
    let canonical = [
        "current",
        "history",
        "catalog",
        "format.md",
        ".gitattributes",
    ];
    let index = PathBuf::from(text(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?);
    let expected = file_digest(&index)?;
    let parent = optional_head(repo)?;
    let mut args = vec!["diff", "--cached", "--quiet"];
    if let Some(p) = &parent {
        args.push(p);
    }
    args.push("--");
    args.extend(canonical);
    let diff = command(repo)
        .args(args)
        .output()
        .map_err(|_| io_error("staged canonical check"))?;
    if !diff.status.success() {
        return Err(DomainError::new(
            "canonical_user_staging_present",
            "Canonical paths already contain user-staged changes; preserve/commit/unstage them explicitly before this Git handoff.",
        ));
    }
    let candidate = local.join(format!("handoff-{}.index", uuid::Uuid::new_v4()));
    if index.exists() {
        fs::copy(&index, &candidate).map_err(|_| io_error("index candidate copy"))?;
    } else {
        pipe_git_index(repo, &candidate, &["read-tree", "--empty"], &[])?;
    }
    let mut list = vec!["ls-files", "-z", "--"];
    list.extend(canonical);
    let paths = run(repo, &list)?;
    pipe_git_index(
        repo,
        &candidate,
        &["update-index", "--force-remove", "-z", "--stdin"],
        &paths,
    )?;
    let mut list = vec!["ls-tree", "-r", "-z", tree, "--"];
    list.extend(canonical);
    let entries = run(repo, &list)?;
    pipe_git_index(
        repo,
        &candidate,
        &["update-index", "-z", "--index-info"],
        &entries,
    )?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&candidate)
        .map_err(|_| io_error("candidate reopen"))?
        .sync_all()
        .map_err(|_| io_error("candidate sync"))?;
    let desired = file_digest(&candidate)?.ok_or_else(|| io_error("candidate digest"))?;
    if file_digest(&index)? != expected {
        return Err(DomainError::new(
            "index_changed",
            "Git index changed while preparing handoff; retry review.",
        ));
    }
    Ok(IndexPlan {
        index,
        candidate,
        expected,
        desired,
        owner_token: uuid::Uuid::new_v4().to_string(),
    })
}
pub(crate) fn apply_ref_index(
    repo: &Path,
    reference: &str,
    expected_head: Option<&str>,
    commit: &str,
    plan: &IndexPlan,
) -> Result<()> {
    let lock_path = plan.index.with_extension("lock");
    let owner_path = plan.index.with_extension("lock.agentlaw-owner");
    let marker = format!(
        "agentlaw-index-handoff-v2\n{}\n{commit}\n{}\n",
        plan.owner_token, plan.desired
    );
    // A completed handoff must never claim/remove a subsequently-created foreign lock.
    if file_digest(&plan.index)?.as_deref() == Some(&plan.desired)
        && optional_head(repo)?.as_deref() == Some(commit)
        && head_reference_matches(repo, reference)?
    {
        clear_index_owner(&owner_path, &marker)?;
        return Ok(());
    }
    let (lock, marker_owned) = match OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(mut f) => {
            f.write_all(marker.as_bytes())
                .map_err(|_| io_error("index lock marker"))?;
            f.sync_all().map_err(|_| io_error("index lock sync"))?;
            save_local_json(&owner_path, &marker)?;
            (f, true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let bytes = fs::read(&lock_path).map_err(|_| io_error("index lock read"))?;
            let owned: Option<String> = File::open(&owner_path)
                .ok()
                .and_then(|f| serde_json::from_reader(f).ok());
            if plan.owner_token.is_empty()
                || (bytes != marker.as_bytes()
                    && !(owned.as_deref() == Some(&marker)
                        && file_digest(&lock_path)?.as_deref() == Some(&plan.desired)))
            {
                return Err(DomainError::new(
                    "git_index_locked",
                    "Git index is locked by another operation; handoff remains pending.",
                ));
            }
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|_| io_error("index lock resume"))?;
            (lock, bytes == marker.as_bytes())
        }
        Err(_) => return Err(io_error("index lock")),
    };
    let actual = file_digest(&plan.index)?;
    if actual != plan.expected && actual.as_deref() != Some(&plan.desired) {
        drop(lock);
        // A stale sidecar alone cannot authorize deletion of a full index lock.
        // It may belong to a later Git operation after our prior handoff.
        if marker_owned {
            fs::remove_file(&lock_path).map_err(|_| io_error("stale index marker"))?;
            clear_index_owner(&owner_path, &marker)?;
        }
        return Err(DomainError::new(
            "index_handoff_pending",
            "User index changed since preparation; no staged entries or Git ref were overwritten.",
        ));
    }
    let symbolic = command(repo)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .map_err(|_| io_error("handoff HEAD binding"))?;
    let current_reference = if symbolic.status.success() {
        String::from_utf8_lossy(&symbolic.stdout).trim().to_owned()
    } else {
        "HEAD".into()
    };
    if current_reference != reference {
        drop(lock);
        if marker_owned {
            fs::remove_file(&lock_path).map_err(|_| io_error("stale branch marker"))?;
            clear_index_owner(&owner_path, &marker)?;
        }
        return Err(DomainError::new(
            "ref_handoff_pending",
            "Checked-out branch changed; no ref or staged entries were overwritten.",
        ));
    }
    let current = optional_head(repo)?;
    if file_digest(&plan.candidate)?.as_deref() != Some(&plan.desired) {
        return Err(io_error("index candidate integrity"));
    }
    if current.as_deref() != Some(commit) && current.as_deref() != expected_head {
        drop(lock);
        if marker_owned {
            fs::remove_file(&lock_path).map_err(|_| io_error("stale ref marker"))?;
            clear_index_owner(&owner_path, &marker)?;
        }
        return Err(DomainError::new(
            "ref_handoff_pending",
            "Git HEAD changed; ref/index handoff remains pending.",
        ));
    }
    // Prepare a complete replacement beside the index, then atomically replace
    // our marker. A crash never leaves a half-written index lock after HEAD moves.
    if actual.as_deref() != Some(&plan.desired) {
        let ready = plan
            .index
            .with_file_name(format!(".agentlaw-index-{}.tmp", uuid::Uuid::new_v4()));
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&ready)
            .map_err(|_| io_error("index ready file"))?;
        std::io::copy(
            &mut File::open(&plan.candidate).map_err(|_| io_error("index candidate"))?,
            &mut output,
        )
        .map_err(|_| io_error("index ready copy"))?;
        output
            .sync_all()
            .map_err(|_| io_error("index ready sync"))?;
        drop(output);
        drop(lock);
        fs::rename(&ready, &lock_path).map_err(|_| io_error("index ready marker replace"))?;
    } else {
        drop(lock);
    }
    if current.as_deref() != Some(commit) {
        let zero = "0".repeat(commit.len());
        run(
            repo,
            &[
                "update-ref",
                reference,
                commit,
                expected_head.unwrap_or(&zero),
            ],
        )?;
    }
    if actual.as_deref() == Some(&plan.desired) {
        fs::remove_file(lock_path).map_err(|_| io_error("completed index lock"))?;
        clear_index_owner(&owner_path, &marker)?;
        return Ok(());
    }
    fs::rename(&lock_path, &plan.index).map_err(|_| io_error("index installation replace"))?;
    clear_index_owner(&owner_path, &marker)?;
    Ok(())
}
fn clear_index_owner(path: &Path, marker: &str) -> Result<()> {
    let owned: Option<String> = File::open(path)
        .ok()
        .and_then(|f| serde_json::from_reader(f).ok());
    if owned.as_deref() == Some(marker) {
        fs::remove_file(path).map_err(|_| io_error("index owner cleanup"))?;
    }
    Ok(())
}
pub(crate) fn ref_index_matches(
    repo: &Path,
    reference: &str,
    commit: &str,
    plan: &IndexPlan,
) -> Result<bool> {
    Ok(file_digest(&plan.index)?.as_deref() == Some(&plan.desired)
        && optional_head(repo)?.as_deref() == Some(commit)
        && head_reference_matches(repo, reference)?)
}
fn head_reference_matches(repo: &Path, reference: &str) -> Result<bool> {
    let symbolic = command(repo)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .map_err(|_| io_error("HEAD branch binding"))?;
    Ok(if symbolic.status.success() {
        String::from_utf8_lossy(&symbolic.stdout).trim() == reference
    } else {
        reference == "HEAD"
    })
}

pub(crate) fn save_local_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut f = File::create(&tmp).map_err(|_| io_error("local handoff create"))?;
    serde_json::to_writer(&mut f, value).map_err(|_| io_error("handoff encode"))?;
    f.sync_all().map_err(|_| io_error("handoff sync"))?;
    drop(f);
    fs::rename(tmp, path).map_err(|_| io_error("handoff replace"))?;
    Ok(())
}
fn import_record(local: &Path, id: &str) -> Result<(PathBuf, StoredImport)> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(DomainError::new(
            "invalid_import_ref",
            "Use the returned import reference.",
        ));
    }
    let dir = local.join("imports").join(id);
    let stored = serde_json::from_reader(
        File::open(dir.join("review.json")).map_err(|_| io_error("import record"))?,
    )
    .map_err(|_| io_error("import record decode"))?;
    Ok((dir, stored))
}
#[cfg(test)]
pub(crate) fn staged_tree(repo: &Path, staged: &Path, local: &Path) -> Result<String> {
    staged_tree_controlled(
        repo,
        staged,
        local,
        &agentlaw_contracts::RequestControl::default(),
    )
}
fn staged_tree_controlled(
    repo: &Path,
    staged: &Path,
    local: &Path,
    control: &agentlaw_contracts::RequestControl,
) -> Result<String> {
    let index = local.join(format!("resolution-{}.index", uuid::Uuid::new_v4()));
    let result = (|| {
        let head = output_cancellable(repo, &["rev-parse", "--verify", "HEAD"], control)?;
        let parent = head
            .status
            .success()
            .then(|| String::from_utf8_lossy(&head.stdout).trim().to_owned());
        let mut cmd = command(repo);
        cmd.env("GIT_INDEX_FILE", git_path(&index))
            .args(["read-tree", parent.as_deref().unwrap_or("--empty")]);
        let out = wait_cancellable(cmd, control)?;
        if !out.status.success() {
            return Err(io_error("resolution index"));
        }
        for path in [
            "current",
            "history",
            "catalog",
            "format.md",
            ".gitattributes",
        ] {
            if staged.join(path).exists() {
                let mut cmd = command(repo);
                cmd.env("GIT_INDEX_FILE", git_path(&index))
                    .env("GIT_WORK_TREE", git_path(staged))
                    .args(["add", "-A", "-f", "--", path]);
                let out = wait_cancellable(cmd, control)?;
                if !out.status.success() {
                    return Err(io_error("resolution capture"));
                }
            }
        }
        let mut cmd = command(repo);
        cmd.env("GIT_INDEX_FILE", git_path(&index))
            .args(["write-tree"]);
        let out = wait_cancellable(cmd, control)?;
        if !out.status.success() {
            return Err(io_error("resolution tree"));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    })();
    let _ = fs::remove_file(index);
    result
}
/// Validate the human/LLM-assisted edits already made through the staged Runtime.
/// Concurrent heads are reported, never chosen here. Token binds exact reviewed bytes.
fn conflicting_current_ids(staged: &Store) -> Result<Vec<String>> {
    let reader = staged.owned_published_reader();
    let mut cursor = None;
    let mut conflicts = Vec::new();
    loop {
        let (_, page, more) = reader
            .inventory_page(cursor.as_deref(), 512)
            .map_err(|e| DomainError::new("staged_inventory", e.to_string()))?;
        for (kind, id) in &page {
            let current = if kind == "memory" {
                reader.acquire_current(id)
            } else {
                reader.acquire_procedure(id)
            }
            .map_err(|e| DomainError::new("staged_current", e.to_string()))?;
            if current.references.len() > 1 {
                conflicts.push(id.clone());
            }
        }
        if !more {
            break;
        }
        cursor = page.last().map(|(k, id)| format!("{k}/{id}"));
    }
    Ok(conflicts)
}
pub fn resolve_import(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    import_ref: &str,
    control: &agentlaw_contracts::RequestControl,
) -> Result<ImportResolution> {
    control.check()?;
    control.phase("acquiring_import");
    let _lane = lane(local)?;
    let (dir, stored) = import_record(local, import_ref)?;
    let (root, generation) = store
        .with_source_read(|r, g| Ok((r.to_path_buf(), g)))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    if root.to_string_lossy() != stored.active_root || generation != stored.review.source_generation
    {
        return Err(DomainError::new(
            "stale_import_review",
            "Active source changed; prepare a fresh review workspace.",
        ));
    }
    let staged = Store::open_read_only_with_coordination(
        &stored.review.staged_path,
        dir.join("runtime/canonical"),
        store.coordination_root(),
    )
    .map_err(|e| DomainError::new("staged_source_unavailable", e.to_string()))?;
    control.phase("verifying_import");
    let audit = staged
        .audit_source()
        .map_err(|e| DomainError::new("invalid_import_resolution", e.to_string()))?;
    let mut conflicts = Vec::new();
    let reader = staged.owned_published_reader();
    let mut cursor = None;
    loop {
        let (_, page, more) = reader
            .inventory_page(cursor.as_deref(), 512)
            .map_err(|e| DomainError::new("staged_inventory", e.to_string()))?;
        for (kind, id) in &page {
            let acquired = if kind == "memory" {
                reader.acquire_current(id)
            } else {
                reader.acquire_procedure(id)
            }
            .map_err(|e| DomainError::new("staged_current", e.to_string()))?;
            if acquired.references.len() > 1 {
                conflicts.push(id.clone());
            }
        }
        if !more {
            break;
        }
        cursor = page.last().map(|(k, id)| format!("{k}/{id}"));
    }
    control.check()?;
    control.phase("fixing_candidate");
    let tree = staged
        .with_source_read(|path, g| {
            if g != audit.source_position.sequence {
                return Err(agentlaw_storage::Error::Stale(
                    "staged review changed".into(),
                ));
            }
            staged_tree_controlled(&root, path, local, control)
                .map_err(|e| agentlaw_storage::Error::ExternalOperation(e.to_string()))
        })
        .map_err(|e| {
            control
                .check()
                .err()
                .unwrap_or_else(|| DomainError::new("staged_capture", e.to_string()))
        })?;
    let token = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{import_ref}\n{tree}\n{}\n{generation}",
                audit.source_position.sequence
            )
            .as_bytes()
        )
    );
    let structural_conflicts = staged
        .import_conflicts()
        .map_err(|e| DomainError::new("import_conflict_read", e.to_string()))?;
    let next_action = match store.validate_imported_source(&staged, generation) {
        Ok(()) => None,
        Err(agentlaw_storage::Error::Stale(message)) => Some(message),
        Err(e) => return Err(DomainError::new("invalid_import_resolution", e.to_string())),
    };
    let resolution = ImportResolution {
        ready_for_user_confirmation: conflicts.is_empty()
            && next_action.is_none()
            && structural_conflicts.iter().all(|c| c.selected.is_some()),
        structural_conflicts,
        next_action,
        import_ref: import_ref.into(),
        resolution_token: token,
        reviewed_tree: tree,
        staged_generation: audit.source_position.sequence,
        conflicting_memory_ids: conflicts,
    };
    save_local_json(&dir.join("resolution.json"), &resolution)?;
    Ok(resolution)
}
/// Explicit user confirmation publishes C6 first, then performs a resumable Git ref CAS.
/// Same import/token resumes the durable handoff. No push and no active-index overwrite.
pub fn publish_import(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    import_ref: &str,
    resolution_token: &str,
    user_confirmed: bool,
    control: &agentlaw_contracts::RequestControl,
) -> Result<ImportPublishReceipt> {
    publish_import_observed(
        store,
        local,
        import_ref,
        resolution_token,
        user_confirmed,
        control,
        &|| Ok(()),
    )
}
pub(crate) fn publish_import_observed(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    import_ref: &str,
    resolution_token: &str,
    user_confirmed: bool,
    control: &agentlaw_contracts::RequestControl,
    after_publication: &dyn Fn() -> Result<()>,
) -> Result<ImportPublishReceipt> {
    control.check()?;
    control.phase("verifying_import_resolution");
    if !user_confirmed {
        return Err(DomainError::new(
            "confirmation_required",
            "Approve the exact resolved import before canonical publication.",
        ));
    }
    let _lane = lane(local)?;
    super::sync::require_git_slot_clear(local)?;
    let (dir, stored) = import_record(local, import_ref)?;
    let root = store
        .with_source_read(|r, _| Ok(r.to_path_buf()))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    if root.to_string_lossy() != stored.active_root {
        return Err(DomainError::new(
            "import_binding_mismatch",
            "Import belongs to another source.",
        ));
    }
    let resolution: ImportResolution = serde_json::from_reader(
        File::open(dir.join("resolution.json")).map_err(|_| io_error("resolved review"))?,
    )
    .map_err(|_| io_error("resolved review decode"))?;
    if resolution.resolution_token != resolution_token || !resolution.ready_for_user_confirmation {
        return Err(DomainError::new(
            "import_review_not_ready",
            "Resolve all concurrent heads and confirm the exact returned resolution token.",
        ));
    }
    if dir.join("completed.json").exists() {
        return serde_json::from_reader(
            File::open(dir.join("completed.json")).map_err(|_| io_error("import receipt"))?,
        )
        .map_err(|_| io_error("import receipt decode"));
    }
    let plan_path = dir.join("handoff.json");
    let plan: ImportHandoff = if plan_path.exists() {
        serde_json::from_reader(File::open(&plan_path).map_err(|_| io_error("handoff read"))?)
            .map_err(|_| io_error("handoff decode"))?
    } else {
        let staged = Store::open_read_only_with_coordination(
            &stored.review.staged_path,
            dir.join("runtime/canonical"),
            store.coordination_root(),
        )
        .map_err(|e| DomainError::new("staged_source_unavailable", e.to_string()))?;
        if optional_head(&root)? != stored.review.active_head
            || store
                .generation()
                .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?
                != stored.review.source_generation
        {
            return Err(DomainError::new(
                "stale_import_review",
                "Active HEAD/source changed after review.",
            ));
        }
        let tree = staged
            .with_source_read(|path, g| {
                if g != resolution.staged_generation {
                    return Err(agentlaw_storage::Error::Stale(
                        "staged generation changed".into(),
                    ));
                }
                staged_tree_controlled(&root, path, local, control)
                    .map_err(|e| agentlaw_storage::Error::ExternalOperation(e.to_string()))
            })
            .map_err(|e| {
                control
                    .check()
                    .err()
                    .unwrap_or_else(|| DomainError::new("stale_import_resolution", e.to_string()))
            })?;
        if tree != resolution.reviewed_tree {
            return Err(DomainError::new(
                "stale_import_resolution",
                "Resolved bytes changed after review.",
            ));
        }
        let incoming = dir.join("incoming");
        run_cancellable(
            &root,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "--",
                &git_path(&incoming).to_string_lossy(),
                &stored.review.incoming_commit,
            ],
            control,
        )?;
        let symbolic = command(&root)
            .args(["symbolic-ref", "-q", "HEAD"])
            .output()
            .map_err(|_| io_error("HEAD reference"))?;
        let reference = if symbolic.status.success() {
            String::from_utf8_lossy(&symbolic.stdout).trim().to_owned()
        } else {
            "HEAD".into()
        };
        let mut args = vec![
            "commit-tree".to_owned(),
            tree,
            "-p".into(),
            stored.review.incoming_commit.clone(),
        ];
        if let Some(head) = &stored.review.active_head {
            if *head != stored.review.incoming_commit {
                args.extend(["-p".into(), head.clone()]);
            }
        }
        args.extend([
            "-m".into(),
            format!("Agentlaw approved import {import_ref}"),
        ]);
        let commit = text(&root, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        let plan = ImportHandoff {
            index_plan: prepare_index_plan(&root, local, &resolution.reviewed_tree)?,
            operation_id: uuid::Uuid::new_v4().to_string(),
            resolution: resolution.clone(),
            commit_oid: commit,
            reference,
            expected_head: stored.review.active_head.clone(),
        };
        save_local_json(&plan_path, &plan)?;
        plan
    };
    if plan.resolution.resolution_token != resolution_token {
        return Err(DomainError::new(
            "handoff_token_mismatch",
            "A different approved import is already decided.",
        ));
    }
    let publication = if let Some(receipt) = store.import_receipt(&plan.operation_id, control)? {
        receipt
    } else {
        control.phase("publishing_import");
        store.publish_import(
            crate::store::lane::ImportPublishInput {
                operation_id: plan.operation_id.clone(),
                staged_root: stored.review.staged_path.clone(),
                staged_local: dir.join("runtime/canonical"),
                expected_generation: stored.review.source_generation,
                staged_generation: plan.resolution.staged_generation,
            },
            control,
        )?
    };
    after_publication()?;
    apply_ref_index(
        &root,
        &plan.reference,
        plan.expected_head.as_deref(),
        &plan.commit_oid,
        &plan.index_plan,
    )?;
    let result = ImportPublishReceipt {
        status: "published_and_committed".into(),
        publication,
        commit_oid: plan.commit_oid,
        pushed: false,
        active_index_preserved: false,
        unrelated_staging_preserved: true,
        canonical_index_updated: true,
    };
    save_local_json(&dir.join("completed.json"), &result)?;
    Ok(result)
}

pub(crate) fn io_error(stage: &str) -> DomainError {
    DomainError::new(
        "git_io",
        format!("Git {stage} failed; local memory was preserved."),
    )
}
pub(crate) fn command(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("--no-replace-objects")
        .arg("-c")
        .arg("core.longpaths=true")
        .arg("-C")
        .arg(git_path(repo))
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    c
}
pub(crate) fn git_path(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(normal) = value.strip_prefix(r"\\?\") {
        PathBuf::from(normal)
    } else {
        path.to_path_buf()
    }
}
pub(crate) fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = command(repo)
        .args(args)
        .output()
        .map_err(|_| io_error("command launch"))?;
    if !out.status.success() {
        return Err(DomainError::new(
            "git_command_failed",
            format!(
                "Git {} failed (exit {:?}); stderr omitted to avoid exposing credentials or remembered content.",
                args.first().unwrap_or(&"operation"),
                out.status.code()
            ),
        ));
    }
    Ok(out.stdout)
}
pub(crate) fn output_cancellable(
    repo: &Path,
    args: &[&str],
    control: &agentlaw_contracts::RequestControl,
) -> Result<std::process::Output> {
    let mut cmd = command(repo);
    cmd.args(args);
    wait_cancellable(cmd, control)
}
fn wait_cancellable(
    mut cmd: Command,
    control: &agentlaw_contracts::RequestControl,
) -> Result<std::process::Output> {
    wait_cancellable_tracked(&mut cmd, control, None)
}
fn wait_cancellable_tracked(
    cmd: &mut Command,
    control: &agentlaw_contracts::RequestControl,
    started: Option<&std::sync::atomic::AtomicBool>,
) -> Result<std::process::Output> {
    use std::io::Read;
    control.check()?;
    super::process::prepare(cmd);
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| io_error("command launch"))?;
    if let Some(started) = started {
        started.store(true, std::sync::atomic::Ordering::Release);
    }
    let mut tree = super::process::Tree::attach(&child).ok();
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io_error("command output"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| io_error("command output"))?;
    let (send, receive) = std::sync::mpsc::channel();
    let out_send = send.clone();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out_send.send((true, stdout.read_to_end(&mut bytes).map(|_| bytes)));
    });
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = send.send((false, stderr.read_to_end(&mut bytes).map(|_| bytes)));
    });
    let mut status = None;
    let mut output = None;
    let mut error_output = None;
    loop {
        if let Err(cancelled) = control.check() {
            if let Some(tree) = tree.take() {
                tree.terminate();
            } else {
                let _ = child.kill();
            }
            let _ = child.wait();
            return Err(cancelled);
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(value)) => status = Some(value),
                Ok(None) => {}
                Err(_) => {
                    if let Some(tree) = tree.take() {
                        tree.terminate();
                    } else {
                        let _ = child.kill();
                    }
                    let _ = child.wait();
                    return Err(io_error("command wait"));
                }
            }
        }
        if status.is_some() && output.is_some() && error_output.is_some() {
            break;
        }
        if output.is_some() && error_output.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        match receive.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok((true, bytes)) => output = Some(bytes.map_err(|_| io_error("command output"))?),
            Ok((false, bytes)) => {
                error_output = Some(bytes.map_err(|_| io_error("command output"))?)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(tree) = tree.take() {
                    tree.terminate();
                } else {
                    let _ = child.kill();
                }
                let _ = child.wait();
                return Err(io_error("command output"));
            }
        }
    }
    drop(tree);
    Ok(std::process::Output {
        status: status.expect("child status was collected"),
        stdout: output.expect("stdout was collected"),
        stderr: error_output.expect("stderr was collected"),
    })
}
fn run_cancellable(
    repo: &Path,
    args: &[&str],
    control: &agentlaw_contracts::RequestControl,
) -> Result<Vec<u8>> {
    let output = output_cancellable(repo, args, control)?;
    if !output.status.success() {
        return Err(DomainError::new("git_command_failed", format!("Git {} failed (exit {:?}); stderr omitted to avoid exposing credentials or remembered content.", args.first().unwrap_or(&"operation"), output.status.code())));
    }
    Ok(output.stdout)
}
pub(crate) fn text_cancellable(
    repo: &Path,
    args: &[&str],
    control: &agentlaw_contracts::RequestControl,
) -> Result<String> {
    String::from_utf8(run_cancellable(repo, args, control)?)
        .map(|text| text.trim().to_owned())
        .map_err(|_| io_error("non-UTF8 metadata"))
}
fn optional_head_cancellable(
    repo: &Path,
    control: &agentlaw_contracts::RequestControl,
) -> Result<Option<String>> {
    let output = output_cancellable(repo, &["rev-parse", "--verify", "HEAD"], control)?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}
fn git_root_cancellable(
    repo: &Path,
    control: &agentlaw_contracts::RequestControl,
) -> Result<PathBuf> {
    let root = text_cancellable(repo, &["rev-parse", "--show-toplevel"], control)?;
    let actual = fs::canonicalize(root).map_err(|_| io_error("repository root"))?;
    let expected = fs::canonicalize(repo).map_err(|_| io_error("source root"))?;
    if actual != expected {
        return Err(DomainError::new(
            "wrong_git_repository",
            "Memory operations cannot use an enclosing project repository.",
        ));
    }
    Ok(actual)
}
fn endpoint_cancellable(
    repo: &Path,
    remote: &str,
    control: &agentlaw_contracts::RequestControl,
) -> Result<String> {
    if remote.is_empty() || remote.starts_with('-') {
        return Err(DomainError::new(
            "invalid_remote",
            "Use an explicit configured remote name.",
        ));
    }
    let values = text_cancellable(
        repo,
        &["remote", "get-url", "--push", "--all", remote],
        control,
    )?;
    if values.lines().count() != 1 {
        return Err(DomainError::new(
            "ambiguous_destination",
            "Exactly one push URL is required for a bound sharing review.",
        ));
    }
    Ok(values)
}
pub(crate) fn text(repo: &Path, args: &[&str]) -> Result<String> {
    String::from_utf8(run(repo, args)?)
        .map(|s| s.trim().to_string())
        .map_err(|_| io_error("non-UTF8 metadata"))
}
pub(crate) fn optional_head(repo: &Path) -> Result<Option<String>> {
    let out = command(repo)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map_err(|_| io_error("read HEAD"))?;
    if out.status.success() {
        Ok(Some(String::from_utf8_lossy(&out.stdout).trim().into()))
    } else {
        Ok(None)
    }
}
pub(crate) fn git_root(repo: &Path) -> Result<PathBuf> {
    let root = text(repo, &["rev-parse", "--show-toplevel"])?;
    let actual = fs::canonicalize(root).map_err(|_| io_error("repository root"))?;
    let expected = fs::canonicalize(repo).map_err(|_| io_error("source root"))?;
    if actual != expected {
        return Err(DomainError::new(
            "wrong_git_repository",
            "Memory operations cannot use an enclosing project repository.",
        ));
    }
    Ok(actual)
}
pub(crate) fn lane(local: &Path) -> Result<File> {
    fs::create_dir_all(local).map_err(|_| io_error("control directory"))?;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(local.join("git-lane.lock"))
        .map_err(|_| io_error("lane"))?;
    f.lock_exclusive().map_err(|_| io_error("lane lock"))?;
    Ok(f)
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn valid_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Capture under C6's source read gate, then create a commit and CAS the expected ref.
/// No fetch, merge, push, user-index write, author invention or force-update occurs.
pub fn continuity_save(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    message: &str,
) -> Result<SaveReceipt> {
    let _lane = lane(local)?;
    super::sync::require_git_slot_clear(local)?;
    let handoff_path = local.join("save-handoff.json");
    if handoff_path.exists() {
        let pending: SaveHandoff = serde_json::from_reader(
            File::open(&handoff_path).map_err(|_| io_error("save handoff read"))?,
        )
        .map_err(|_| io_error("save handoff decode"))?;
        let root = store
            .with_source_read(|r, _| Ok(r.to_path_buf()))
            .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
        if root != pending.root {
            return Err(io_error("save handoff binding"));
        }
        apply_ref_index(
            &root,
            &pending.reference,
            pending.parent.as_deref(),
            &pending.receipt.commit_oid,
            &pending.index_plan,
        )?;
        fs::remove_file(&handoff_path).map_err(|_| io_error("completed save handoff"))?;
    }
    let index = local.join(format!("capture-{}.index", uuid::Uuid::new_v4()));
    let captured = store
        .with_source_read(|root, generation| {
            let result = (|| -> Result<_> {
                git_root(root)?;
                let parent = optional_head(root)?;
                let symbolic = command(root)
                    .args(["symbolic-ref", "-q", "HEAD"])
                    .output()
                    .map_err(|_| io_error("symbolic HEAD"))?;
                let reference = if symbolic.status.success() {
                    String::from_utf8_lossy(&symbolic.stdout).trim().to_string()
                } else {
                    "HEAD".to_string()
                };
                let mut read = command(root);
                read.env("GIT_INDEX_FILE", git_path(&index))
                    .arg("read-tree");
                if let Some(p) = &parent {
                    read.arg(p);
                } else {
                    read.arg("--empty");
                }
                if !read
                    .output()
                    .map_err(|_| io_error("private index"))?
                    .status
                    .success()
                {
                    return Err(io_error("private index initialization"));
                }
                let mut paths = BTreeSet::new();
                for dir in ["current", "history", "catalog"] {
                    let base = root.join(dir);
                    if !base.exists() {
                        continue;
                    }
                    let mut stack = vec![base];
                    while let Some(path) = stack.pop() {
                        for entry in
                            fs::read_dir(path).map_err(|_| io_error("canonical enumeration"))?
                        {
                            let entry = entry.map_err(|_| io_error("canonical entry"))?;
                            let ty = entry.file_type().map_err(|_| io_error("canonical type"))?;
                            if ty.is_symlink() {
                                return Err(DomainError::new(
                                    "unsupported_source_symlink",
                                    "Canonical Git capture does not follow symlinks.",
                                ));
                            }
                            if ty.is_dir() {
                                stack.push(entry.path());
                            } else if entry.path().extension().is_some_and(|s| s == "md") {
                                paths.insert(
                                    entry
                                        .path()
                                        .strip_prefix(root)
                                        .map_err(|_| io_error("canonical path"))?
                                        .to_string_lossy()
                                        .replace('\\', "/"),
                                );
                            }
                        }
                    }
                }
                for p in ["format.md", ".gitattributes"] {
                    if root.join(p).is_file() {
                        paths.insert(p.into());
                    }
                }
                // Include already tracked canonical names so a reviewed deletion is captured too.
                for path in run(
                    root,
                    &[
                        "ls-files",
                        "-z",
                        "--",
                        "current",
                        "history",
                        "catalog",
                        "format.md",
                        ".gitattributes",
                    ],
                )?
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                {
                    let path = std::str::from_utf8(path)
                        .map_err(|_| io_error("canonical filename UTF8"))?;
                    if path.ends_with(".md") || path == ".gitattributes" {
                        paths.insert(path.into());
                    }
                }
                let paths: Vec<_> = paths.into_iter().collect();
                for group in paths.chunks(128) {
                    let out = command(root)
                        .env("GIT_INDEX_FILE", git_path(&index))
                        .args(["add", "-A", "-f", "--"])
                        .args(group)
                        .output()
                        .map_err(|_| io_error("private canonical add"))?;
                    if !out.status.success() {
                        return Err(io_error("private canonical add"));
                    }
                }
                let out = command(root)
                    .env("GIT_INDEX_FILE", git_path(&index))
                    .arg("write-tree")
                    .output()
                    .map_err(|_| io_error("write-tree"))?;
                if !out.status.success() {
                    return Err(io_error("write-tree"));
                }
                let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
                Ok((root.to_path_buf(), generation, parent, reference, tree))
            })();
            result.map_err(|e| {
                agentlaw_storage::Error::ExternalOperation(format!("{}: {}", e.code, e.message))
            })
        })
        .map_err(|e| DomainError::new("git_capture_failed", e.to_string()));
    let _ = fs::remove_file(&index);
    let (repo, generation, parent, reference, tree) = captured?;
    if let Some(parent) = &parent {
        if text(&repo, &["rev-parse", &format!("{parent}^{{tree}}")])? == tree {
            return Ok(SaveReceipt {
                status: "unchanged".into(),
                commit_oid: parent.clone(),
                tree_oid: tree,
                source_generation: generation,
                pushed: false,
            });
        }
    }
    let mut cmd = command(&repo);
    cmd.arg("commit-tree").arg(&tree);
    if let Some(parent) = &parent {
        cmd.arg("-p").arg(parent);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| io_error("commit-tree"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| io_error("commit message"))?
        .write_all(message.as_bytes())
        .map_err(|_| io_error("commit message"))?;
    let output = child
        .wait_with_output()
        .map_err(|_| io_error("commit-tree wait"))?;
    if !output.status.success() {
        return Err(DomainError::new(
            "git_commit_failed",
            "Git could not create the local commit. Configure Git author/signing policy explicitly; no identity was invented and no push occurred.",
        ));
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !valid_oid(&commit) {
        return Err(io_error("commit OID"));
    }
    let receipt = SaveReceipt {
        status: "committed".into(),
        commit_oid: commit,
        tree_oid: tree,
        source_generation: generation,
        pushed: false,
    };
    let plan = SaveHandoff {
        root: repo.clone(),
        reference,
        parent,
        index_plan: prepare_index_plan(&repo, local, &receipt.tree_oid)?,
        receipt: receipt.clone(),
    };
    save_local_json(&handoff_path, &plan)?;
    apply_ref_index(
        &repo,
        &plan.reference,
        plan.parent.as_deref(),
        &plan.receipt.commit_oid,
        &plan.index_plan,
    )?;
    fs::remove_file(handoff_path).map_err(|_| io_error("completed save handoff"))?;
    Ok(receipt)
}

pub(crate) fn endpoint(repo: &Path, remote: &str) -> Result<String> {
    if remote.is_empty() || remote.starts_with('-') {
        return Err(DomainError::new(
            "invalid_remote",
            "Use an explicit configured remote name.",
        ));
    }
    let values = text(repo, &["remote", "get-url", "--push", "--all", remote])?;
    if values.lines().count() != 1 {
        return Err(DomainError::new(
            "ambiguous_destination",
            "Exactly one push URL is required for a bound sharing review.",
        ));
    }
    Ok(values)
}
fn save_review(local: &Path, id: &str, value: &StoredReview) -> Result<()> {
    let dir = local.join("share-reviews");
    fs::create_dir_all(&dir).map_err(|_| io_error("review directory"))?;
    let path = dir.join(format!("{id}.json"));
    let bytes = serde_json::to_vec(value).map_err(|_| io_error("review encoding"))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| io_error("review create"))?;
    file.write_all(&bytes)
        .map_err(|_| io_error("review write"))?;
    file.sync_all().map_err(|_| io_error("review sync"))
}
pub fn inspect_share(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    remote: &str,
    target_ref: &str,
) -> Result<ShareReview> {
    let _lane = lane(local)?;
    let repo = store
        .with_source_read(|root, _| Ok(root.to_path_buf()))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    git_root(&repo)?;
    if !target_ref.starts_with("refs/heads/") {
        return Err(DomainError::new(
            "invalid_target_ref",
            "Use an explicit refs/heads/... destination.",
        ));
    }
    run(&repo, &["check-ref-format", target_ref])?;
    let endpoint = endpoint(&repo, remote)?;
    let commit = optional_head(&repo)?.ok_or_else(|| {
        DomainError::new(
            "commit_required",
            "Create a local continuity commit before sharing.",
        )
    })?;
    let tree = text(&repo, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    let review_id = uuid::Uuid::new_v4().to_string();
    let scan = super::git_scan::seal(&repo, &commit, &local.join("share-scans").join(&review_id))?;
    let closure_digest = scan.object_set_digest.clone();
    let findings = scan.findings.clone();
    let review = ShareReview {
        review_ref: review_id,
        commit_oid: commit,
        tree_oid: tree,
        remote: remote.into(),
        target_ref: target_ref.into(),
        policy_version: POLICY.into(),
        findings,
        includes_all_reachable_history: true,
    };
    save_review(
        local,
        &review.review_ref,
        &StoredReview {
            review: review.clone(),
            repo: repo.to_string_lossy().into(),
            endpoint_digest: hash(endpoint.as_bytes()),
            closure_digest,
            scan: Some(scan),
        },
    )?;
    Ok(review)
}
pub fn push_review(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    review_id: &str,
    accept_findings: bool,
    control: &agentlaw_contracts::RequestControl,
) -> Result<PushReceipt> {
    control.check()?;
    control.phase("pushing_reviewed_commit");
    if uuid::Uuid::parse_str(review_id).is_err() {
        return Err(DomainError::new(
            "invalid_review_ref",
            "Use the returned review reference.",
        ));
    }
    let _lane = lane(local)?;
    let path = local
        .join("share-reviews")
        .join(format!("{review_id}.json"));
    let stored: StoredReview =
        serde_json::from_reader(File::open(path).map_err(|_| io_error("review read"))?)
            .map_err(|_| io_error("review decode"))?;
    let r = &stored.review;
    let repo = store
        .with_source_read(|root, _| Ok(root.to_path_buf()))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    let bound_endpoint = endpoint_cancellable(&repo, &r.remote, control)?;
    if repo.to_string_lossy() != stored.repo
        || r.review_ref != review_id
        || r.policy_version != POLICY
        || hash(bound_endpoint.as_bytes()) != stored.endpoint_digest
    {
        return Err(DomainError::new(
            "sharing_review_stale",
            "Repository, destination, or scan policy changed; inspect again.",
        ));
    }
    let scan=stored.scan.as_ref().ok_or_else(||DomainError::new("sharing_scan_receipt_required","This older review has no sealed scan receipt. Inspect once with the current build before sharing."))?;
    control.check()?;
    super::git_scan::validate(scan)?;
    control.check()?;
    if scan.object_set_digest != stored.closure_digest
        || scan.findings != r.findings
        || scan.tree_oid != r.tree_oid
        || scan.commit_oid != r.commit_oid
    {
        return Err(DomainError::new(
            "sharing_review_stale",
            "Reviewed immutable content no longer matches.",
        ));
    }
    if !r.findings.is_empty() && !accept_findings {
        return Err(DomainError::new(
            "sharing_choice_required",
            "Potential secret patterns occur in outgoing history. Ask whether to share unchanged or cancel. Masking/exclusion remain HOLD-04; no source or Git history was modified.",
        ));
    }
    let refspec = format!("{}:{}", r.commit_oid, r.target_ref);
    control.check()?;
    let started = std::sync::atomic::AtomicBool::new(false);
    let mut cmd = command(&scan.transfer_store);
    cmd.args(["push", "--porcelain", "--", &bound_endpoint, &refspec]);
    let outcome = match wait_cancellable_tracked(&mut cmd, control, Some(&started)) {
        Ok(output) => output,
        Err(error)
            if started.load(std::sync::atomic::Ordering::Acquire)
                && matches!(error.code.as_str(), "cancelled" | "git_process_containment") =>
        {
            return Ok(PushReceipt {
                status: "sharing_outcome_unknown".into(),
                commit_oid: r.commit_oid.clone(),
                target_ref: r.target_ref.clone(),
                local_source_preserved: true,
                review_ref: Some(review_id.to_owned()),
            })
        }
        Err(error) => return Err(error),
    };
    let status = if outcome.status.success() {
        "pushed"
    } else {
        let remote = output_cancellable(
            &repo,
            &["ls-remote", "--refs", "--", &bound_endpoint, &r.target_ref],
            control,
        );
        match remote {
            Ok(out)
                if out.status.success()
                    && String::from_utf8_lossy(&out.stdout)
                        .split_whitespace()
                        .next()
                        == Some(r.commit_oid.as_str()) =>
            {
                "pushed_confirmed_after_response_loss"
            }
            Err(error) if error.code == "cancelled" => "sharing_outcome_unknown",
            _ => "sharing_outcome_unknown",
        }
    };
    Ok(PushReceipt {
        status: status.into(),
        commit_oid: r.commit_oid.clone(),
        target_ref: r.target_ref.clone(),
        local_source_preserved: true,
        review_ref: if status == "sharing_outcome_unknown" {
            Some(review_id.to_owned())
        } else {
            None
        },
    })
}

/// Network fetch writes only an isolated bare object/ref cache, never active files or HEAD.
pub fn fetch_share(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    remote: &str,
    control: &agentlaw_contracts::RequestControl,
) -> Result<FetchReceipt> {
    control.check()?;
    control.phase("fetching_remote");
    let _lane = lane(local)?;
    let repo = store
        .with_source_read(|root, _| Ok(root.to_path_buf()))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    git_root_cancellable(&repo, control)?;
    if remote.is_empty() || remote.starts_with('-') {
        return Err(DomainError::new(
            "invalid_remote",
            "Use a configured remote name.",
        ));
    }
    let url = text_cancellable(&repo, &["remote", "get-url", "--all", remote], control)?;
    if url.lines().count() != 1 {
        return Err(DomainError::new(
            "ambiguous_destination",
            "Exactly one fetch URL is required.",
        ));
    }
    let fetch_ref = uuid::Uuid::new_v4().to_string();
    let cache = local.join("fetch-cache").join(&fetch_ref);
    fs::create_dir_all(&cache).map_err(|_| io_error("fetch cache"))?;
    run_cancellable(&cache, &["init", "--bare"], control)?;
    run_cancellable(
        &cache,
        &[
            "fetch",
            "--no-tags",
            "--",
            &url,
            "+refs/heads/*:refs/agentlaw/incoming/*",
        ],
        control,
    )?;
    let refs = text_cancellable(
        &cache,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/agentlaw/incoming/",
        ],
        control,
    )?;
    let fetched_refs = refs.lines().map(str::to_owned).collect();
    Ok(FetchReceipt {
        fetch_ref,
        remote: remote.into(),
        fetched_refs,
        active_source_unchanged: true,
    })
}
/// Prepare the incoming immutable tree for review. This does NOT claim that a clean
/// checkout is a semantic merge or authorize replacing active current/history.
pub fn prepare_import(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    commit: &str,
    control: &agentlaw_contracts::RequestControl,
) -> Result<ImportReview> {
    prepare_import_with_ref(
        store,
        local,
        commit,
        &uuid::Uuid::new_v4().to_string(),
        control,
    )
}
pub(crate) fn prepare_import_with_ref(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    commit: &str,
    import_ref: &str,
    control: &agentlaw_contracts::RequestControl,
) -> Result<ImportReview> {
    control.check()?;
    control.phase("acquiring_import");
    if !valid_oid(commit) {
        return Err(DomainError::new(
            "invalid_commit",
            "Use the complete immutable commit OID returned by fetch/inspection.",
        ));
    }
    let _lane = lane(local)?;
    let (active_root, generation) = store
        .with_source_read(|root, g| Ok((root.to_path_buf(), g)))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    git_root_cancellable(&active_root, control)?;
    let mut source = None;
    control.phase("verifying_import");
    let candidate = output_cancellable(
        &active_root,
        &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        control,
    )?;
    if candidate.status.success() {
        source = Some(active_root.clone());
    } else {
        let caches = local.join("fetch-cache");
        if caches.exists() {
            for entry in fs::read_dir(caches).map_err(|_| io_error("fetch-cache inventory"))? {
                let path = entry.map_err(|_| io_error("fetch-cache entry"))?.path();
                let found = output_cancellable(
                    &path,
                    &["cat-file", "-e", &format!("{commit}^{{commit}}")],
                    control,
                )?;
                if found.status.success() {
                    source = Some(path);
                    break;
                }
            }
        }
    }
    let source = source.ok_or_else(|| {
        DomainError::new(
            "incoming_commit_unavailable",
            "Fetch the requested immutable commit into the preparation cache first.",
        )
    })?;
    let import_ref = import_ref.to_owned();
    control.phase("building_review_workspace");
    control.check()?;
    let dir = local.join("imports").join(&import_ref);
    let staged = dir.join("incoming");
    fs::create_dir_all(&staged).map_err(|_| io_error("staged source"))?;
    run_cancellable(&staged, &["init"], control)?;
    run_cancellable(
        &staged,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
            &git_path(&source).to_string_lossy(),
            commit,
        ],
        control,
    )?;
    run_cancellable(&staged, &["checkout", "--detach", commit], control)?;
    let validated = Store::attach_existing_with_coordination(
        &staged,
        dir.join("incoming-control"),
        store.coordination_root(),
    )
    .map_err(|e| DomainError::new("incoming_source_invalid", e.to_string()))?;
    let tree = text_cancellable(
        &staged,
        &["rev-parse", &format!("{commit}^{{tree}}")],
        control,
    )?;
    let merged = dir.join("source");
    let union = store
        .prepare_import_union(&validated, &merged, &dir.join("runtime/canonical"))
        .map_err(|e| DomainError::new("import_union_requires_review", e.to_string()))?;
    let review = ImportReview {
        completion: None,
        conflicting_memory_ids: conflicting_current_ids(&union)?,
        next_action: Some("Inspect structural_conflicts; use share import call with exact conflicting memory IDs to read full heads, then explicitly reconcile and resolve.".into()),
        structural_conflicts: union
            .import_conflicts()
            .map_err(|e| DomainError::new("import_conflict_read", e.to_string()))?,
        import_ref: import_ref.clone(),
        incoming_commit: commit.into(),
        reviewed_tree: tree,
        active_head: optional_head_cancellable(&active_root, control)?,
        source_generation: generation,
        status: "review_required".into(),
        staged_path: merged,
        active_source_unchanged: true,
        canonical_publish_supported: true,
    };
    let record = StoredImport {
        review: review.clone(),
        active_root: active_root.to_string_lossy().into(),
    };
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dir.join("review.json"))
        .map_err(|_| io_error("import review create"))?;
    file.write_all(&serde_json::to_vec(&record).map_err(|_| io_error("import review serialize"))?)
        .map_err(|_| io_error("import review write"))?;
    file.sync_all()
        .map_err(|_| io_error("import review sync"))?;
    Ok(review)
}
pub fn inspect_import(
    store: &super::handle::GitStoreHandle,
    local: &Path,
    import_ref: &str,
    control: &agentlaw_contracts::RequestControl,
) -> Result<ImportReview> {
    control.check()?;
    if uuid::Uuid::parse_str(import_ref).is_err() {
        return Err(DomainError::new(
            "invalid_import_ref",
            "Use the returned import reference.",
        ));
    }
    let _lane = lane(local)?;
    let path = local.join("imports").join(import_ref).join("review.json");
    let stored: StoredImport =
        serde_json::from_reader(File::open(path).map_err(|_| io_error("import review read"))?)
            .map_err(|_| io_error("import review decode"))?;
    let (root, generation) = store
        .with_source_read(|r, g| Ok((r.to_string_lossy().to_string(), g)))
        .map_err(|e| DomainError::new("source_unavailable", e.to_string()))?;
    if root != stored.active_root || stored.review.import_ref != import_ref {
        return Err(DomainError::new(
            "import_binding_mismatch",
            "The staged import belongs to a different source binding.",
        ));
    }
    let mut review = stored.review;
    let dir = local.join("imports").join(import_ref);
    if dir.join("completed.json").exists() {
        let completed: ImportPublishReceipt = serde_json::from_reader(
            File::open(dir.join("completed.json")).map_err(|_| io_error("import completion"))?,
        )
        .map_err(|_| io_error("import completion decode"))?;
        review.status = completed.status.clone();
        review.completion = Some(completed);
        review.active_source_unchanged = false;
        review.next_action=Some("Local memory publication and Git handoff completed; pushed=false means no remote sharing. Later memory saves do not invalidate this receipt.".into());
        return Ok(review);
    }
    if dir.join("handoff.json").exists() {
        let handoff: ImportHandoff = serde_json::from_reader(
            File::open(dir.join("handoff.json")).map_err(|_| io_error("import handoff"))?,
        )
        .map_err(|_| io_error("import handoff decode"))?;
        let applied = store
            .import_receipt(&handoff.operation_id, control)?
            .is_some();
        review.status = if applied {
            "canonical_published_git_handoff_pending"
        } else {
            "approved_publication_prepared"
        }
        .into();
        review.active_source_unchanged = !applied;
        review.next_action=Some("Resume publish with the retained resolution token. Finish only the unfinished publication/Git handoff; do not prepare a new import.".into());
        return Ok(review);
    }
    let stage = Store::open_read_only_with_coordination(
        &review.staged_path,
        local
            .join("imports")
            .join(import_ref)
            .join("runtime/canonical"),
        store.coordination_root(),
    )
    .map_err(|e| DomainError::new("staged_source_unavailable", e.to_string()))?;
    review.structural_conflicts = stage
        .import_conflicts()
        .map_err(|e| DomainError::new("import_conflict_read", e.to_string()))?;
    review.conflicting_memory_ids = conflicting_current_ids(&stage)?;
    review.next_action = Some("Use share import call with exact conflicting_memory_ids to read all heads; explicitly choose retained structural sides, then reconcile through staged evolve/consolidate and resolve.".into());
    if review
        .structural_conflicts
        .iter()
        .any(|c| c.selected.is_none())
    {
        review.status = "structural_choices_required".into();
    }
    let tree = text(
        &local.join("imports").join(import_ref).join("incoming"),
        &["rev-parse", &format!("{}^{{tree}}", review.incoming_commit)],
    )?;
    if tree != review.reviewed_tree {
        return Err(DomainError::new(
            "import_integrity_failed",
            "The reviewed immutable tree no longer matches.",
        ));
    }
    if generation != review.source_generation {
        review.status = "review_required_source_changed".into();
    }
    Ok(review)
}

#[cfg(test)]
mod tests;
