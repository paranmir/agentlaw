//! Prepare a store independently, then atomically select it. No in-flight rebinding.
use crate::{config, machine};
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};
use std::{fs, path::Path};

pub fn connect(state: &Path, path: &Path, create: bool) -> Result<Value> {
    connect_with_control(
        state,
        path,
        create,
        agentlaw_flows::RequestControl::default(),
    )
}
pub fn connect_with_control(
    state: &Path,
    path: &Path,
    create: bool,
    control: agentlaw_flows::RequestControl,
) -> Result<Value> {
    connect_selection_with_control(state, path, create, control).map(|(result, _)| result)
}

/// Return the exact selection prepared under setup.lock so the initiating
/// request cannot drift to a later concurrent global store selection.
pub(crate) fn connect_selection_with_control(
    state: &Path,
    path: &Path,
    create: bool,
    control: agentlaw_flows::RequestControl,
) -> Result<(Value, config::Config)> {
    control.check()?;
    if !path.is_absolute() {
        return Err(DomainError::new(
            "invalid_path",
            "Provide an absolute memory store path.",
        ));
    }
    fs::create_dir_all(state)
        .map_err(|_| DomainError::new("configuration_io", "Cannot prepare local setup state."))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("setup.lock"))
        .map_err(|_| DomainError::new("configuration_io", "Cannot open setup coordination."))?;
    fs2::FileExt::lock_exclusive(&lock)
        .map_err(|_| DomainError::new("configuration_io", "Cannot coordinate store setup."))?;
    let previous = config::load(state)?;
    let mut same = false;
    if let Some(selected) = &previous {
        same = selected.memory_store_path == path
            || fs::canonicalize(&selected.memory_store_path)
                .ok()
                .zip(fs::canonicalize(path).ok())
                .is_some_and(|(a, b)| a == b);
        if same {
            config::require_existing_binding(state, selected)?;
            if !path.is_dir() || !path.join("format.md").is_file() {
                return Err(DomainError::new("connected_store_unavailable", "The selected store is missing or invalid. Its selection was preserved, not reported as a successful reconnect."));
            }
            agentlaw_storage::Store::open_with_coordination(path,selected.runtime_root(state).join("canonical"), config::coordination_root(state))
                .map_err(|_|DomainError::new("connected_store_unavailable", "The selected store failed validation or requires recovery. Its selection was preserved."))?;
        }
    }
    if create && !same {
        if path.exists()
            && fs::read_dir(path)
                .map_err(|_| {
                    DomainError::new("store_unreadable", "Cannot inspect the proposed store.")
                })?
                .next()
                .is_some()
        {
            return Err(DomainError::new(
                "store_not_empty",
                "Store creation does not overwrite a nonempty directory.",
            ));
        }
        fs::create_dir_all(path).map_err(|_| {
            DomainError::new(
                "store_unavailable",
                "Cannot create the confirmed store directory.",
            )
        })?;
    } else if !path.is_dir() || !path.join("format.md").is_file() {
        return Err(DomainError::new("invalid_store", "The requested existing store is absent or has no format descriptor. The previous selection and data were preserved."));
    }
    let source = fs::canonicalize(path)
        .map_err(|_| DomainError::new("store_unavailable", "Cannot resolve the selected store."))?;
    let installation = fs::canonicalize(state)
        .map_err(|_| DomainError::new("configuration_io", "Cannot resolve installation state."))?;
    if installation.starts_with(&source) {
        return Err(DomainError::new(
            "invalid_store_location",
            "The canonical memory store must not contain installation-local state.",
        ));
    }
    let local = if previous.is_none() && !state.join("runtime/control.sqlite").exists() {
        state.join("runtime")
    } else {
        config::binding_root(state, &source)?
    };
    if !create {
        agentlaw_storage::Store::attach_existing_with_coordination(
            &source,
            local.join("canonical"),
            config::coordination_root(state),
        )
        .map_err(|_| {
            DomainError::new(
                "store_validation_failed",
                "The proposed store failed validation. The previous selection remains usable.",
            )
        })?;
    }
    let machine = machine::load_or_create(state)?;
    let user_id = previous
        .as_ref()
        .map(|c| c.user_id.clone())
        .unwrap_or_else(|| "personal".into());
    let mut prepared = agentlaw_flows::Runtime::open_with_machine_and_coordination(
        &source,
        &local,
        &user_id,
        &machine.machine_id,
        config::coordination_root(state),
    )?;
    let worker_configuration = crate::installed::worker_config(state)?;
    let semantic_required = worker_configuration.model.is_some();
    if semantic_required {
        control.phase("attaching_embedding_worker");
        let worker=agentlaw_worker::ProcessRuntime::attach(&worker_configuration)
            .map_err(|_|DomainError::new("semantic_build_failed","Cannot attach the configured model for store preparation. The previous store remains selected; fix the worker and retry."))?;
        prepared = prepared.with_worker(worker);
    }
    let build = prepared.prepare_for_connection(control.clone())?;
    if semantic_required && build["semantic_complete"] != true {
        return Err(DomainError::new("semantic_build_incomplete","The configured semantic index is not ready. The previous selection remains active; repair model setup and repeat this connection to resume."));
    }
    control.check()?;
    let next = config::Config {
        memory_store_path: source.clone(),
        user_id,
        runtime_path: if local == state.join("runtime") {
            None
        } else {
            Some(local)
        },
        history_response_limit_bytes: previous
            .as_ref()
            .map(|c| c.history_response_limit_bytes)
            .unwrap_or_else(config::default_history_limit),
        response_limit_bytes: previous
            .as_ref()
            .map(|c| c.response_limit_bytes)
            .unwrap_or_else(config::default_response_limit),
    };
    match &previous {
        Some(old) if old != &next => config::replace_selection(state, old, &next)?,
        Some(_) => {}
        None => config::save_initial(state, &next)?,
    }
    Ok((
        json!({"connected":true,"memory_store_path":source,"created":create&&!same,"changed":!same,"derived_build":build,
        "client_application":"next_request","machine_id":machine.machine_id,"machine_name":machine.display_name,
        "next_action":if machine.display_name.is_none() {"Ask the user for a recognizable name for this installation, then use `machine name --value <name>`. Existing requests and retained proposals remain with their original store."} else {"Existing frontends use this store on their next request. In-flight requests and retained proposals stay with their original store."}}),
        next,
    ))
}
