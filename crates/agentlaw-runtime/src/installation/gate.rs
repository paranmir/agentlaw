//! Reads and writes the update maintenance marker.
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn error(code: &str, message: &str) -> DomainError {
    DomainError::new(code, message)
}
pub fn path(state: &Path) -> PathBuf {
    state.join("update-maintenance.json")
}

pub fn check_start(state: &Path) -> Result<()> {
    let bytes = match fs::read(path(state)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(error(
                "update_maintenance",
                "Cannot read the update maintenance gate.",
            ))
        }
    };
    let marker: Value = serde_json::from_slice(&bytes).map_err(|_| {
        error(
            "update_maintenance",
            "The update maintenance gate is invalid.",
        )
    })?;
    let id = marker["plan_id"].as_str().ok_or_else(|| {
        error(
            "update_maintenance",
            "The update maintenance gate has no plan.",
        )
    })?;
    if std::env::var("AGENTLAW_UPDATE_PROBE_PLAN").ok().as_deref() == Some(id) {
        if let (Some(candidate), Some(hash), Ok(current)) = (
            marker["candidate"].as_str(),
            marker["candidate_sha256"].as_str(),
            std::env::current_exe(),
        ) {
            if fs::canonicalize(&current)
                .ok()
                .zip(fs::canonicalize(candidate).ok())
                .is_some_and(|(a, b)| a == b)
                && super::build_identity(&current).ok().as_deref() == Some(hash)
            {
                return Ok(());
            }
        }
    }
    Err(error(
        "update_maintenance",
        "Agentlaw is being replaced; restart the harness after the update command completes.",
    ))
}

pub struct MaintenanceGate {
    path: PathBuf,
    id: String,
    retain_on_drop: bool,
}
impl MaintenanceGate {
    pub fn open(state: &Path, id: &str, candidate: &Path, hash: &str) -> Result<Self> {
        let path = path(state);
        let marker =
            json!({"plan_id":id,"candidate":candidate,"candidate_sha256":hash,"drain":false});
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
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
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
            id: id.into(),
            retain_on_drop: false,
        })
    }
    pub fn retain_for_recovery(&mut self) {
        self.retain_on_drop = true;
    }
    pub fn set_drain(&self, drain: bool) -> Result<()> {
        let mut marker = self.read_owned()?;
        marker["drain"] = json!(drain);
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("update_maintenance", "Invalid gate path."))?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| error("update_maintenance", "Cannot stage the maintenance gate."))?;
        serde_json::to_writer(&mut tmp, &marker)
            .map_err(|_| error("update_maintenance", "Cannot encode the maintenance gate."))?;
        tmp.flush()
            .and_then(|_| tmp.as_file().sync_all())
            .map_err(|_| error("update_maintenance", "Cannot persist the maintenance gate."))?;
        tmp.persist(&self.path)
            .map_err(|_| error("update_maintenance", "Cannot publish the maintenance gate."))?;
        Ok(())
    }
    fn read_owned(&self) -> Result<Value> {
        let marker: Value =
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
        Ok(marker)
    }
    pub fn close(&mut self) -> Result<()> {
        self.read_owned()?;
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
        if !self.retain_on_drop && self.read_owned().is_ok() {
            let _ = fs::remove_file(&self.path);
        }
    }
}
