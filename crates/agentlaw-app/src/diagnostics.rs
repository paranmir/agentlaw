//! Read-only source diagnosis and explicit, non-destructive derived recovery.
use crate::{config, installed, machine};
use agentlaw_contracts::{DomainError, Result};
use agentlaw_storage::Store;
use serde_json::{json, Value};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

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
pub enum DoctorMode {
    Health,
    Audit,
}

fn runtime_probe_to_worker(_probe: agentlaw_runtime::client::Probe) -> Value {
    unimplemented!("runtime probe to worker diagnosis")
}

pub fn doctor(state: &Path, mode: DoctorMode) -> Result<Value> {
    doctor_with_health(state, mode, &|| {
        Ok(runtime_probe_to_worker(agentlaw_runtime::client::probe(
            state,
            env!("CARGO_PKG_VERSION"),
            std::time::Duration::from_secs(5),
        )))
    })
}

pub fn doctor_with_health(
    state: &Path,
    mode: DoctorMode,
    health: &dyn Fn() -> Result<Value>,
) -> Result<Value> {
    let selected = config::load(state)?;
    let worker=health().unwrap_or_else(|error|json!({
        "code":"worker_diagnosis_incomplete","message":error.to_string(),
        "next_action":"Preserve worker state and inspect its database access/integrity. No worker was started or reset by this diagnosis."
    }));
    let observed_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let Some(selected) = selected else {
        return Ok(
            json!({"status":"setup_required","read_only":true,"worker_runtime":worker,"observed_at_ms":observed_at_ms,"next_action":"Propose a memory store location and ask the user before creating or connecting it."}),
        );
    };
    let mut result = json!({"status":"health_checked","read_only":true,"worker_runtime":worker,"observed_at_ms":observed_at_ms});
    if matches!(mode, DoctorMode::Health) {
        return Ok(result);
    }
    let audit = (|| -> Result<Value> {
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
        if control["present"] == false {
            return Err(DomainError::new(
                "local_state_missing",
                "The local control database is missing.",
            ));
        }
        let corrupt = [&control, &journal]
            .iter()
            .any(|v| v["present"] == true && v["integrity_ok"] != true);
        if corrupt {
            return Err(DomainError::new(
                "local_state_corrupt",
                "A local database failed its integrity check.",
            ));
        }
        Ok(json!({"status":"passed","source":audit,
        "local_databases":{"control":control,"publication_journal":journal},
        "background_indexing":background}))
    })();
    result["audit"] = match audit {
        Ok(value) => value,
        Err(error) => {
            json!({"status":"failed","error":{"code":error.code,"message":error.message}})
        }
    };
    result["temporary_derived_audit"] = json!(true);
    result["checks"] = json!([
        "current_frame_lengths_digests_and_domain_values",
        "history_frame_integrity",
        "causal_parents_and_ownership",
        "lossless_delta_reconstruction",
        "checkpoint_state_digests",
        "current_history_agreement",
        "redirect_destinations_and_cycles",
        "local_database_integrity"
    ]);
    result["not_checked"] = json!([
        "semantic_retrieval_quality",
        "all_platform_power_loss_behavior",
        "harness_instruction_visibility"
    ]);
    result["next_action"] = json!("Diagnosis does not modify memory or proposals. For interrupted publication or a missing derived ledger/index, use repair. Never delete canonical Markdown, pending proposals or original recovery decisions to make a warning disappear.");
    Ok(result)
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
mod tests;
