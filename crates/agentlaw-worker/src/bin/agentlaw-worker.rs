//! Standalone daemon host; the main Agentlaw executable exposes the same hidden route.
use agentlaw_worker::{run_daemon, ModelAssets, RuntimeConfig};
use std::path::PathBuf;
fn main() {
    if let Err(error) = run() {
        eprintln!("worker startup failed: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let route = args.next();
    if route.as_deref() == Some("model-child") && args.next().is_none() {
        return agentlaw_worker::run_model_child();
    }
    if route.as_deref() != Some("worker-daemon") {
        return Err("expected worker-daemon".into());
    }
    let mut state = None;
    let mut model = None;
    let mut tokenizer = None;
    let mut library = None;
    while let Some(arg) = args.next() {
        let value = PathBuf::from(args.next().ok_or("missing argument value")?);
        match arg.as_str() {
            "--state-dir" => state = Some(value),
            "--model" => model = Some(value),
            "--tokenizer" => tokenizer = Some(value),
            "--ort-library" => library = Some(value),
            _ => return Err("unknown worker option".into()),
        }
    }
    let model = match (model, tokenizer, library) {
        (None, None, None) => None,
        (Some(onnx_model), Some(tokenizer_json), Some(runtime_library)) => Some(ModelAssets {
            onnx_model,
            tokenizer_json,
            runtime_library,
        }),
        _ => return Err("model, tokenizer, and runtime paths must be supplied together".into()),
    };
    run_daemon(RuntimeConfig {
        state_dir: state.ok_or("state directory required")?,
        executable: std::env::current_exe()?,
        model,
    })
}
