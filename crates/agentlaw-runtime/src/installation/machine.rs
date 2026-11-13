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
    if let Some(machine) = inspect(state)? {
        return Ok(machine);
    }
    let path = state.join("machine.json");
    {
        fs::create_dir_all(state).map_err(|_| err())?;
    }
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

pub fn inspect(state: &Path) -> Result<Option<Machine>> {
    let path = state.join("machine.json");
    match fs::read_to_string(&path) {
        Ok(text) => {
            let value = agentlaw_contracts::validation::decode_unique(&text)?;
            let machine: Machine = serde_json::from_value(value).map_err(|_| err())?;
            agentlaw_storage::validate_id(&machine.machine_id).map_err(|_| err())?;
            Ok(Some(machine))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(err()),
    }
}

/// Validate a display name without reading or creating the machine identity.
pub fn validate_name(display_name: &str) -> Result<()> {
    if display_name.trim().is_empty() || display_name.len() > 256 {
        return Err(DomainError::new(
            "invalid_machine_name",
            "Choose a nonempty human-readable name of at most 256 UTF-8 bytes.",
        ));
    }
    Ok(())
}

pub fn name(state: &Path, display_name: &str) -> Result<Machine> {
    validate_name(display_name)?;
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
mod tests;
