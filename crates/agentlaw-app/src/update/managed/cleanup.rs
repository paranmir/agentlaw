//! Pinned cleanup of owned managed-install artifacts. A preview is never a deletion.
use super::*;

mod legacy;

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
    #[serde(default)]
    retrying: bool,
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

fn inspection_error(reason: std::io::Error) -> DomainError {
    inspection_failure(reason, "cleanup_unknown", "Cannot inspect a cleanup item.")
}
fn inspection_failure(reason: std::io::Error, code: &str, message: &str) -> DomainError {
    if is_sharing_violation(&reason) {
        error("cleanup_deferred", "An item is in use.")
    } else {
        error(code, message)
    }
}

fn manifest_digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(inspection_error)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(inspection_error)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
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
        .map_err(|reason| {
            let code = reason.code().0 as u32 & 0xffff;
            if matches!(code, 32 | 33) {
                error("cleanup_deferred", "An item is in use.")
            } else {
                error(
                    "inspection_unknown",
                    "Cannot open a cleanup object by handle.",
                )
            }
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

fn manifest_entry_with_hash(
    root: &Path,
    path: &Path,
    hash_contents: bool,
) -> Result<ManifestEntry> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| error("cleanup_scope", "Cleanup escaped its approved directory."))?
        .to_path_buf();
    let metadata = fs::symlink_metadata(path).map_err(|reason| {
        inspection_failure(
            reason,
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
        sha256: (kind == "file" && hash_contents)
            .then(|| manifest_digest(path))
            .transpose()?,
    })
}

#[cfg(test)]
fn complete_manifest(path: &Path) -> Result<Vec<ManifestEntry>> {
    complete_manifest_with_hash(path, true)
}

fn complete_manifest_with_hash(path: &Path, hash_contents: bool) -> Result<Vec<ManifestEntry>> {
    let mut out = Vec::new();
    let root_metadata = fs::symlink_metadata(path).map_err(|reason| {
        inspection_failure(reason, "cleanup_drift", "Cannot inspect the cleanup root.")
    })?;
    let root_identity = entry_identity(path, &root_metadata)?;
    let root_volume = root_identity.split(':').next();
    let mut pending = vec![path.to_path_buf()];
    while let Some(entry) = pending.pop() {
        let info = manifest_entry_with_hash(path, &entry, hash_contents)?;
        if info.identity.split(':').next() != root_volume {
            return Err(error(
                "cleanup_scope",
                "A cleanup item crosses a volume or mount boundary.",
            ));
        }
        if info.kind == "directory" {
            for child in fs::read_dir(&entry).map_err(|reason| {
                inspection_failure(
                    reason,
                    "cleanup_drift",
                    "Cannot enumerate a cleanup directory.",
                )
            })? {
                pending.push(
                    child
                        .map_err(|reason| {
                            inspection_failure(
                                reason,
                                "cleanup_drift",
                                "Cannot inspect a cleanup entry.",
                            )
                        })?
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

#[cfg(test)]
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

struct ProcessEntry {
    pid: u32,
    name: String,
    exe: Option<PathBuf>,
    exe_exists: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ProcessReference {
    None,
    InUse,
}

fn processes_reference_path(
    own_pid: u32,
    own_exe: &Path,
    processes: &[ProcessEntry],
    root: &Path,
    path: &Path,
) -> Result<ProcessReference> {
    let root = normalize_process_path(root);
    let path = normalize_process_path(path);
    if normalize_process_path(own_exe).starts_with(&path) {
        return Ok(ProcessReference::InUse);
    }
    for process in processes.iter().filter(|process| process.pid != own_pid) {
        let in_use = match &process.exe {
            Some(exe) => {
                let exe = normalize_process_path(exe);
                exe.starts_with(&path) || (!process.exe_exists && exe.starts_with(&root))
            }
            None => process.name.to_ascii_lowercase().starts_with("agentlaw"),
        };
        if in_use {
            return Ok(ProcessReference::InUse);
        }
    }
    Ok(ProcessReference::None)
}

#[cfg(windows)]
fn normalize_process_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    const SEPARATOR: u16 = b'\\' as u16;
    const EXTENDED_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, b'?' as u16, SEPARATOR];
    const UNC_PREFIX: &[u16] = &[b'U' as u16, b'N' as u16, b'C' as u16, SEPARATOR];
    let wide: Vec<_> = path.as_os_str().encode_wide().collect();
    let Some(ordinary) = wide.strip_prefix(EXTENDED_PREFIX) else {
        return path.to_path_buf();
    };
    if let Some(network) = ordinary.strip_prefix(UNC_PREFIX) {
        let mut unc = vec![SEPARATOR, SEPARATOR];
        unc.extend_from_slice(network);
        return std::ffi::OsString::from_wide(&unc).into();
    }
    std::ffi::OsString::from_wide(ordinary).into()
}

#[cfg(not(windows))]
fn normalize_process_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
thread_local! {
    static PROCESS_OBSERVATIONS: std::cell::RefCell<Option<(PathBuf, Vec<ProcessEntry>)>> =
        const { std::cell::RefCell::new(None) };
}

fn referenced_by_processes(root: &Path, path: &Path) -> Result<ProcessReference> {
    #[cfg(test)]
    if let Some(result) = PROCESS_OBSERVATIONS.with(|observations| {
        let observations = observations.borrow();
        let (installation, processes) = observations.as_ref()?;
        if installation != root {
            return None;
        }
        Some(processes_reference_path(
            std::process::id(),
            &std::env::current_exe().expect("inspect test executable"),
            processes,
            root,
            path,
        ))
    }) {
        return result;
    }
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
    let own_exe = std::env::current_exe().map_err(|_| {
        error(
            "inspection_unknown",
            "Cannot identify the cleanup executable.",
        )
    })?;
    let mut entries = Vec::new();
    for (pid, process) in system.processes() {
        let executable = process
            .exe()
            .filter(|exe| !exe.as_os_str().is_empty())
            .map(Path::to_path_buf);
        #[cfg(target_os = "linux")]
        let executable =
            executable.or_else(|| fs::read_link(format!("/proc/{}/exe", pid.as_u32())).ok());
        if executable.is_none() {
            #[cfg(target_os = "linux")]
            {
                let proc = PathBuf::from(format!("/proc/{}", pid.as_u32()));
                if !proc.exists()
                    || fs::read_to_string(proc.join("status"))
                        .ok()
                        .is_some_and(|status| {
                            status.lines().any(|line| {
                                line.starts_with("State:")
                                    && (line.contains("Z (zombie)") || line.contains("X (dead)"))
                            })
                        })
                {
                    continue;
                }
            }
        }
        let canonical = executable
            .as_ref()
            .and_then(|exe| fs::canonicalize(exe).ok());
        let exe_exists = canonical.is_some() || executable.as_ref().is_some_and(|exe| exe.exists());
        let exe = canonical
            .or(executable)
            .map(|exe| normalize_process_path(&exe));
        entries.push(ProcessEntry {
            pid: pid.as_u32(),
            name: process.name().to_string_lossy().into_owned(),
            exe,
            exe_exists,
        });
    }
    processes_reference_path(own.as_u32(), &own_exe, &entries, root, path)
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

fn unfinished_work(root: &Path, path: &Path, finishing_plan: Option<&str>) -> Result<bool> {
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
                if finishing_plan == Some(id)
                    && plan.phase == "finalizing"
                    && plan.activation_verified
                    && plan.recovery_obligations_closed
                {
                    continue;
                }
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

fn eligible(root: &Path, kind: &str, path: &Path, finishing_plan: Option<&str>) -> Result<bool> {
    if unfinished_work(root, path, finishing_plan)? || referenced_by_registrations(root, path)? {
        return Ok(false);
    }
    if referenced_by_processes(root, path)? == ProcessReference::InUse {
        return Err(error("cleanup_deferred", "An item is in use by a process."));
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
    Ok(
        (plan.phase == "completed" || (finishing_plan == Some(id) && plan.phase == "finalizing"))
            && plan.activation_verified
            && plan.recovery_obligations_closed,
    )
}

fn validate_item(plan: &CleanupPlan, index: usize, finishing_plan: Option<&str>) -> Result<bool> {
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
    if legacy::protected(&plan.root, &item.source) {
        return Ok(false);
    }
    if item.kind == "legacy" {
        if legacy::index_directory(&plan.root, &item.source)
            && (!item.removed_entries.is_empty() || item.entry_intent.is_some())
        {
            let mut removed = item.removed_entries.clone();
            if let Some(intent) = &item.entry_intent {
                if !exists_exact(&item.source.join(intent))? {
                    removed.insert(intent.clone());
                }
            }
            let metadata: Vec<_> = item
                .manifest
                .iter()
                .filter(|entry| {
                    entry.relative.components().count() == 1
                        && entry
                            .relative
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with("vector") && name.ends_with(".sqlite")
                            })
                })
                .collect();
            // With all metadata removed, the pinned manifest is the remaining
            // ownership proof; verify_remaining rejects new or replaced objects.
            if !metadata.is_empty()
                && metadata
                    .iter()
                    .all(|entry| removed.contains(&entry.relative))
            {
                return Ok(true);
            }
            return Ok(legacy::eligible_index_after_removals(
                &plan.root,
                &item.source,
                &removed,
            ));
        }
        return legacy::eligible(&plan.root, &item.source);
    }
    if matches!(
        item.kind.as_str(),
        "old_update" | "old_cleanup" | "old_install"
    ) {
        return record_is_eligible(plan, finishing_plan, item);
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
    eligible(&plan.root, &item.kind, &item.source, finishing_plan)
}

fn record_id(path: &Path) -> Option<&str> {
    if path.extension()? != "json" {
        return None;
    }
    let id = path.file_stem()?.to_str()?;
    uuid::Uuid::parse_str(id).ok()?;
    Some(id)
}

fn record_is_eligible(
    plan: &CleanupPlan,
    current: Option<&str>,
    item: &CleanupItem,
) -> Result<bool> {
    let state = plan.root.join("state");
    match item.kind.as_str() {
        "old_update" if item.source.parent() == Some(state.join("update-plans").as_path()) => {
            let Some(id) = record_id(&item.source) else {
                return Ok(false);
            };
            if Some(id) == current {
                return Ok(false);
            }
            let old = load(&plan.root, id)?;
            if old.phase == "previewed" {
                return Ok(true);
            }
            if old.phase != "completed"
                || !old.activation_verified
                || !old.recovery_obligations_closed
                || !old.cleanup_completed
            {
                return Ok(false);
            }
            let Some(cleanup_id) = old.cleanup_plan_id.as_deref() else {
                return Ok(false);
            };
            let path = cleanup_plan_path(&plan.root, cleanup_id)?;
            if let Some(record) = plan
                .items
                .iter()
                .find(|entry| entry.kind == "old_cleanup" && entry.source == path)
            {
                if record.phase == "removed" {
                    return Ok(true);
                }
            }
            Ok(cleanup_load(&plan.root, cleanup_id)?
                .items
                .iter()
                .all(|item| item.phase == "removed"))
        }
        "old_cleanup" if item.source.parent() == Some(state.join("cleanup-plans").as_path()) => {
            let Some(id) = record_id(&item.source) else {
                return Ok(false);
            };
            if id == plan.id {
                return Ok(false);
            }
            let old = cleanup_load(&plan.root, id)?;
            if !old.items.iter().all(|item| item.phase == "removed") {
                return Ok(false);
            }
            for path in legacy::children(&state.join("update-plans"))? {
                if let Some(owner) = record_id(&path) {
                    let update = load(&plan.root, owner)?;
                    if update.cleanup_plan_id.as_deref() == Some(id) {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        }
        "old_install" if item.source.parent() == Some(state.as_path()) => {
            let Some(name) = item.source.file_name().and_then(|s| s.to_str()) else {
                return Ok(false);
            };
            let Some(id) = name
                .strip_prefix("install-completed-")
                .and_then(|s| s.strip_suffix(".json"))
            else {
                return Ok(false);
            };
            if uuid::Uuid::parse_str(id).is_err() || state.join("install-pending.json").exists() {
                return Ok(false);
            }
            let bytes = fs::read(&item.source)
                .map_err(|_| error("cleanup_unknown", "Cannot inspect an installation record."))?;
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| error("cleanup_unknown", "An installation record is invalid."))?;
            Ok(value.get("targets").is_some_and(Value::is_array)
                && value.get("update_plan_id").and_then(Value::as_str) != current)
        }
        _ => Ok(false),
    }
}

fn previous_records(root: &Path, current: &str) -> Result<Vec<(&'static str, PathBuf)>> {
    let state = root.join("state");
    let mut tracked = BTreeSet::new();
    for path in legacy::children(&state.join("cleanup-plans"))? {
        let Some(id) = record_id(&path) else { continue };
        let cleanup = cleanup_load(root, id)?;
        tracked.extend(
            cleanup
                .items
                .into_iter()
                .filter(|item| item.phase != "removed")
                .map(|item| item.source),
        );
    }
    let referenced_updates: Vec<_> = tracked
        .iter()
        .filter(|path| path.parent() == Some(state.join("update-plans").as_path()))
        .cloned()
        .collect();
    for path in referenced_updates {
        if let Some(id) = record_id(&path) {
            if exists_exact(&path)? {
                if let Some(cleanup_id) = load(root, id)?.cleanup_plan_id {
                    tracked.insert(cleanup_plan_path(root, &cleanup_id)?);
                }
            }
        }
    }
    let mut cleanup_records = Vec::new();
    let mut updates = Vec::new();
    for path in legacy::children(&state.join("update-plans"))? {
        let Some(id) = record_id(&path) else { continue };
        if id == current {
            continue;
        }
        let old = load(root, id)?;
        if old.phase == "previewed" {
            updates.push(("old_update", path));
        } else if old.phase == "completed"
            && old.activation_verified
            && old.recovery_obligations_closed
            && old.cleanup_completed
        {
            let Some(cleanup_id) = old.cleanup_plan_id.as_deref() else {
                return Err(error(
                    "cleanup_unknown",
                    "A completed update has no cleanup record.",
                ));
            };
            let cleanup = cleanup_load(root, cleanup_id)?;
            if cleanup.items.iter().all(|item| item.phase == "removed") {
                cleanup_records.push(("old_cleanup", cleanup_plan_path(root, cleanup_id)?));
                updates.push(("old_update", path));
            }
        }
    }
    let mut referenced = BTreeSet::new();
    for path in legacy::children(&state.join("update-plans"))? {
        let Some(id) = record_id(&path) else { continue };
        let plan = load(root, id)?;
        if (id == current || plan.phase != "completed") && plan.cleanup_plan_id.is_some() {
            referenced.insert(plan.cleanup_plan_id.unwrap());
        }
    }
    let mut selected: BTreeSet<PathBuf> = cleanup_records
        .iter()
        .map(|(_, path)| path.clone())
        .collect();
    for path in legacy::children(&state.join("cleanup-plans"))? {
        let Some(id) = record_id(&path) else { continue };
        if referenced.contains(id) || selected.contains(&path) {
            continue;
        }
        let cleanup = cleanup_load(root, id)?;
        if cleanup.items.iter().all(|item| item.phase == "removed") {
            selected.insert(path.clone());
            cleanup_records.push(("old_cleanup", path));
        }
    }
    updates.extend(cleanup_records);
    for path in legacy::children(&state)? {
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name
            .strip_prefix("install-completed-")
            .and_then(|s| s.strip_suffix(".json"))
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        {
            let raw = fs::read(&path)
                .map_err(|_| error("cleanup_unknown", "Cannot inspect an installation record."))?;
            let value: Value = serde_json::from_slice(&raw)
                .map_err(|_| error("cleanup_unknown", "An installation record is invalid."))?;
            if value.get("update_plan_id").and_then(Value::as_str) != Some(current) {
                updates.push(("old_install", path));
            }
        }
    }
    updates.retain(|(_, path)| !tracked.contains(path));
    Ok(updates)
}

fn exists_exact(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(reason) => Err(inspection_failure(
            reason,
            "inspection_unknown",
            "Cannot inspect a cleanup path.",
        )),
    }
}

fn safe_ancestors(root: &Path, path: &Path) -> Result<bool> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| error("cleanup_scope", "Cleanup escaped its managed root."))?;
    let Some(parent) = relative.parent() else {
        return Ok(false);
    };
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(error(
                "cleanup_scope",
                "Cleanup contains an invalid path component.",
            ));
        };
        current.push(name);
        if !legacy::ordinary_directory(&current)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn worker_lifetime_guards(root: &Path) -> Result<Option<Vec<fs::File>>> {
    let worker = root.join("state/worker");
    let metadata = match fs::symlink_metadata(&worker) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(Vec::new())),
        Err(_) => return Err(error("cleanup_lock", "Cannot inspect worker coordination.")),
    };
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
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(7);
        }
        let lock = match options.open(worker.join(name)) {
            Ok(lock) => lock,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) if is_sharing_violation(&e) => return Ok(None),
            Err(_) => {
                return Err(error(
                    "cleanup_lock",
                    "Cannot inspect a worker lifetime lock.",
                ))
            }
        };
        if lock.try_lock_exclusive().is_err() {
            return Ok(None);
        }
        guards.push(lock);
    }
    Ok(Some(guards))
}

fn verify_remaining(item: &CleanupItem) -> Result<()> {
    let observed = complete_manifest_with_hash(&item.source, item.kind != "legacy")?;
    for entry in &observed {
        if item.removed_entries.contains(&entry.relative)
            || !item.manifest.iter().any(|expected| expected == entry)
        {
            return Err(error(
                "cleanup_drift",
                "Cleanup contains an unknown or changed object.",
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum CleanupCheckpoint {
    EntryRecorded,
    ManifestPinned,
}

trait Removal {
    fn remove(&self, path: &Path, directory: bool) -> std::io::Result<()>;
    fn inspect(&self, path: &Path, hash_contents: bool) -> Result<Vec<ManifestEntry>> {
        complete_manifest_with_hash(path, hash_contents)
    }
    fn checkpoint(&self, _: CleanupCheckpoint) {}
}

struct FilesystemRemoval;

impl Removal for FilesystemRemoval {
    fn remove(&self, path: &Path, directory: bool) -> std::io::Result<()> {
        if directory {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        }
    }
}

fn remove_item(plan: &mut CleanupPlan, index: usize, removal: &impl Removal) -> Result<()> {
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
        let target = if entry.relative.as_os_str().is_empty() {
            plan.items[index].source.clone()
        } else {
            plan.items[index].source.join(&entry.relative)
        };
        if legacy::protected(&plan.root, &target) {
            return Err(error(
                "cleanup_protected",
                "The model liveness lock is not a cleanup item.",
            ));
        }
        if exists_exact(&target)? {
            if manifest_entry_with_hash(
                &plan.items[index].source,
                &target,
                plan.items[index].kind != "legacy",
            )? != entry
            {
                return Err(error("cleanup_drift", "An entry changed before removal."));
            }
            plan.items[index].entry_intent = Some(entry.relative.clone());
            plan.items[index].phase = "removing".into();
            cleanup_save(plan)?;
            if let Err(reason) = removal.remove(&target, entry.kind == "directory") {
                if is_sharing_violation(&reason) {
                    return Err(error("cleanup_deferred", "An item is in use."));
                }
                let _ = reason;
                return Err(error("cleanup_unknown", "Cannot confirm item removal."));
            }
        } else if plan.items[index].entry_intent.as_ref() != Some(&entry.relative) {
            return Err(error(
                "cleanup_unknown",
                "An entry vanished without its own intent.",
            ));
        }
        if exists_exact(&target)? {
            return Err(error(
                "cleanup_unknown",
                "Removal returned success but the entry remains.",
            ));
        }
        plan.items[index].removed_entries.insert(entry.relative);
        plan.items[index].entry_intent = None;
        cleanup_save(plan)?;
        removal.checkpoint(CleanupCheckpoint::EntryRecorded);
    }
    plan.items[index].phase = "removed".into();
    plan.items[index].retrying = false;
    cleanup_save(plan)?;
    Ok(())
}

fn is_sharing_violation(error: &std::io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))
}

fn retryable(item: &CleanupItem) -> bool {
    // A deferred item is eligible; an interrupted retry may have a fresh
    // manifest or a committed removal with no outstanding entry intent.
    // Older plans can also have a recorded intent without the retry flag.
    // Unknown, drifted and blocked outcomes remain incomplete for inspection.
    item.phase == "cleanup_deferred"
        || (item.phase == "removing" && item.entry_intent.is_some())
        || (item.retrying && matches!(item.phase.as_str(), "removing" | "approved"))
}

fn execute_plan_locked(root: &Path, id: &str, finishing_plan: Option<&str>) -> Result<Value> {
    execute_plan_with(root, id, finishing_plan, &FilesystemRemoval, false, true)
}

fn execute_plan_with(
    root: &Path,
    id: &str,
    finishing_plan: Option<&str>,
    removal: &impl Removal,
    retry_only: bool,
    allow_legacy: bool,
) -> Result<Value> {
    let mut plan = cleanup_load(&root, id)?;
    if !plan.permanent_internal_removal_approved {
        plan.permanent_internal_removal_approved = true;
        cleanup_save(&plan)?;
    }
    for item in &mut plan.items {
        if legacy::protected(root, &item.source) {
            item.phase = "preserved".into();
            item.entry_intent = None;
            item.retrying = false;
        }
    }
    cleanup_save(&plan)?;
    let Some(worker_guards) = worker_lifetime_guards(&root)? else {
        let mut result = cleanup_report(&plan);
        result["status"] = json!("deferred");
        result["reason"] = json!("worker_runtime_active");
        return Ok(result);
    };
    // Keep the old-worker lifetime exclusion through revalidation and removal.
    // Delete-sharing handles retain lifetime exclusion on Windows. Remove the
    // retired daemon admission paths last, after all other data removal, so a
    // legacy launcher cannot bypass their held handles by recreating the paths.
    let _worker_guards = worker_guards;
    let admission = root.join("state/worker");
    let mut order: Vec<_> = (0..plan.items.len()).collect();
    order.sort_by_key(|&index| {
        let path = &plan.items[index].source;
        path == &admission.join("daemon.lock") || path == &admission.join("launch.lock")
    });
    for index in order {
        if plan.items[index].kind == "legacy" && !allow_legacy {
            continue;
        }
        if matches!(plan.items[index].phase.as_str(), "removed" | "preserved") {
            continue;
        }
        if retry_only && !retryable(&plan.items[index]) {
            continue;
        }
        if retry_only && !plan.items[index].retrying {
            plan.items[index].retrying = true;
            cleanup_save(&plan)?;
        }
        let source = plan.items[index].source.clone();
        let holding = plan.items[index].holding.clone();
        if !source.starts_with(root)
            || holding
                != root
                    .join(".cleanup-holding")
                    .join(id)
                    .join(index.to_string())
        {
            return Err(error(
                "cleanup_plan_invalid",
                "A cleanup path escaped its pinned scope.",
            ));
        }
        match safe_ancestors(root, &source) {
            Ok(true) => {}
            _ => {
                plan.items[index].phase = "cleanup_unknown".into();
                cleanup_save(&plan)?;
                continue;
            }
        }
        let (source_present, holding_present) =
            match (exists_exact(&source), exists_exact(&holding)) {
                (Ok(source), Ok(holding)) => (source, holding),
                (Err(problem), _) | (_, Err(problem)) => {
                    plan.items[index].phase = if problem.code == "cleanup_deferred" {
                        "cleanup_deferred"
                    } else {
                        "cleanup_unknown"
                    }
                    .into();
                    cleanup_save(&plan)?;
                    continue;
                }
            };
        if plan.items[index].manifest.is_empty() && !holding_present {
            if !source_present {
                plan.items[index].phase = "removed".into();
                plan.items[index].retrying = false;
                cleanup_save(&plan)?;
                continue;
            }
            match complete_manifest_with_hash(&source, plan.items[index].kind != "legacy") {
                Ok(manifest) => {
                    plan.items[index].manifest = manifest;
                    plan.items[index].phase = "approved".into();
                    cleanup_save(&plan)?;
                    removal.checkpoint(CleanupCheckpoint::ManifestPinned);
                }
                Err(problem) => {
                    plan.items[index].phase = problem.code;
                    cleanup_save(&plan)?;
                    continue;
                }
            }
        }
        if source_present && holding_present {
            plan.items[index].phase = "outcome_unknown".into();
            cleanup_save(&plan)?;
            continue;
        }
        if !source_present && !holding_present {
            let item = &plan.items[index];
            let intended_root = item.entry_intent.as_ref() == Some(&PathBuf::new())
                && !item.removed_entries.contains(&PathBuf::new())
                && item.removed_entries.len() + 1 == item.manifest.len()
                && item
                    .manifest
                    .iter()
                    .any(|entry| entry.relative.as_os_str().is_empty())
                && item.manifest.iter().all(|entry| {
                    entry.relative.as_os_str().is_empty()
                        || item.removed_entries.contains(&entry.relative)
                });
            let recorded_root = item.entry_intent.is_none()
                && item.removed_entries.contains(&PathBuf::new())
                && item.removed_entries.len() == item.manifest.len()
                && item
                    .manifest
                    .iter()
                    .all(|entry| item.removed_entries.contains(&entry.relative));
            if intended_root || recorded_root {
                plan.items[index].removed_entries.insert(PathBuf::new());
                plan.items[index].entry_intent = None;
                plan.items[index].phase = "removed".into();
                plan.items[index].retrying = false;
            } else {
                plan.items[index].phase = "outcome_unknown".into();
            }
            cleanup_save(&plan)?;
            continue;
        }
        let eligible = match validate_item(&plan, index, finishing_plan) {
            Ok(value) => value,
            Err(problem) => {
                plan.items[index].phase = if problem.code == "cleanup_deferred" {
                    "cleanup_deferred"
                } else {
                    "cleanup_unknown"
                }
                .into();
                cleanup_save(&plan)?;
                continue;
            }
        };
        if !eligible {
            let dependent_on_busy_record = if plan.items[index].kind == "old_cleanup" {
                let id = record_id(&plan.items[index].source);
                plan.items
                    .iter()
                    .filter(|item| item.kind == "old_update" && item.phase == "cleanup_deferred")
                    .filter_map(|item| record_id(&item.source))
                    .filter_map(|owner| load(root, owner).ok())
                    .any(|owner| owner.cleanup_plan_id.as_deref() == id)
            } else {
                false
            };
            plan.items[index].phase = if dependent_on_busy_record {
                "cleanup_deferred"
            } else {
                "blocked_reference"
            }
            .into();
            cleanup_save(&plan)?;
            continue;
        }
        if source_present
            && plan.items[index].removed_entries.is_empty()
            && plan.items[index].entry_intent.is_none()
        {
            match removal.inspect(&source, plan.items[index].kind != "legacy") {
                Ok(manifest) if manifest == plan.items[index].manifest => {}
                Ok(_) => {
                    plan.items[index].phase = "drifted".into();
                    cleanup_save(&plan)?;
                    continue;
                }
                Err(problem) => {
                    plan.items[index].phase = if problem.code == "cleanup_deferred" {
                        "cleanup_deferred"
                    } else {
                        "cleanup_unknown"
                    }
                    .into();
                    cleanup_save(&plan)?;
                    continue;
                }
            }
        }
        if holding_present {
            plan.items[index].phase = "outcome_unknown".into();
            cleanup_save(&plan)?;
            continue;
        }
        if let Err(problem) = remove_item(&mut plan, index, removal) {
            plan.items[index].phase = problem.code.clone();
            cleanup_save(&plan)?;
        }
    }
    Ok(cleanup_report(&plan))
}

fn cleanup_report(plan: &CleanupPlan) -> Value {
    json!({"cleanup_plan_id":plan.id,"root":plan.root,"items":plan.items.iter().map(|item| json!({
        "path":item.source,"phase":item.phase,"removed_entries":item.removed_entries,"entry_intent":item.entry_intent
    })).collect::<Vec<_>>()})
}

/// Retry only known sharing violations. Failures here must not prevent normal
/// memory use; the existing plan retains the result for later inspection.
pub(super) fn retry_pending(root: &Path) {
    retry_pending_with(root, &FilesystemRemoval, true);
}

fn retry_pending_with(root: &Path, removal: &impl Removal, allow_legacy: bool) {
    retry_pending_inner(root, removal, allow_legacy, false);
}

pub(super) fn retry_pending_under_update_lock(root: &Path) {
    retry_pending_inner(root, &FilesystemRemoval, true, true);
}

fn retry_pending_inner(
    root: &Path,
    removal: &impl Removal,
    allow_legacy: bool,
    update_locked: bool,
) {
    let state = root.join("state");
    let Ok(paths) = legacy::children(&state.join("update-plans")) else {
        return;
    };
    let pending = paths.iter().any(|path| {
        let Some(id) = record_id(path) else {
            return false;
        };
        let Ok(plan) = load(root, id) else {
            return false;
        };
        let Some(cleanup_id) = plan.cleanup_plan_id.as_deref() else {
            return false;
        };
        plan.phase == "completed"
            && cleanup_load(root, cleanup_id).ok().is_some_and(|cleanup| {
                cleanup
                    .items
                    .iter()
                    .any(|item| retryable(item) && (allow_legacy || item.kind != "legacy"))
            })
    });
    if !pending {
        return;
    }
    let _update_lock = if update_locked {
        None
    } else {
        let Ok(lock) = root_update_lock(root) else {
            return;
        };
        Some(lock)
    };
    let Ok(_install_lock) = install::lock_install_state(&state) else {
        return;
    };
    if maintenance_path(&state).exists() {
        return;
    }
    // The existing runtime owner lock excludes admission and is released by the
    // kernel on death. Completed cleanup must not create an update marker.
    let Ok(Some(_owner)) = agentlaw_runtime::installation::owner::OwnerLock::try_acquire(&state)
    else {
        return;
    };
    for path in paths {
        let Some(id) = record_id(&path) else { continue };
        let Ok(plan) = load(root, id) else { continue };
        if plan.phase != "completed"
            || !plan.activation_verified
            || !plan.recovery_obligations_closed
            || !plan.cleanup_completed
        {
            continue;
        }
        let Some(cleanup_id) = plan.cleanup_plan_id.as_deref() else {
            continue;
        };
        let Ok(cleanup) = cleanup_load(root, cleanup_id) else {
            continue;
        };
        if cleanup
            .items
            .iter()
            .any(|item| retryable(item) && (allow_legacy || item.kind != "legacy"))
        {
            let _ = execute_plan_with(root, cleanup_id, Some(id), removal, true, allow_legacy);
        }
    }
}

/// Pin the verified update's artifacts and exact previous-version operating paths.
/// The caller owns the update/install locks and has completed the candidate probe.
/// Later startup retries consume this saved plan without creating a new inventory.
pub(super) fn cleanup_update_plan(plan: &mut Plan) -> Result<()> {
    if !plan.activation_verified || !plan.recovery_obligations_closed || plan.phase != "finalizing"
    {
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
        paths.extend(
            legacy::inventory(&plan.root)?
                .into_iter()
                .map(|path| ("legacy", path)),
        );
        paths.extend(previous_records(&plan.root, &plan.id)?);
        let mut items = Vec::new();
        for (kind, source) in paths {
            if !safe_ancestors(&plan.root, &source)? {
                return Err(error("cleanup_unknown", "A cleanup ancestor disappeared."));
            }
            let (manifest, phase) = match complete_manifest_with_hash(&source, kind != "legacy") {
                Ok(manifest) => (manifest, "approved"),
                Err(problem) if problem.code == "cleanup_deferred" => {
                    (Vec::new(), "cleanup_deferred")
                }
                Err(problem) => return Err(problem),
            };
            let owned = match kind {
                "previous" => {
                    source == previous_bundle(plan)
                        && (manifest.is_empty()
                            || bundle_hashes(&source)? == plan.bin_before_hashes)
                }
                "stage" => {
                    source == stage(&plan.root, &plan.id)
                        && (manifest.is_empty() || is_owned_stage(&source, plan, &manifest))
                }
                "version" => {
                    source.parent() == Some(versions.as_path())
                        && (manifest.is_empty() || is_owned_version(&source, &manifest))
                }
                "legacy" | "old_update" | "old_cleanup" | "old_install" => true,
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
                phase: phase.into(),
                removed_entries: BTreeSet::new(),
                entry_intent: None,
                retrying: false,
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
    execute_plan_locked(&plan.root, &id, Some(&plan.id))?;
    let observed = cleanup_load(&plan.root, &id)?;
    if observed.items.iter().any(|item| {
        !matches!(
            item.phase.as_str(),
            "removed" | "cleanup_deferred" | "preserved"
        )
    }) {
        return Err(error(
            "cleanup_incomplete",
            "The cleanup outcome could not be confirmed.",
        ));
    }
    Ok(())
}

pub(super) fn cleanup_complete(plan: &Plan) -> Result<bool> {
    let Some(id) = &plan.cleanup_plan_id else {
        return Ok(false);
    };
    let cleanup = cleanup_load(&plan.root, id)?;
    Ok(cleanup.items.iter().all(|item| {
        matches!(
            item.phase.as_str(),
            "removed" | "cleanup_deferred" | "preserved"
        )
    }) && cleanup
        .items
        .iter()
        .any(|item| item.kind == "previous" && item.source == previous_bundle(plan))
        && cleanup
            .items
            .iter()
            .any(|item| item.kind == "stage" && item.source == stage(&plan.root, &plan.id)))
}
#[cfg(test)]
mod tests;

/// Known sharing violations are successful update completion with pending work,
/// not a request for approval or a claim that the bytes were removed.
pub(super) fn pending_paths(plan: &Plan) -> Result<Vec<PathBuf>> {
    if !plan.cleanup_completed {
        return Ok(Vec::new());
    }
    let Some(id) = &plan.cleanup_plan_id else {
        return Ok(Vec::new());
    };
    Ok(cleanup_load(&plan.root, id)?
        .items
        .into_iter()
        .filter(|item| item.phase == "cleanup_deferred")
        .map(|item| item.source)
        .collect())
}
