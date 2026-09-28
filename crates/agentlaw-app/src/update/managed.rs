//! Pinned managed update with a synchronous launcher-owned replacement.
use crate::{config, install};
use agentlaw_contracts::{DomainError, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use sysinfo::{get_current_pid, ProcessStatus, ProcessesToUpdate, System};

mod cleanup;

const REPO_RELEASE: &str = "https://github.com/paranmir/agentlaw/releases/download";
const MAINTENANCE_FILE: &str = "update-maintenance.json";

fn maintenance_path(state: &Path) -> PathBuf {
    state.join(MAINTENANCE_FILE)
}

/// Normal frontends cannot start while a replacement is in progress. The
/// candidate's private MCP probe is admitted only for the pinned plan.
pub fn check_frontend_start(state: &Path) -> Result<()> {
    let path = maintenance_path(state);
    if !path.exists() {
        return Ok(());
    }
    let raw = fs::read(&path).map_err(|_| {
        error(
            "update_maintenance",
            "Cannot read the update maintenance gate.",
        )
    })?;
    let marker: Value = serde_json::from_slice(&raw).map_err(|_| {
        error(
            "update_maintenance",
            "The update maintenance gate is invalid.",
        )
    })?;
    let Some(id) = marker["plan_id"].as_str() else {
        return Err(error(
            "update_maintenance",
            "The update maintenance gate has no plan.",
        ));
    };
    let probe = std::env::var("AGENTLAW_UPDATE_PROBE_PLAN").ok();
    if probe.as_deref() == Some(id) {
        let root = state
            .parent()
            .ok_or_else(|| error("update_maintenance", "Invalid managed state path."))?;
        let plan = load(root, id)?;
        let expected = candidate_executable(&plan)?;
        let current = std::env::current_exe()
            .map_err(|_| error("update_maintenance", "Cannot identify the MCP executable."))?;
        if fs::canonicalize(current)
            .ok()
            .zip(fs::canonicalize(expected).ok())
            .is_some_and(|(running, candidate)| running == candidate)
        {
            return Ok(());
        }
    }
    Err(error(
        "update_maintenance",
        "Agentlaw is being replaced; restart the harness after the update command completes.",
    ))
}

fn error(code: &str, message: &str) -> DomainError {
    DomainError::new(code, message)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Registration {
    harness: String,
    directory: PathBuf,
    config_path: PathBuf,
    instructions_path: PathBuf,
    receipt_path: PathBuf,
    config_before: String,
    instructions_before: String,
    receipt_before: String,
    #[serde(default)]
    config_after: Option<String>,
    #[serde(default)]
    instructions_after: Option<String>,
    #[serde(default)]
    receipt_after: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Plan {
    id: String,
    root: PathBuf,
    tag: String,
    archive: String,
    archive_digest: String,
    bin_before: String,
    bin_before_hashes: BTreeMap<String, String>,
    registrations: Vec<Registration>,
    phase: String,
    #[serde(default)]
    bundle_hashes: Option<BTreeMap<String, String>>,
    #[serde(default)]
    launcher_digest: Option<String>,
    #[serde(default)]
    previous_bundle: Option<PathBuf>,
    #[serde(default)]
    activation_verified: bool,
    #[serde(default)]
    recovery_obligations_closed: bool,
    #[serde(default)]
    cleanup_plan_id: Option<String>,
    #[serde(default)]
    cleanup_completed: bool,
}

fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .map_err(|_| error("update_unreadable", "An update artifact is unreadable."))?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| error("update_unreadable", "Cannot hash an update artifact."))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn text(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|_| {
        error(
            "update_drift",
            "A managed installation file is missing or unreadable. Preview again after inspection.",
        )
    })
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| error("update_path", "Invalid update plan path."))?;
    fs::create_dir_all(parent)
        .map_err(|_| error("update_io", "Cannot create the update plan directory."))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| error("update_io", "Cannot stage an update plan."))?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| error("update_io", "Cannot encode an update plan."))?;
    tmp.write_all(&bytes)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|_| error("update_io", "Cannot persist an update plan."))?;
    tmp.persist(path)
        .map_err(|_| error("update_io", "Cannot publish an update plan."))?;
    Ok(())
}

fn plan_path(root: &Path, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id).map_err(|_| {
        error(
            "invalid_arguments",
            "Use the exact returned update plan ID.",
        )
    })?;
    Ok(root.join("state/update-plans").join(format!("{id}.json")))
}

fn stage(root: &Path, id: &str) -> PathBuf {
    root.join(format!(".update-{id}"))
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "agentlaw.exe"
    } else {
        "agentlaw"
    }
}

fn candidate_executable(plan: &Plan) -> Result<PathBuf> {
    let hash = plan
        .bundle_hashes
        .as_ref()
        .and_then(|hashes| hashes.get(binary_name()))
        .ok_or_else(|| {
            error(
                "update_plan_invalid",
                "The candidate runtime hash is missing.",
            )
        })?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(error(
            "update_plan_invalid",
            "Invalid candidate runtime hash.",
        ));
    }
    Ok(plan
        .root
        .join("state/versions")
        .join(format!(
            "{}-{}",
            plan.tag.trim_start_matches('v'),
            &hash[..16]
        ))
        .join(binary_name()))
}

fn publish_candidate(plan: &Plan, source: &Path) -> Result<PathBuf> {
    let destination = candidate_executable(plan)?;
    let expected = plan
        .bundle_hashes
        .as_ref()
        .and_then(|hashes| hashes.get(binary_name()))
        .ok_or_else(|| error("update_plan_invalid", "Missing candidate hash."))?;
    let parent = destination
        .parent()
        .ok_or_else(|| error("update_path", "Invalid version path."))?;
    if parent.exists() {
        let entries = fs::read_dir(parent)
            .map_err(|_| error("update_io", "Cannot inspect the version directory."))?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| error("update_io", "Cannot inspect a version entry."))?;
        if entries.len() != 1
            || entries[0].path() != destination
            || digest(&destination)? != *expected
        {
            return Err(error(
                "update_drift",
                "An existing version directory differs from the candidate.",
            ));
        }
        return Ok(destination);
    }
    fs::create_dir(parent).map_err(|_| {
        error(
            "update_io",
            "Cannot create the candidate version directory.",
        )
    })?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| error("update_io", "Cannot stage the candidate runtime."))?;
    let mut input =
        fs::File::open(source).map_err(|_| error("update_io", "Cannot read candidate runtime."))?;
    std::io::copy(&mut input, &mut staged)
        .map_err(|_| error("update_io", "Cannot copy candidate runtime."))?;
    staged
        .as_file()
        .sync_all()
        .map_err(|_| error("update_io", "Cannot persist candidate runtime."))?;
    #[cfg(unix)]
    staged
        .as_file()
        .set_permissions(
            fs::metadata(source)
                .map_err(|_| error("update_io", "Cannot inspect candidate permissions."))?
                .permissions(),
        )
        .map_err(|_| error("update_io", "Cannot set candidate permissions."))?;
    if digest(staged.path())? != *expected {
        return Err(error(
            "update_checksum",
            "The copied candidate differs from the verified release.",
        ));
    }
    staged.persist_noclobber(&destination).map_err(|_| {
        error(
            "update_io",
            "Cannot publish the candidate runtime without replacement.",
        )
    })?;
    Ok(destination)
}

