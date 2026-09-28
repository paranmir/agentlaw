//! Explicit, pinned managed-install update. Preparation never replaces a live bundle.
use crate::{config, install};
use agentlaw_contracts::{DomainError, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};
use sysinfo::{get_current_pid, ProcessStatus, ProcessesToUpdate, System};

const REPO_RELEASE: &str = "https://github.com/paranmir/agentlaw/releases/download";

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
    helper_digest: Option<String>,
    #[serde(default)]
    bundle_hashes: Option<BTreeMap<String, String>>,
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

pub fn preview() -> Result<Value> {
    let root = managed_root()?;
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
        helper_digest: None,
        bundle_hashes: None,
    };
    save(&plan)?;
    let registrations: Vec<Value> = plan.registrations.iter().map(|r| json!({
        "harness":r.harness,"config_path":r.config_path,"bootstrap_path":r.instructions_path,
        "current_executable":serde_json::from_str::<Value>(&r.receipt_before).ok().and_then(|v|v["executable"].as_str().map(str::to_owned))
    })).collect();
    Ok(json!({"status":"confirmation_required","plan_id":plan.id,
        "running_version":env!("CARGO_PKG_VERSION"),"latest_version":tag,
        "root":root,"asset":archive,"sha256":digest,"registrations":registrations,
        "next_action":"Review this pinned target and existing managed registrations, then run agentlaw update --confirm-update <plan_id>. Preparation will not replace running executables."}))
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
    let mut plan = load(&root, id)?;
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
        return Ok(prepared_result(&plan));
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
    let helper_dir = stage.join("helper");
    fs::create_dir_all(&helper_dir).map_err(|_| {
        error(
            "update_io",
            "Cannot create the independent update helper directory.",
        )
    })?;
    let helper = helper_dir.join(binary_name());
    fs::copy(&candidate, &helper)
        .map_err(|_| error("update_io", "Cannot stage an independent update helper."))?;
    if digest(&helper)? != digest(&candidate)? {
        return Err(error(
            "update_checksum",
            "The independent helper differs from the verified candidate.",
        ));
    }
    #[cfg(unix)]
    {
        fs::set_permissions(
            &helper,
            fs::metadata(&candidate)
                .map_err(|_| error("update_io", "Cannot read candidate permissions."))?
                .permissions(),
        )
        .map_err(|_| error("update_io", "Cannot make the update helper executable."))?;
    }
    plan.helper_digest = Some(digest(&helper)?);
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
    plan.phase = "prepared".into();
    save(&plan)?;
    Ok(prepared_result(&plan))
}

fn prepared_result(plan: &Plan) -> Value {
    let helper = stage(&plan.root, &plan.id)
        .join("helper")
        .join(binary_name());
    json!({"status":"prepared","plan_id":plan.id,"helper":helper,
        "helper_arguments":["update","apply",plan.id,"--root",plan.root],
        "root":plan.root,"next_action":"Close all harnesses and Agentlaw MCP, broker, worker and CLI processes using this root. Do not restart them until application completes or safely defers. Then run the returned helper with the returned argument array. It will report remaining processes and can resume the same plan."})
}

fn verify_staged(plan: &Plan) -> Result<()> {
    let helper = stage(&plan.root, &plan.id)
        .join("helper")
        .join(binary_name());
    if plan.helper_digest.as_deref() != Some(digest(&helper)?.as_str()) {
        return Err(error(
            "update_checksum",
            "The staged helper no longer matches its verified digest.",
        ));
    }
    let bundle = if plan.root.join("bin").join(binary_name()).is_file()
        && plan.phase != "prepared"
        && plan.phase != "moving_old_bundle"
    {
        plan.root.join("bin")
    } else {
        stage(&plan.root, &plan.id).join("unpacked")
    };
    for (name, expected) in plan.bundle_hashes.as_ref().ok_or_else(|| {
        error(
            "update_plan_invalid",
            "The plan has no verified bundle hashes.",
        )
    })? {
        if digest(&bundle.join(name))? != *expected {
            return Err(error(
                "update_checksum",
                "A staged or installed bundle file differs from the approved release.",
            ));
        }
    }
    Ok(())
}

