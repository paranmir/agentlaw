//! Pinned cleanup of owned managed-install artifacts. A preview is never a deletion.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ManifestEntry {
    relative: PathBuf,
    kind: String,
    identity: String,
    sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CleanupItem {
    kind: String,
    source: PathBuf,
    holding: PathBuf,
    manifest: Vec<ManifestEntry>,
    phase: String,
    #[serde(default)]
    removed_entries: BTreeSet<PathBuf>,
    #[serde(default)]
    entry_intent: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CleanupPlan {
    id: String,
    root: PathBuf,
    marker_sha256: String,
    permanent_internal_removal_approved: bool,
    items: Vec<CleanupItem>,
}

fn cleanup_plan_path(root: &Path, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id)
        .map_err(|_| error("invalid_arguments", "Use the exact cleanup plan ID."))?;
    Ok(root.join("state/cleanup-plans").join(format!("{id}.json")))
}

fn cleanup_load(root: &Path, id: &str) -> Result<CleanupPlan> {
    let raw = fs::read(cleanup_plan_path(root, id)?)
        .map_err(|_| error("cleanup_plan_missing", "The cleanup plan is missing."))?;
    if raw.len() > 4 * 1024 * 1024 {
        return Err(error(
            "cleanup_plan_invalid",
            "The cleanup plan is too large.",
        ));
    }
    let plan: CleanupPlan = serde_json::from_slice(&raw)
        .map_err(|_| error("cleanup_plan_invalid", "The cleanup plan is invalid."))?;
    if plan.id != id
        || plan.root != root
        || plan.marker_sha256 != digest(&root.join(".agentlaw-layout"))?
    {
        return Err(error(
            "cleanup_plan_invalid",
            "The managed root or layout marker changed.",
        ));
    }
    Ok(plan)
}

fn cleanup_save(plan: &CleanupPlan) -> Result<()> {
    atomic_json(&cleanup_plan_path(&plan.root, &plan.id)?, plan)
}

fn entry_identity(path: &Path, _metadata: &fs::Metadata) -> Result<String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            core::PCWSTR,
            Win32::{
                Foundation::CloseHandle,
                Storage::FileSystem::{
                    CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
                    FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
                    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
                },
            },
        };
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(|_| {
            error(
                "inspection_unknown",
                "Cannot open a cleanup object by handle.",
            )
        })?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let read = unsafe { GetFileInformationByHandle(handle, &mut info) };
        let _ = unsafe { CloseHandle(handle) };
        read.map_err(|_| {
            error(
                "inspection_unknown",
                "Cannot obtain a cleanup object's file ID.",
            )
        })?;
        Ok(format!(
            "{}:{}",
            info.dwVolumeSerialNumber,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64
        ))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!("{}:{}", _metadata.dev(), _metadata.ino()))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok(format!(
            "{:?}:{:?}:{}",
            _metadata.created().ok(),
            _metadata.modified().ok(),
            _metadata.len()
        ))
    }
}

fn manifest_entry(root: &Path, path: &Path) -> Result<ManifestEntry> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| error("cleanup_scope", "Cleanup escaped its approved directory."))?
        .to_path_buf();
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        error(
            "cleanup_drift",
            "A cleanup item disappeared or cannot be inspected.",
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(error("cleanup_scope", "A cleanup item contains a link."));
    }
    #[cfg(windows)]
    if std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 0x400 != 0 {
        return Err(error(
            "cleanup_scope",
            "A cleanup item contains a reparse point.",
        ));
    }
    let kind = if metadata.is_dir() {
        "directory"
    } else if metadata.is_file() {
        "file"
    } else {
        return Err(error(
            "cleanup_scope",
            "A cleanup item has an unsupported file type.",
        ));
    };
    Ok(ManifestEntry {
        relative,
        kind: kind.into(),
        identity: entry_identity(path, &metadata)?,
        sha256: (kind == "file").then(|| digest(path)).transpose()?,
    })
}

