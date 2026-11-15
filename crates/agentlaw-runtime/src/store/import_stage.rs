//! Isolated import review execution.
use super::service::StoreService;
use agentlaw_contracts::{DomainError, RequestControl, Result};

pub fn apply_choices(
    service: &StoreService,
    stage: &crate::git::git_ops::ImportStageContext,
    choices: serde_json::Value,
    confirmed: bool,
    control: &RequestControl,
) -> Result<()> {
    use crate::store::lane::{LaneCommand, LaneReply};
    use agentlaw_storage::import_conflict::ImportSide;
    use std::collections::BTreeMap;
    if !confirmed {
        return Err(DomainError::new(
            "confirmation_required",
            "Explicit structural choices require user confirmation.",
        ));
    }
    let choices: BTreeMap<String, ImportSide> = serde_json::from_value(choices).map_err(|_| {
        DomainError::new(
            "invalid_arguments",
            "Expected an object mapping conflict IDs to local or incoming.",
        )
    })?;
    if choices.is_empty() || choices.keys().any(|id| uuid::Uuid::parse_str(id).is_err()) {
        return Err(DomainError::new(
            "invalid_arguments",
            "Provide returned conflict IDs and explicit choices.",
        ));
    }
    control.check()?;
    let dir = stage
        .runtime_path
        .parent()
        .ok_or_else(|| DomainError::new("invalid_import_request", "Import path is invalid."))?;
    let execution = dir.join("choices-pending.json");
    #[derive(serde::Serialize, serde::Deserialize)]
    struct ChoiceExecution {
        operation: String,
        choices: BTreeMap<String, ImportSide>,
    }
    let operation = if execution.exists() {
        let pending: ChoiceExecution = serde_json::from_reader(
            std::fs::File::open(&execution)
                .map_err(|e| DomainError::new("git_io", e.to_string()))?,
        )
        .map_err(|e| DomainError::new("git_io", e.to_string()))?;
        if pending.choices != choices {
            return Err(DomainError::new(
                "pending_structural_choice",
                "Resume the preceding explicit choices first.",
            ));
        }
        pending.operation
    } else {
        let operation = uuid::Uuid::new_v4().to_string();
        crate::git::git_ops::save_local_json(
            &execution,
            &ChoiceExecution {
                operation: operation.clone(),
                choices: choices.clone(),
            },
        )?;
        operation
    };
    let lane = service.lane_sender();
    let result = lane.call(
        LaneCommand::ImportChoices {
            operation_id: operation.clone(),
            choices,
        },
        control,
    );
    if let Err(error) = result {
        if matches!(
            lane.call(
                LaneCommand::ImportChoiceDecided(operation),
                &RequestControl::default()
            ),
            Ok(LaneReply::Reserved(false))
        ) {
            std::fs::remove_file(&execution)
                .map_err(|e| DomainError::new("git_io", e.to_string()))?;
        }
        return Err(if error.code == "cancelled" {
            error
        } else {
            DomainError::new("structural_choice_failed", error.message)
        });
    }
    std::fs::remove_file(execution).map_err(|e| DomainError::new("git_io", e.to_string()))?;
    Ok(())
}

pub fn call(
    service: &StoreService,
    snapshot: &crate::installation::registry::ConfigSnapshot,
    request: serde_json::Value,
    control: &RequestControl,
) -> Result<serde_json::Value> {
    use crate::protocol::{Classified, QueryRequest};
    let text = serde_json::to_string(&request)
        .map_err(|e| DomainError::new("invalid_input", e.to_string()))?;
    let request = agentlaw_contracts::wire::ValidatedRequest::parse_public(&text)?;
    let classified = crate::protocol::classify(request);
    let is_history = matches!(&classified, Classified::Query(QueryRequest::History(_)));
    let mut value = match classified {
        Classified::Query(request) => service.query_import(request, snapshot, control)?,
        Classified::Command(crate::protocol::CommandRequest::Sync(_)) => {
            return Err(DomainError::new(
                "sync_requires_app_adapter",
                "Sync is unavailable in an isolated import review.",
            ))
        }
        Classified::Command(crate::protocol::CommandRequest::Connect(_))
        | Classified::Setup(_)
        | Classified::Admin(_) => {
            return Err(DomainError::new(
                "invalid_import_request",
                "Only read-only project discovery is allowed in an import review.",
            ))
        }
        Classified::Command(request) => service.command_import(request, snapshot, control)?,
    };
    value["workspace"] = serde_json::json!("isolated_import_review");
    value["active_memory_changed"] = serde_json::json!(false);
    service.deliver_import(value, is_history, snapshot)
}