fn inspect_processes(root: &Path) -> Value {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let own = get_current_pid().ok();
    if own.is_none() || !system.processes().contains_key(&own.unwrap()) {
        return json!({"status":"inspection_unknown","reason":"The OS process list did not include this helper."});
    }
    let mut remaining = Vec::new();
    let mut unknown = Vec::new();
    for (pid, process) in system.processes() {
        if Some(*pid) == own {
            continue;
        }
        #[cfg(target_os = "linux")]
        if own.is_some_and(|self_pid| same_linux_thread_group(*pid, self_pid)) {
            continue;
        }
        // A zombie has exited and cannot execute the old bundle. Unix may
        // retain its executable path until the parent reaps it.
        if matches!(
            process.status(),
            ProcessStatus::Zombie | ProcessStatus::Dead
        ) {
            continue;
        }
        let name = process.name().to_string_lossy().to_ascii_lowercase();
        if !name.starts_with("agentlaw") {
            continue;
        }
        let Some(exe) = process.exe().filter(|path| !path.as_os_str().is_empty()) else {
            unknown.push(
                json!({"pid":pid.to_string(),"name":name,"reason":"Executable path unavailable."}),
            );
            continue;
        };
        let path = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        if path.starts_with(root) {
            let role = process
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            remaining.push(json!({"pid":pid.to_string(),"path":path,"role":role,
                "process_status":process.status().to_string(),
                "started_at":process.start_time()}));
        }
    }
    if !unknown.is_empty() {
        json!({"status":"inspection_unknown","candidates":unknown})
    } else if !remaining.is_empty() {
        json!({"status":"pending_exit","processes":remaining})
    } else {
        json!({"status":"clear"})
    }
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

fn offline_guard(plan: &Plan) -> Option<Value> {
    let inspection = inspect_processes(&plan.root);
    if inspection["status"] == "clear" {
        return None;
    }
    Some(
        json!({"status":"incomplete","plan_id":plan.id,"phase":plan.phase,
        "inspection":inspection,"previous_bundle":plan.root.join(".bin-previous"),
        "staged_bundle":stage(&plan.root,&plan.id).join("unpacked"),
        "next_action":"Keep Agentlaw offline. Resolve the reported process or inspection uncertainty, then rerun the same staged helper with update apply and this plan ID. No forced termination or blind rollback was performed."}),
    )
}

fn verify_helper(plan: &Plan) -> Result<()> {
    let expected = stage(&plan.root, &plan.id)
        .join("helper")
        .join(binary_name());
    if fs::canonicalize(&expected).ok()
        != std::env::current_exe()
            .ok()
            .and_then(|p| fs::canonicalize(p).ok())
    {
        return Err(error(
            "update_helper_required",
            "Run the independently staged helper shown by the prepared plan.",
        ));
    }
    Ok(())
}

pub fn apply(id: &str, requested_root: &Path) -> Result<Value> {
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
    let mut plan = load(&root, id)?;
    if plan.phase == "previewed" {
        return Err(error(
            "update_plan_state",
            "Prepare and verify the approved bundle before applying it.",
        ));
    }
    verify_helper(&plan)?;
    verify_staged(&plan)?;
    if plan.phase == "completed" {
        return Ok(json!({"status":"completed","plan_id":id,"root":root,
            "running_version":env!("CARGO_PKG_VERSION")}));
    }
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(root.join(".update.lock"))
        .map_err(|_| error("update_lock", "Cannot open the managed update lock."))?;
    lock_file.try_lock_exclusive().map_err(|_| {
        error(
            "update_busy",
            "Another updater is already applying to this root.",
        )
    })?;
    if let Some(blocked) = offline_guard(&plan) {
        return Ok(blocked);
    }
    let staged_bundle = stage(&root, id).join("unpacked");
    let bin = root.join("bin");
    let previous = root.join(".bin-previous");
    if plan.phase == "prepared" {
        check_before(&plan)?;
        if previous.exists() {
            return Err(error("update_recovery", "A prior bundle is retained; inspect its interrupted installation before applying another plan."));
        }
        plan.phase = "moving_old_bundle".into();
        save(&plan)?;
    }
    if plan.phase == "moving_old_bundle" {
        if let Some(blocked) = offline_guard(&plan) {
            return Ok(blocked);
        }
        if bin.exists() && !previous.exists() {
            fs::rename(&bin, &previous).map_err(|_| {
                error(
                    "update_io",
                    "Could not retain the previous bundle; no new bundle was published.",
                )
            })?;
        }
        plan.phase = "publishing_new_bundle".into();
        save(&plan)?;
    }
    if plan.phase == "publishing_new_bundle" {
        if let Some(blocked) = offline_guard(&plan) {
            return Ok(blocked);
        }
        if !bin.exists() {
            fs::rename(&staged_bundle, &bin).map_err(|_| error("update_io", "The new bundle could not be published; the previous bundle was retained for external recovery."))?;
        }
        plan.phase = "refreshing_registrations".into();
        save(&plan)?;
    }
    if plan.phase == "refreshing_registrations" {
        if let Some(blocked) = offline_guard(&plan) {
            return Ok(blocked);
        }
        let installed = Command::new(bin.join(binary_name()))
            .arg("--version")
            .output()
            .map_err(|_| error("update_probe", "The published bundle could not be probed."))?;
        if !installed.status.success()
            || String::from_utf8_lossy(&installed.stdout).trim()
                != format!("agentlaw {}", plan.tag.trim_start_matches('v'))
        {
            return Err(error("update_probe", "The published bundle is not the approved version. Keep the previous bundle for recovery."));
        }
        for index in 0..plan.registrations.len() {
            if let Some(blocked) = offline_guard(&plan) {
                return Ok(blocked);
            }
            if plan.registrations[index].config_after.is_none() {
                let r = &plan.registrations[index];
                let harness = install::Harness::parse(&r.harness)?;
                let hash = digest(&std::env::current_exe().map_err(|_| {
                    error(
                        "update_helper_required",
                        "Cannot identify the staged helper.",
                    )
                })?)?;
                let executable = state
                    .join("versions")
                    .join(format!("{}-{}", env!("CARGO_PKG_VERSION"), &hash[..16]))
                    .join(binary_name());
                let config_after =
                    install::configuration(harness, &r.config_before, &executable, &state, true)?;
                let instructions_after = install::upsert_bootstrap(
                    &r.instructions_before,
                    &install::bootstrap(&executable, &state),
                )?;
                let receipt_after = json!({"configuration":config_after,"executable":executable,
                    "instructions":r.instructions_path})
                .to_string();
                let r = &mut plan.registrations[index];
                r.config_after = Some(config_after);
                r.instructions_after = Some(instructions_after);
                r.receipt_after = Some(receipt_after);
                save(&plan)?;
            }
            let r = &plan.registrations[index];
            let expected = vec![
                (
                    r.config_path.clone(),
                    r.config_before.clone(),
                    r.config_after.clone().unwrap_or_default(),
                ),
                (
                    r.instructions_path.clone(),
                    r.instructions_before.clone(),
                    r.instructions_after.clone().unwrap_or_default(),
                ),
                (
                    r.receipt_path.clone(),
                    r.receipt_before.clone(),
                    r.receipt_after.clone().unwrap_or_default(),
                ),
            ];
            install::recover_matching_update_journal(&state, &expected)?;
            let current_config = text(&r.config_path)?;
            let current_instructions = text(&r.instructions_path)?;
            let current_receipt = text(&r.receipt_path)?;
            let already = r
                .config_after
                .as_ref()
                .is_some_and(|after| after == &current_config)
                && r.instructions_after
                    .as_ref()
                    .is_some_and(|after| after == &current_instructions)
                && r.receipt_after
                    .as_ref()
                    .is_some_and(|after| after == &current_receipt);
            if already {
                continue;
            }
            if (current_config != r.config_before
                && r.config_after.as_ref() != Some(&current_config))
                || (current_instructions != r.instructions_before
                    && r.instructions_after.as_ref() != Some(&current_instructions))
                || (current_receipt != r.receipt_before
                    && r.receipt_after.as_ref() != Some(&current_receipt))
            {
                return Err(error("update_drift", "A harness registration changed outside this plan. The new bundle is installed, but registration refresh is incomplete."));
            }
            let harness = install::Harness::parse(&r.harness)?;
            let result = install::install(&state, harness, &r.directory, None, true)?;
            if result["status"] != "installed" {
                return Err(error(
                    "update_install",
                    "A managed harness could not be refreshed.",
                ));
            }
            let r = &plan.registrations[index];
            if text(&r.config_path)? != r.config_after.as_deref().unwrap_or_default()
                || text(&r.instructions_path)?
                    != r.instructions_after.as_deref().unwrap_or_default()
                || text(&r.receipt_path)? != r.receipt_after.as_deref().unwrap_or_default()
            {
                return Err(error(
                    "update_install",
                    "A managed registration differs from the pinned after-state.",
                ));
            }
            save(&plan)?;
        }
        if state.join("install-pending.json").exists() {
            return Err(error("update_recovery", "An installation journal outside this pinned plan remains; inspect it before completing the update."));
        }
        plan.phase = "completed".into();
        save(&plan)?;
    }
    Ok(json!({"status":plan.phase,"plan_id":id,"root":root,
        "running_version":env!("CARGO_PKG_VERSION"),
        "next_action":"Start a new approved harness session and verify initialize.serverInfo.version, tool description, and an ordinary recall. Existing sessions keep their previous running version. The retained previous bundle is recovery material."}))
}

pub fn status(id: &str) -> Result<Value> {
    let root = managed_root()?;
    let plan = load(&root, id)?;
    Ok(
        json!({"plan_id":id,"phase":plan.phase,"tag":plan.tag,"root":root,
        "registrations":plan.registrations.iter().map(|r|json!({"harness":r.harness,"config_path":r.config_path,
            "refreshed":r.config_after.is_some()})).collect::<Vec<_>>() }),
    )
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