fn bundle_hashes(directory: &Path) -> Result<BTreeMap<String, String>> {
    let expected: BTreeSet<&str> = [
        binary_name(),
        if cfg!(windows) {
            "agentlaw-worker.exe"
        } else {
            "agentlaw-worker"
        },
        "LICENSE.agentlaw",
    ]
    .into_iter()
    .collect();
    let mut hashes = BTreeMap::new();
    for entry in fs::read_dir(directory)
        .map_err(|_| error("update_drift", "The managed binary bundle is unavailable."))?
    {
        let entry =
            entry.map_err(|_| error("update_drift", "Cannot inspect the managed bundle."))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry
            .file_type()
            .map_err(|_| error("update_drift", "Cannot inspect a bundle entry."))?;
        if !expected.contains(name.as_str()) || !kind.is_file() || kind.is_symlink() {
            return Err(error(
                "update_drift",
                "The managed bundle has an unexpected entry; inspect it before updating.",
            ));
        }
        hashes.insert(name, digest(&entry.path())?);
    }
    if hashes.len() != expected.len() {
        return Err(error(
            "update_drift",
            "The managed binary bundle is incomplete.",
        ));
    }
    Ok(hashes)
}

fn target_archive() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("agentlaw-x86_64-pc-windows-msvc.zip"),
        ("linux", "x86_64") => Ok("agentlaw-x86_64-unknown-linux-gnu.tar.gz"),
        ("macos", "aarch64") => Ok("agentlaw-aarch64-apple-darwin.tar.gz"),
        ("macos", "x86_64") => Ok("agentlaw-x86_64-apple-darwin.tar.gz"),
        _ => Err(error(
            "unsupported",
            "No managed Agentlaw release asset exists for this platform.",
        )),
    }
}

fn managed_root() -> Result<PathBuf> {
    let state = config::state_root()?;
    let root = config::managed_install_root(&state)?.ok_or_else(|| {
        error(
            "update_unmanaged",
            "This executable does not use a verified managed Agentlaw root.",
        )
    })?;
    let exe = fs::canonicalize(std::env::current_exe().map_err(|_| {
        error(
            "update_unmanaged",
            "Cannot identify the running executable.",
        )
    })?)
    .map_err(|_| error("update_unmanaged", "Cannot resolve the running executable."))?;
    let root = fs::canonicalize(root).map_err(|_| {
        error(
            "update_unmanaged",
            "Cannot resolve the managed installation root.",
        )
    })?;
    if !exe.starts_with(root.join("bin")) && !exe.starts_with(root.join("state/versions")) {
        return Err(error(
            "update_unmanaged",
            "A source or staged executable cannot approve replacement of a managed installation.",
        ));
    }
    Ok(root)
}

fn download(url: &str, destination: &Path, timeout: &str) -> Result<()> {
    let output = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            timeout,
            "--max-filesize",
            "134217728",
            "--header",
            "User-Agent: agentlaw-update",
            "--output",
        ])
        .arg(destination)
        .arg(url)
        .output()
        .map_err(|_| {
            error(
                "update_network",
                "curl is required to download a release asset.",
            )
        })?;
    if !output.status.success() {
        return Err(error(
            "update_network",
            "The pinned release asset could not be downloaded.",
        ));
    }
    Ok(())
}

fn asset_url(tag: &str, asset: &str) -> String {
    format!("{REPO_RELEASE}/{tag}/{asset}")
}

fn release_tag() -> Result<String> {
    let latest = super::check();
    let tag = latest["latest_version"].as_str().ok_or_else(|| {
        error(
            "update_network",
            "The latest full release could not be verified.",
        )
    })?;
    super::version(tag)
        .ok_or_else(|| error("update_release", "The release version is invalid."))?;
    Ok(tag.to_owned())
}

fn expected_digest(sums: &str, asset: &str) -> Result<String> {
    let matches: Vec<&str> = sums
        .lines()
        .filter_map(|line| {
            let (hash, name) = line.split_once(char::is_whitespace)?;
            (name.trim().trim_start_matches('*') == asset
                && hash.len() == 64
                && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(hash)
        })
        .collect();
    if matches.len() != 1 {
        return Err(error(
            "update_checksum",
            "The release checksum file does not contain one valid asset digest.",
        ));
    }
    Ok(matches[0].to_ascii_lowercase())
}

fn registration_list(root: &Path) -> Result<Vec<Registration>> {
    let mut out = Vec::new();
    for item in fs::read_dir(root.join("state"))
        .map_err(|_| error("update_unmanaged", "Cannot read installation receipts."))?
    {
        let item = item.map_err(|_| {
            error(
                "update_unmanaged",
                "Cannot enumerate installation receipts.",
            )
        })?;
        let name = item.file_name().to_string_lossy().into_owned();
        let harness = if name.starts_with("harness-codex-") && name.ends_with(".json") {
            "codex"
        } else if name.starts_with("harness-oh-my-pi-") && name.ends_with(".json") {
            "oh-my-pi"
        } else {
            continue;
        };
        let receipt_path = item.path();
        let receipt_before = text(&receipt_path)?;
        let receipt: Value = serde_json::from_str(&receipt_before)
            .map_err(|_| error("update_unmanaged", "A managed harness receipt is invalid."))?;
        let instructions_path = receipt["instructions"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| error("update_unmanaged", "A receipt has no instructions path."))?;
        let directory = instructions_path
            .parent()
            .ok_or_else(|| {
                error(
                    "update_unmanaged",
                    "A receipt has an invalid instructions path.",
                )
            })?
            .to_path_buf();
        let config_path = directory.join(if harness == "codex" {
            "config.toml"
        } else {
            "mcp.json"
        });
        let config_before = text(&config_path)?;
        let instructions_before = text(&instructions_path)?;
        if !install::same_owned_entry(
            install::Harness::parse(harness)?,
            receipt["configuration"].as_str().unwrap_or(""),
            &config_before,
        ) {
            return Err(error(
                "update_drift",
                "A managed harness no longer has its receipted Agentlaw entry.",
            ));
        }
        out.push(Registration {
            harness: harness.into(),
            directory,
            config_path,
            instructions_path,
            receipt_path,
            config_before,
            instructions_before,
            receipt_before,
            config_after: None,
            instructions_after: None,
            receipt_after: None,
        });
    }
    Ok(out)
}

fn load(root: &Path, id: &str) -> Result<Plan> {
    let bytes = fs::read(plan_path(root, id)?)
        .map_err(|_| error("update_plan_missing", "The pinned update plan is missing."))?;
    if bytes.len() > 1024 * 1024 {
        return Err(error(
            "update_plan_invalid",
            "The update plan is too large.",
        ));
    }
    let plan: Plan = serde_json::from_slice(&bytes)
        .map_err(|_| error("update_plan_invalid", "The update plan is invalid."))?;
    if plan.id != id
        || plan.root != root
        || super::version(&plan.tag).is_none()
        || plan.archive != target_archive()?
        || plan.archive_digest.len() != 64
    {
        return Err(error(
            "update_plan_invalid",
            "The update plan does not match this installation.",
        ));
    }
    Ok(plan)
}

fn save(plan: &Plan) -> Result<()> {
    atomic_json(&plan_path(&plan.root, &plan.id)?, plan)
}

fn root_update_lock(root: &Path) -> Result<fs::File> {
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(root.join(".update.lock"))
        .map_err(|_| error("update_lock", "Cannot open the managed update lock."))?;
    lock.try_lock_exclusive().map_err(|_| {
        error(
            "update_busy",
            "Another updater is already applying to this root.",
        )
    })?;
    Ok(lock)
}

fn unfinished_plan(root: &Path) -> Result<Option<Plan>> {
    let directory = root.join("state/update-plans");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error("update_recovery", "Cannot inspect update plans.")),
    };
    let mut selected = None;
    for entry in entries {
        let entry =
            entry.map_err(|_| error("update_recovery", "Cannot enumerate update plans."))?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| error("update_plan_invalid", "An update plan name is invalid."))?;
        let plan = load(root, id)?;
        if plan.phase == "previewed" {
            continue;
        }
        if plan.phase == "completed" {
            if plan.activation_verified
                && plan.recovery_obligations_closed
                && plan.cleanup_completed
                && cleanup::cleanup_complete(&plan)?
            {
                continue;
            }
            return Err(error(
                "update_recovery",
                &format!("Plan {id} claims completion with unfinished cleanup."),
            ));
        }
        if !matches!(plan.phase.as_str(), "prepared" | "finalizing") {
            return Err(error(
                "update_recovery",
                &format!("Plan {id} is mid-update; inspect its maintenance gate."),
            ));
        }
        if selected.replace(plan).is_some() {
            return Err(error(
                "update_recovery",
                "More than one update plan needs recovery; inspect their exact IDs.",
            ));
        }
    }
    Ok(selected)
}

