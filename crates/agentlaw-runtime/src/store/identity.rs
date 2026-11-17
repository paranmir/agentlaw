//! Store identity construction from an installation snapshot.

use crate::{
    installation::{machine, registry::ConfigSnapshot},
    ports::{RuntimeError, RuntimeResult},
};
use agentlaw_flows::store::StoreIdentity;
use std::path::Path;

pub fn resolve_import_stage(
    installation: &Path,
    snapshot: &ConfigSnapshot,
    stage: &crate::git::git_ops::ImportStageContext,
) -> RuntimeResult<StoreIdentity> {
    use sha2::{Digest, Sha256};
    let active = resolve(installation, snapshot)?;
    let reference = stage
        .runtime_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|v| v.to_str())
        .ok_or_else(|| RuntimeError::StoreUnavailable("Invalid import reference.".into()))?;
    uuid::Uuid::parse_str(reference)
        .map_err(|_| RuntimeError::StoreUnavailable("Invalid import reference.".into()))?;
    let digest = Sha256::digest(
        format!(
            "agentlaw-import-binding\0{}\0{}",
            active.binding_id, reference
        )
        .as_bytes(),
    );
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let derived = uuid::Uuid::from_bytes(bytes).to_string();
    let marker_path = stage.runtime_path.join("control-initialized.json");
    let db_path = stage.runtime_path.join("control.sqlite");
    let marker = match std::fs::read(&marker_path) {
        Ok(bytes) => {
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
                RuntimeError::Domain(agentlaw_contracts::DomainError::new(
                    "control_backup_required",
                    "Staged control identity is invalid; restore its authority.",
                ))
            })?;
            Some(
                value
                    .get("binding_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RuntimeError::Domain(agentlaw_contracts::DomainError::new(
                            "control_backup_required",
                            "Staged control identity is missing.",
                        ))
                    })?
                    .to_owned(),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(RuntimeError::StoreUnavailable(error.to_string())),
    };
    let recorded = if db_path.exists() {
        use rusqlite::OptionalExtension;
        let db = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| {
            RuntimeError::Domain(agentlaw_contracts::DomainError::new(
                "control_backup_required",
                "Staged control database cannot be inspected.",
            ))
        })?;
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='settings')",
                [],
                |row| row.get(0),
            )
            .map_err(|_| {
                RuntimeError::Domain(agentlaw_contracts::DomainError::new(
                    "control_backup_required",
                    "Staged control database cannot be inspected.",
                ))
            })?;
        if exists {
            db.query_row(
                "SELECT value FROM settings WHERE key='binding_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| {
                RuntimeError::Domain(agentlaw_contracts::DomainError::new(
                    "control_backup_required",
                    "Staged control identity cannot be inspected.",
                ))
            })?
        } else {
            None
        }
    } else {
        None
    };
    if marker.is_some() && (recorded.is_none() || marker != recorded) {
        return Err(RuntimeError::Domain(agentlaw_contracts::DomainError::new(
            "control_backup_required",
            "Staged control marker and database do not match.",
        )));
    }
    let binding_id = recorded.or(marker).unwrap_or(derived);
    if uuid::Uuid::parse_str(&binding_id).is_err() || binding_id == active.binding_id {
        return Err(RuntimeError::Domain(agentlaw_contracts::DomainError::new(
            "store_binding_mismatch",
            "Staged control identity is invalid or matches the active binding.",
        )));
    }
    Ok(StoreIdentity {
        root: stage.store_path.clone(),
        local: stage.runtime_path.clone(),
        binding_id,
        ..active
    })
}

pub fn resolve(installation: &Path, snapshot: &ConfigSnapshot) -> RuntimeResult<StoreIdentity> {
    let local = snapshot.config.runtime_root(installation);
    let binding_id = match std::fs::read(local.join("control-initialized.json")) {
        Ok(bytes) => {
            let marker: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?;
            marker
                .get("binding_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RuntimeError::StoreUnavailable("missing binding identity".into()))?
                .to_owned()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use rusqlite::OptionalExtension;
            let db = local.join("control.sqlite");
            if db.exists() {
                let connection = rusqlite::Connection::open_with_flags(
                    db,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )
                .map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?;
                let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='settings')", [], |r| r.get(0)).map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?;
                if exists {
                    connection
                        .query_row(
                            "SELECT value FROM settings WHERE key='binding_id'",
                            [],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()
                        .map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
                } else {
                    uuid::Uuid::new_v4().to_string()
                }
            } else {
                uuid::Uuid::new_v4().to_string()
            }
        }
        Err(e) => return Err(RuntimeError::StoreUnavailable(e.to_string())),
    };
    Ok(StoreIdentity {
        root: snapshot.config.memory_store_path.clone(),
        local,
        user_id: snapshot.config.user_id.clone(),
        machine_id: machine::load_or_create(installation)?.machine_id,
        binding_id,
        history_limit: snapshot.config.history_response_limit_bytes,
        response_limit: snapshot.config.response_limit_bytes,
        config_file: Some(
            std::path::absolute(installation.join("config.json"))
                .map_err(|e| RuntimeError::StoreUnavailable(e.to_string()))?,
        ),
    })
}
