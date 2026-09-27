use agentlaw_app::{config, installed::InstalledBackend};
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};
use std::{
    io::{self, Read},
    path::PathBuf,
};

fn input() -> Result<String> {
    let mut bytes = Vec::new();
    io::stdin()
        .take((agentlaw_app::MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| DomainError::new("transport_io", "Could not read stdin."))?;
    if bytes.len() > agentlaw_app::MAX_REQUEST_BYTES {
        return Err(DomainError::new(
            "transport_capacity",
            "CLI input exceeds 16 MiB; no partial request was executed.",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| DomainError::new("invalid_encoding", "Expected UTF-8 JSON input."))
}

fn import_choices(
    path: &str,
) -> Result<std::collections::BTreeMap<String, agentlaw_storage::import_conflict::ImportSide>> {
    let text = if path == "-" {
        input()?
    } else {
        let file = std::fs::File::open(path).map_err(|_| {
            DomainError::new(
                "input_unreadable",
                "Cannot read the structural choice file; no import was modified.",
            )
        })?;
        let mut bytes = Vec::new();
        file.take((agentlaw_app::MAX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                DomainError::new(
                    "input_unreadable",
                    "Cannot read the complete structural choice file.",
                )
            })?;
        if bytes.len() > agentlaw_app::MAX_REQUEST_BYTES {
            return Err(DomainError::new(
                "transport_capacity",
                "Structural choices exceed 16 MiB; no partial choice was applied.",
            ));
        }
        String::from_utf8(bytes).map_err(|_| {
            DomainError::new("invalid_encoding", "Structural choices must be UTF-8 JSON.")
        })?
    };
    let value = agentlaw_contracts::validation::decode_unique(&text)?;
    let choices: std::collections::BTreeMap<String, agentlaw_storage::import_conflict::ImportSide> = serde_json::from_value(value)
        .map_err(|_| DomainError::new("invalid_arguments", "Expected an object mapping returned conflict_id values to \"local\" or \"incoming\". Explain both states and obtain the user's choice first."))?;
    if choices.is_empty() || choices.keys().any(|id| uuid::Uuid::parse_str(id).is_err()) {
        return Err(DomainError::new("invalid_arguments", "Provide at least one returned conflict_id and an explicit local/incoming choice. Do not invent conflict identifiers."));
    }
    Ok(choices)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--version"] || args == ["-V"] {
        println!("agentlaw {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // Private broker-owned child protocol, not an MCP action or user workflow.
    // Launch configuration arrives over inherited stdin, not command-line text.
    if args == ["model-child"] {
        if agentlaw_worker::run_model_child().is_err() {
            eprintln!("agentlaw: private model child failed; consult the broker diagnostic");
            std::process::exit(1);
        }
        return;
    }
    if args.len() >= 2 && args[0..2] == ["learned-procedure", "search"] {
        match procedure_search(&args[2..]) {
            Ok(Some(summary)) => println!("{summary}"),
            Ok(None) => {}
            Err(error) => {
                eprintln!("{}", agentlaw_app::error_payload(&error));
                std::process::exit(agentlaw_app::exit_code(&error));
            }
        }
        return;
    }
    if args.len() >= 2 && args[0..2] == ["learned-procedure", "list"] {
        let result = agentlaw_app::inventory::parse(&args[2..]).and_then(|options| {
            let (store, state) = selected_store()?;
            agentlaw_app::inventory::list(&store, &state.join("exports"), &options)
        });
        match result {
            Ok(Some(summary)) => println!("{summary}"),
            Ok(None) => {}
            Err(error) => {
                eprintln!("{}", agentlaw_app::error_payload(&error));
                std::process::exit(agentlaw_app::exit_code(&error));
            }
        }
        return;
    }
    if args.first().map(String::as_str) == Some("worker-daemon") {
        let result = agentlaw_app::installed::daemon_args(&args).and_then(|configuration| {
            agentlaw_worker::run_daemon(configuration).map_err(|_| {
                DomainError::new(
                    "worker_failed",
                    "Embedding worker terminated with an error.",
                )
            })
        });
        if let Err(error) = result {
            eprintln!("{}", agentlaw_app::error_payload(&error));
            std::process::exit(1);
        }
        return;
    }
    if args == ["mcp", "serve", "--stdio"] {
        let result = InstalledBackend::start().and_then(|backend| {
            agentlaw_app::transport::serve(io::BufReader::new(io::stdin()), io::stdout(), backend)
        });
        if let Err(error) = result {
            eprintln!("{}", agentlaw_app::error_payload(&error));
            std::process::exit(agentlaw_app::exit_code(&error));
        }
        return;
    }
    match run(&args) {
        Ok(value) => println!("{value}"),
        Err(error) => {
            println!("{}", agentlaw_app::error_payload(&error));
            std::process::exit(agentlaw_app::exit_code(&error));
        }
    }
}

fn run(args: &[String]) -> Result<Value> {
    if args.is_empty() || args == ["--help"] || args == ["help"] {
        return Ok(
            json!({"name":"agentlaw","status":"implementation_in_progress","commands":[
            "schema", "call --json -", "mcp serve --stdio", "config path", "config get history.response_limit_bytes|response_limit_bytes", "config set history.response_limit_bytes|response_limit_bytes <positive-bytes>",
            "install --harness codex|oh-my-pi [--harness-dir <absolute>] [--model-manifest <path>] --confirm-install", "machine inspect", "machine name --value <display-name>", "doctor", "repair", "history export --memory-id <uuid> --output <new-file>",
            "store propose-location", "store create --path <absolute> --confirm-create", "store connect --path <absolute>",
            "learned-procedure list [--scope <kind>] [--project <id-or-hint>] [--machine <id>] [--output <path>] [--format jsonl|table]",
            "learned-procedure search --query <text> [--limit <positive-integer>] [--scope <kind>] [--project <id-or-hint>] [--machine <id>] [--output <path>] [--format jsonl|table]",
            "continuity save", "share inspect --remote <name> --target-ref <refs/heads/name>",
            "share push --review <returned-ref> [--allow-sensitive --user-confirmed]"
            ,"share fetch --remote <name>","share import prepare --commit <oid>","share import inspect --ref <returned-ref>","share import call --ref <returned-ref> --json -", "share import resolve --ref <returned-ref> [--choices <JSON-file-or-> --user-confirmed]", "share import publish --ref <returned-ref> --resolution <returned-token> --user-confirmed"
        ],"note":"Search defaults to all management scopes and five procedure IDs; list is complete inventory. Import call edits the isolated review workspace, resolve freezes the reviewed state, and publish requires explicit user confirmation. Model availability, harness verification and power-loss guarantees must be checked through diagnostics and test evidence."}),
        );
    }
    if args == ["schema"] {
        return Ok(agentlaw_app::schema());
    }
    if args.first().map(String::as_str) == Some("install") {
        let mut harness = None;
        let mut directory = None;
        let mut manifest = None;
        let mut confirmed = false;
        let mut i = 1;
        while i < args.len() {
            if args[i] == "--confirm-install" {
                if confirmed {
                    return Err(DomainError::new(
                        "invalid_arguments",
                        "Duplicate installation confirmation.",
                    ));
                }
                confirmed = true;
                i += 1;
                continue;
            }
            if i + 1 >= args.len() {
                return Err(DomainError::new(
                    "invalid_arguments",
                    "An installation option has no value.",
                ));
            }
            match args[i].as_str() {
                "--harness" if harness.is_none() => {
                    harness = Some(agentlaw_app::install::Harness::parse(&args[i + 1])?)
                }
                "--harness-dir" if directory.is_none() => {
                    directory = Some(PathBuf::from(&args[i + 1]))
                }
                "--model-manifest" if manifest.is_none() => {
                    manifest = Some(PathBuf::from(&args[i + 1]))
                }
                _ => {
                    return Err(DomainError::new(
                        "invalid_arguments",
                        "Unknown or duplicate installation option.",
                    ))
                }
            }
            i += 2;
        }
        let harness = harness.ok_or_else(|| {
            DomainError::new(
                "harness_required",
                "Specify --harness codex or --harness oh-my-pi.",
            )
        })?;
        let directory = match directory {
            Some(p) => p,
            None => harness.default_directory()?,
        };
        return agentlaw_app::install::install(
            &config::state_root()?,
            harness,
            &directory,
            manifest.as_deref(),
            confirmed,
        );
    }
    if args == ["doctor"] {
        return agentlaw_app::diagnostics::doctor(&config::state_root()?);
    }
    if args == ["repair"] {
        return agentlaw_app::diagnostics::repair(&config::state_root()?, cli_control());
    }
    if args.len() == 6
        && args[0..3] == ["history", "export", "--memory-id"]
        && args[4] == "--output"
    {
        let (store, _) = selected_store()?;
        return agentlaw_app::history_export::export(&store, &args[3], &PathBuf::from(&args[5]));
    }
    if args == ["call", "--json", "-"] {
        use agentlaw_app::Backend;
        let request = agentlaw_contracts::parse_request(&input()?)?;
        return InstalledBackend::start()?.call_with_control(request, cli_control());
    }
    if args == ["config", "path"] {
        return Ok(json!({"path":config::state_root()?.join("config.json")}));
    }
    if args.len() == 4 && args[0..2] == ["config", "set"] {
        return config::set_limit(&config::state_root()?, &args[2], &args[3]);
    }
    if args == ["machine", "inspect"] {
        return agentlaw_contracts::to_value(agentlaw_app::machine::load_or_create(
            &config::state_root()?,
        )?);
    }
    if args.len() == 4 && args[0..3] == ["machine", "name", "--value"] {
        return agentlaw_contracts::to_value(agentlaw_app::machine::name(
            &config::state_root()?,
            &args[3],
        )?);
    }
    if args == ["config", "get", "history.response_limit_bytes"] {
        let state = config::state_root()?;
        let value = config::load(&state)?
            .map(|c| c.history_response_limit_bytes)
            .unwrap_or_else(config::default_history_limit);
        return Ok(
            json!({"key":"history.response_limit_bytes","value":value,"config_path":state.join("config.json"),"configuration_field":"history_response_limit_bytes"}),
        );
    }
    if args == ["config", "get", "response_limit_bytes"] {
        let state = config::state_root()?;
        let value = config::load(&state)?
            .map(|c| c.response_limit_bytes)
            .unwrap_or_else(config::default_response_limit);
        return Ok(
            json!({"key":"response_limit_bytes","value":value,"config_path":state.join("config.json"),"configuration_field":"response_limit_bytes"}),
        );
    }
    if args == ["continuity", "save"] {
        let (store, state) = selected_store()?;
        return agentlaw_contracts::to_value(agentlaw_app::git_ops::continuity_save(
            &store,
            &state.join("git"),
            "Agentlaw continuity snapshot\n",
        )?);
    }
    if args.len() == 6
        && args[0..2] == ["share", "inspect"]
        && args[2] == "--remote"
        && args[4] == "--target-ref"
    {
        let (store, state) = selected_store()?;
        let mut result = agentlaw_contracts::to_value(agentlaw_app::git_ops::inspect_share(
            &store,
            &state.join("git"),
            &args[3],
            &args[5],
        )?)?;
        if result["findings"].as_array().is_some_and(|v| !v.is_empty()) {
            result["next_action"] = json!("Explain the detected pattern categories in the user's language. Ask whether to share unchanged or cancel. Do not infer permission from a private destination, modify memory, mask content or rewrite history without a separate decision.");
        }
        return Ok(result);
    }
    if args.len() >= 4 && args[0..3] == ["share", "push", "--review"] {
        let accepted = match &args[4..] {
            [] => false,
            [allow, confirmed] if allow == "--allow-sensitive" && confirmed == "--user-confirmed" => true,
            _ => return Err(DomainError::new("invalid_arguments", "Sharing flagged content requires both --allow-sensitive and --user-confirmed. Without a decision, omit both and no flagged content will be pushed.")),
        };
        let (store, state) = selected_store()?;
        return agentlaw_contracts::to_value(agentlaw_app::git_ops::push_review(
            &store,
            &state.join("git"),
            &args[3],
            accepted,
        )?);
    }
    if args.len() == 4 && args[0..3] == ["share", "fetch", "--remote"] {
        let (store, state) = selected_store()?;
        return agentlaw_contracts::to_value(agentlaw_app::git_ops::fetch_share(
            &store,
            &state.join("git"),
            &args[3],
        )?);
    }
    if args.len() == 5 && args[0..4] == ["share", "import", "prepare", "--commit"] {
        let (store, state) = selected_store()?;
        let mut result = agentlaw_contracts::to_value(agentlaw_app::git_ops::prepare_import(
            &store,
            &state.join("git"),
            &args[4],
        )?)?;
        result["next_action"]=json!("This is an isolated review workspace, not active memory. Inspect structural_conflicts, explain both states in the user's language, and ask the user to choose local or incoming where required. Submit agreed structural choices with share import resolve --ref <import_ref> --choices <JSON-file-or-> --user-confirmed. Use share import call --ref <import_ref> --json - with ordinary agentlaw requests for memory-head inspection and agreed semantic edits. No active memory or Git HEAD has changed. Publication requires a separate confirmation of the complete resolved import.");
        return Ok(result);
    }
    if args.len() == 5 && args[0..4] == ["share", "import", "inspect", "--ref"] {
        let (store, state) = selected_store()?;
        return agentlaw_contracts::to_value(agentlaw_app::git_ops::inspect_import(
            &store,
            &state.join("git"),
            &args[4],
        )?);
    }
    if args.len() == 7
        && args[0..4] == ["share", "import", "call", "--ref"]
        && args[5..] == ["--json", "-"]
    {
        let request = agentlaw_contracts::parse_request(&input()?)?;
        if matches!(&request,agentlaw_contracts::Request::ConnectProjectMemory(c) if c.memory_store_path.is_some())
        {
            return Err(DomainError::new("invalid_import_request","An import-review request cannot switch the installation's memory store. Omit memory_store_path."));
        }
        let (store, local) = selected_store()?;
        let stage = agentlaw_app::git_ops::get_import_stage(&store, &local.join("git"), &args[4])?;
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
        return agentlaw_app::delivery::adapt(value, &state, selected.response_limit_bytes);
    }
    if args.len() >= 5 && args[0..4] == ["share", "import", "resolve", "--ref"] {
        let choices = match &args[5..] {
            [] => None,
            [flag, path, confirmed] if flag == "--choices" && confirmed == "--user-confirmed" => Some(import_choices(path)?),
            _ => return Err(DomainError::new("invalid_arguments", "Use share import resolve --ref <ref>, or add --choices <JSON-file-or-> --user-confirmed after the user has chosen how to resolve the structural conflicts.")),
        };
        let (store, local) = selected_store()?;
        let resolution = match choices {
            Some(choices) => agentlaw_app::git_ops::resolve_import_with_choices(
                &store,
                &local.join("git"),
                &args[4],
                &choices,
                true,
            )?,
            None => agentlaw_app::git_ops::resolve_import(&store, &local.join("git"), &args[4])?,
        };
        let mut result = agentlaw_contracts::to_value(resolution)?;
        if result["next_action"].is_null() {
            result["next_action"]=json!("If unresolved structural conflicts or concurrent heads remain, inspect them and ask the user to decide before editing the isolated review workspace. Otherwise explain the exact resolved import and request confirmation. Only after confirmation, use share import publish --ref <import_ref> --resolution <resolution_token> --user-confirmed. Do not invent confirmation.");
        }
        return Ok(result);
    }
    if (args.len() == 7 || args.len() == 8)
        && args[0..4] == ["share", "import", "publish", "--ref"]
        && args[5] == "--resolution"
    {
        let confirmed = args.len() == 8 && args[7] == "--user-confirmed";
        if args.len() == 8 && !confirmed {
            return Err(DomainError::new(
                "invalid_arguments",
                "Unexpected import-publication option.",
            ));
        }
        let (store, local) = selected_store()?;
        return agentlaw_contracts::to_value(agentlaw_app::git_ops::publish_import(
            &store,
            &local.join("git"),
            &args[4],
            &args[6],
            confirmed,
        )?);
    }
    if args == ["store", "propose-location"] {
        return Ok(
            json!({"proposed_path":config::proposed_memory_store_path()?,"created":false,
            "next_action":"Ask the user to confirm this memory store location or choose another. Create only after confirmation."}),
        );
    }
    if args.len() >= 4
        && args[0] == "store"
        && matches!(args[1].as_str(), "create" | "connect")
        && args[2] == "--path"
    {
        let create = args[1] == "create";
        if (create && (args.len() != 5 || args[4] != "--confirm-create"))
            || (!create && args.len() != 4)
        {
            return Err(DomainError::new(
                "invalid_arguments",
                "Create requires --confirm-create; connect accepts only --path.",
            ));
        }
        return agentlaw_app::setup::connect_with_control(
            &config::state_root()?,
            &PathBuf::from(&args[3]),
            create,
            cli_control(),
        );
    }
    Err(DomainError::new("invalid_arguments","Unknown command or invalid options. Run agentlaw --help for the complete supported command forms."))
}

fn cli_control() -> agentlaw_flows::RequestControl {
    let last = std::sync::Mutex::new(String::new());
    agentlaw_flows::RequestControl::new(
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        move |phase| {
            // stderr is progress only; stdout remains one machine-readable result.
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

fn procedure_search(args: &[String]) -> Result<Option<Value>> {
    use std::io::Write;
    let (mut query, mut limit) = (None, None);
    let mut filters = Vec::new();
    if args.len() % 2 != 0 {
        return Err(DomainError::new(
            "invalid_arguments",
            "Search options require values.",
        ));
    }
    for pair in args.chunks_exact(2) {
        match pair[0].as_str() {
            "--query" if query.is_none() => query = Some(pair[1].clone()),
            "--limit" if limit.is_none() => {
                limit = Some(
                    pair[1]
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| {
                            DomainError::new(
                                "invalid_arguments",
                                "Search limit must be a positive integer.",
                            )
                        })?,
                )
            }
            "--query" | "--limit" => {
                return Err(DomainError::new(
                    "invalid_arguments",
                    "Duplicate search option.",
                ))
            }
            _ => filters.extend_from_slice(pair),
        }
    }
    let query = query
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| DomainError::new("invalid_arguments", "Search requires --query <text>."))?;
    let options = agentlaw_app::inventory::parse(&filters)?;
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
        &query,
        options.scope.as_deref(),
        options.project.as_deref(),
        options.machine.as_deref(),
        limit.unwrap_or(5),
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

fn selected_store() -> Result<(agentlaw_storage::Store, PathBuf)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn help_lists_supported_operating_lifecycle() {
        let help = run(&["--help".into()]).unwrap();
        let commands = help["commands"].as_array().unwrap();
        for prefix in [
            "install ",
            "repair",
            "machine inspect",
            "config get ",
            "learned-procedure search ",
            "share import call ",
            "share import resolve ",
            "share import publish ",
        ] {
            assert!(
                commands
                    .iter()
                    .any(|c| c.as_str().unwrap().starts_with(prefix)),
                "missing {prefix}"
            );
        }
    }
    #[test]
    fn search_requires_query_and_rejects_duplicate_limit_before_opening_store() {
        assert_eq!(procedure_search(&[]).unwrap_err().code, "invalid_arguments");
        assert_eq!(
            procedure_search(&[
                "--query".into(),
                "test".into(),
                "--limit".into(),
                "1".into(),
                "--limit".into(),
                "2".into()
            ])
            .unwrap_err()
            .code,
            "invalid_arguments"
        );
    }
}