pub fn preview() -> Result<Value> {
    let root = managed_root()?;
    let _lock = root_update_lock(&root)?;
    if maintenance_path(&root.join("state")).exists() {
        return Err(error(
            "update_busy",
            "A managed update already owns the maintenance gate; use the stable launcher.",
        ));
    }
    if let Some(plan) = unfinished_plan(&root)? {
        return Ok(json!({"status":"confirmation_required","plan_id":plan.id,
            "running_version":env!("CARGO_PKG_VERSION"),"latest_version":plan.tag,
            "next_action":"The stable launcher resumes this previously approved update before checking for a newer release."}));
    }
    let tag = release_tag()?;
    if super::version(&tag) <= super::version(env!("CARGO_PKG_VERSION")) {
        return Ok(
            json!({"status":"up_to_date","running_version":env!("CARGO_PKG_VERSION"),"latest_version":tag}),
        );
    }
    let archive = target_archive()?.to_owned();
    let temp =
        tempfile::tempdir().map_err(|_| error("update_io", "Cannot prepare a release preview."))?;
    let sums_path = temp.path().join("SHA256SUMS");
    download(&asset_url(&tag, "SHA256SUMS"), &sums_path, "15")?;
    let digest = expected_digest(&text(&sums_path)?, &archive)?;
    let bin_before = digest_file_or_missing(&root.join("bin").join(binary_name()))?;
    let bin_before_hashes = bundle_hashes(&root.join("bin"))?;
    let plan = Plan {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.clone(),
        tag: tag.clone(),
        archive: archive.clone(),
        archive_digest: digest.clone(),
        bin_before,
        bin_before_hashes,
        registrations: registration_list(&root)?,
        phase: "previewed".into(),
        bundle_hashes: None,
        launcher_digest: None,
        previous_bundle: None,
        activation_verified: false,
        recovery_obligations_closed: false,
        cleanup_plan_id: None,
        cleanup_completed: false,
    };
    save(&plan)?;
    let registrations: Vec<Value> = plan.registrations.iter().map(|r| json!({
        "harness":r.harness,"config_path":r.config_path,"bootstrap_path":r.instructions_path,
        "current_executable":serde_json::from_str::<Value>(&r.receipt_before).ok().and_then(|v|v["executable"].as_str().map(str::to_owned))
    })).collect();
    Ok(json!({"status":"confirmation_required","plan_id":plan.id,
        "running_version":env!("CARGO_PKG_VERSION"),"latest_version":tag,
        "root":root,"asset":archive,"sha256":digest,"registrations":registrations,
        "cleanup_scope":"exact current managed bin and registered version, plus this update's stage and previous bundle",
        "next_action":"The managed launcher will verify this pinned target, complete installation and cleanup, then request an ordinary harness restart."}))
}

fn digest_file_or_missing(path: &Path) -> Result<String> {
    if path.is_file() {
        digest(path)
    } else {
        Ok("missing".into())
    }
}

fn check_before(plan: &Plan) -> Result<()> {
    if digest_file_or_missing(&plan.root.join("bin").join(binary_name()))? != plan.bin_before
        || bundle_hashes(&plan.root.join("bin"))? != plan.bin_before_hashes
    {
        return Err(error(
            "update_drift",
            "The managed binary changed after preview.",
        ));
    }
    for r in &plan.registrations {
        if text(&r.config_path)? != r.config_before
            || text(&r.instructions_path)? != r.instructions_before
            || text(&r.receipt_path)? != r.receipt_before
        {
            return Err(error(
                "update_drift",
                "A managed harness registration or bootstrap changed after preview.",
            ));
        }
    }
    Ok(())
}

