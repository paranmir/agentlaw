//! Installation adapter: model ownership remains in the shared worker process.
use crate::{config, Backend};
use agentlaw_contracts::{DomainError, Request, Result};
use agentlaw_worker::{Client, ModelAssets, ProcessRuntime, RuntimeConfig};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct InstalledBackend {
    runtime: Option<agentlaw_flows::Runtime>,
    worker: Option<Client>,
    worker_error: Option<DomainError>,
    state: PathBuf,
    selected: Option<config::Config>,
}

pub fn worker_config(state: &Path) -> Result<RuntimeConfig> {
    let paths = [
        "AGENTLAW_ONNX_MODEL",
        "AGENTLAW_TOKENIZER",
        "AGENTLAW_ORT_LIBRARY",
    ]
    .map(|name| std::env::var_os(name).map(PathBuf::from));
    let model =
        match paths {
            [None, None, None] => crate::install::installed_assets(state)?,
            [Some(onnx_model), Some(tokenizer_json), Some(runtime_library)] => {
                if [&onnx_model, &tokenizer_json, &runtime_library]
                    .iter()
                    .any(|p| !p.is_absolute())
                {
                    return Err(DomainError::new(
                        "invalid_configuration",
                        "Model, tokenizer and ONNX Runtime paths must be absolute.",
                    ));
                }
                Some(ModelAssets {
                    onnx_model,
                    tokenizer_json,
                    runtime_library,
                })
            }
            _ => return Err(DomainError::new(
                "invalid_configuration",
                "Set AGENTLAW_ONNX_MODEL, AGENTLAW_TOKENIZER and AGENTLAW_ORT_LIBRARY together.",
            )),
        };
    Ok(RuntimeConfig {
        state_dir: state.join("worker"),
        executable: std::env::current_exe().map_err(|_| {
            DomainError::new(
                "worker_unavailable",
                "Could not locate the worker host executable.",
            )
        })?,
        model,
    })
}

impl InstalledBackend {
    /// Attach immediately at frontend startup. Model loading proceeds in the daemon;
    /// initialization and tool discovery do not wait for model inference.
    pub fn start() -> Result<Self> {
        let state = config::state_root()?;
        let (worker, worker_error) = match worker_config(&state) {
            Ok(configuration) => match ProcessRuntime::attach(&configuration) {
                Ok(client) => (Some(client), None),
                Err(_) => (None, Some(DomainError::new("semantic_unavailable", "The shared embedding worker could not be attached. Check the worker state and configured artifact paths; lexical recall remains available."))),
            },
            Err(error) => (None, Some(error)),
        };
        Ok(Self {
            runtime: None,
            worker,
            worker_error,
            state,
            selected: None,
        })
    }
}