fn complete_manifest(path: &Path) -> Result<Vec<ManifestEntry>> {
    let mut out = Vec::new();
    let root_metadata = fs::symlink_metadata(path)
        .map_err(|_| error("cleanup_drift", "Cannot inspect the cleanup root."))?;
    let root_identity = entry_identity(path, &root_metadata)?;
    let root_volume = root_identity.split(':').next();
    let mut pending = vec![path.to_path_buf()];
    while let Some(entry) = pending.pop() {
        let info = manifest_entry(path, &entry)?;
        if info.identity.split(':').next() != root_volume {
            return Err(error(
                "cleanup_scope",
                "A cleanup item crosses a volume or mount boundary.",
            ));
        }
        if info.kind == "directory" {
            for child in fs::read_dir(&entry)
                .map_err(|_| error("cleanup_drift", "Cannot enumerate a cleanup directory."))?
            {
                pending.push(
                    child
                        .map_err(|_| error("cleanup_drift", "Cannot inspect a cleanup entry."))?
                        .path(),
                );
            }
        }
        out.push(info);
        if out.len() > 4096 {
            return Err(error(
                "cleanup_scope",
                "A cleanup item is too large to pin safely.",
            ));
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

fn complete_manifest_at(path: &Path, expected: &[ManifestEntry]) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    Ok(complete_manifest(path)? == expected)
}

fn is_owned_version(path: &Path, entries: &[ManifestEntry]) -> bool {
    if entries.len() != 2 || entries[0].relative != Path::new("") {
        return false;
    }
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some((release, prefix)) = name.rsplit_once('-') else {
        return false;
    };
    !release.is_empty()
        && prefix.len() == 16
        && prefix.bytes().all(|b| b.is_ascii_hexdigit())
        && entries[1].relative == Path::new(binary_name())
        && entries[1]
            .sha256
            .as_ref()
            .is_some_and(|hash| hash.starts_with(prefix))
}

fn is_owned_stage(path: &Path, plan: &Plan, entries: &[ManifestEntry]) -> bool {
    let names: BTreeSet<PathBuf> = entries.iter().map(|entry| entry.relative.clone()).collect();
    let expected: BTreeSet<PathBuf> = [
        PathBuf::new(),
        PathBuf::from(&plan.archive),
        PathBuf::from("launcher.asset"),
    ]
    .into_iter()
    .collect();
    path == stage(&plan.root, &plan.id)
        && names == expected
        && entries.iter().any(|entry| {
            entry.relative == Path::new(&plan.archive)
                && entry.sha256.as_ref() == Some(&plan.archive_digest)
        })
        && plan.launcher_digest.as_ref().is_some_and(|hash| {
            entries.iter().any(|entry| {
                entry.relative == Path::new("launcher.asset") && entry.sha256.as_ref() == Some(hash)
            })
        })
}

fn referenced_by_processes(root: &Path, path: &Path) -> Result<bool> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let own = get_current_pid()
        .ok()
        .ok_or_else(|| error("inspection_unknown", "Cannot inspect the cleanup process."))?;
    if !system.processes().contains_key(&own) {
        return Err(error(
            "inspection_unknown",
            "The cleanup process is absent from the process snapshot.",
        ));
    }
    for (pid, process) in system.processes() {
        let Some(exe) = process.exe() else {
            if process
                .name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("agentlaw")
            {
                return Err(error(
                    "inspection_unknown",
                    "An Agentlaw process has an unknown executable path.",
                ));
            }
            continue;
        };
        let exe = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        if *pid == own && exe.starts_with(path) {
            return Ok(true);
        }
        if exe.starts_with(path) {
            return Ok(true);
        }
        if exe.starts_with(root) && !exe.exists() {
            return Err(error(
                "inspection_unknown",
                "A relevant process path cannot be verified.",
            ));
        }
    }
    Ok(false)
}

fn referenced_by_registrations(root: &Path, path: &Path) -> Result<bool> {
    for registration in registration_list(root)? {
        let receipt: Value = agentlaw_contracts::validation::decode_unique(
            &registration.receipt_before,
        )
        .map_err(|_| {
            error(
                "inspection_unknown",
                "A managed registration receipt is invalid.",
            )
        })?;
        let executable = receipt["executable"]
            .as_str()
            .ok_or_else(|| error("inspection_unknown", "A receipt lacks its executable path."))?;
        if Path::new(executable).starts_with(path) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn unfinished_work(root: &Path, path: &Path) -> Result<bool> {
    if root.join("state/install-pending.json").exists() {
        return Ok(true);
    }
    let plans = root.join("state/update-plans");
    if plans.exists() {
        for entry in fs::read_dir(plans)
            .map_err(|_| error("inspection_unknown", "Cannot inspect update plans."))?
        {
            let entry =
                entry.map_err(|_| error("inspection_unknown", "Cannot enumerate update plans."))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            let plan = load(root, id)?;
            if plan.phase != "completed" {
                if path == stage(root, id) || path == previous_bundle(&plan) {
                    return Ok(true);
                }
                for registration in &plan.registrations {
                    for receipt in [
                        Some(registration.receipt_before.as_str()),
                        registration.receipt_after.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        let value: Value = agentlaw_contracts::validation::decode_unique(receipt)
                            .map_err(|_| {
                            error(
                                "inspection_unknown",
                                "An unfinished plan has an invalid registration receipt.",
                            )
                        })?;
                        if value["executable"]
                            .as_str()
                            .is_some_and(|target| Path::new(target).starts_with(path))
                        {
                            return Ok(true);
                        }
                    }
                }
            }
        }
    }
    Ok(false)
}

fn eligible(root: &Path, kind: &str, path: &Path) -> Result<bool> {
    if unfinished_work(root, path)?
        || referenced_by_registrations(root, path)?
        || referenced_by_processes(root, path)?
    {
        return Ok(false);
    }
    if kind == "version" {
        return Ok(true);
    }
    let id = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| {
            name.strip_prefix(if kind == "stage" {
                ".update-"
            } else {
                ".bin-previous-"
            })
        })
        .ok_or_else(|| error("cleanup_scope", "The cleanup item has no update plan ID."))?;
    let plan = load(root, id)?;
    Ok(plan.phase == "completed" && plan.activation_verified && plan.recovery_obligations_closed)
}

fn validate_item(plan: &CleanupPlan, index: usize) -> Result<bool> {
    let item = &plan.items[index];
    let expected_holding = plan
        .root
        .join(".cleanup-holding")
        .join(&plan.id)
        .join(index.to_string());
    if item.holding != expected_holding
        || item.source == item.holding
        || !item.source.starts_with(&plan.root)
    {
        return Err(error(
            "cleanup_plan_invalid",
            "A cleanup plan path escaped its pinned scope.",
        ));
    }
    let owned = match item.kind.as_str() {
        "version" => {
            item.source.parent() == Some(plan.root.join("state/versions").as_path())
                && is_owned_version(&item.source, &item.manifest)
        }
        "stage" => item
            .source
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(".update-"))
            .and_then(|id| load(&plan.root, id).ok())
            .is_some_and(|update| is_owned_stage(&item.source, &update, &item.manifest)),
        "previous" => item
            .source
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(".bin-previous-"))
            .and_then(|id| load(&plan.root, id).ok())
            .is_some_and(|update| {
                item.source == previous_bundle(&update)
                    && item
                        .manifest
                        .iter()
                        .filter(|entry| entry.kind == "file")
                        .count()
                        == 3
                    && update.bin_before_hashes
                        == item
                            .manifest
                            .iter()
                            .filter_map(|entry| {
                                entry
                                    .relative
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .zip(entry.sha256.clone())
                                    .map(|(name, hash)| (name.to_owned(), hash))
                            })
                            .collect()
            }),
        _ => false,
    };
    if !owned {
        return Err(error(
            "cleanup_plan_invalid",
            "An item is not an owned managed artifact.",
        ));
    }
    eligible(&plan.root, &item.kind, &item.source)
}