pub fn prepare(id: &str) -> Result<Value> {
    let root = managed_root()?;
    let _lock = root_update_lock(&root)?;
    let mut plan = load(&root, id)?;
    if matches!(plan.phase.as_str(), "finalizing" | "completed") {
        if !plan.activation_verified || !plan.recovery_obligations_closed {
            return Err(error(
                "update_recovery",
                "The candidate has no completed verification record.",
            ));
        }
        if maintenance_path(&root.join("state")).exists() {
            return Err(error(
                "update_busy",
                "The maintenance gate changed; retry through the stable launcher.",
            ));
        }
        let observed = status_snapshot(&plan)?;
        if observed["bundle"] != "installed_and_verified"
            || observed["registrations"] != "verified"
            || (plan.phase == "completed" && observed["status"] != "completed")
        {
            return Err(error(
                "update_drift",
                "The approved candidate is not ready for finalization.",
            ));
        }
        return verified_handoff(&plan);
    }
    if plan.phase != "previewed" && plan.phase != "prepared" {
        return Err(error(
            "update_plan_state",
            "This plan has already entered application; inspect its status.",
        ));
    }
    if plan.phase == "previewed" {
        check_before(&plan)?;
        if release_tag()? != plan.tag {
            return Err(error(
                "update_drift",
                "The latest published release changed after preview. Create a new pinned plan.",
            ));
        }
        let sums = tempfile::tempdir()
            .map_err(|_| error("update_io", "Cannot recheck the release checksum."))?;
        let sums_path = sums.path().join("SHA256SUMS");
        download(&asset_url(&plan.tag, "SHA256SUMS"), &sums_path, "15")?;
        if expected_digest(&text(&sums_path)?, &plan.archive)? != plan.archive_digest {
            return Err(error(
                "update_drift",
                "The release asset digest changed after preview. Create a new pinned plan.",
            ));
        }
    }
    let stage = stage(&root, id);
    if plan.phase == "prepared" {
        verify_staged(&plan)?;
        let mut gate = MaintenanceGate::open(&plan)?;
        gate.retain_for_recovery();
        return verified_handoff(&plan);
    }
    fs::create_dir_all(&stage).map_err(|_| error("update_io", "Cannot create update staging."))?;
    let archive_path = stage.join(&plan.archive);
    if !archive_path.is_file() || digest(&archive_path)? != plan.archive_digest {
        download(&asset_url(&plan.tag, &plan.archive), &archive_path, "120")?;
    }
    if digest(&archive_path)? != plan.archive_digest {
        return Err(error(
            "update_checksum",
            "The pinned release archive digest does not match the approved preview.",
        ));
    }
    let listing = Command::new(if cfg!(windows) { "tar.exe" } else { "tar" })
        .arg("-tf")
        .arg(&archive_path)
        .output()
        .map_err(|_| {
            error(
                "update_extract",
                "Cannot inspect the verified release archive.",
            )
        })?;
    let names: BTreeSet<String> = String::from_utf8(listing.stdout)
        .map_err(|_| {
            error(
                "update_extract",
                "The release archive has invalid entry names.",
            )
        })?
        .lines()
        .map(|entry| entry.trim_start_matches("./").to_owned())
        .collect();
    let expected: BTreeSet<String> = [
        binary_name(),
        if cfg!(windows) {
            "agentlaw-worker.exe"
        } else {
            "agentlaw-worker"
        },
        if cfg!(windows) {
            "agentlaw-launcher.exe"
        } else {
            "agentlaw-launcher"
        },
        "LICENSE",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if !listing.status.success() || names != expected {
        return Err(error(
            "update_extract",
            "The release archive contains unexpected or missing paths.",
        ));
    }
    let unpacked = stage.join("unpacked");
    fs::create_dir_all(&unpacked)
        .map_err(|_| error("update_io", "Cannot create bundle staging."))?;
    let output = Command::new(if cfg!(windows) { "tar.exe" } else { "tar" })
        .arg(if cfg!(windows) { "-xf" } else { "-xzf" })
        .arg(&archive_path)
        .arg("-C")
        .arg(&unpacked)
        .output()
        .map_err(|_| {
            error(
                "update_extract",
                "Cannot extract the verified release archive.",
            )
        })?;
    if !output.status.success() {
        return Err(error(
            "update_extract",
            "The verified release archive could not be extracted.",
        ));
    }
    let candidate = unpacked.join(binary_name());
    if !candidate.is_file()
        || !unpacked
            .join(if cfg!(windows) {
                "agentlaw-worker.exe"
            } else {
                "agentlaw-worker"
            })
            .is_file()
        || !unpacked.join("LICENSE").is_file()
        || !unpacked
            .join(if cfg!(windows) {
                "agentlaw-launcher.exe"
            } else {
                "agentlaw-launcher"
            })
            .is_file()
    {
        return Err(error(
            "update_extract",
            "The verified release archive lacks the expected complete bundle.",
        ));
    }
    let version = Command::new(&candidate)
        .arg("--version")
        .output()
        .map_err(|_| {
            error(
                "update_probe",
                "The candidate executable could not be started.",
            )
        })?;
    if !version.status.success()
        || String::from_utf8_lossy(&version.stdout).trim()
            != format!("agentlaw {}", plan.tag.trim_start_matches('v'))
    {
        return Err(error(
            "update_probe",
            "The candidate executable version does not match the pinned release.",
        ));
    }
    let schema = Command::new(&candidate)
        .arg("schema")
        .output()
        .map_err(|_| error("update_probe", "The candidate schema could not be checked."))?;
    if !schema.status.success() || serde_json::from_slice::<Value>(&schema.stdout).is_err() {
        return Err(error("update_probe", "The candidate schema is invalid."));
    }
    let license = unpacked.join("LICENSE");
    let installed_license = unpacked.join("LICENSE.agentlaw");
    if installed_license.exists() {
        if digest(&installed_license)? != digest(&license)? {
            return Err(error(
                "update_drift",
                "A staged license differs from the pinned archive.",
            ));
        }
        fs::remove_file(&license)
            .map_err(|_| error("update_io", "Cannot clear duplicate staged license."))?;
    } else {
        fs::rename(&license, &installed_license)
            .map_err(|_| error("update_io", "Cannot stage the bundle license."))?;
    }
    let launcher = unpacked.join(if cfg!(windows) {
        "agentlaw-launcher.exe"
    } else {
        "agentlaw-launcher"
    });
    fs::rename(&launcher, stage.join("launcher.asset"))
        .map_err(|_| error("update_io", "Cannot retain the release launcher asset."))?;
    plan.launcher_digest = Some(digest(&stage.join("launcher.asset"))?);
    let mut hashes = BTreeMap::new();
    for name in [
        binary_name(),
        if cfg!(windows) {
            "agentlaw-worker.exe"
        } else {
            "agentlaw-worker"
        },
        "LICENSE.agentlaw",
    ] {
        hashes.insert(name.to_owned(), digest(&unpacked.join(name))?);
    }
    plan.bundle_hashes = Some(hashes);
    publish_candidate(&plan, &candidate)?;
    plan.phase = "prepared".into();
    save(&plan)?;
    let mut gate = MaintenanceGate::open(&plan)?;
    gate.retain_for_recovery();
    verified_handoff(&plan)
}

fn verified_handoff(plan: &Plan) -> Result<Value> {
    let candidate = candidate_executable(plan)?;
    let candidate_sha256 = plan
        .bundle_hashes
        .as_ref()
        .and_then(|hashes| hashes.get(binary_name()))
        .ok_or_else(|| error("update_plan_invalid", "The candidate hash is missing."))?;
    if digest(&candidate)? != *candidate_sha256 {
        return Err(error(
            "update_checksum",
            "The candidate differs from the approved update plan.",
        ));
    }
    Ok(
        json!({"status":"handoff_ready","plan_id":plan.id,"candidate":candidate,
        "candidate_sha256":candidate_sha256,"root":plan.root,
        "next_action":"The managed launcher continues this update synchronously; this handoff is not installation success."}),
    )
}

fn verify_staged(plan: &Plan) -> Result<()> {
    let candidate = candidate_executable(plan)?;
    let archive = stage(&plan.root, &plan.id).join(&plan.archive);
    if digest(&archive)? != plan.archive_digest {
        return Err(error(
            "update_checksum",
            "The approved release archive has changed.",
        ));
    }
    let expected = plan.bundle_hashes.as_ref().ok_or_else(|| {
        error(
            "update_plan_invalid",
            "The plan has no verified bundle hashes.",
        )
    })?;
    if expected.get(binary_name()) != Some(&digest(&candidate)?) {
        return Err(error(
            "update_checksum",
            "The versioned candidate differs from the approved release.",
        ));
    }
    let staged = stage(&plan.root, &plan.id).join("unpacked");
    let installed = plan.root.join("bin");
    if staged.exists() {
        if bundle_hashes(&staged)? != *expected {
            return Err(error(
                "update_checksum",
                "The staged bundle differs from the approved release.",
            ));
        }
    } else if installed.exists() && bundle_hashes(&installed)? == *expected {
        // The staged directory has already been published as bin.
    } else {
        return Err(error(
            "update_recovery",
            "Neither staged nor installed candidate bundle is verified.",
        ));
    }
    Ok(())
}

fn probe_candidate_mcp(plan: &Plan) -> Result<()> {
    let candidate = candidate_executable(plan)?;
    let mut child = Command::new(candidate)
        .args(["mcp", "serve", "--stdio"])
        .env("AGENTLAW_HOME", plan.root.join("state"))
        .env("AGENTLAW_UPDATE_PROBE_PLAN", &plan.id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| error("update_probe", "Cannot start the candidate MCP."))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("update_probe", "Candidate MCP stdout is unavailable."))?;
    let (lines, responses_in) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        for _ in 0..3 {
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() || line.len() > 1_048_576 || line.is_empty() {
                break;
            }
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"agentlaw-update-probe","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"agentlaw","arguments":{"action":"recall","recall":{
                "recall_for":"Read-only Agentlaw managed update readiness probe"}}}}),
    ];
    if let Some(mut input) = child.stdin.take() {
        for message in messages {
            serde_json::to_writer(&mut input, &message)
                .map_err(|_| error("update_probe", "Cannot encode an MCP probe request."))?;
            input
                .write_all(b"\n")
                .map_err(|_| error("update_probe", "Cannot send the MCP probe."))?;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut responses = BTreeMap::new();
    for _ in 0..3 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = match responses_in.recv_timeout(remaining) {
            Ok(line) => line,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error(
                    "update_probe",
                    "The candidate MCP did not return all probe responses.",
                ));
            }
        };
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error(
                    "update_probe",
                    "The candidate MCP returned invalid JSON-RPC.",
                ));
            }
        };
        if value.get("error").is_some() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error(
                "update_probe",
                "The candidate MCP returned a JSON-RPC error.",
            ));
        }
        if let Some(id) = value["id"].as_u64() {
            responses.insert(id, value);
        }
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error(
                    "update_probe",
                    "The candidate MCP probe did not finish safely.",
                ));
            }
        }
    };
    let _ = reader.join();
    if !status.success() {
        return Err(error("update_probe", "The candidate MCP probe failed."));
    }
    let init = responses
        .get(&1)
        .ok_or_else(|| error("update_probe", "MCP initialize has no response."))?;
    if init["result"]["serverInfo"]["version"] != plan.tag.trim_start_matches('v') {
        return Err(error(
            "update_probe",
            "The candidate MCP reported the wrong version.",
        ));
    }
    let listed = responses
        .get(&2)
        .ok_or_else(|| error("update_probe", "MCP tools/list has no response."))?;
    if !listed["result"]["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "agentlaw"))
    {
        return Err(error(
            "update_probe",
            "The candidate MCP did not list Agentlaw.",
        ));
    }
    let recall = responses
        .get(&3)
        .ok_or_else(|| error("update_probe", "MCP recall has no response."))?;
    if recall["result"]["isError"] != false {
        return Err(error("update_probe", "The candidate MCP recall failed."));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn same_linux_thread_group(pid: sysinfo::Pid, self_pid: sysinfo::Pid) -> bool {
    // Linux lists a process's threads under /proc as separate task IDs on
    // some hosts. Only the OS-confirmed threads of this helper are exempt.
    let path = format!("/proc/{}/status", pid.as_u32());
    fs::read_to_string(path)
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Tgid:"))
                .and_then(|value| value.trim().parse::<u32>().ok())
        })
        .is_some_and(|tgid| tgid == self_pid.as_u32())
}

