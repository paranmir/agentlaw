//! Typed execution for setup, configuration, identity, and audit commands.

use super::{
    commands::{
        ConfigCommand, ConfigKey, ContinuityCommand, HistoryCommand, InstallArgs, MachineCommand,
        StoreCommand,
    },
    dispatch::{cli_control, selected_store},
};
use agentlaw_app::config as configuration;
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};

pub(super) fn install(args: InstallArgs) -> Result<Value> {
    let harness = agentlaw_app::install::Harness::parse(args.harness.as_str())?;
    let directory = match args.harness_dir {
        Some(directory) => directory,
        None => harness.default_directory()?,
    };
    agentlaw_app::install::validate_path(&directory)?;
    agentlaw_app::install::install(
        &configuration::state_root()?,
        harness,
        &directory,
        args.model_manifest.as_deref(),
        args.confirm_install,
    )
}

pub(super) fn config(command: ConfigCommand) -> Result<Value> {
    match command {
        ConfigCommand::Path => Ok(json!({"path":configuration::state_root()?.join("config.json")})),
        ConfigCommand::Get(args) => {
            let state = configuration::state_root()?;
            let selected = configuration::load(&state)?;
            let (value, field) = match args.key {
                ConfigKey::HistoryResponseLimitBytes => (
                    selected
                        .map(|selected| selected.history_response_limit_bytes)
                        .unwrap_or_else(configuration::default_history_limit),
                    "history_response_limit_bytes",
                ),
                ConfigKey::ResponseLimitBytes => (
                    selected
                        .map(|selected| selected.response_limit_bytes)
                        .unwrap_or_else(configuration::default_response_limit),
                    "response_limit_bytes",
                ),
            };
            Ok(
                json!({"key":args.key.as_str(),"value":value,"config_path":state.join("config.json"),"configuration_field":field}),
            )
        }
        ConfigCommand::Set(args) => {
            let key = args.key.as_str();
            let value = args.value.to_string();
            configuration::validate_limit(key, &value)?;
            configuration::set_limit(&configuration::state_root()?, key, &value)
        }
    }
}

pub(super) fn machine(command: MachineCommand) -> Result<Value> {
    match command {
        MachineCommand::Inspect => agentlaw_contracts::to_value(
            agentlaw_app::machine::load_or_create(&configuration::state_root()?)?,
        ),
        MachineCommand::Name(args) => {
            agentlaw_app::machine::validate_name(&args.value)?;
            agentlaw_contracts::to_value(agentlaw_app::machine::name(
                &configuration::state_root()?,
                &args.value,
            )?)
        }
    }
}

pub(super) fn store(command: StoreCommand) -> Result<Value> {
    let (path, create) = match command {
        StoreCommand::ProposeLocation => {
            return Ok(
                json!({"proposed_path":configuration::proposed_memory_store_path()?,"created":false,
                "next_action":"Ask the user to confirm this memory store location or choose another. Create only after confirmation."}),
            );
        }
        StoreCommand::Create(args) => (args.path, true),
        StoreCommand::Connect(args) => (args.path, false),
    };
    agentlaw_app::setup::validate_path(&path)?;
    agentlaw_app::setup::connect_with_control(
        &configuration::state_root()?,
        &path,
        create,
        cli_control(),
    )
}

pub(super) fn history(command: HistoryCommand) -> Result<Value> {
    match command {
        HistoryCommand::Export(args) => {
            agentlaw_storage::validate_id(&args.memory_id).map_err(|_| {
                DomainError::new("invalid_memory_id", "Use a returned UUID memory ID.")
            })?;
            let (store, _) = selected_store()?;
            agentlaw_app::history_export::export(&store, &args.memory_id, &args.output)
        }
    }
}

pub(super) fn continuity(command: ContinuityCommand) -> Result<Value> {
    match command {
        ContinuityCommand::Save => {
            let (store, local) = selected_store()?;
            agentlaw_contracts::to_value(agentlaw_app::git_ops::continuity_save(
                &store,
                &local.join("git"),
                "Agentlaw continuity snapshot\n",
            )?)
        }
    }
}
