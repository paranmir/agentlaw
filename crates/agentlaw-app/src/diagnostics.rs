//! Read-only source diagnosis and explicit, non-destructive derived recovery.
use crate::{config, installed, machine};
use agentlaw_contracts::{DomainError, Result};
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
    let mut error=DomainError::new(code,format!("Diagnosis or repair could not complete: {e}. Source, retained proposals and recovery materials were preserved; do not treat this as a clean result."));
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
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| {
                DomainError::new(
                    "local_state_unreadable",
                    "A local database cannot be inspected. No database was reset.",
                )
            })?;
    let result: String = db
        .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
        .map_err(|_| {
            DomainError::new(
                "local_state_unreadable",
                "A local database integrity check failed.",
            )
        })?;
    Ok(json!({"present":true,"integrity_ok":result=="ok"}))
}
pub fn doctor(state: &Path) -> Result<Value> {
    // Inspect durable worker state only. Diagnosis does not start or reconnect it.
    let worker=agentlaw_worker::inspect_runtime(&state.join("worker")).unwrap_or_else(|error|json!({
        "code":"worker_diagnosis_incomplete","message":error.to_string(),
        "next_action":"Preserve worker state and inspect its database access/integrity. No worker was started or reset by this diagnosis."
    }));
    let Some(selected) = config::load(state)? else {
        return Ok(
            json!({"status":"setup_required","read_only":true,"worker_runtime":worker,"next_action":"Propose a memory store location and ask the user before creating or connecting it."}),
        );
    };
    let local = selected.runtime_root(state);
    // Does not bootstrap/recover/migrate the store. Owned temporary disk spools
    // are necessary to check a large DAG without holding all bodies in RAM.
    let store = Store::open_read_only_with_coordination(
        &selected.memory_store_path,
        local.join("canonical"),
        config::coordination_root(state),
    )
    .map_err(storage_error)?;
    let audit = store.audit_source().map_err(storage_error)?;
    let control = database_health(&local.join("control.sqlite"))?;
    let journal = database_health(&local.join("canonical/journal.sqlite"))?;
    let background = match agentlaw_flows::Runtime::background_status(&local) {
        Ok(status) => status,
        Err(error) => Some(error),
    };
    let missing = [&control, &journal].iter().any(|v| v["present"] == false);
    let healthy = [&control, &journal]
        .iter()
        .all(|v| v["integrity_ok"] == true);
    Ok(
        json!({"status":if healthy{"source_and_local_integrity_checked"}else if missing{"local_state_missing"}else{"local_state_corrupt"},
        "read_only":true,"temporary_derived_audit":true,"source":audit,
        "local_databases":{"control":control,"publication_journal":journal},
        "background_indexing":background,
        "worker_runtime":worker,
        "checks":["current_frame_lengths_digests_and_domain_values","history_frame_integrity","causal_parents_and_ownership","lossless_delta_reconstruction","checkpoint_state_digests","current_history_agreement","redirect_destinations_and_cycles","local_database_integrity"],
        "not_checked":["semantic_retrieval_quality","all_platform_power_loss_behavior","harness_instruction_visibility"],
        "next_action":"Diagnosis does not modify memory or proposals. For interrupted publication or a missing derived ledger/index, use repair. Never delete canonical Markdown, pending proposals or original recovery decisions to make a warning disappear."}),
    )
}
pub fn repair(state: &Path, control: agentlaw_flows::RequestControl) -> Result<Value> {
    let selected = config::load(state)?.ok_or_else(|| {
        DomainError::new(
            "memory_store_connection_required",
            "Connect a memory store before repair.",
        )
    })?;
    let local = selected.runtime_root(state);
    control.check()?;
    config::require_existing_binding(state, &selected)?;
    control.phase("recovering_recorded_publication");
    // Open may finish the exact durable decision. It never invents a new memory.
    let mut journal_repair = None;
    let store = match Store::open_with_coordination(
        &selected.memory_store_path,
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
                    &selected.memory_store_path,
                    local.join("canonical"),
                    config::coordination_root(state),
                )
                .map_err(storage_error)?,
            );
            Store::open_with_coordination(
                &selected.memory_store_path,
                local.join("canonical"),
                config::coordination_root(state),
            )
            .map_err(storage_error)?
        }
        Err(error) => return Err(storage_error(error)),
    };
    control.check()?;
    control.phase("auditing_canonical_source");
    let audit = store.audit_source().map_err(storage_error)?;
    control.check()?;
    control.phase("repairing_publication_ledger");
    let ledger = store.rebuild_control_ledger().map_err(storage_error)?;
    let control_health = database_health(&local.join("control.sqlite"));
    if control_health.as_ref().is_err()
        || control_health
            .as_ref()
            .is_ok_and(|v| v["present"] != true || v["integrity_ok"] != true)
    {
        return Err(DomainError::new("control_backup_required",format!("Canonical recovery was preserved, but {} is absent or cannot be validated. This selected binding's database contains authoritative unpublished proposals and authoring decisions, not just a rebuildable index. Preserve any remaining database and WAL/SHM sidecars. Stop clients and restore a consistent backup for this same binding, or investigate permissions/corruption. Repair will not replace it with an empty database or claim those proposals were recovered.",local.join("control.sqlite").display())));
    }
    let identity = machine::load_or_create(state)?;
    let mut runtime = agentlaw_flows::Runtime::open_with_machine_and_coordination(
        &selected.memory_store_path,
        &local,
        &selected.user_id,
        &identity.machine_id,
        config::coordination_root(state),
    )?;
    let worker_config = installed::worker_config(state)?;
    if worker_config.model.is_some() {
        runtime=runtime.with_worker(agentlaw_worker::ProcessRuntime::attach(&worker_config)
            .map_err(|_|DomainError::new("worker_unavailable","Source recovery completed, but the model worker could not attach. Retry repair after resolving worker setup; source was not rolled back."))?);
    }
    let derived = runtime.repair_derived(control)?;
    Ok(
        json!({"status":"repaired","source":audit,"publication_ledger":ledger,"journal_repair":journal_repair,"derived":derived,
        "original_memory_preserved":true,"retained_proposals_preserved":true,
        "next_action":"Resume the original request. A semantic-unavailable diagnostic still means vector rebuilding is incomplete; lexical recovery is not a claim of semantic completion."}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_installation_is_not_created_by_diagnosis() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("absent");
        assert_eq!(doctor(&state).unwrap()["status"], "setup_required");
        assert!(!state.exists());
    }
}