#[cfg(windows)]
fn move_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{core::PCWSTR, Win32::Storage::FileSystem::MoveFileW};
    let from: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe { MoveFileW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr())) }
        .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn move_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let to = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn exists_exact(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(error(
            "inspection_unknown",
            "Cannot inspect a cleanup path.",
        )),
    }
}

fn worker_lifetime_guards(root: &Path) -> Result<Option<Vec<fs::File>>> {
    let worker = root.join("state/worker");
    fs::create_dir_all(&worker)
        .map_err(|_| error("cleanup_lock", "Cannot open worker coordination."))?;
    let metadata = fs::symlink_metadata(&worker)
        .map_err(|_| error("cleanup_lock", "Cannot inspect worker coordination."))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(error(
            "cleanup_scope",
            "Worker coordination is not an ordinary directory.",
        ));
    }
    #[cfg(windows)]
    if std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 0x400 != 0 {
        return Err(error(
            "cleanup_scope",
            "Worker coordination is a reparse point.",
        ));
    }
    let mut guards = Vec::new();
    for name in ["launch.lock", "daemon.lock", "model.lock"] {
        let lock = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(worker.join(name))
            .map_err(|_| error("cleanup_lock", "Cannot inspect a worker lifetime lock."))?;
        if lock.try_lock_exclusive().is_err() {
            return Ok(None);
        }
        guards.push(lock);
    }
    Ok(Some(guards))
}

