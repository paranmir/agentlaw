//! Explicit, recoverable user-level harness installation. Never invoked by recall.
use agentlaw_contracts::{DomainError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const BEGIN: &str = "<!-- agentlaw managed bootstrap: begin -->";
const END: &str = "<!-- agentlaw managed bootstrap: end -->";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Harness {
    Codex,
    OhMyPi,
}
impl Harness {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "codex" => Ok(Self::Codex),
            "oh-my-pi" => Ok(Self::OhMyPi),
            _ => Err(err("Use harness codex or oh-my-pi.")),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OhMyPi => "oh-my-pi",
        }
    }
    pub fn default_directory(self) -> Result<PathBuf> {
        let explicit = std::env::var_os(match self {
            Self::Codex => "CODEX_HOME",
            Self::OhMyPi => "PI_CODING_AGENT_DIR",
        });
        if let Some(path) = explicit {
            return absolute(PathBuf::from(path));
        }
        let base = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .ok_or_else(|| err("Choose an absolute --harness-dir."))?;
        if self == Self::Codex {
            return Ok(base.join(".codex"));
        }
        if let Some(profile) =
            std::env::var_os("OMP_PROFILE").or_else(|| std::env::var_os("PI_PROFILE"))
        {
            let p = PathBuf::from(&profile);
            if p.components().count() != 1
                || !p
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
            {
                return Err(err(
                    "Invalid Oh My Pi profile; supply --harness-dir explicitly.",
                ));
            }
            return Ok(base.join(".omp/profiles").join(profile).join("agent"));
        }
        Ok(base.join(".omp/agent"))
    }
}
fn err(s: &str) -> DomainError {
    DomainError::new("installation_failed", s)
}
/// Validate an installation target without inspecting files or harness state.
pub fn validate_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(err("Installation paths must be absolute."))
    }
}
fn absolute(p: PathBuf) -> Result<PathBuf> {
    validate_path(&p)?;
    Ok(p)
}
fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(err(
            "Existing settings are unreadable or not UTF-8; they were not replaced.",
        )),
    }
}
fn digest_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).map_err(|_| err("An installation artifact is unreadable."))?;
    let mut h = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = f
            .read(&mut bytes)
            .map_err(|_| err("Cannot hash an installation artifact."))?;
        if n == 0 {
            break;
        }
        h.update(&bytes[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn atomic_text(path: &Path, text: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| err("Invalid installation target."))?;
    fs::create_dir_all(parent).map_err(|_| err("Cannot create an installation directory."))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| err("Cannot prepare installation output."))?;
    tmp.write_all(text.as_bytes())
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|_| err("Cannot persist installation output."))?;
    tmp.persist(path).map_err(|_| {
        err("Cannot replace installation output; recover using the retained installation journal.")
    })?;
    Ok(())
}
fn copy_verified(from: &Path, to: &Path, hash: &str) -> Result<()> {
    if to.exists() {
        if digest_file(to)? == hash {
            return Ok(());
        }
        return Err(err(
            "A versioned artifact differs from its manifest. Existing bytes were preserved.",
        ));
    }
    let parent = to.parent().ok_or_else(|| err("Invalid artifact path."))?;
    fs::create_dir_all(parent).map_err(|_| err("Cannot create the artifact directory."))?;
    let mut tmp =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| err("Cannot stage an artifact."))?;
    std::io::copy(
        &mut fs::File::open(from).map_err(|_| err("Cannot open the source artifact."))?,
        tmp.as_file_mut(),
    )
    .map_err(|_| err("Cannot copy an artifact."))?;
    tmp.as_file()
        .sync_all()
        .map_err(|_| err("Cannot persist an artifact."))?;
    if digest_file(tmp.path())? != hash {
        return Err(err(
            "Artifact checksum mismatch; no executable/configuration was activated.",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::metadata(from)
            .map_err(|_| err("Cannot read artifact permissions."))?
            .permissions()
            .mode();
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(permissions))
            .map_err(|_| err("Cannot install artifact permissions."))?;
    }
    tmp.persist_noclobber(to).map_err(|_|err("Artifact installation raced with another writer; retry without deleting existing files."))?;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    pub model_build_id: String,
    pub onnx_model: Asset,
    pub tokenizer_json: Asset,
    pub runtime_library: Asset,
}
pub fn installed_assets(state: &Path) -> Result<Option<agentlaw_worker::ModelAssets>> {
    let Some(text) = read_optional(&state.join("model-assets.json"))? else {
        return Ok(None);
    };
    let manifest: ModelManifest =
        serde_json::from_value(agentlaw_contracts::validation::decode_unique(&text)?)
            .map_err(|_| err("Installed model manifest is malformed."))?;
    // Content hashes are checked when installing, then the worker validates its loaded artifacts.
    for asset in [
        &manifest.onnx_model,
        &manifest.tokenizer_json,
        &manifest.runtime_library,
    ] {
        if !asset.path.is_absolute() || !asset.path.is_file() {
            return Err(err("An installed model artifact is missing; repair the installation, not the memory store."));
        }
    }
    Ok(Some(agentlaw_worker::ModelAssets {
        onnx_model: manifest.onnx_model.path,
        tokenizer_json: manifest.tokenizer_json.path,
        runtime_library: manifest.runtime_library.path,
    }))
}
fn read_manifest(path: &Path) -> Result<ModelManifest> {
    let text =
        fs::read_to_string(path).map_err(|_| err("Cannot read the supplied model manifest."))?;
    let manifest: ModelManifest =
        serde_json::from_value(agentlaw_contracts::validation::decode_unique(&text)?)
            .map_err(|_| err("Invalid model manifest."))?;
    if manifest.model_build_id.trim().is_empty() {
        return Err(err("The manifest must identify the model build."));
    }
    for asset in [
        &manifest.onnx_model,
        &manifest.tokenizer_json,
        &manifest.runtime_library,
    ] {
        if !asset.path.is_absolute()
            || asset.sha256.len() != 64
            || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || digest_file(&asset.path)? != asset.sha256.to_ascii_lowercase()
        {
            return Err(err("The supplied model paths or checksums are invalid."));
        }
    }
    Ok(manifest)
}
pub(crate) fn bootstrap(executable: &Path, state: &Path) -> String {
    let invocation = if cfg!(windows) {
        format!(
            "& '{}'",
            executable.display().to_string().replace('\'', "''")
        )
    } else {
        format!(
            "'{}'",
            executable.display().to_string().replace('\'', "'\\''")
        )
    };
    let state_assignment = if cfg!(windows) {
        format!(
            "$env:AGENTLAW_HOME='{}'",
            state.display().to_string().replace('\'', "''")
        )
    } else {
        format!(
            "AGENTLAW_HOME='{}'",
            state.display().to_string().replace('\'', "'\\''")
        )
    };
    format!("{BEGIN}\n## Agentlaw memory\n\nUse Agentlaw without waiting for a memory request. When work starts or resumes, recall relevant context and read applicable standing rules returned with recall_for before dependent work; for project work, check for a matching active Task before creating one. Recall again for new questions raised by emerging evidence or similarity to prior work. Reuse results only while they remain in context and cover the same question and conditions. After compaction or a session interruption, recall before dependent work even with a summary, unless this recall already succeeded since resumption. Before substantial project work, recall and reuse or create a matching Task. Before another substantial step or a wait, use remember_this if its last confirmed version and accessible durable references would not let a fresh session resume. Save reusable decisions, corrections, friction, and findings before relying on them. Batch only other progress that leaves this restart state intact at phase boundaries or before finishing; skip incidental chat, unchanged saves, and routine reports.\nFor project work, use the actual work folder confirmed by the harness, never the MCP or home cwd alone. In recall, supply project_path and include_active_tasks=true; use restore_context=true only when project context is missing or incomplete. Without a confirmed project, recall only user/machine context. Follow the current tool schema and result guidance.\nIf agentlaw is hidden, try host tool discovery if available; if unavailable (not denied), use the installed CLI.\n\nCLI fallback: set `{state_assignment}` in the child shell, then pass one JSON request on stdin to `{invocation} call --json -`. Use `{invocation} schema` when the contract is unavailable. The CLI and MCP use the same installation.\n{END}")
}
pub(crate) fn upsert_bootstrap(previous: &str, body: &str) -> Result<String> {
    let begins: Vec<_> = previous.match_indices(BEGIN).map(|(i, _)| i).collect();
    let ends: Vec<_> = previous.match_indices(END).map(|(i, _)| i).collect();
    match (begins.as_slice(),ends.as_slice()) {
        ([],[])=>Ok(format!("{}{}{}\n",previous,if previous.is_empty(){""}else if previous.ends_with('\n'){"\n"}else{"\n\n"},body)),
        ([start],[end]) if start<end=>Ok(format!("{}{}{}",&previous[..*start],body,&previous[*end+END.len()..])),
        _=>Err(err("Managed instruction markers are inconsistent. Existing instructions were not rewritten.")),
    }
}

