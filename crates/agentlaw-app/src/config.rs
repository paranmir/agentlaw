//! Installation-local selection; paths never establish a project identity.
use agentlaw_contracts::{DomainError, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const MANAGED_LAYOUT_MARKER: &str = ".agentlaw-layout";
const MANAGED_LAYOUT_VERSION: &str = "agentlaw-managed-layout-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub memory_store_path: PathBuf,
    pub user_id: String,
    #[serde(default = "default_history_limit")]
    pub history_response_limit_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<PathBuf>,
    #[serde(default = "default_response_limit")]
    pub response_limit_bytes: usize,
}

impl Config {
    pub fn runtime_root(&self, installation: &Path) -> PathBuf {
        self.runtime_path
            .clone()
            .unwrap_or_else(|| installation.join("runtime"))
    }
}

pub fn default_response_limit() -> usize {
    64 * 1024
}

pub fn default_history_limit() -> usize {
    8192
}

/// A published selection names an existing authority, never permission to
/// recreate missing source/proposal state. Fresh setup uses a separate path.
pub fn require_existing_binding(installation: &Path, selected: &Config) -> Result<()> {
    let local = selected.runtime_root(installation);
    if !selected.memory_store_path.is_dir()
        || !selected.memory_store_path.join("format.md").is_file()
    {
        return Err(DomainError::new("connected_store_unavailable", "The selected memory store is missing or no longer has its format descriptor. Restore its source or explicitly connect a different existing store. No empty replacement was created."));
    }
    if !local.join("control.sqlite").is_file() {
        return Err(DomainError::new("control_backup_required", "The selected binding's authoritative proposal database is missing. Preserve its WAL/SHM and recovery material and restore a consistent backup; reconnecting or repair must not invent empty proposal state."));
    }
    if !local.join("canonical/source-fence").is_file() {
        return Err(DomainError::new("recovery_required", "The selected binding's publication fence is missing. Preserve source and local recovery material and restore the matching fence from backup. This is not a fresh store and its publication history was not reset."));
    }
    if !installation.join("machine.json").is_file() {
        return Err(DomainError::new("machine_identity_recovery_required", "This installation already has a selected store but its machine identity is missing. Restore the installation identity from backup before continuing; a replacement identity was not generated."));
    }
    Ok(())
}

pub fn state_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AGENTLAW_HOME") {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            return Ok(p);
        }
        return Err(DomainError::new(
            "invalid_configuration",
            "AGENTLAW_HOME must be absolute.",
        ));
    }
    let executable = std::env::current_exe().map_err(|_| {
        DomainError::new(
            "invalid_installation",
            "Cannot locate the Agentlaw executable to determine its installation. Set AGENTLAW_HOME explicitly or repair the installation.",
        )
    })?;
    {
        if let Some(directory) = executable.parent() {
            if directory.file_name().is_some_and(|name| name == "bin") {
                if let Some(root) = directory.parent() {
                    let state = root.join("state");
                    if managed_install_root(&state)?.is_some() {
                        return Ok(state);
                    }
                    if managed_candidate_state_exists(&state)? {
                        return Err(DomainError::new(
                            "invalid_installation",
                            "Agentlaw state exists beside this executable but its layout marker is missing. Repair the installation before using memory.",
                        ));
                    }
                }
            }
            if let Some(versions) = directory.parent() {
                if versions.file_name().is_some_and(|name| name == "versions") {
                    if let Some(state) = versions.parent() {
                        if managed_install_root(state)?.is_some() {
                            return Ok(state.to_path_buf());
                        }
                        if managed_candidate_state_exists(state)? {
                            return Err(DomainError::new(
                                "invalid_installation",
                                "Agentlaw version state exists but its layout marker is missing. Repair the installation before using memory.",
                            ));
                        }
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    let profile = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(windows))]
    let profile = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(profile) = profile.filter(|path| path.is_absolute()) {
        let state = profile.join("Agentlaw/state");
        if managed_install_root(&state)?.is_some() {
            return Ok(state);
        }
    }
    Err(DomainError::new(
        "configuration_required",
        "Install Agentlaw under the user profile or set AGENTLAW_HOME to an absolute local state directory. Existing AppData state is not selected automatically.",
    ))
}

fn managed_candidate_state_exists(state: &Path) -> Result<bool> {
    match fs::symlink_metadata(state) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(DomainError::new(
            "invalid_installation",
            "Cannot inspect Agentlaw state beside this executable. Repair the installation before using memory.",
        )),
    }
}

/// A release installer writes this marker outside AppData. Older and development
/// installations require an explicit AGENTLAW_HOME instead of an AppData fallback.
pub fn managed_install_root(state: &Path) -> Result<Option<PathBuf>> {
    if state.file_name().is_none_or(|name| name != "state") {
        return Ok(None);
    }
    let Some(root) = state.parent() else {
        return Ok(None);
    };
    let marker_path = root.join(MANAGED_LAYOUT_MARKER);
    let marker_metadata = match fs::symlink_metadata(&marker_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(DomainError::new(
                "invalid_installation",
                "Cannot inspect the Agentlaw layout marker. Repair the installation before using memory.",
            ));
        }
    };
    if !marker_metadata.is_file() || marker_metadata.file_type().is_symlink() {
        return Err(DomainError::new(
            "invalid_installation",
            "The Agentlaw layout marker must be an ordinary file.",
        ));
    }
    let marker = match fs::read_to_string(&marker_path) {
        Ok(marker) => marker,
        Err(_) => {
            return Err(DomainError::new(
                "invalid_installation",
                "Cannot read the Agentlaw layout marker. Repair the installation before using memory.",
            ));
        }
    };
    if marker.trim() != MANAGED_LAYOUT_VERSION {
        return Err(DomainError::new(
            "invalid_installation",
            "The Agentlaw layout marker is invalid or unsupported. Repair the installation before using memory.",
        ));
    }
    match fs::symlink_metadata(state) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(DomainError::new(
                "invalid_installation",
                "Agentlaw state must be an ordinary directory in this installation.",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(DomainError::new(
                "invalid_installation",
                "Cannot inspect Agentlaw state. Repair the installation before using memory.",
            ));
        }
    }
    Ok(Some(root.to_path_buf()))
}