fn verify_remaining(item: &CleanupItem) -> Result<()> {
    let observed = complete_manifest(&item.holding)?;
    for entry in &observed {
        if item.removed_entries.contains(&entry.relative)
            || !item.manifest.iter().any(|expected| expected == entry)
        {
            return Err(error(
                "cleanup_drift",
                "Holding contains an unknown or changed object.",
            ));
        }
    }
    for expected in &item.manifest {
        if !observed
            .iter()
            .any(|entry| entry.relative == expected.relative)
            && !item.removed_entries.contains(&expected.relative)
            && item.entry_intent.as_ref() != Some(&expected.relative)
        {
            return Err(error(
                "cleanup_unknown",
                "An entry vanished without a recorded intent.",
            ));
        }
    }
    Ok(())
}

fn remove_held_item(plan: &mut CleanupPlan, index: usize) -> Result<()> {
    verify_remaining(&plan.items[index])?;
    let mut entries = plan.items[index].manifest.clone();
    entries.sort_by_key(|entry| {
        (
            entry.kind == "directory",
            std::cmp::Reverse(entry.relative.components().count()),
        )
    });
    for entry in entries {
        if plan.items[index].removed_entries.contains(&entry.relative) {
            continue;
        }
        let target = plan.items[index].holding.join(&entry.relative);
        if exists_exact(&target)? {
            if manifest_entry(&plan.items[index].holding, &target)? != entry {
                return Err(error("cleanup_drift", "An entry changed before removal."));
            }
            plan.items[index].entry_intent = Some(entry.relative.clone());
            plan.items[index].phase = "removing".into();
            cleanup_save(plan)?;
            if entry.kind == "file" {
                let _ = fs::remove_file(&target);
            } else {
                let _ = fs::remove_dir(&target);
            }
        } else if plan.items[index].entry_intent.as_ref() != Some(&entry.relative) {
            return Err(error(
                "cleanup_unknown",
                "An entry vanished without its own intent.",
            ));
        }
        if exists_exact(&target)? {
            return Err(error(
                "cleanup_deferred",
                "An entry remains; retry after its handles close.",
            ));
        }
        plan.items[index].removed_entries.insert(entry.relative);
        plan.items[index].entry_intent = None;
        cleanup_save(plan)?;
    }
    plan.items[index].phase = "removed".into();
    cleanup_save(plan)?;
    Ok(())
}

fn execute_plan_locked(root: &Path, id: &str) -> Result<Value> {
    let mut plan = cleanup_load(&root, id)?;
    if !plan.permanent_internal_removal_approved {
        plan.permanent_internal_removal_approved = true;
        cleanup_save(&plan)?;
    }
    let Some(_worker_guards) = worker_lifetime_guards(&root)? else {
        return Ok(
            json!({"status":"deferred","reason":"worker_runtime_active","cleanup_plan_id":id,
            "next_action":"Retry approved cleanup after the worker exits naturally."}),
        );
    };
    for index in 0..plan.items.len() {
        if plan.items[index].phase == "removed" {
            continue;
        }
        let source = plan.items[index].source.clone();
        let holding = plan.items[index].holding.clone();
        let source_present = exists_exact(&source)?;
        let holding_present = exists_exact(&holding)?;
        if source_present && holding_present {
            plan.items[index].phase = "outcome_unknown".into();
            cleanup_save(&plan)?;
            continue;
        }
        if !source_present && !holding_present {
            if plan.items[index].entry_intent.as_ref() == Some(&PathBuf::new())
                && plan.items[index].removed_entries.len() + 1 == plan.items[index].manifest.len()
            {
                plan.items[index].removed_entries.insert(PathBuf::new());
                plan.items[index].entry_intent = None;
                plan.items[index].phase = "removed".into();
            } else {
                plan.items[index].phase = "outcome_unknown".into();
            }
            cleanup_save(&plan)?;
            continue;
        }
        if !validate_item(&plan, index)? {
            plan.items[index].phase = "blocked_reference".into();
            cleanup_save(&plan)?;
            continue;
        }
        if source_present {
            if !complete_manifest_at(&source, &plan.items[index].manifest)? {
                plan.items[index].phase = "drifted".into();
                cleanup_save(&plan)?;
                continue;
            }
            let parent = holding
                .parent()
                .ok_or_else(|| error("cleanup_plan_invalid", "Invalid holding path."))?;
            fs::create_dir_all(parent)
                .map_err(|_| error("cleanup_io", "Cannot create cleanup holding."))?;
            if exists_exact(&holding)? {
                plan.items[index].phase = "outcome_unknown".into();
                cleanup_save(&plan)?;
                continue;
            }
            plan.items[index].phase = "move_intent".into();
            cleanup_save(&plan)?;
            if move_no_replace(&source, &holding).is_err() {
                plan.items[index].phase = "deferred_move".into();
                cleanup_save(&plan)?;
                continue;
            }
            if !complete_manifest_at(&holding, &plan.items[index].manifest)? {
                plan.items[index].phase = "outcome_unknown".into();
                cleanup_save(&plan)?;
                continue;
            }
            plan.items[index].phase = "held".into();
            cleanup_save(&plan)?;
        }
        if let Err(problem) = remove_held_item(&mut plan, index) {
            plan.items[index].phase = problem.code.clone();
            cleanup_save(&plan)?;
        }
    }
    if plan.items.iter().all(|item| item.phase == "removed") {
        let _ = fs::remove_dir(root.join(".cleanup-holding").join(id));
        let _ = fs::remove_dir(root.join(".cleanup-holding"));
    }
    Ok(
        json!({"cleanup_plan_id":id,"root":root,"items":plan.items.iter().map(|item| json!({
        "path":item.source,"holding":item.holding,"phase":item.phase,
        "removed_entries":item.removed_entries,"entry_intent":item.entry_intent
    })).collect::<Vec<_>>() }),
    )
}