pub(crate) fn same_owned_bootstrap(before: &str, now: &str) -> bool {
    fn owned(text: &str) -> Option<&str> {
        let (start, rest) = text.split_once(BEGIN)?;
        let (body, end) = rest.split_once(END)?;
        (!start.contains(END)
            && !body.contains(BEGIN)
            && !end.contains(BEGIN)
            && !end.contains(END))
        .then_some(body)
    }
    owned(before).is_some_and(|expected| owned(now) == Some(expected))
}
pub(crate) fn configuration(
    harness: Harness,
    previous: &str,
    executable: &Path,
    state: &Path,
    may_replace: bool,
) -> Result<String> {
    let command = executable
        .to_str()
        .ok_or_else(|| err("The executable path must be UTF-8."))?;
    let state = state
        .to_str()
        .ok_or_else(|| err("Installation path must be UTF-8."))?;
    match harness {
        Harness::Codex => {
            let mut doc = previous
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| err("Codex config is not valid TOML; nothing was replaced."))?;
            if doc
                .get("mcp_servers")
                .and_then(|v| v.get("agentlaw"))
                .is_some()
                && !may_replace
            {
                return Err(err("An unmanaged agentlaw MCP entry already exists. Inspect it before replacing it."));
            }
            let mut server = toml_edit::Table::new();
            server["command"] = toml_edit::value(command);
            let mut args = toml_edit::Array::new();
            for s in ["mcp", "serve", "--stdio"] {
                args.push(s);
            }
            server["args"] = toml_edit::value(args);
            let mut env = toml_edit::Table::new();
            env["AGENTLAW_HOME"] = toml_edit::value(state);
            server["env"] = toml_edit::Item::Table(env);
            server["startup_timeout_sec"] = toml_edit::value(30);
            server["tool_timeout_sec"] = toml_edit::value(3600);
            if doc.get("mcp_servers").is_none() {
                doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            if !doc["mcp_servers"].is_table() {
                return Err(err(
                    "Codex mcp_servers must be a table; existing settings were preserved.",
                ));
            }
            doc["mcp_servers"]["agentlaw"] = toml_edit::Item::Table(server);
            Ok(doc.to_string())
        }
        Harness::OhMyPi => {
            let mut doc = if previous.trim().is_empty() {
                json!({})
            } else {
                agentlaw_contracts::validation::decode_unique(previous)?
            };
            let object = doc
                .as_object_mut()
                .ok_or_else(|| err("Oh My Pi config must be an object."))?;
            let servers = object
                .entry("mcpServers")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| err("mcpServers must be an object."))?;
            if servers.contains_key("agentlaw") && !may_replace {
                return Err(err("An unmanaged agentlaw MCP entry already exists. Inspect it before replacing it."));
            }
            servers.insert("agentlaw".into(),json!({"type":"stdio","command":command,"args":["mcp","serve","--stdio"],"env":{"AGENTLAW_HOME":state},"timeout":0}));
            serde_json::to_string_pretty(&doc).map_err(|_| err("Cannot encode Oh My Pi settings."))
        }
    }
}

