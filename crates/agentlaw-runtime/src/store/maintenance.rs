//! Read-only audit and binding-exclusive source recovery.
use crate::installation::{config, registry::ConfigSnapshot};
use agentlaw_contracts::{DomainError, RequestControl, Result};
use agentlaw_storage::Store;
use serde_json::{json, Value};
use std::path::Path;

fn storage_error(e: agentlaw_storage::Error) -> DomainError {
    let code = match &e {
        agentlaw_storage::Error::RecoveryRequired(_) => "recovery_required",
        agentlaw_storage::Error::LocalBindingMismatch => "binding_mismatch",
        agentlaw_storage::Error::CoverageLost => "coverage_lost",
        agentlaw_storage::Error::Stale(_) => "source_changed",
        agentlaw_storage::Error::Corrupt(_) => "source_corrupt",
        agentlaw_storage::Error::InsufficientResource { .. } => "insufficient_resource",
        agentlaw_storage::Error::ResourceUnknown(_) => "resource_capacity_unknown",
        agentlaw_storage::Error::Io(_) => "diagnostic_io",
        agentlaw_storage::Error::Sql(_) => "local_state_unreadable",
        _ => "diagnostic_incomplete",
    };
    let mut error = DomainError::new(code, format!("Diagnosis or repair could not complete: {e}. Source, retained proposals and recovery materials were preserved; do not treat this as a clean result."));
    error.retryable = matches!(
        e,
        agentlaw_storage::Error::InsufficientResource { .. }
            | agentlaw_storage::Error::ResourceUnknown(_)
            | agentlaw_storage::Error::Stale(_)
    );
    error
}
fn database_health(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({"present":false}));
    }
    let db = agentlaw_sqlite::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| {
            DomainError::new(
                "local_state_unreadable",
                "A local database cannot be inspected. No database was reset.",
            )
        })?;
    let result: String = db
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|_| {
            DomainError::new(
                "local_state_unreadable",
                "A local database integrity check failed.",
            )
        })?;
    Ok(json!({"present":true,"integrity_ok":result == "ok"}))
}
fn journal_corrupt(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let inspect = || -> std::result::Result<String, rusqlite::Error> {
        let db =
            agentlaw_sqlite::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
    };
    match inspect() {
        Ok(result) => Ok(result != "ok"),
        Err(rusqlite::Error::SqliteFailure(detail, _)) if matches!(detail.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => Ok(true),
        Err(error) => Err(DomainError::new("local_state_unreadable", format!("The publication journal could not be inspected: {error}. No database was replaced."))),
    }
}
pub fn audit(
    state: &Path,
    snapshot: &ConfigSnapshot,
    control: &RequestControl,
    background: Value,
) -> Result<Value> {
    control.check()?;
    let local = snapshot.config.runtime_root(state);
    let result = (|| -> Result<Value> {
        let store = Store::open_read_only_with_coordination(
            &snapshot.config.memory_store_path,
            local.join("canonical"),
            config::coordination_root(state),
        )
        .map_err(storage_error)?;
        let source = store.audit_source().map_err(storage_error)?;
        control.check()?;
        let control_db = database_health(&local.join("control.sqlite"))?;
        let journal = database_health(&local.join("canonical/journal.sqlite"))?;
        if control_db["present"] == false {
            return Err(DomainError::new(
                "local_state_missing",
                "The local control database is missing.",
            ));
        }
        if [&control_db, &journal]
            .iter()
            .any(|v| v["present"] == true && v["integrity_ok"] != true)
        {
            return Err(DomainError::new(
                "local_state_corrupt",
                "A local database failed its integrity check.",
            ));
        }
        Ok(
            json!({"status":"passed","source":source,"local_databases":{"control":control_db,"publication_journal":journal},"background_indexing":background}),
        )
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) if error.code == "cancelled" => Err(error),
        Err(error) => {
            Ok(json!({"status":"failed","error":{"code":error.code,"message":error.message}}))
        }
    }
}

pub fn repair(
    state: &Path,
    snapshot: &ConfigSnapshot,
    control: &RequestControl,
    reopen: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    let local = snapshot.config.runtime_root(state);
    control.check()?;
    config::require_existing_binding(state, &snapshot.config)?;
    control.phase("recovering_recorded_publication");
    let mut journal_repair = None;
    let mut store = match Store::open_with_coordination(
        &snapshot.config.memory_store_path,
        local.join("canonical"),
        config::coordination_root(state),
    ) {
        Ok(store) => store,
        Err(agentlaw_storage::Error::Sql(rusqlite::Error::SqliteFailure(ref detail, _)))
            if matches!(
                detail.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            control.check()?;
            control.phase("rebuilding_corrupt_journal_from_retained_decisions");
            journal_repair = Some(
                Store::repair_local_journal_with_coordination(
                    &snapshot.config.memory_store_path,
                    local.join("canonical"),
                    config::coordination_root(state),
                )
                .map_err(storage_error)?,
            );
            Store::open_with_coordination(
                &snapshot.config.memory_store_path,
                local.join("canonical"),
                config::coordination_root(state),
            )
            .map_err(storage_error)?
        }
        Err(error) => return Err(storage_error(error)),
    };
    control.check()?;
    if journal_corrupt(&local.join("canonical/journal.sqlite"))? {
        drop(store);
        control.check()?;
        control.phase("rebuilding_corrupt_journal_from_retained_decisions");
        journal_repair = Some(
            Store::repair_local_journal_with_coordination(
                &snapshot.config.memory_store_path,
                local.join("canonical"),
                config::coordination_root(state),
            )
            .map_err(storage_error)?,
        );
        store = Store::open_with_coordination(
            &snapshot.config.memory_store_path,
            local.join("canonical"),
            config::coordination_root(state),
        )
        .map_err(storage_error)?;
    }
    control.check()?;
    control.phase("auditing_canonical_source");
    let source = store.audit_source().map_err(storage_error)?;
    control.check()?;
    control.phase("repairing_publication_ledger");
    let ledger = store.rebuild_control_ledger().map_err(storage_error)?;
    let healthy = database_health(&local.join("control.sqlite"));
    if healthy.as_ref().is_err()
        || healthy
            .as_ref()
            .is_ok_and(|v| v["present"] != true || v["integrity_ok"] != true)
    {
        return Err(DomainError::new("control_backup_required", format!("Canonical recovery was preserved, but {} is absent or cannot be validated. Preserve the control database and restore a consistent backup; proposals were not reset.", local.join("control.sqlite").display())));
    }
    drop(store);
    control.check()?;
    let derived = reopen()?;
    Ok(
        json!({"status":"repaired","source":source,"publication_ledger":ledger,"journal_repair":journal_repair,"derived":derived,"original_memory_preserved":true,"retained_proposals_preserved":true,"next_action":"Resume the original request. A semantic-unavailable diagnostic still means vector rebuilding is incomplete; lexical recovery is not a claim of semantic completion."}),
    )
}
