//! Worker configuration for operating commands.
use agentlaw_contracts::{DomainError, Result};
use agentlaw_worker::ModelAssets;
use std::path::{Path, PathBuf};

pub fn model_assets(state: &Path) -> Result<Option<ModelAssets>> {
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
    Ok(model)
}