pub(crate) fn same_owned_entry(harness: Harness, before: &str, now: &str) -> bool {
    match harness {
        Harness::Codex => {
            let (Ok(a), Ok(b)) = (
                before.parse::<toml_edit::DocumentMut>(),
                now.parse::<toml_edit::DocumentMut>(),
            ) else {
                return false;
            };
            let find = |d: &toml_edit::DocumentMut| {
                d.get("mcp_servers")
                    .and_then(|s| s.get("agentlaw"))
                    .map(ToString::to_string)
            };
            let a = find(&a);
            a.is_some() && a == find(&b)
        }
        Harness::OhMyPi => {
            let (Ok(a), Ok(b)) = (
                agentlaw_contracts::validation::decode_unique(before),
                agentlaw_contracts::validation::decode_unique(now),
            ) else {
                return false;
            };
            a.get("mcpServers")
                .and_then(|s| s.get("agentlaw"))
                .is_some()
                && a["mcpServers"]["agentlaw"] == b["mcpServers"]["agentlaw"]
        }
    }
}

/// Resolve the instruction file the harness would use, without changing it.
pub(crate) fn effective_instructions_path(harness: Harness, directory: &Path) -> Result<PathBuf> {
    if harness == Harness::Codex
        && read_optional(&directory.join("AGENTS.override.md"))?
            .is_some_and(|text| !text.trim().is_empty())
    {
        Ok(directory.join("AGENTS.override.md"))
    } else {
        Ok(directory.join("AGENTS.md"))
    }
}

