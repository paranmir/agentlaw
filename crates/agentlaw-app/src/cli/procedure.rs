//! Procedure inventory and search preserve their streaming output contract.

use super::{
    commands::{LearnedProcedureCommand, ProcedureSearchArgs},
    dispatch::selected_store,
};
use agentlaw_app::{config, inventory};
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};
use std::io::{self, Write};

pub(super) fn execute(command: LearnedProcedureCommand) -> Result<Option<Value>> {
    match command {
        LearnedProcedureCommand::List(args) => {
            let options = args.into();
            let (store, local) = selected_store()?;
            inventory::list(&store, &local.join("exports"), &options)
        }
        LearnedProcedureCommand::Search(args) => search(args),
    }
}

fn search(args: ProcedureSearchArgs) -> Result<Option<Value>> {
    let options: inventory::Options = args.inventory.into();
    let state = config::state_root()?;
    let selected = config::load(&state)?.ok_or_else(|| {
        DomainError::new(
            "memory_store_connection_required",
            "Connect a store before procedure search.",
        )
    })?;
    let identity = agentlaw_app::machine::load_or_create(&state)?;
    let mut runtime = agentlaw_flows::Runtime::open_with_machine_and_coordination(
        &selected.memory_store_path,
        selected.runtime_root(&state),
        &selected.user_id,
        &identity.machine_id,
        config::coordination_root(&state),
    )?;
    let worker = agentlaw_app::installed::worker_config(&state)?;
    if worker.model.is_some() {
        runtime = runtime.with_worker(agentlaw_worker::ProcessRuntime::attach(&worker).map_err(
            |_| {
                DomainError::new(
                    "worker_unavailable",
                    "Cannot attach configured semantic search worker.",
                )
            },
        )?)
    }
    let result = runtime.search_procedures_filtered(
        &args.query,
        options.scope.as_deref(),
        options.project.as_deref(),
        options.machine.as_deref(),
        args.limit.unwrap_or(5),
    )?;
    for diagnostic in result["diagnostics"].as_array().into_iter().flatten() {
        eprintln!("{diagnostic}")
    }
    eprintln!(
        "agentlaw: {} matched procedure IDs, {} shown",
        result["matched"], result["shown"]
    );
    let rows = result["procedures"].as_array().ok_or_else(|| {
        DomainError::new("search_failed", "Procedure result descriptors are missing.")
    })?;
    let output = |writer: &mut dyn Write| -> Result<()> {
        if options.table {
            writeln!(writer, "PROCEDURE ID\tNAME\tUSE WHEN\tAPPLICABILITY")
                .map_err(|_| DomainError::new("output_failed", "Cannot write search output."))?
        }
        for row in rows {
            if options.table {
                writeln!(
                    writer,
                    "{}\t{}\t{}\t{}",
                    row["procedure_id"], row["name"], row["use_when"], row["applicability"]
                )
            } else {
                writeln!(writer, "{row}")
            }
            .map_err(|_| DomainError::new("output_failed", "Cannot write search output."))?
        }
        writer
            .flush()
            .map_err(|_| DomainError::new("output_failed", "Cannot flush search output."))
    };
    if let Some(path) = &options.output {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()
                .map_err(|_| DomainError::new("output_failed", "Cannot resolve output path."))?
                .join(path)
        };
        let parent = path
            .parent()
            .ok_or_else(|| DomainError::new("invalid_output_path", "Output path has no parent."))?;
        let parent = std::fs::canonicalize(parent)
            .map_err(|_| DomainError::new("invalid_output_path", "Output parent must exist."))?;
        let source = std::fs::canonicalize(&selected.memory_store_path)
            .map_err(|_| DomainError::new("source_unavailable", "Cannot verify source path."))?;
        if parent.starts_with(source) {
            return Err(DomainError::new(
                "invalid_output_path",
                "Search output cannot be written inside canonical memory source.",
            ));
        }
        if path.exists() {
            return Err(DomainError::new(
                "output_exists",
                "Choose a new output file; search does not overwrite files.",
            ));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| DomainError::new("output_failed", "Cannot prepare search output."))?;
        output(temporary.as_file_mut())?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| DomainError::new("output_failed", "Cannot synchronize search output."))?;
        temporary.persist_noclobber(&path).map_err(|_| {
            DomainError::new(
                "output_exists",
                "Search output was not installed; destination must remain absent.",
            )
        })?;
        Ok(Some(
            json!({"output":path,"matched":result["matched"],"shown":result["shown"],"complete":result["matched"]==result["shown"]}),
        ))
    } else {
        output(&mut io::stdout().lock())?;
        Ok(None)
    }
}