pub fn proposed_memory_store_path() -> Result<PathBuf> {
    let state = state_root()?;
    Ok(managed_install_root(&state)?
        .map(|root| root.join("memory"))
        .unwrap_or_else(|| state.join("memory")))
}

pub fn model_artifact_root(state: &Path) -> Result<PathBuf> {
    Ok(managed_install_root(state)?
        .map(|root| root.join("models"))
        .unwrap_or_else(|| state.join("artifacts")))
}

pub fn coordination_root(state: &Path) -> PathBuf {
    state.join("source-coordination")
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn managed_layout_requires_its_exact_marker_and_state_child() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let state = root.join("state");
        assert_eq!(managed_install_root(&state).unwrap(), None);
        assert_eq!(
            model_artifact_root(&state).unwrap(),
            state.join("artifacts")
        );
        fs::write(root.join(MANAGED_LAYOUT_MARKER), "unknown-layout\n").unwrap();
        assert!(managed_install_root(&state).is_err());
        fs::write(
            root.join(MANAGED_LAYOUT_MARKER),
            format!("{MANAGED_LAYOUT_VERSION}\n"),
        )
        .unwrap();
        assert_eq!(
            managed_install_root(&state).unwrap(),
            Some(root.to_path_buf())
        );
        fs::write(&state, "not a directory").unwrap();
        assert!(managed_install_root(&state).is_err());
        fs::remove_file(&state).unwrap();
        assert_eq!(model_artifact_root(&state).unwrap(), root.join("models"));
        assert_eq!(managed_install_root(&root.join("other")).unwrap(), None);
    }
}

pub fn load(root: &Path) -> Result<Option<Config>> {
    match fs::read(root.join("config.json")) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                DomainError::new("invalid_configuration", "Configuration is not UTF-8.")
            })?;
            let value = agentlaw_contracts::validation::decode_unique(text)?;
            let config: Config = serde_json::from_value(value).map_err(|_| {
                DomainError::new(
                    "invalid_configuration",
                    "Configuration format is invalid; existing data was not reset.",
                )
            })?;
            if !config.memory_store_path.is_absolute() {
                return Err(DomainError::new(
                    "invalid_configuration",
                    "Memory store path must be absolute.",
                ));
            }
            if config.history_response_limit_bytes == 0 || config.response_limit_bytes < 4096 {
                return Err(DomainError::new(
                    "invalid_configuration",
                    "history_response_limit_bytes must be positive and response_limit_bytes must be at least 4096.",
                ));
            }
            if config.runtime_path.as_ref().is_some_and(|p| {
                let Ok(relative) = p.strip_prefix(root.join("bindings")) else {
                    return true;
                };
                let name = relative.to_string_lossy();
                !p.is_absolute() || name.len() != 64 || !name.bytes().all(|b| b.is_ascii_hexdigit())
            }) {
                return Err(DomainError::new("invalid_configuration", "The selected binding state must stay inside this installation's bindings directory."));
            }
            Ok(Some(config))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(DomainError::new(
            "configuration_unreadable",
            "Configuration could not be read; existing data was not reset.",
        )),
    }
}

