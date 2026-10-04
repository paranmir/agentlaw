//! Typed command execution. Resource acquisition starts at this boundary.

use super::{
    commands::{
        Command, HelpFormat, McpCommand, SupportCommand, UpdateArgs, UpdateCommand,
        WorkerDaemonArgs,
    },
    general, help, input, procedure, share, sync,
};
use agentlaw_app::{config, installed::InstalledBackend, Backend};
use agentlaw_contracts::{DomainError, Result};
use serde_json::Value;
use std::{io, path::PathBuf};

pub(crate) fn execute(command: Command) -> Result<Option<Value>> {
    let value = match command {
        Command::Help(args) => {
            let information = match (args.path.is_empty(), args.format) {
                (true, None | Some(HelpFormat::Json)) => help::root_json(),
                (_, None | Some(HelpFormat::Text)) => help::text(&args.path)?,
                (false, Some(HelpFormat::Json)) => help::describe(&args.path)?,
            };
            println!("{information}");
            return Ok(None);
        }
        Command::Describe(args) => {
            println!("{}", help::describe(&args.path)?);
            return Ok(None);
        }
        Command::Schema => agentlaw_app::schema(),
        Command::Call(_) => {
            let request = agentlaw_contracts::parse_request(&input::read_stdin()?)?;
            InstalledBackend::start()?.call_with_control(request, cli_control())?
        }
        Command::Mcp {
            command: McpCommand::Serve(args),
        } => {
            if !args.stdio {
                return Err(DomainError::new(
                    "invalid_arguments",
                    "MCP serving requires --stdio.",
                ));
            }
            let state = config::state_root()?;
            agentlaw_app::update::managed::check_frontend_start(&state)?;
            let backend = InstalledBackend::start()?;
            let advisor = agentlaw_app::update::Advisor::new(state);
            agentlaw_app::transport::serve_with_advisor(
                io::BufReader::new(io::stdin()),
                io::stdout(),
                backend,
                Some(advisor),
            )?;
            return Ok(None);
        }
        Command::Update(args) => update(args)?,
        Command::Support {
            command: SupportCommand::Star(args),
        } => agentlaw_app::update::support::star(args.ask_again)?,
        Command::Install(args) => general::install(args)?,
        Command::Doctor => agentlaw_app::diagnostics::doctor(&config::state_root()?)?,
        Command::Repair => {
            agentlaw_app::diagnostics::repair(&config::state_root()?, cli_control())?
        }
        Command::Config { command } => general::config(command)?,
        Command::Machine { command } => general::machine(command)?,
        Command::History { command } => general::history(command)?,
        Command::Store { command } => general::store(command)?,
        Command::LearnedProcedure { command } => return procedure::execute(command),
        Command::Continuity { command } => general::continuity(command)?,
        Command::Sync { command } => sync::execute(command)?,
        Command::Share { command } => share::execute(command)?,
        Command::WorkerDaemon(args) => {
            let configuration = worker_configuration(args)?;
            agentlaw_worker::run_daemon(configuration).map_err(|_| {
                DomainError::new(
                    "worker_failed",
                    "Embedding worker terminated with an error.",
                )
            })?;
            return Ok(None);
        }
        Command::ModelChild => {
            agentlaw_worker::run_model_child().map_err(|_| {
                DomainError::new("model_child_failed", "The private model child failed.")
            })?;
            return Ok(None);
        }
    };
    Ok(Some(value))
}

fn update(args: UpdateArgs) -> Result<Value> {
    match (args.confirm_update, args.command) {
        (Some(plan), None) => agentlaw_app::update::managed::prepare(&plan),
        (None, None) => agentlaw_app::update::managed::preview(),
        (None, Some(UpdateCommand::Check)) => Ok(agentlaw_app::update::check()),
        (None, Some(UpdateCommand::Status(args))) => match args.root {
            Some(root) => agentlaw_app::update::managed::status_with_root(&args.plan_id, &root),
            None => agentlaw_app::update::managed::status(&args.plan_id),
        },
        (None, Some(UpdateCommand::Apply(args))) => {
            agentlaw_app::update::managed::apply(&args.plan_id, &args.root)
        }
        (Some(_), Some(_)) => Err(DomainError::new(
            "invalid_arguments",
            "Update preparation cannot be combined with a subcommand.",
        )),
    }
}

fn worker_configuration(args: WorkerDaemonArgs) -> Result<agentlaw_worker::RuntimeConfig> {
    if !args.state_dir.is_absolute()
        || args
            .model
            .iter()
            .chain(args.tokenizer.iter())
            .chain(args.ort_library.iter())
            .any(|path| !path.is_absolute())
    {
        return Err(DomainError::new(
            "invalid_arguments",
            "Worker paths must be absolute.",
        ));
    }
    let model = match (args.model, args.tokenizer, args.ort_library) {
        (None, None, None) => None,
        (Some(onnx_model), Some(tokenizer_json), Some(runtime_library)) => {
            Some(agentlaw_worker::ModelAssets {
                onnx_model,
                tokenizer_json,
                runtime_library,
            })
        }
        _ => {
            return Err(DomainError::new(
                "invalid_arguments",
                "Supply all three model artifact paths together.",
            ))
        }
    };
    let executable = std::env::current_exe().map_err(|_| {
        DomainError::new(
            "worker_unavailable",
            "Could not locate the worker executable.",
        )
    })?;
    Ok(agentlaw_worker::RuntimeConfig {
        state_dir: args.state_dir,
        executable,
        model,
    })
}

pub(super) fn cli_control() -> agentlaw_flows::RequestControl {
    let last = std::sync::Mutex::new(String::new());
    agentlaw_flows::RequestControl::new(
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        move |phase| {
            // stderr is progress only; stdout remains the command's result.
            let Ok(mut previous) = last.lock() else {
                return;
            };
            if previous.as_str() == phase {
                return;
            }
            *previous = phase.into();
            eprintln!("agentlaw: {phase}");
        },
    )
}

pub(super) fn selected_store() -> Result<(agentlaw_storage::Store, PathBuf)> {
    let state = config::state_root()?;
    let selected = config::load(&state)?.ok_or_else(|| {
        DomainError::new(
            "memory_store_connection_required",
            "Connect a memory store before using operating commands.",
        )
    })?;
    let local = selected.runtime_root(&state);
    config::require_existing_binding(&state, &selected)?;
    let store = agentlaw_storage::Store::open_with_coordination(
        &selected.memory_store_path,
        local.join("canonical"),
        config::coordination_root(&state),
    )
    .map_err(|_| {
        DomainError::new(
            "source_unavailable",
            "The selected source needs diagnosis or recovery; no Git operation was performed.",
        )
    })?;
    Ok((store, local))
}
