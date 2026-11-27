//! Prepare a store independently, then atomically select it. No in-flight rebinding.
use super::registry::{ConfigRegistry, ConfigSnapshot, ConfigState};
use super::{config, machine};
use crate::protocol::{BindingRef, SetupCommand};
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::{fs, path::Path};

/// Validate a proposed canonical-store path without preparing installation state.
pub fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(DomainError::new(
            "invalid_path",
            "Provide an absolute memory store path.",
        ));
    }
    Ok(())
}

pub fn connect_prepared(
    state: &Path,
    path: &Path,
    create: bool,
    control: &agentlaw_flows::RequestControl,
    registry: &ConfigRegistry,
    prepare: impl FnOnce(&Arc<ConfigSnapshot>, &agentlaw_flows::RequestControl) -> Result<Value>,
) -> Result<(Value, Arc<ConfigSnapshot>)> {
    control.check()?;
    validate_path(path)?;
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
    if !create || same {
        agentlaw_storage::Store::validate_existing_format(path).map_err(|_| {
            DomainError::new(
                if same {
                    "connected_store_unavailable"
                } else {
                    "store_validation_failed"
                },
                "The requested store failed validation. The previous selection was preserved.",
            )
        })?;
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
    let machine = machine::load_or_create(state)?;
    let user_id = previous
        .as_ref()
        .map(|c| c.user_id.clone())
        .unwrap_or_else(|| "personal".into());
    fs::create_dir_all(&local).map_err(|e| DomainError::new("configuration_io", e.to_string()))?;
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
    let key = agentlaw_storage::binding_identity(&source, &next.runtime_root(state))
        .map_err(|e| DomainError::new("store_unavailable", e.to_string()))?;
    let snapshot = Arc::new(ConfigSnapshot {
        reference: BindingRef::new(&key, &next.user_id),
        key,
        config: next.clone(),
    });
    let build = prepare(&snapshot, control)?;
    control.check()?;
    match &previous {
        Some(old) if old != &next => config::replace_selection(state, old, &next)?,
        Some(_) => {}
        None => config::save_initial(state, &next)?,
    }
    registry.publish(ConfigState::Ready(snapshot.clone()));
    Ok((
        json!({"connected":true,"memory_store_path":source,"created":create&&!same,"changed":!same,"derived_build":build,
        "client_application":"next_request","machine_id":machine.machine_id,"machine_name":machine.display_name,
        "next_action":if machine.display_name.is_none() {"Ask the user for a recognizable name for this installation, then use `machine name --value <name>`. Existing requests and retained proposals remain with their original store."} else {"Existing frontends use this store on their next request. In-flight requests and retained proposals stay with their original store."}}),
        snapshot,
    ))
}

pub fn command(state: &Path, registry: &ConfigRegistry, command: SetupCommand) -> Result<Value> {
    match command {
        SetupCommand::ConfigSet { key, value } => {
            let number = config::validate_limit(&key, &value)?;
            let current = match registry.current() {
                ConfigState::Ready(snapshot) => snapshot,
                ConfigState::Missing => {
                    return Err(DomainError::new(
                        "memory_store_connection_required",
                        "Connect a memory store first.",
                    ))
                }
                ConfigState::Invalid(failure) => return Err(failure.into_domain()),
            };
            let mut next = current.config.clone();
            if key == "history.response_limit_bytes" {
                next.history_response_limit_bytes = number;
            } else {
                next.response_limit_bytes = number;
            }
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(state.join("setup.lock"))
                .map_err(|e| DomainError::new("configuration_io", e.to_string()))?;
            fs2::FileExt::lock_exclusive(&lock)
                .map_err(|e| DomainError::new("configuration_io", e.to_string()))?;
            config::write_selection(state, &next)?;
            registry.publish(ConfigState::Ready(Arc::new(ConfigSnapshot {
                reference: current.reference,
                key: current.key.clone(),
                config: next,
            })));
            Ok(
                json!({"key":key,"value":number,"applies":"next_request","config_path":state.join("config.json")}),
            )
        }
        SetupCommand::MachineName { value } => {
            agentlaw_contracts::to_value(machine::name(state, &value)?)
        }
        SetupCommand::StoreConnect { .. } => Err(DomainError::new(
            "invalid_input",
            "Store connection requires preparation.",
        )),
    }
}