#[derive(Serialize, Deserialize)]
struct Replacement {
    path: PathBuf,
    before: Option<String>,
    after: String,
}
#[derive(Serialize, Deserialize)]
struct InstallJournal {
    targets: Vec<Replacement>,
}
fn recover_journal(state: &Path) -> Result<()> {
    let path = state.join("install-pending.json");
    let Some(raw) = read_optional(&path)? else {
        return Ok(());
    };
    let journal: InstallJournal = serde_json::from_value(
        agentlaw_contracts::validation::decode_unique(&raw)?,
    )
    .map_err(|_| err("Installation recovery journal is invalid; preserve it for diagnosis."))?;
    // Validate every compare-and-swap precondition before replacing any target.
    for r in &journal.targets {
        let current = read_optional(&r.path)?;
        if current != r.before && current.as_deref() != Some(&r.after) {
            return Err(err("Settings changed since interrupted installation. No recovery overwrite was attempted; inspect install-pending.json."));
        }
    }
    for r in journal.targets {
        if read_optional(&r.path)?.as_deref() != Some(&r.after) {
            atomic_text(&r.path, &r.after)?;
        }
    }
    fs::rename(
        &path,
        state.join(format!("install-completed-{}.json", uuid::Uuid::new_v4())),
    )
    .map_err(|_| {
        err("Installation completed but its recovery journal could not be marked complete.")
    })?;
    Ok(())
}

/// The updater holds this lock from its last reachable-artifact scan through publication.
pub(crate) fn lock_install_state(state: &Path) -> Result<fs::File> {
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(state.join("install.lock"))
        .map_err(|_| err("Cannot lock installation state."))?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| {
        DomainError::new(
            "update_busy",
            "Another installation is changing this root; retry the same approved update plan.",
        )
    })?;
    Ok(lock)
}

/// Apply only the complete before/after target set approved by one update plan.
/// The caller already owns update.lock and install.lock, in that order.
pub(crate) fn apply_pinned_registration_locked(
    state: &Path,
    expected: &[(PathBuf, String, String)],
    candidate: &Path,
    executable: &Path,
    candidate_hash: &str,
) -> Result<()> {
    if expected.len() != 3
        || expected.iter().any(|(path, _, _)| !path.is_absolute())
        || expected.iter().enumerate().any(|(index, (path, _, _))| {
            expected
                .iter()
                .skip(index + 1)
                .any(|(next, _, _)| path == next)
        })
    {
        return Err(err("The pinned registration target set is invalid."));
    }
    let journal_path = state.join("install-pending.json");
    let journal = read_optional(&journal_path)?
        .map(|raw| {
            serde_json::from_value::<InstallJournal>(agentlaw_contracts::validation::decode_unique(
                &raw,
            )?)
            .map_err(|_| {
                err("Installation recovery journal is invalid; preserve it for diagnosis.")
            })
        })
        .transpose()?;
    if journal.as_ref().is_some_and(|journal| {
        journal.targets.len() != expected.len()
            || !journal.targets.iter().all(|target| {
                expected.iter().any(|(path, before, after)| {
                    target.path == *path
                        && target.before.as_deref() == Some(before.as_str())
                        && target.after == *after
                })
            })
    }) {
        return Err(err(
            "A foreign installation journal exists; this update did not replay it.",
        ));
    }
    let mut all_after = true;
    for (path, before, after) in expected {
        let current = read_optional(path)?;
        if current.as_deref() != Some(before.as_str()) && current.as_deref() != Some(after.as_str())
        {
            return Err(err(
                "A pinned registration changed outside the update plan.",
            ));
        }
        all_after &= current.as_deref() == Some(after.as_str());
    }
    copy_verified(candidate, executable, candidate_hash)?;
    if journal.is_none() && !all_after {
        let targets = expected
            .iter()
            .map(|(path, before, after)| Replacement {
                path: path.clone(),
                before: Some(before.clone()),
                after: after.clone(),
            })
            .collect();
        atomic_text(
            &journal_path,
            &serde_json::to_string_pretty(&InstallJournal { targets })
                .map_err(|_| err("Cannot encode installation recovery journal."))?,
        )?;
    }
    if journal_path.exists() {
        recover_journal(state)?;
    }
    for (path, _, after) in expected {
        if read_optional(path)?.as_deref() != Some(after.as_str()) {
            return Err(err(
                "A pinned registration differs from its approved after-state.",
            ));
        }
    }
    Ok(())
}

