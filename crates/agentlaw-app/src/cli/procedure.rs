//! Procedure inventory and search preserve their streaming output contract.
use super::{
    commands::{InventoryArgs, LearnedProcedureCommand, ProcedureFormat},
    general::runtime_admin,
};
use agentlaw_contracts::{DomainError, Result};
use agentlaw_runtime::protocol::{AdminCommand, InventoryFilter};
use serde_json::{json, Value};
use std::io::{self, Write};

pub(super) fn execute(command: LearnedProcedureCommand) -> Result<Option<Value>> {
    let (args, query) = match command {
        LearnedProcedureCommand::List(args) => (args, None),
        LearnedProcedureCommand::Search(args) => (args.inventory, Some((args.query, args.limit))),
    };
    let InventoryArgs {
        scope,
        project,
        machine,
        output,
        format,
    } = args;
    let filter = InventoryFilter {
        scope: scope.map(|s| s.as_str().to_owned()),
        project,
        machine,
        table: format == ProcedureFormat::Table,
    };
    let temporary = if output.is_none() {
        Some(tempfile::tempdir().map_err(|e| DomainError::new("output_failed", e.to_string()))?)
    } else {
        None
    };
    let path = match output {
        Some(path) => std::path::absolute(path)
            .map_err(|e| DomainError::new("output_failed", e.to_string()))?,
        None => temporary.as_ref().unwrap().path().join("procedures.jsonl"),
    };
    let searching = query.is_some();
    let command = match query {
        Some((query, limit)) => AdminCommand::ProcedureSearch {
            query,
            limit,
            filter,
            output: path.clone(),
        },
        None => AdminCommand::ProcedureList {
            filter,
            output: path.clone(),
        },
    };
    let result = runtime_admin(command)?;
    if let Some(notice) = result["competing_notice"].as_str() {
        eprintln!("{notice}");
    }
    if searching {
        for diagnostic in result["diagnostics"].as_array().into_iter().flatten() {
            eprintln!("{diagnostic}");
        }
        eprintln!(
            "agentlaw: {} matched procedure IDs, {} shown",
            result["matched"], result["shown"]
        );
    }
    if temporary.is_some() {
        let mut file = std::fs::File::open(&path)
            .map_err(|e| DomainError::new("output_failed", e.to_string()))?;
        let mut stdout = io::stdout().lock();
        io::copy(&mut file, &mut stdout)
            .and_then(|_| stdout.flush())
            .map_err(|e| DomainError::new("output_failed", e.to_string()))?;
        Ok(None)
    } else if searching {
        Ok(Some(
            json!({"output":path,"matched":result["matched"],"shown":result["shown"],"complete":result["complete"]}),
        ))
    } else {
        Ok(Some(
            json!({"output":path,"procedure_count":result["procedure_count"],"complete":true}),
        ))
    }
}
