//! Typed execution for setup, configuration, identity, and audit commands.

use super::{
    commands::{
        ConfigCommand, ContinuityCommand, HistoryCommand, InstallArgs, MachineCommand, StoreCommand,
    },
    dispatch::cli_control,
};
use agentlaw_app::config as configuration;
use agentlaw_contracts::{DomainError, Result};
use agentlaw_runtime::protocol::{AdminCommand, SetupCommand};
use serde_json::{json, Value};

fn runtime_setup(command: SetupCommand) -> Result<Value> {
    let state = configuration::state_root()?;
    agentlaw_app::update::managed::check_frontend_start(&state)?;
    agentlaw_app::frontend::RuntimeBackend::new(state, false)?.setup(command, &cli_control())
}
pub(super) fn runtime_admin(command: AdminCommand) -> Result<Value> {
    let state = configuration::state_root()?;
    agentlaw_app::frontend::RuntimeBackend::new(state, false)?.admin(&command, &cli_control())
}

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
            let backend = agentlaw_app::frontend::RuntimeBackend::new(state.clone(), false)?;
            if let Some(value) = backend.config_get_if_running(args.key.as_str())? {
                return Ok(value);
            }
            let selected = configuration::load(&state)?;
            configuration::get_value(&state, selected.as_ref(), args.key.as_str())
        }
        ConfigCommand::Set(args) => {
            let key = args.key.as_str();
            let value = args.value.to_string();
            configuration::validate_limit(key, &value)?;
            runtime_setup(SetupCommand::ConfigSet {
                key: key.into(),
                value,
            })
        }
    }
}

pub(super) fn machine(command: MachineCommand) -> Result<Value> {
    match command {
        MachineCommand::Inspect => {
            match agentlaw_app::machine::inspect(&configuration::state_root()?)? {
                Some(machine) => agentlaw_contracts::to_value(machine),
                None => Ok(json!({"machine_id":null,"display_name":null})),
            }
        }
        MachineCommand::Name(args) => {
            agentlaw_app::machine::validate_name(&args.value)?;
            runtime_setup(SetupCommand::MachineName { value: args.value })
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
    runtime_setup(SetupCommand::StoreConnect { path, create })
}

pub(super) fn history(command: HistoryCommand) -> Result<Value> {
    match command {
        HistoryCommand::Export(args) => {
            agentlaw_storage::validate_id(&args.memory_id).map_err(|_| {
                DomainError::new("invalid_memory_id", "Use a returned UUID memory ID.")
            })?;
            let output = std::path::absolute(args.output)
                .map_err(|e| DomainError::new("invalid_output_path", e.to_string()))?;
            runtime_admin(AdminCommand::HistoryExport {
                memory_id: args.memory_id,
                output,
            })
        }
    }
}

pub(super) fn continuity(command: ContinuityCommand) -> Result<Value> {
    match command {
        ContinuityCommand::Save => runtime_admin(AdminCommand::ContinuitySave),
    }
}
