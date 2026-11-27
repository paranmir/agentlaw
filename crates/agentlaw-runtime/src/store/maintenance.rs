//! Read-only audit and binding-exclusive source recovery.
use crate::installation::{config, registry::ConfigSnapshot};
use agentlaw_contracts::{DomainError, RequestControl, Result};
use agentlaw_flows::store::PUBLICATION_RECOVERY_NEXT_ACTION;
use agentlaw_storage::Store;
use serde_json::{json, Value};
use std::path::Path;

fn storage_error(e: agentlaw_storage::Error) -> DomainError {
    let recovery_cause = e.decided_recovery_cause();
    let cause = recovery_cause.unwrap_or(&e);
    let code = match cause {
        agentlaw_storage::Error::RecoveryRequired(_) => "recovery_required",
        agentlaw_storage::Error::LocalBindingMismatch => "binding_mismatch",
        agentlaw_storage::Error::CoverageLost => "coverage_lost",
        agentlaw_storage::Error::Stale(_) => "source_changed",
        agentlaw_storage::Error::Corrupt(_) => "source_corrupt",
        agentlaw_storage::Error::InsufficientResource { .. } => "insufficient_resource",
        agentlaw_storage::Error::ResourceUnknown(_) => "resource_capacity_unknown",
        agentlaw_storage::Error::SourceRead { .. } => "diagnostic_io",
        agentlaw_storage::Error::Io(_) | agentlaw_storage::Error::LocalRecallSpool(_) => {
            "local_state_unreadable"
        }
        agentlaw_storage::Error::Sql(_) => "local_state_unreadable",
        _ => "diagnostic_incomplete",
    };
    let mut error = DomainError::new(code, format!("Diagnosis or repair could not complete: {e}. Source, retained proposals and recovery materials were preserved; do not treat this as a clean result."));
    error.retryable = matches!(
        cause,
        agentlaw_storage::Error::InsufficientResource { .. }
            | agentlaw_storage::Error::ResourceUnknown(_)
            | agentlaw_storage::Error::Stale(_)
    );
    if recovery_cause.is_some() {
        error = error.with_next_action(PUBLICATION_RECOVERY_NEXT_ACTION);
    }
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
fn audit_report_failure(error: DomainError) -> Value {
    json!({"status":"failed","error":{"code":error.code,"message":error.message}})
}
pub fn audit(
    state: &Path,
    snapshot: &ConfigSnapshot,
    control: &RequestControl,
    background: Value,
) -> Result<Value> {
    control.check()?;
    let local = snapshot.config.runtime_root(state);
    let store = match Store::open_read_only_with_coordination(
        &snapshot.config.memory_store_path,
        local.join("canonical"),
        config::coordination_root(state),
    ) {
        Ok(store) => store,
        Err(cause) => return Ok(audit_report_failure(storage_error(cause))),
    };
    let source = match store.audit_source() {
        Ok(source) => source,
        Err(cause) => {
            use agentlaw_contracts::{AuditFailureDetails, AuditSourceFailure};
            let (kind, action) = match &cause {
                agentlaw_storage::Error::Corrupt(_) => (
                    AuditSourceFailure::Corrupt,
                    "Preserve the source and restore the damaged canonical data from a verified copy before auditing again.",
                ),
                agentlaw_storage::Error::SourceRead { .. } => (
                    AuditSourceFailure::Read,
                    "Restore read access to the source and repeat doctor --audit.",
                ),
                _ => return Ok(audit_report_failure(storage_error(cause))),
            };
            return Err(storage_error(cause)
                .with_next_action(action)
                .with_diagnostic(AuditFailureDetails {
                    audit_source_failure: kind,
                    worker_runtime: None,
                    observed_at_ms: None,
                }));
        }
    };
    control.check()?;
    let result = (|| -> Result<Value> {
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
    Ok(result.unwrap_or_else(audit_report_failure))
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
        Err(error)
            if matches!(
                error.decided_recovery_cause().unwrap_or(&error),
                agentlaw_storage::Error::Sql(rusqlite::Error::SqliteFailure(detail, _))
                    if matches!(
                        detail.code,
                        rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                    )
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