pub fn install(
    state: &Path,
    harness: Harness,
    directory: &Path,
    manifest: Option<&Path>,
    confirmed: bool,
) -> Result<Value> {
    absolute(state.to_path_buf())?;
    absolute(directory.to_path_buf())?;
    let current =
        std::env::current_exe().map_err(|_| err("Cannot locate the installer executable."))?;
    let hash = digest_file(&current)?;
    let executable = state
        .join("versions")
        .join(format!("{}-{}", env!("CARGO_PKG_VERSION"), &hash[..16]))
        .join(if cfg!(windows) {
            "agentlaw.exe"
        } else {
            "agentlaw"
        });
    let config = directory.join(if harness == Harness::Codex {
        "config.toml"
    } else {
        "mcp.json"
    });
    let instructions = effective_instructions_path(harness, directory)?;
    if !confirmed {
        return Ok(
            json!({"status":"confirmation_required","harness":harness.name(),"executable":executable,"config_path":config,"instructions_path":instructions,"model_manifest":manifest,"next_action":"Confirm these user-level targets with the user, then repeat with --confirm-install. Existing model/machine/memory state will be preserved. No files have been changed."}),
        );
    }
    fs::create_dir_all(state).map_err(|_| err("Cannot create installation state."))?;
    // A managed update inspects references under update.lock before publishing
    // or retiring a bundle. Installation must not create a new reference during
    // that interval, and every writer takes the locks in the same order.
    let _update_lock = if let Some(root) = crate::config::managed_install_root(state)? {
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".update.lock"))
            .map_err(|_| err("Cannot lock managed update state."))?;
        fs2::FileExt::lock_exclusive(&lock)
            .map_err(|_| err("Cannot coordinate with a managed update."))?;
        Some(lock)
    } else {
        None
    };
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("install.lock"))
        .map_err(|_| err("Cannot lock installation state."))?;
    fs2::FileExt::lock_exclusive(&lock).map_err(|_| err("Cannot coordinate installation."))?;
    recover_journal(state)?;
    let receipt = state.join(format!(
        "harness-{}-{}.json",
        harness.name(),
        format!(
            "{:x}",
            Sha256::digest(config.as_os_str().as_encoded_bytes())
        )
    ));
    let existing = read_optional(&config)?;
    let previous_receipt = read_optional(&receipt)?
        .map(|s| agentlaw_contracts::validation::decode_unique(&s))
        .transpose()?;
    let owned = previous_receipt
        .as_ref()
        .and_then(|r| r["configuration"].as_str())
        .is_some_and(|before| same_owned_entry(harness, before, existing.as_deref().unwrap_or("")));
    let after = configuration(
        harness,
        existing.as_deref().unwrap_or(""),
        &executable,
        state,
        owned,
    )?;
    let old_instructions = read_optional(&instructions)?;
    let next_instructions = upsert_bootstrap(
        old_instructions.as_deref().unwrap_or(""),
        &bootstrap(&executable, state),
    )?;
    let mut targets = vec![
        Replacement {
            path: config.clone(),
            before: existing,
            after: after.clone(),
        },
        Replacement {
            path: instructions.clone(),
            before: old_instructions,
            after: next_instructions,
        },
    ];
    copy_verified(&current, &executable, &hash)?;
    if let Some(path) = manifest {
        let mut model = read_manifest(path)?;
        for (name, asset) in [
            ("model.onnx", &mut model.onnx_model),
            ("tokenizer.json", &mut model.tokenizer_json),
            (
                if cfg!(windows) {
                    "onnxruntime.dll"
                } else if cfg!(target_os = "macos") {
                    "libonnxruntime.dylib"
                } else {
                    "libonnxruntime.so"
                },
                &mut model.runtime_library,
            ),
        ] {
            let destination = crate::config::model_artifact_root(state)?
                .join(&asset.sha256)
                .join(name);
            copy_verified(
                &asset.path,
                &destination,
                &asset.sha256.to_ascii_lowercase(),
            )?;
            asset.path = destination;
        }
        targets.push(Replacement {
            path: state.join("model-assets.json"),
            before: read_optional(&state.join("model-assets.json"))?,
            after: serde_json::to_string_pretty(&model)
                .map_err(|_| err("Cannot encode installed model manifest."))?,
        });
    }
    targets.push(Replacement {
        path: receipt.clone(),
        before: read_optional(&receipt)?,
        after: json!({"configuration":after,"executable":executable,"instructions":instructions})
            .to_string(),
    });
    atomic_text(
        &state.join("install-pending.json"),
        &serde_json::to_string_pretty(&InstallJournal { targets })
            .map_err(|_| err("Cannot encode installation recovery journal."))?,
    )?;
    recover_journal(state)?;
    let machine = crate::machine::load_or_create(state)?;
    Ok(
        json!({"status":"installed","harness":harness.name(),"executable":executable,"config_path":config,"instructions_path":instructions,"machine_id":machine.machine_id,"semantic_model_configured":installed_assets(state)?.is_some(),"next_action":"Start a new harness session and verify agentlaw tool discovery and these instructions. Installation does not create a project binding or prove model compliance. If no model bundle was supplied, lexical recall remains available with an explicit semantic-unavailable diagnostic."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_bootstrap_matches_accepted_contract() {
        let guidance = super::super::GUIDANCE.replace("\r\n", "\n");
        let accepted = guidance
            .split_once("## Bootstrap")
            .unwrap()
            .1
            .split_once("```text\n")
            .unwrap()
            .1
            .split_once("\n```")
            .unwrap()
            .0;
        let expected = accepted
            .replace(
                "{state_assignment}",
                if cfg!(windows) {
                    "$env:AGENTLAW_HOME='state'"
                } else {
                    "AGENTLAW_HOME='state'"
                },
            )
            .replace(
                "{invocation}",
                if cfg!(windows) {
                    "& 'agentlaw'"
                } else {
                    "'agentlaw'"
                },
            );
        let generated = bootstrap(Path::new("agentlaw"), Path::new("state"));
        let body = generated
            .split_once("## Agentlaw memory\n\n")
            .unwrap()
            .1
            .split_once(&format!("\n{END}"))
            .unwrap()
            .0;
        assert_eq!(body, expected);
    }
    #[test]
    fn instruction_replacement_preserves_user_text_and_refuses_ambiguous_markers() {
        let first = upsert_bootstrap("User rules\n", &format!("{BEGIN}\nold\n{END}")).unwrap();
        let second = upsert_bootstrap(&first, &format!("{BEGIN}\nnew\n{END}")).unwrap();
        assert!(same_owned_bootstrap(
            &second,
            &format!("Unrelated user rule\n{second}")
        ));
        assert!(!same_owned_bootstrap(
            &second,
            &second.replace("new", "changed")
        ));
        assert!(!same_owned_bootstrap(
            &second,
            &format!("{second}\n{BEGIN}")
        ));
        assert!(second.starts_with("User rules\n"));
        assert_eq!(second.matches(BEGIN).count(), 1);
        assert!(!second.contains("old"));
        assert!(upsert_bootstrap(BEGIN, "x").is_err());
    }
    #[test]
    fn config_preserves_other_servers_and_requires_ownership() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("app.exe");
        let old = "model = 'keep'\n[mcp_servers.other]\ncommand = 'other'\n";
        let text = configuration(Harness::Codex, old, &exe, d.path(), false).unwrap();
        let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some("keep"));
        assert_eq!(
            doc["mcp_servers"]["other"]["command"].as_str(),
            Some("other")
        );
        assert!(configuration(Harness::Codex, &text, &exe, d.path(), false).is_err());
        assert!(configuration(Harness::Codex, &text, &exe, d.path(), true).is_ok());
        let json = configuration(
            Harness::OhMyPi,
            "{\"mcpServers\":{\"other\":{\"command\":\"keep\"}}}",
            &exe,
            d.path(),
            false,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "keep");
    }
    #[test]
    fn interrupted_install_resumes_without_overwriting_new_user_edits() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("AGENTS.md");
        fs::write(&target, "before").unwrap();
        atomic_text(
            &d.path().join("install-pending.json"),
            &serde_json::to_string(&InstallJournal {
                targets: vec![Replacement {
                    path: target.clone(),
                    before: Some("before".into()),
                    after: "after".into(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        fs::write(&target, "user changed").unwrap();
        assert!(recover_journal(d.path()).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "user changed");
        fs::write(&target, "before").unwrap();
        recover_journal(d.path()).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "after");
        assert!(!d.path().join("install-pending.json").exists());
    }
}
