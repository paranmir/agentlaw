//! Typed sharing and isolated-import execution.

use super::{
    commands::{ImportCommand, ShareCommand},
    dispatch::{cli_control, selected_store},
    input::{import_choices, read_stdin},
};
use agentlaw_app::{config, git_ops};
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};

pub(super) fn execute(command: ShareCommand) -> Result<Value> {
    match command {
        ShareCommand::Inspect(args) => {
            let (store, local) = selected_store()?;
            let mut result = agentlaw_contracts::to_value(git_ops::inspect_share(
                &store,
                &local.join("git"),
                &args.remote,
                &args.target_ref,
            )?)?;
            if result["findings"].as_array().is_some_and(|v| !v.is_empty()) {
                result["next_action"] = json!("Explain the detected pattern categories in the user's language. Ask whether to share unchanged or cancel. Do not infer permission from a private destination, modify memory, mask content or rewrite history without a separate decision.");
            }
            Ok(result)
        }
        ShareCommand::Push(args) => {
            let (store, local) = selected_store()?;
            agentlaw_contracts::to_value(git_ops::push_review(
                &store,
                &local.join("git"),
                &args.review,
                args.allow_sensitive && args.user_confirmed,
            )?)
        }
        ShareCommand::Fetch(args) => {
            let (store, local) = selected_store()?;
            agentlaw_contracts::to_value(git_ops::fetch_share(
                &store,
                &local.join("git"),
                &args.remote,
            )?)
        }
        ShareCommand::Import { command } => import(command),
    }
}

fn import(command: ImportCommand) -> Result<Value> {
    match command {
        ImportCommand::Prepare(args) => {
            let (store, local) = selected_store()?;
            let mut result = agentlaw_contracts::to_value(git_ops::prepare_import(
                &store,
                &local.join("git"),
                &args.commit,
            )?)?;
            result["next_action"]=json!("This is an isolated review workspace, not active memory. Inspect structural_conflicts, explain both states in the user's language, and ask the user to choose local or incoming where required. Submit agreed structural choices with share import resolve --ref <import_ref> --choices <JSON-file-or-> --user-confirmed. Use share import call --ref <import_ref> --json - with ordinary agentlaw requests for memory-head inspection and agreed semantic edits. No active memory or Git HEAD has changed. Publication requires a separate confirmation of the complete resolved import.");
            Ok(result)
        }
        ImportCommand::Inspect(args) => {
            let (store, local) = selected_store()?;
            agentlaw_contracts::to_value(git_ops::inspect_import(
                &store,
                &local.join("git"),
                &args.import_ref,
            )?)
        }
        ImportCommand::Call(args) => {
            let request = agentlaw_contracts::parse_request(&read_stdin()?)?;
            if matches!(&request,agentlaw_contracts::Request::ConnectProjectMemory(c) if c.memory_store_path.is_some())
            {
                return Err(DomainError::new("invalid_import_request","An import-review request cannot switch the installation's memory store. Omit memory_store_path."));
            }
            let (store, local) = selected_store()?;
            let stage = git_ops::get_import_stage(&store, &local.join("git"), &args.import_ref)?;
            let state = config::state_root()?;
            let selected = config::load(&state)?.ok_or_else(|| {
                DomainError::new(
                    "memory_store_connection_required",
                    "A store must remain selected.",
                )
            })?;
            let identity = agentlaw_app::machine::load_or_create(&state)?;
            let mut runtime = agentlaw_flows::Runtime::open_with_machine_and_coordination(
                &stage.store_path,
                &stage.runtime_path,
                &selected.user_id,
                &identity.machine_id,
                config::coordination_root(&state),
            )?;
            let configuration = agentlaw_app::installed::worker_config(&state)?;
            if configuration.model.is_some() {
                runtime = runtime.with_worker(
                    agentlaw_worker::ProcessRuntime::attach(&configuration).map_err(|_| {
                        DomainError::new(
                            "worker_unavailable",
                            "Cannot attach the model for import review; active memory was not changed.",
                        )
                    })?,
                );
            }
            let mut value = runtime.call_with_control(request, cli_control())?;
            value["workspace"] = json!("isolated_import_review");
            value["active_memory_changed"] = json!(false);
            agentlaw_app::delivery::adapt(value, &state, selected.response_limit_bytes)
        }
        ImportCommand::Resolve(args) => {
            let choices = args.choices.as_deref().map(import_choices).transpose()?;
            let (store, local) = selected_store()?;
            let resolution = match choices {
                Some(choices) => git_ops::resolve_import_with_choices(
                    &store,
                    &local.join("git"),
                    &args.import_ref,
                    &choices,
                    args.user_confirmed,
                )?,
                None => git_ops::resolve_import(&store, &local.join("git"), &args.import_ref)?,
            };
            let mut result = agentlaw_contracts::to_value(resolution)?;
            if result["next_action"].is_null() {
                result["next_action"]=json!("If unresolved structural conflicts or concurrent heads remain, inspect them and ask the user to decide before editing the isolated review workspace. Otherwise explain the exact resolved import and request confirmation. Only after confirmation, use share import publish --ref <import_ref> --resolution <resolution_token> --user-confirmed. Do not invent confirmation.");
            }
            Ok(result)
        }
        ImportCommand::Publish(args) => {
            let (store, local) = selected_store()?;
            agentlaw_contracts::to_value(git_ops::publish_import(
                &store,
                &local.join("git"),
                &args.import_ref,
                &args.resolution,
                args.user_confirmed,
            )?)
        }
    }
}