/// Consume only artifacts pinned by the update being finished. The caller
/// already owns the root update and install locks and has completed the MCP
/// probe, so no broad inventory or second user confirmation is involved.
pub(super) fn cleanup_update_plan(plan: &mut Plan) -> Result<()> {
    if !plan.activation_verified || !plan.recovery_obligations_closed || plan.phase != "completed" {
        return Err(error(
            "cleanup_plan_state",
            "The candidate is not verified for cleanup.",
        ));
    }
    let id = if let Some(id) = &plan.cleanup_plan_id {
        cleanup_load(&plan.root, id)?;
        id.clone()
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        let mut paths = Vec::<(&str, PathBuf)>::new();
        paths.push(("previous", previous_bundle(plan)));
        paths.push(("stage", stage(&plan.root, &plan.id)));
        let versions = plan.root.join("state/versions");
        let candidate = candidate_executable(plan)?;
        let mut old_versions = BTreeSet::new();
        for registration in &plan.registrations {
            let receipt: Value =
                agentlaw_contracts::validation::decode_unique(&registration.receipt_before)
                    .map_err(|_| {
                        error(
                            "cleanup_plan_invalid",
                            "A prior registration receipt is invalid.",
                        )
                    })?;
            let executable = receipt["executable"].as_str().ok_or_else(|| {
                error("cleanup_plan_invalid", "A prior receipt has no executable.")
            })?;
            let executable = PathBuf::from(executable);
            if executable.parent().and_then(Path::parent) != Some(versions.as_path())
                || executable.file_name() != Some(std::ffi::OsStr::new(binary_name()))
            {
                return Err(error(
                    "cleanup_scope",
                    "A prior runtime is outside managed versions.",
                ));
            }
            if executable != candidate {
                let directory = executable
                    .parent()
                    .ok_or_else(|| error("cleanup_scope", "Invalid prior runtime."))?;
                old_versions.insert(directory.to_path_buf());
            }
        }
        for version in old_versions {
            paths.push(("version", version));
        }
        let mut items = Vec::new();
        for (kind, source) in paths {
            let manifest = complete_manifest(&source)?;
            let owned = match kind {
                "previous" => {
                    source == previous_bundle(plan)
                        && bundle_hashes(&source)? == plan.bin_before_hashes
                }
                "stage" => is_owned_stage(&source, plan, &manifest),
                "version" => {
                    source.parent() == Some(versions.as_path())
                        && is_owned_version(&source, &manifest)
                }
                _ => false,
            };
            if !owned {
                return Err(error(
                    "cleanup_scope",
                    "An update artifact is not exactly product-owned.",
                ));
            }
            let holding = plan
                .root
                .join(".cleanup-holding")
                .join(&id)
                .join(items.len().to_string());
            items.push(CleanupItem {
                kind: kind.into(),
                source,
                holding,
                manifest,
                phase: "approved".into(),
                removed_entries: BTreeSet::new(),
                entry_intent: None,
            });
        }
        let cleanup = CleanupPlan {
            id: id.clone(),
            root: plan.root.clone(),
            marker_sha256: digest(&plan.root.join(".agentlaw-layout"))?,
            permanent_internal_removal_approved: true,
            items,
        };
        cleanup_save(&cleanup)?;
        plan.cleanup_plan_id = Some(id.clone());
        save(plan)?;
        id
    };
    execute_plan_locked(&plan.root, &id)?;
    let observed = cleanup_load(&plan.root, &id)?;
    if observed.items.iter().any(|item| item.phase != "removed") {
        return Err(error(
            "cleanup_incomplete",
            "Approved update debris remains; resume this plan after inspecting the exact blocker.",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

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
}