fn previous_bundle(plan: &Plan) -> PathBuf {
    plan.root.join(format!(".bin-previous-{}", plan.id))
}

struct MaintenanceGate {
    path: PathBuf,
    id: String,
    retain_on_drop: bool,
}

impl MaintenanceGate {
    fn open(plan: &Plan) -> Result<Self> {
        let path = maintenance_path(&plan.root.join("state"));
        let candidate = candidate_executable(plan)?;
        let expected = plan
            .bundle_hashes
            .as_ref()
            .and_then(|hashes| hashes.get(binary_name()))
            .ok_or_else(|| error("update_plan_invalid", "The candidate hash is missing."))?;
        let marker = json!({"plan_id":plan.id,"candidate":candidate,"candidate_sha256":expected,"drain":false});
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                serde_json::to_writer(&mut file, &marker).map_err(|_| {
                    error("update_maintenance", "Cannot write the maintenance gate.")
                })?;
                file.sync_all().map_err(|_| {
                    error("update_maintenance", "Cannot persist the maintenance gate.")
                })?;
            }
            Err(reason) if reason.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing: Value = serde_json::from_slice(&fs::read(&path).map_err(|_| {
                    error("update_maintenance", "Cannot inspect the maintenance gate.")
                })?)
                .map_err(|_| error("update_maintenance", "The maintenance gate is invalid."))?;
                if existing["plan_id"] != marker["plan_id"]
                    || existing["candidate"] != marker["candidate"]
                    || existing["candidate_sha256"] != marker["candidate_sha256"]
                {
                    return Err(error(
                        "update_busy",
                        "A different update owns this managed root.",
                    ));
                }
            }
            Err(_) => {
                return Err(error(
                    "update_maintenance",
                    "Cannot create the maintenance gate.",
                ))
            }
        }
        Ok(Self {
            path,
            id: plan.id.clone(),
            retain_on_drop: false,
        })
    }

    fn retain_for_recovery(&mut self) {
        self.retain_on_drop = true;
    }

    fn set_drain(&self, drain: bool) -> Result<()> {
        let mut marker: Value =
            serde_json::from_slice(&fs::read(&self.path).map_err(|_| {
                error("update_maintenance", "Cannot inspect the maintenance gate.")
            })?)
            .map_err(|_| error("update_maintenance", "The maintenance gate changed."))?;
        if marker["plan_id"] != self.id {
            return Err(error(
                "update_maintenance",
                "Another plan owns the maintenance gate.",
            ));
        }
        marker["drain"] = json!(drain);
        atomic_json(&self.path, &marker)
    }

    fn close(&mut self) -> Result<()> {
        let raw = fs::read(&self.path)
            .map_err(|_| error("update_maintenance", "The maintenance gate disappeared."))?;
        let marker: Value = serde_json::from_slice(&raw)
            .map_err(|_| error("update_maintenance", "The maintenance gate changed."))?;
        if marker["plan_id"] != self.id {
            return Err(error(
                "update_maintenance",
                "Another plan owns the maintenance gate.",
            ));
        }
        fs::remove_file(&self.path).map_err(|_| {
            error(
                "update_maintenance",
                "Cannot reopen normal Agentlaw startup.",
            )
        })?;
        self.retain_on_drop = true;
        Ok(())
    }
}

