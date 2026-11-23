//! Fixtures shared by runtime tests: an isolated installation and fake RAM ports.

pub mod files;
pub mod index_work;
pub mod installation;
pub mod semantic;
pub mod write_audit;

pub use files::*;
pub use index_work::*;
pub use installation::*;
pub use semantic::*;
#[cfg(test)]
pub(crate) mod store_service;

/// A test-owned child speaking the same length-prefixed JSON protocol as the worker.
/// Assets and the request transcript live exclusively in the caller's temporary directory.
#[cfg(test)]
pub(crate) fn model_child_executable() -> std::path::PathBuf {
    static BUILD: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let directory = BUILD.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("model_fixture.rs");
        std::fs::write(&source, r#"
use std::io::{Read, Write};
use serde_json::{Value, json};
fn read(r: &mut impl Read) -> Option<Value> {
    let mut length = [0; 4];
    r.read_exact(&mut length).ok()?;
    let mut body = vec![0; u32::from_be_bytes(length) as usize];
    r.read_exact(&mut body).unwrap();
    Some(serde_json::from_slice(&body).unwrap())
}
fn send(w: &mut impl Write, value: Value) {
    let body = serde_json::to_vec(&value).unwrap();
    w.write_all(&(body.len() as u32).to_be_bytes()).unwrap();
    w.write_all(&body).unwrap();
    w.flush().unwrap();
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--probe") { return; }
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let launch = read(&mut input).unwrap();
    let asset = std::path::PathBuf::from(launch["assets"]["onnx_model"].as_str().unwrap());
    let root = asset.parent().unwrap();
    let mut starts = std::fs::OpenOptions::new().create(true).append(true).open(root.join("starts.jsonl")).unwrap();
    writeln!(starts, "{}", launch["incarnation"]).unwrap();
    if !asset.exists() || root.join("offline").exists() {
        send(&mut output, json!({"Unavailable":{"cause":"fixture model assets missing"}}));
        return;
    }
    send(&mut output, json!({"Ready":{"model":"fixture-model"}}));
    while let Some(request) = read(&mut input) {
        if request == "Stop" { return; }
        if let Some(split) = request.get("Split") {
            let text = split["text"].as_str().unwrap();
            let mut start = 0;
            let ranges: Vec<_> = text.split_inclusive('|').map(|piece| {
                let end = start + piece.len();
                let ret = (start, end);
                start = end;
                ret
            }).collect();
            let mut transcript = std::fs::OpenOptions::new().create(true).append(true).open(root.join("splits.jsonl")).unwrap();
            writeln!(transcript, "{}", split["text"]).unwrap();
            send(&mut output, json!({"Pieces":{"id":split["id"],"attempt":split["attempt"],"result":{"Ok":ranges}}}));
            continue;
        }
        let embed = &request["Embed"];
        if let Ok(trigger) = std::fs::read_to_string(root.join("crash-query")) {
            if embed["text"].as_str().is_some_and(|text| text.contains(&trigger)) {
                std::fs::write(root.join("offline"), b"crashed model remains unavailable").unwrap();
                std::fs::write(root.join("crashed"), b"query reached child after preparation").unwrap();
                std::process::exit(27);
            }
        }
        if root.join("offline").exists() {
            send(&mut output, json!({"Unavailable":{"cause":"fixture model assets missing"}}));
            return;
        }
        let mut transcript = std::fs::OpenOptions::new().create(true).append(true).open(root.join("requests.jsonl")).unwrap();
        writeln!(transcript, "{}", embed["text"]).unwrap();
        let mut vector = vec![0.0_f32; 256];
        vector[0] = 1.0;
        if root.join("distinct-vectors").exists() {
            let index = match embed["text"].as_str().unwrap() {
                "aa|" => 2, "bb|" => 3, "cc|" => 4,
                other => panic!("unexpected fixture piece {other:?}"),
            };
            vector[0] = 0.6;
            vector[index] = 0.8;
            let mut vectors = std::fs::OpenOptions::new().create(true).append(true).open(root.join("vectors.jsonl")).unwrap();
            writeln!(vectors, "{}", json!({"text":embed["text"],"vector":vector})).unwrap();
        }
        let truncated = embed["overflow"] == "Prefix" && embed["text"] == "token-truncated query";
        send(&mut output, json!({"Result":{"id":embed["id"],"attempt":embed["attempt"],"truncated":truncated,"result":{"Ok":vector}}}));
    }
}
"#).unwrap();
        // Newer Cargo build layouts keep each dependency in its own directory
        // instead of one flat `deps` folder, so collect every library directory
        // under the profile that built this test.
        let executable = std::env::current_exe().unwrap();
        let profile = executable.ancestors().skip(1).find(|dir| dir.join("build").is_dir())
            .expect("Cargo profile directory").to_owned();
        let (mut library_dirs, mut candidates, mut pending) = (Vec::new(), Vec::new(), vec![profile]);
        while let Some(dir) = pending.pop() {
            let mut has_library = false;
            for path in std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()) {
                if path.is_dir() {
                    if path.file_name().is_some_and(|name| name != "incremental") { pending.push(path); }
                } else if path.extension().is_some_and(|e| e == "rlib") {
                    has_library = true;
                    if path.file_name().unwrap().to_string_lossy().starts_with("libserde_json-") { candidates.push(path); }
                }
            }
            if has_library { library_dirs.push(dir); }
        }
        let library = candidates.into_iter()
            .max_by_key(|p| std::fs::metadata(p).unwrap().modified().unwrap()).expect("compiled serde_json");
        // One search path per dependency can exceed the Windows command-line limit.
        let arguments = directory.path().join("model_fixture.args");
        std::fs::write(&arguments, library_dirs.iter()
            .map(|dir| format!("-L\ndependency={}\n", dir.display())).collect::<String>()).unwrap();
        let output = std::process::Command::new("rustc")
            .arg("--edition=2021").arg("--crate-name=model_fixture").arg(&source)
            .arg("--extern").arg(format!("serde_json={}", library.display()))
            .arg(format!("@{}", arguments.display()))
            .arg("-o").arg(directory.path().join("model_fixture.exe"))
            .output().unwrap();
        assert!(output.status.success(), "helper compilation: {}", String::from_utf8_lossy(&output.stderr));
        // Production reuses its running executable; prepare this generated image
        // before the tests start timing model readiness and recovery.
        let mut probe = std::process::Command::new(directory.path().join("model_fixture.exe"));
        probe.arg("--probe").stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            probe.creation_flags(0x08000000);
        }
        let output = probe.output().expect("start generated model fixture");
        assert!(output.status.success(), "helper probe: {}", String::from_utf8_lossy(&output.stderr));
        directory
    });
    directory.path().join("model_fixture.exe")
}

#[cfg(test)]
pub(crate) fn model_assets(root: &std::path::Path) -> agentlaw_worker::ModelAssets {
    std::fs::create_dir_all(root).unwrap();
    let assets = agentlaw_worker::ModelAssets {
        onnx_model: root.join("model.onnx"),
        tokenizer_json: root.join("tokenizer.json"),
        runtime_library: root.join("runtime.dll"),
    };
    for path in [
        &assets.onnx_model,
        &assets.tokenizer_json,
        &assets.runtime_library,
    ] {
        std::fs::write(path, b"test protocol fixture").unwrap();
    }
    assets
}

#[cfg(test)]
pub(crate) fn model_transcript(root: &std::path::Path, name: &str) -> Vec<serde_json::Value> {
    match std::fs::read_to_string(root.join(name)) {
        Ok(text) => text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(error) => panic!("{error}"),
    }
}
