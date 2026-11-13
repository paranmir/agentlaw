//! Read-only source diagnosis and explicit, non-destructive derived recovery.
use crate::{config, frontend::RuntimeBackend};
use agentlaw_contracts::{DomainError, Result};

use serde_json::{json, Value};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub enum DoctorMode {
    Health,
    Audit,
}

fn runtime_probe_to_worker(probe: agentlaw_runtime::client::Probe) -> Value {
    use agentlaw_runtime::client::Probe;
    match probe {
        Probe::NotRunning {
            stale_endpoint: false,
        } => json!({"status":"not_running"}),
        Probe::NotRunning {
            stale_endpoint: true,
        } => json!({"status":"not_running", "stale_endpoint":true}),
        Probe::Unresponsive => {
            json!({"code":"worker_diagnosis_incomplete", "message":"runtime_unresponsive: runtime did not respond."})
        }
        Probe::Mismatch(reply) => {
            json!({"code":"worker_diagnosis_incomplete", "message":format!("runtime mismatch: {reply:?}")})
        }
        Probe::Running(snapshot) => {
            use agentlaw_runtime::ports::{ComponentKind, ComponentState};
            let mut diagnostics = serde_json::Map::new();
            let components: Vec<Value> = snapshot.components.into_iter().map(|(id, status)| {
                let kind = format!("{:?}", id.kind).to_ascii_lowercase();
                let state = format!("{:?}", status.state).to_ascii_lowercase();
                if matches!(status.state, ComponentState::Failed | ComponentState::Degraded | ComponentState::Recovering) {
                    if let Some(reason) = &status.reason {
                        let key = match (id.kind, id.binding.as_deref()) {
                            (ComponentKind::Model, _) => "model_load".to_owned(),
                            (ComponentKind::Semantic, Some(binding)) => format!("vector_index:{binding}"),
                            (_, Some(binding)) => format!("{kind}:{binding}"),
                            _ => kind.clone(),
                        };
                        diagnostics.insert(key, json!(reason));
                    }
                }
                let observed_at_ms = status.observed_at.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                json!({"kind":kind,"binding":id.binding,"state":state,"reason":status.reason,"observed_at_ms":observed_at_ms})
            }).collect();
            json!({"status":"running","components":components,"activity":snapshot.activity,"diagnostics":diagnostics})
        }
    }
}

pub fn doctor(state: &Path, mode: DoctorMode) -> Result<Value> {
    doctor_with_health(state, mode, &|| {
        let executable = std::env::current_exe()
            .map_err(|e| DomainError::new("worker_diagnosis_incomplete", e.to_string()))?;
        let build = agentlaw_runtime::installation::build_identity(&executable)
            .map_err(|e| DomainError::new("worker_diagnosis_incomplete", e.to_string()))?;
        Ok(runtime_probe_to_worker(agentlaw_runtime::client::probe(
            state,
            &build,
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
    let Some(_selected) = selected else {
        return Ok(
            json!({"status":"setup_required","read_only":true,"worker_runtime":worker,"observed_at_ms":observed_at_ms,"next_action":"Propose a memory store location and ask the user before creating or connecting it."}),
        );
    };
    let mut result = json!({"status":"health_checked","read_only":true,"worker_runtime":worker,"observed_at_ms":observed_at_ms});
    if matches!(mode, DoctorMode::Health) {
        return Ok(result);
    }
    let audit = match RuntimeBackend::new(state.to_owned(), false) {
        Ok(mut backend) => {
            let audit = backend.admin(
                &agentlaw_runtime::protocol::AdminCommand::Audit,
                &agentlaw_contracts::RequestControl::default(),
            );
            if let Ok(snapshot) = backend.health() {
                result["worker_runtime"] =
                    runtime_probe_to_worker(agentlaw_runtime::client::Probe::Running(snapshot));
                result["observed_at_ms"] = json!(SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64);
            }
            audit
        }
        Err(error) => Err(error),
    };
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
#[cfg(test)]
mod tests;