impl Drop for MaintenanceGate {
    fn drop(&mut self) {
        if !self.retain_on_drop {
            if let Ok(raw) = fs::read(&self.path) {
                if serde_json::from_slice::<Value>(&raw)
                    .ok()
                    .is_some_and(|marker| marker["plan_id"] == self.id)
                {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }
}

fn verified_broker_pid(plan: &Plan) -> Result<Option<sysinfo::Pid>> {
    let worker = plan.root.join("state/worker");
    let lock = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(worker.join("daemon.lock"))
    {
        Ok(lock) => lock,
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(error(
                "inspection_unknown",
                "Cannot inspect the broker lifetime lock.",
            ))
        }
    };
    match lock.try_lock_exclusive() {
        Ok(()) => return Ok(None),
        Err(reason)
            if reason.kind() == std::io::ErrorKind::WouldBlock
                || (cfg!(windows) && reason.raw_os_error() == Some(33)) => {}
        Err(_) => {
            return Err(error(
                "inspection_unknown",
                "Cannot prove the broker lifetime lock is held.",
            ))
        }
    }
    let endpoint: Value = serde_json::from_slice(
        &fs::read(worker.join("endpoint.json"))
            .map_err(|_| error("inspection_unknown", "The active broker has no endpoint."))?,
    )
    .map_err(|_| {
        error(
            "inspection_unknown",
            "The active broker endpoint is invalid.",
        )
    })?;
    let pid = endpoint["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| {
            error(
                "inspection_unknown",
                "The active broker has no process identity.",
            )
        })?;
    Ok(Some(sysinfo::Pid::from_u32(pid)))
}

fn wait_for_managed_processes(plan: &Plan, allow_workers: bool) -> Result<()> {
    let own = get_current_pid()
        .ok()
        .ok_or_else(|| error("inspection_unknown", "Cannot identify the updater process."))?;
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        let broker_pid = if allow_workers {
            verified_broker_pid(plan)?
        } else {
            None
        };
        if !system.processes().contains_key(&own) {
            return Err(error(
                "inspection_unknown",
                "The updater is absent from the process list.",
            ));
        }
        let mut blockers = Vec::new();
        for (pid, process) in system.processes() {
            if *pid == own
                || matches!(
                    process.status(),
                    ProcessStatus::Zombie | ProcessStatus::Dead
                )
            {
                continue;
            }
            #[cfg(target_os = "linux")]
            if same_linux_thread_group(*pid, own) {
                continue;
            }
            let Some(exe) = process.exe().filter(|path| !path.as_os_str().is_empty()) else {
                if process
                    .name()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .starts_with("agentlaw")
                {
                    blockers.push(format!("PID {} has an unknown executable", pid.as_u32()));
                }
                continue;
            };
            let path = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
            if path.starts_with(plan.root.join("command")) {
                continue;
            }
            if path.starts_with(&plan.root) {
                if allow_workers
                    && broker_pid
                        .is_some_and(|broker| *pid == broker || process.parent() == Some(broker))
                {
                    continue;
                }
                blockers.push(format!("PID {}: {}", pid.as_u32(), path.display()));
            }
        }
        if blockers.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(error(
                "update_process_active",
                &format!(
                    "Agentlaw could not establish process exit before replacement: {}",
                    blockers.join(", ")
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn verify_effective_targets(plan: &Plan) -> Result<()> {
    for registration in &plan.registrations {
        let harness = install::Harness::parse(&registration.harness)?;
        let effective = install::effective_instructions_path(harness, &registration.directory)?;
        if effective != registration.instructions_path {
            return Err(error(
                "update_drift",
                "instruction_target_changed: the harness now selects a different instruction file.",
            ));
        }
    }
    Ok(())
}

fn bundle_state(
    path: &Path,
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
) -> Result<&'static str> {
    if !path.exists() {
        return Ok("missing");
    }
    let found = bundle_hashes(path)?;
    if found == *old {
        Ok("old")
    } else if found == *new {
        Ok("new")
    } else {
        Err(error(
            "update_drift",
            "A managed bundle has bytes outside the pinned before/after states.",
        ))
    }
}

fn incomplete_io(plan: &Plan, operation: &str) -> Value {
    json!({"status":"incomplete","plan_id":plan.id,"phase":plan.phase,
        "reason":"bundle_rename_deferred","operation":operation,
        "next_action":"The pinned replacement did not finish. Resume this plan using the stable Agentlaw command after resolving the exact file blocker."})
}

fn registration_after(
    plan: &mut Plan,
    index: usize,
) -> Result<(PathBuf, Vec<(PathBuf, String, String)>)> {
    let state = plan.root.join("state");
    let executable = candidate_executable(plan)?;
    let registration = &mut plan.registrations[index];
    let harness = install::Harness::parse(&registration.harness)?;
    let config_after = install::configuration(
        harness,
        &registration.config_before,
        &executable,
        &state,
        true,
    )?;
    let instructions_after = install::upsert_bootstrap(
        &registration.instructions_before,
        &install::bootstrap(&executable, &state),
    )?;
    let receipt_after = json!({"configuration":config_after,"executable":executable,
        "instructions":registration.instructions_path})
    .to_string();
    for (saved, expected) in [
        (&registration.config_after, &config_after),
        (&registration.instructions_after, &instructions_after),
        (&registration.receipt_after, &receipt_after),
    ] {
        if saved.as_ref().is_some_and(|value| value != expected) {
            return Err(error(
                "update_plan_invalid",
                "A pinned registration after-state changed.",
            ));
        }
    }
    if registration.config_after.is_none() {
        registration.config_after = Some(config_after.clone());
        registration.instructions_after = Some(instructions_after.clone());
        registration.receipt_after = Some(receipt_after.clone());
        save(plan)?;
    }
    let registration = &plan.registrations[index];
    Ok((
        executable,
        vec![
            (
                registration.config_path.clone(),
                registration.config_before.clone(),
                config_after,
            ),
            (
                registration.instructions_path.clone(),
                registration.instructions_before.clone(),
                instructions_after,
            ),
            (
                registration.receipt_path.clone(),
                registration.receipt_before.clone(),
                receipt_after,
            ),
        ],
    ))
}

fn finish_finalizing(plan: &mut Plan) -> Result<Value> {
    if plan.phase != "finalizing"
        || !plan.activation_verified
        || !plan.recovery_obligations_closed
        || !plan.cleanup_completed
        || !cleanup::cleanup_complete(plan)?
        || plan.root.join("state/install-pending.json").exists()
        || maintenance_path(&plan.root.join("state")).exists()
    {
        return Err(error(
            "update_recovery",
            "The update still has unfinished finalization work.",
        ));
    }
    let observed = status_snapshot(plan)?;
    if observed["bundle"] != "installed_and_verified" || observed["registrations"] != "verified" {
        return Err(error(
            "update_drift",
            "The installed candidate changed before final completion.",
        ));
    }
    plan.phase = "completed".into();
    save(plan)?;
    status_snapshot(plan)
}

fn apply_live(id: &str, requested_root: &Path) -> Result<Value> {
    if !requested_root.is_absolute() {
        return Err(error(
            "invalid_arguments",
            "The managed update root must be absolute.",
        ));
    }
    let state = requested_root.join("state");
    let root = config::managed_install_root(&state)?.ok_or_else(|| {
        error(
            "update_unmanaged",
            "The plan's managed root is unavailable.",
        )
    })?;
    let root = fs::canonicalize(root)
        .map_err(|_| error("update_unmanaged", "Cannot resolve the managed root."))?;
    let _update_lock = root_update_lock(&root)?;
    let state = root.join("state");
    let mut plan = load(&root, id)?;
    if plan.phase == "completed" {
        return status_snapshot(&plan);
    }
    if plan.phase == "previewed" {
        return Err(error(
            "update_plan_state",
            "Prepare the approved bundle before applying it.",
        ));
    }
    let candidate = fs::canonicalize(candidate_executable(&plan)?)
        .map_err(|_| error("update_candidate_required", "The candidate is unavailable."))?;
    let running = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|_| {
            error(
                "update_candidate_required",
                "Cannot identify this executable.",
            )
        })?;
    if candidate != running {
        return Err(error(
            "update_candidate_required",
            "Only the verified candidate may finish this update.",
        ));
    }
    if plan.phase == "finalizing" {
        if plan.bundle_hashes.as_ref() != Some(&bundle_hashes(&plan.root.join("bin"))?) {
            return Err(error(
                "update_drift",
                "The installed candidate bundle changed.",
            ));
        }
        if plan.cleanup_completed && !maintenance_path(&state).exists() {
            return finish_finalizing(&mut plan);
        }
    } else {
        verify_staged(&plan)?;
    }
    let mut gate = MaintenanceGate::open(&plan)?;
    if plan.phase != "prepared" {
        gate.retain_for_recovery();
    }
    wait_for_managed_processes(&plan, true)?;
    gate.set_drain(true)?;
    wait_for_managed_processes(&plan, false)?;
    if plan.cleanup_completed {
        if plan.phase != "finalizing" || !cleanup::cleanup_complete(&plan)? {
            return Err(error(
                "update_recovery",
                "Cleanup completion is inconsistent.",
            ));
        }
        gate.close()?;
        return finish_finalizing(&mut plan);
    }
    gate.set_drain(false)?;
    let mut install_lock = Some(install::lock_install_state(&state)?);
    verify_effective_targets(&plan)?;
    if plan.phase == "finalizing" {
        let observed = status_snapshot(&plan)?;
        if observed["bundle"] != "installed_and_verified" || observed["registrations"] != "verified"
        {
            return Err(error(
                "update_drift",
                "The installed candidate changed during finalization.",
            ));
        }
        cleanup::cleanup_update_plan(&mut plan)?;
        plan.cleanup_completed = true;
        save(&plan)?;
        drop(install_lock.take());
        gate.close()?;
        return finish_finalizing(&mut plan);
    }
    let old = &plan.bin_before_hashes;
    let new = plan.bundle_hashes.as_ref().ok_or_else(|| {
        error(
            "update_plan_invalid",
            "The candidate bundle hashes are absent.",
        )
    })?;
    let bin = root.join("bin");
    let staged = stage(&root, id).join("unpacked");
    let previous = previous_bundle(&plan);
    if plan
        .previous_bundle
        .as_ref()
        .is_some_and(|saved| saved != &previous)
    {
        return Err(error(
            "update_plan_invalid",
            "The previous bundle path changed.",
        ));
    }
    if plan.previous_bundle.is_none() {
        plan.previous_bundle = Some(previous.clone());
        save(&plan)?;
    }
    let mut bin_state = bundle_state(&bin, old, new)?;
    let mut staged_state = bundle_state(&staged, old, new)?;
    let mut previous_state = bundle_state(&previous, old, new)?;
    if previous_state == "new" || staged_state == "old" {
        return Err(error(
            "update_drift",
            "Update bundle positions contradict this plan.",
        ));
    }
    if matches!(
        plan.phase.as_str(),
        "prepared" | "moving_old_bundle" | "publishing_new_bundle"
    ) && state.join("install-pending.json").exists()
    {
        return Err(error(
            "update_recovery",
            "An installation journal exists before this plan's registration phase.",
        ));
    }
    if plan.phase == "prepared" {
        check_before(&plan)?;
        if (bin_state, staged_state, previous_state) != ("old", "new", "missing") {
            return Err(error(
                "update_drift",
                "Prepared bundle positions changed after approval.",
            ));
        }
        plan.phase = "moving_old_bundle".into();
        save(&plan)?;
        gate.retain_for_recovery();
    }
    if plan.phase == "moving_old_bundle" {
        if (bin_state, staged_state, previous_state) == ("old", "new", "missing") {
            verify_effective_targets(&plan)?;
            if fs::rename(&bin, &previous).is_err() {
                return Ok(incomplete_io(&plan, "retain_previous_bundle"));
            }
            bin_state = "missing";
            previous_state = "old";
        }
        if (bin_state, staged_state, previous_state) != ("missing", "new", "old")
            && (bin_state, staged_state, previous_state) != ("new", "missing", "old")
        {
            return Err(error(
                "update_drift",
                "The old bundle move cannot be reconciled with this plan.",
            ));
        }
        plan.phase = "publishing_new_bundle".into();
        save(&plan)?;
    }
    if plan.phase == "publishing_new_bundle" {
        if (bin_state, staged_state, previous_state) == ("missing", "new", "old") {
            verify_effective_targets(&plan)?;
            if fs::rename(&staged, &bin).is_err() {
                return Ok(incomplete_io(&plan, "publish_candidate_bundle"));
            }
            bin_state = "new";
            staged_state = "missing";
        }
        if (bin_state, staged_state, previous_state) != ("new", "missing", "old") {
            return Err(error(
                "update_drift",
                "The new bundle publication cannot be reconciled with this plan.",
            ));
        }
        plan.phase = "refreshing_registrations".into();
        save(&plan)?;
    }
    if matches!(
        plan.phase.as_str(),
        "refreshing_registrations" | "verifying_candidate"
    ) {
        if (bin_state, staged_state, previous_state) != ("new", "missing", "old") {
            return Err(error(
                "update_drift",
                "Published bundle positions differ from the approved plan.",
            ));
        }
        if plan.phase == "refreshing_registrations" {
            let candidate = candidate_executable(&plan)?;
            let hash = digest(&candidate)?;
            for index in 0..plan.registrations.len() {
                verify_effective_targets(&plan)?;
                let (executable, expected) = registration_after(&mut plan, index)?;
                install::apply_pinned_registration_locked(
                    &state,
                    &expected,
                    &candidate,
                    &executable,
                    &hash,
                )?;
            }
            if state.join("install-pending.json").exists() {
                return Err(error(
                    "update_recovery",
                    "An installation journal remains after registration refresh.",
                ));
            }
            verify_effective_targets(&plan)?;
            plan.phase = "verifying_candidate".into();
            save(&plan)?;
        }
        if plan.phase == "verifying_candidate" {
            drop(install_lock.take());
            probe_candidate_mcp(&plan)?;
            wait_for_managed_processes(&plan, true)?;
            gate.set_drain(true)?;
            wait_for_managed_processes(&plan, false)?;
            install_lock = Some(install::lock_install_state(&state)?);
            let observed = status_snapshot(&plan)?;
            if observed["bundle"] != "installed_and_verified"
                || observed["registrations"] != "verified"
            {
                return Err(error(
                    "update_probe",
                    "The candidate registration changed during the probe.",
                ));
            }
            plan.activation_verified = true;
            plan.recovery_obligations_closed = true;
            plan.phase = "finalizing".into();
            save(&plan)?;
        }
        cleanup::cleanup_update_plan(&mut plan)?;
        plan.cleanup_completed = true;
        save(&plan)?;
        drop(install_lock.take());
        gate.close()?;
        return finish_finalizing(&mut plan);
    }
    Err(error(
        "update_plan_invalid",
        "The update plan has an unknown phase.",
    ))
}

pub fn apply(id: &str, requested_root: &Path) -> Result<Value> {
    apply_live(id, requested_root)
}

fn closed_successor(plan: &Plan) -> Result<Option<Plan>> {
    let Some(mut expected) = plan.bundle_hashes.clone() else {
        return Ok(None);
    };
    let current = bundle_hashes(&plan.root.join("bin")).ok();
    let directory = plan.root.join("state/update-plans");
    let mut seen = BTreeSet::from([plan.id.clone()]);
    for _ in 0..64 {
        if current.as_ref() == Some(&expected) {
            return Ok(None);
        }
        let mut successor = None;
        for entry in fs::read_dir(&directory)
            .map_err(|_| error("update_plan_invalid", "Cannot inspect update lineage."))?
        {
            let entry = entry
                .map_err(|_| error("update_plan_invalid", "Cannot enumerate update lineage."))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if seen.contains(id) {
                continue;
            }
            let candidate = load(&plan.root, id)?;
            if candidate.bin_before_hashes == expected
                && candidate.phase == "completed"
                && candidate.activation_verified
                && candidate.recovery_obligations_closed
                && candidate.cleanup_completed
                && cleanup::cleanup_complete(&candidate)?
            {
                if successor.is_some() {
                    return Err(error(
                        "update_plan_invalid",
                        "Update lineage has two successors.",
                    ));
                }
                successor = Some(candidate);
            }
        }
        let Some(next) = successor else {
            return Ok(None);
        };
        seen.insert(next.id.clone());
        expected = next.bundle_hashes.clone().ok_or_else(|| {
            error(
                "update_plan_invalid",
                "A closed update has no bundle hashes.",
            )
        })?;
        if current.as_ref() == Some(&expected) {
            return Ok(Some(next));
        }
    }
    Err(error("update_plan_invalid", "Update lineage is too deep."))
}

fn status_snapshot(plan: &Plan) -> Result<Value> {
    let cleanup_verified = plan.cleanup_completed && cleanup::cleanup_complete(plan)?;
    if plan.phase == "completed" && cleanup_verified && plan.recovery_obligations_closed {
        if let Some(successor) = closed_successor(plan)? {
            let latest = status_snapshot(&successor)?;
            return Ok(
                json!({"status":"superseded","plan_id":plan.id,"tag":plan.tag,
                "root":plan.root,"phase":plan.phase,"activation":"verified",
                "bundle":"superseded","registrations":"superseded",
                "superseded_by_plan_id":successor.id,"current_installation":latest,
                "previous_bundle":previous_bundle(plan),
                "previous_bundle_retained":previous_bundle(plan).exists(),
                "cleanup":"completed",
                "next_action":"This closed historical plan has a verified completed successor; never republish its older bundle."}),
            );
        }
    }
    let expected_new = plan.bundle_hashes.as_ref();
    let bundle = if let Some(new) = expected_new {
        match bundle_hashes(&plan.root.join("bin")) {
            Ok(found) if found == *new => "installed_and_verified",
            Ok(found) if found == plan.bin_before_hashes => "previous",
            Ok(_) => "drifted",
            Err(_) if !plan.root.join("bin").exists() => "missing",
            Err(_) => "drifted",
        }
    } else {
        "not_prepared"
    };
    let mut registration_items = Vec::new();
    let mut overall = "verified";
    for registration in &plan.registrations {
        let harness = install::Harness::parse(&registration.harness)?;
        let effective = install::effective_instructions_path(harness, &registration.directory).ok();
        let target_changed = effective.as_ref() != Some(&registration.instructions_path);
        let current = [
            text(&registration.config_path).ok(),
            text(&registration.instructions_path).ok(),
            text(&registration.receipt_path).ok(),
        ];
        let after = [
            &registration.config_after,
            &registration.instructions_after,
            &registration.receipt_after,
        ];
        let before = [
            &registration.config_before,
            &registration.instructions_before,
            &registration.receipt_before,
        ];
        let owned_config = |expected: &str| {
            current[0]
                .as_deref()
                .is_some_and(|now| install::same_owned_entry(harness, expected, now))
        };
        let owned_instructions = |expected: &str| {
            current[1]
                .as_deref()
                .is_some_and(|now| install::same_owned_bootstrap(expected, now))
        };
        let config_after = after[0].as_deref().is_some_and(|value| owned_config(value));
        let instructions_after = after[1]
            .as_deref()
            .is_some_and(|value| owned_instructions(value));
        let receipt_after = after[2].is_some() && after[2].as_ref() == current[2].as_ref();
        let all_after = config_after && instructions_after && receipt_after;
        let all_known = (owned_config(before[0]) || config_after)
            && (owned_instructions(before[1]) || instructions_after)
            && (current[2].as_deref() == Some(before[2]) || receipt_after);
        let mut artifact_verified = false;
        if all_after {
            if let Some(receipt) = current[2]
                .as_ref()
                .and_then(|raw| agentlaw_contracts::validation::decode_unique(raw).ok())
            {
                if let Some(path) = receipt["executable"].as_str() {
                    artifact_verified = expected_new
                        .and_then(|hashes| hashes.get(binary_name()))
                        .is_some_and(|hash| digest(Path::new(path)).ok().as_ref() == Some(hash));
                }
            }
        }
        let state = if target_changed || !all_known || (all_after && !artifact_verified) {
            "drifted"
        } else if all_after {
            "verified"
        } else {
            "partial"
        };
        if state == "drifted" {
            overall = "drifted";
        } else if state == "partial" && overall == "verified" {
            overall = "partial";
        }
        registration_items.push(
            json!({"harness":registration.harness,"config_path":registration.config_path,
            "instructions_path":registration.instructions_path,"state":state,
            "reason":if target_changed {Some("instruction_target_changed")} else {None}}),
        );
    }
    let activation = if bundle == "installed_and_verified" && overall == "verified" {
        if plan.activation_verified {
            "verified"
        } else {
            "restart_required"
        }
    } else {
        "verification_pending"
    };
    let broker_runtime = observed_broker_runtime(plan);
    let maintenance_open = maintenance_path(&plan.root.join("state")).exists();
    let completed = plan.phase == "completed"
        && plan.activation_verified
        && cleanup_verified
        && plan.recovery_obligations_closed
        && !maintenance_open
        && bundle == "installed_and_verified"
        && overall == "verified";
    Ok(
        json!({"status":if completed {"completed"} else {"incomplete"},
        "plan_id":plan.id,"phase":plan.phase,"tag":plan.tag,"root":plan.root,
        "bundle":bundle,"registrations":overall,"registration_details":registration_items,
        "current_session":"unknown","activation":activation,"broker_runtime":broker_runtime,
        "previous_bundle":previous_bundle(plan),
        "cleanup":if cleanup_verified {"completed"} else {"incomplete"},
        "recovery_obligations_closed":plan.recovery_obligations_closed,
        "maintenance_gate_open":maintenance_open,
        "next_action":if completed {
            "Restart the harness normally. The candidate MCP and recall have already passed the internal readiness probe."
        } else {
            "The update is incomplete. Resume this pinned plan after inspecting the reported blocker; do not report restart as successful installation."
        }}),
    )
}

fn observed_broker_runtime(plan: &Plan) -> &'static str {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let old = plan.bin_before_hashes.get(binary_name());
    let new = plan
        .bundle_hashes
        .as_ref()
        .and_then(|hashes| hashes.get(binary_name()));
    let mut found = None;
    for process in system.processes().values() {
        if !process.cmd().iter().any(|arg| arg == "worker-daemon") {
            continue;
        }
        let Some(path) = process.exe() else {
            return "unknown";
        };
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !path.starts_with(&plan.root) {
            continue;
        }
        let Ok(hash) = digest(&path) else {
            return "unknown";
        };
        let kind = if old == Some(&hash) {
            "old"
        } else if new == Some(&hash) {
            "new"
        } else {
            "unknown"
        };
        if found.is_some_and(|prior| prior != kind) {
            return "unknown";
        }
        found = Some(kind);
    }
    found.unwrap_or("unknown")
}

pub fn status_with_root(id: &str, requested_root: &Path) -> Result<Value> {
    if !requested_root.is_absolute() {
        return Err(error(
            "invalid_arguments",
            "The managed update root must be absolute.",
        ));
    }
    let state = requested_root.join("state");
    let root = config::managed_install_root(&state)?.ok_or_else(|| {
        error(
            "update_unmanaged",
            "The plan's managed root is unavailable.",
        )
    })?;
    let root = fs::canonicalize(root)
        .map_err(|_| error("update_unmanaged", "Cannot resolve the managed root."))?;
    status_snapshot(&load(&root, id)?)
}

pub fn status(id: &str) -> Result<Value> {
    let root = managed_root()?;
    status_with_root(id, &root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_line_must_match_the_exact_asset_once() {
        let hash = "a".repeat(64);
        let sums = format!("{hash}  agentlaw-other.zip\n{hash}  agentlaw-x.zip\n");
        assert_eq!(expected_digest(&sums, "agentlaw-x.zip").unwrap(), hash);
        assert!(expected_digest(&sums, "agentlaw.zip").is_err());
        assert!(expected_digest(
            &(sums.clone() + &format!("{hash}  agentlaw-x.zip\n")),
            "agentlaw-x.zip"
        )
        .is_err());
    }
}
