//! Store identity construction from an installation snapshot.

use crate::{
    installation::{machine, registry::ConfigSnapshot},
    ports::{RuntimeError, RuntimeResult},
};
use agentlaw_flows::runtime::StoreIdentity;
use std::path::Path;

pub fn resolve_import_stage(
    _installation: &Path,
    _snapshot: &ConfigSnapshot,
    _stage: &crate::git::git_ops::ImportStageContext,
) -> RuntimeResult<StoreIdentity> {
    Err(RuntimeError::StoreUnavailable(
        "Import stage is not implemented.".into(),
    ))
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