/// Caller holds setup.lock. The whole selection changes at one replace boundary;
/// opened Runtime instances retain their old store until the next request.
pub fn replace_selection(root: &Path, expected: &Config, next: &Config) -> Result<()> {
    use std::io::Write;
    if load(root)?.as_ref() != Some(expected) {
        return Err(DomainError::new("configuration_changed", "Selection changed while the next store was being prepared. Retry without discarding either store."));
    }
    let mut temp = tempfile::NamedTempFile::new_in(root)
        .map_err(|_| DomainError::new("configuration_io", "Cannot prepare the next selection."))?;
    serde_json::to_writer_pretty(temp.as_file_mut(), next)
        .map_err(|_| DomainError::new("configuration_io", "Cannot encode the next selection."))?;
    temp.flush()
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|_| DomainError::new("configuration_io", "Cannot persist the next selection."))?;
    temp.persist(root.join("config.json")).map_err(|_| {
        DomainError::new(
            "configuration_io",
            "Cannot install the next selection. The previous configuration remains selected.",
        )
    })?;
    Ok(())
}

pub fn binding_root(root: &Path, source: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let path = fs::canonicalize(source).map_err(|_| {
        DomainError::new(
            "store_unavailable",
            "Cannot resolve the selected store location.",
        )
    })?;
    let legacy = root.join("runtime/control.sqlite");
    if legacy.is_file() {
        let db = rusqlite::Connection::open_with_flags(
            &legacy,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| {
            DomainError::new(
                "binding_unavailable",
                "The original local binding cannot be inspected.",
            )
        })?;
        let bound: String = db
            .query_row(
                "SELECT value FROM settings WHERE key='store_root'",
                [],
                |r| r.get(0),
            )
            .map_err(|_| {
                DomainError::new(
                    "binding_unavailable",
                    "The original local binding cannot be read.",
                )
            })?;
        if Path::new(&bound) == path {
            return Ok(root.join("runtime"));
        }
    }
    // This key coordinates a local folder, not the portable project identity.
    Ok(root.join("bindings").join(format!(
        "{:x}",
        Sha256::digest(path.as_os_str().as_encoded_bytes())
    )))
}

pub fn save_initial(root: &Path, config: &Config) -> Result<()> {
    use std::io::Write;
    fs::create_dir_all(root).map_err(|_| {
        DomainError::new(
            "configuration_io",
            "Could not create the local state directory.",
        )
    })?;
    let temp = root.join(format!(".config-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| {
            DomainError::new("configuration_io", "Could not prepare the configuration.")
        })?;
    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|_| DomainError::new("configuration_io", "Could not encode configuration."))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| {
            DomainError::new("configuration_io", "Could not persist the configuration.")
        })?;
    drop(file);
    // Hard-link installation is no-clobber: a concurrent first connection cannot be overwritten.
    fs::hard_link(&temp, root.join("config.json")).map_err(|_| {
        DomainError::new(
            "configuration_changed",
            "Configuration already exists or could not be installed; inspect it before retrying.",
        )
    })?;
    fs::remove_file(&temp).map_err(|_| {
        DomainError::new(
            "configuration_cleanup",
            "Configuration installed but temporary cleanup failed.",
        )
    })?;
    Ok(())
}

/// Only documented delivery settings are mutable here; binding changes go through setup.
pub fn set_limit(root: &Path, key: &str, value: &str) -> Result<serde_json::Value> {
    let number: usize = value.parse().map_err(|_| {
        DomainError::new(
            "invalid_configuration",
            "Use a positive integer byte count within the platform's supported range.",
        )
    })?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("setup.lock"))
        .map_err(|_| {
            DomainError::new(
                "configuration_io",
                "Connect a store before changing its delivery settings.",
            )
        })?;
    fs2::FileExt::lock_exclusive(&lock).map_err(|_| {
        DomainError::new(
            "configuration_io",
            "Cannot coordinate the configuration update.",
        )
    })?;
    let old = load(root)?.ok_or_else(|| {
        DomainError::new(
            "memory_store_connection_required",
            "Connect a memory store first.",
        )
    })?;
    let mut next = old.clone();
    match key {
        "history.response_limit_bytes" if number > 0 => next.history_response_limit_bytes = number,
        "response_limit_bytes" if number >= 4096 => next.response_limit_bytes = number,
        _ => return Err(DomainError::new("invalid_configuration", "Supported settings: history.response_limit_bytes (>0), response_limit_bytes (>=4096). These control delivery, never truncate memory.")),
    }
    replace_selection(root, &old, &next)?;
    Ok(
        serde_json::json!({"key":key,"value":number,"applies":"next_request","config_path":root.join("config.json")}),
    )
}