impl Backend for InstalledBackend {
    fn call(&mut self, request: Request) -> Result<Value> {
        self.call_with_control(request, agentlaw_flows::RequestControl::default())
    }
    fn call_with_control(
        &mut self,
        mut request: Request,
        control: agentlaw_flows::RequestControl,
    ) -> Result<Value> {
        control.check()?;
        let mut connected_selection = None;
        if let Request::ConnectProjectMemory(connect) = &mut request {
            if let Some(path) = connect.memory_store_path.take() {
                control.phase("preparing_memory_store");
                let (_, selected) = crate::setup::connect_selection_with_control(
                    &self.state,
                    Path::new(&path),
                    false,
                    control.clone(),
                )?;
                connected_selection = Some(selected);
            }
        }
        // Pin one selection at the request boundary; a concurrent switch affects
        // the next request, never this call or its retained operation identity.
        let selected = match connected_selection {
            Some(selected) => selected,
            None => config::load(&self.state)?.ok_or_else(|| DomainError::new(
                "memory_store_connection_required",
                "No memory store is connected. Run `agentlaw store propose-location`, ask the user to confirm the location, then use store create/connect. Do not create a project identity before connecting its memory store.",
            ))?,
        };
        config::require_existing_binding(&self.state, &selected)?;
        if self.selected.as_ref() != Some(&selected) || self.runtime.is_none() {
            let machine = crate::machine::load_or_create(&self.state)?;
            control.phase("opening_memory_store");
            let runtime = agentlaw_flows::Runtime::open_with_machine_and_coordination(
                &selected.memory_store_path,
                selected.runtime_root(&self.state),
                &selected.user_id,
                machine.machine_id,
                config::coordination_root(&self.state),
            )?
            .with_history_response_limit(selected.history_response_limit_bytes);
            if self.worker.is_none() {
                match worker_config(&self.state).and_then(|c|ProcessRuntime::attach(&c).map_err(|_|DomainError::new("semantic_unavailable","The embedding worker could not be attached; inspect worker diagnostics."))) {
                    Ok(worker) => { self.worker=Some(worker); self.worker_error=None; },
                    Err(error) => { self.worker_error=Some(error); },
                }
            }
            self.runtime = Some(match self.worker.take() {
                Some(worker) => runtime.with_worker(worker),
                None => runtime,
            });
            self.selected = Some(selected.clone());
        }
        let recall = matches!(request, Request::Recall(_));
        let limit = if matches!(request, Request::History(_)) {
            selected
                .response_limit_bytes
                .min(selected.history_response_limit_bytes)
        } else {
            selected.response_limit_bytes
        };
        let mut result = self
            .runtime
            .as_mut()
            .expect("runtime initialized above")
            .call_with_control(request, control)?;
        if recall {
            if let Some(error) = &self.worker_error {
                let diagnostics = result
                    .as_object_mut()
                    .ok_or_else(|| {
                        DomainError::new(
                            "invalid_result",
                            "Runtime returned a non-object recall result.",
                        )
                    })?
                    .entry("diagnostics")
                    .or_insert_with(|| Value::Array(vec![]));
                diagnostics
                    .as_array_mut()
                    .ok_or_else(|| {
                        DomainError::new("invalid_result", "Recall diagnostics were not an array.")
                    })?
                    .push(serde_json::to_value(error).map_err(|_| {
                        DomainError::new(
                            "invalid_result",
                            "Could not encode the worker diagnostic.",
                        )
                    })?);
            }
        }
        if let Some(notice) = result
            .get_mut("response_limit")
            .and_then(Value::as_object_mut)
        {
            notice.insert(
                "config_file".into(),
                serde_json::json!(self.state.join("config.json")),
            );
        }
        crate::delivery::adapt(result, &self.state, limit)
    }
}

/// Private worker entry point, deliberately absent from the public MCP surface.
pub fn daemon_args(args: &[String]) -> Result<RuntimeConfig> {
    let mut state = None;
    let mut model = None;
    let mut tokenizer = None;
    let mut library = None;
    if args.first().map(String::as_str) != Some("worker-daemon") || args.len() % 2 != 1 {
        return Err(DomainError::new(
            "invalid_arguments",
            "Invalid worker daemon arguments.",
        ));
    }
    for pair in args[1..].chunks_exact(2) {
        let slot = match pair[0].as_str() {
            "--state-dir" => &mut state,
            "--model" => &mut model,
            "--tokenizer" => &mut tokenizer,
            "--ort-library" => &mut library,
            _ => {
                return Err(DomainError::new(
                    "invalid_arguments",
                    "Unknown worker daemon option.",
                ))
            }
        };
        if slot.is_some() {
            return Err(DomainError::new(
                "invalid_arguments",
                "Duplicate worker daemon option.",
            ));
        }
        let path = PathBuf::from(&pair[1]);
        if !path.is_absolute() {
            return Err(DomainError::new(
                "invalid_arguments",
                "Worker paths must be absolute.",
            ));
        }
        *slot = Some(path);
    }
    let model = match (model, tokenizer, library) {
        (None, None, None) => None,
        (Some(onnx_model), Some(tokenizer_json), Some(runtime_library)) => Some(ModelAssets {
            onnx_model,
            tokenizer_json,
            runtime_library,
        }),
        _ => {
            return Err(DomainError::new(
                "invalid_arguments",
                "Supply all three model artifact paths together.",
            ))
        }
    };
    Ok(RuntimeConfig {
        state_dir: state.ok_or_else(|| {
            DomainError::new("invalid_arguments", "Worker state directory is required.")
        })?,
        executable: std::env::current_exe().map_err(|_| {
            DomainError::new(
                "worker_unavailable",
                "Could not locate the worker executable.",
            )
        })?,
        model,
    })
}
