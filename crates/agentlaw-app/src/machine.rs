//! Installation identity is distinct from any selected memory-store binding.
use agentlaw_contracts::{DomainError, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub machine_id: String,
    pub display_name: Option<String>,
}
fn err() -> DomainError {
    DomainError::new("machine_identity_unavailable", "The installation identity cannot be read or persisted. Existing identities were not replaced.")
}

pub fn load_or_create(state: &Path) -> Result<Machine> {
    let path = state.join("machine.json");
    if path.exists() {
        let value = agentlaw_contracts::validation::decode_unique(
            &fs::read_to_string(&path).map_err(|_| err())?,
        )?;
        let machine: Machine = serde_json::from_value(value).map_err(|_| err())?;
        agentlaw_storage::validate_id(&machine.machine_id).map_err(|_| err())?;
        return Ok(machine);
    }
    fs::create_dir_all(state).map_err(|_| err())?;
    if state.join("config.json").exists() {
        return Err(err());
    }
    let id = uuid::Uuid::new_v4().to_string();
    agentlaw_storage::validate_id(&id).map_err(|_| err())?;
    let machine = Machine {
        machine_id: id,
        display_name: None,
    };
    let mut tmp = tempfile::NamedTempFile::new_in(state).map_err(|_| err())?;
    serde_json::to_writer_pretty(tmp.as_file_mut(), &machine).map_err(|_| err())?;
    tmp.flush()
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|_| err())?;
    match tmp.persist_noclobber(&path) {
        Ok(_) => Ok(machine),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => load_or_create(state),
        Err(_) => Err(err()),
    }
}

pub fn name(state: &Path, display_name: &str) -> Result<Machine> {
    if display_name.trim().is_empty() || display_name.len() > 256 {
        return Err(DomainError::new(
            "invalid_machine_name",
            "Choose a nonempty human-readable name of at most 256 UTF-8 bytes.",
        ));
    }
    let mut machine = load_or_create(state)?;
    machine.display_name = Some(display_name.to_owned());
    let mut tmp = tempfile::NamedTempFile::new_in(state).map_err(|_| err())?;
    serde_json::to_writer_pretty(tmp.as_file_mut(), &machine).map_err(|_| err())?;
    tmp.flush()
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|_| err())?;
    tmp.persist(state.join("machine.json")).map_err(|_| err())?;
    Ok(machine)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn naming_and_reload_never_change_identity() {
        let root = tempfile::tempdir().unwrap();
        let first = load_or_create(root.path()).unwrap();
        assert!(first.display_name.is_none());
        let named = name(root.path(), "개발 노트북").unwrap();
        assert_eq!(named.machine_id, first.machine_id);
        assert_eq!(
            load_or_create(root.path()).unwrap().display_name,
            named.display_name
        );
        fs::write(root.path().join("machine.json"), "{}").unwrap();
        assert!(load_or_create(root.path()).is_err());
    }

    #[test]
    fn selected_store_never_regenerates_missing_machine_identity() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("config.json"), b"selected").unwrap();
        assert!(load_or_create(root.path()).is_err());
        assert!(!root.path().join("machine.json").exists());
    }
}
