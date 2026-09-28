//! Stable public command entrypoint for a managed Agentlaw installation.
//! The launcher lives outside the binary bundle replaced by an update.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{BufRead, Read},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "agentlaw.exe"
    } else {
        "agentlaw"
    }
}

fn fail(message: &str) -> ! {
    println!("{}", json!({"code":"update_incomplete","message":message}));
    std::process::exit(1)
}

fn root() -> PathBuf {
    let exe = env::current_exe().unwrap_or_else(|_| fail("Cannot locate the Agentlaw launcher."));
    let command = exe
        .parent()
        .unwrap_or_else(|| fail("Invalid launcher location."));
    if command.file_name().is_none_or(|name| name != "command") {
        fail("Run the launcher from a managed Agentlaw command directory.");
    }
    let root = command
        .parent()
        .unwrap_or_else(|| fail("Invalid managed root."));
    if fs::read_to_string(root.join(".agentlaw-layout"))
        .ok()
        .is_none_or(|marker| marker.trim() != "agentlaw-managed-layout-v1")
    {
        fail("This launcher has no verified managed Agentlaw root.");
    }
    fs::canonicalize(root).unwrap_or_else(|_| fail("Cannot resolve the managed root."))
}

fn invoke(exe: &Path, root: &Path, args: &[&str]) -> Output {
    let mut child = Command::new(exe)
        .args(args)
        .env("AGENTLAW_HOME", root.join("state"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|_| fail("Cannot run the managed Agentlaw executable."));
    let stdout = child
        .stdout
        .take()
        .unwrap_or_else(|| fail("Cannot read the managed command."));
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = Vec::new();
        let _ = std::io::BufReader::new(stdout).read_until(b'\n', &mut line);
        let _ = send.send(line);
    });
    let status = child
        .wait()
        .unwrap_or_else(|_| fail("The managed command could not finish."));
    let stdout = receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap_or_else(|_| fail("The managed command returned no complete result."));
    Output {
        status,
        stdout,
        stderr: Vec::new(),
    }
}

fn json_result(output: &Output, phase: &str) -> Value {
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| fail(&format!("{phase} returned an invalid result.")));
    if !output.status.success() {
        println!("{value}");
        std::process::exit(1);
    }
    value
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| fail("The update handoff is incomplete."))
}

fn digest(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap_or_else(|_| fail("The candidate is missing."));
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .unwrap_or_else(|_| fail("The candidate cannot be verified."));
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    format!("{:x}", hash.finalize())
}

fn finish_update(root: &Path, plan: &str, candidate: &Path, expected: &str) {
    let versions = fs::canonicalize(root.join("state").join("versions"))
        .unwrap_or_else(|_| fail("The version store is unavailable."));
    let resolved =
        fs::canonicalize(candidate).unwrap_or_else(|_| fail("The candidate path is unavailable."));
    if !resolved.starts_with(&versions)
        || resolved
            .file_name()
            .is_none_or(|name| name != binary_name())
        || digest(&resolved) != expected
    {
        fail("The update candidate differs from the approved handoff.");
    }
    let output = invoke(
        &resolved,
        root,
        &[
            "update",
            "apply",
            &plan,
            "--root",
            root.to_str().unwrap_or(""),
        ],
    );
    let result = json_result(&output, "Candidate installation");
    if result["status"] != "completed" || result["plan_id"] != plan {
        fail("The candidate did not report a completed installation.");
    }
    let status = json_result(
        &invoke(
            &resolved,
            root,
            &[
                "update",
                "status",
                &plan,
                "--root",
                root.to_str().unwrap_or(""),
            ],
        ),
        "Update status",
    );
    if status["status"] != "completed"
        || status["plan_id"] != plan
        || status["recovery_obligations_closed"] != true
        || status["cleanup"] != "completed"
    {
        fail("The candidate exited before verified installation and cleanup completed.");
    }
    println!(
        "{}",
        json!({"status":"installed","plan_id":plan,"installed_version":status["tag"],
        "next_action":"Restart the harness normally. Agentlaw is installed and ready."})
    );
}

fn update(root: &Path) {
    let maintenance = root.join("state").join("update-maintenance.json");
    if maintenance.exists() {
        let marker: Value = serde_json::from_slice(
            &fs::read(&maintenance)
                .unwrap_or_else(|_| fail("Cannot inspect the interrupted update.")),
        )
        .unwrap_or_else(|_| fail("The interrupted update marker is invalid."));
        let plan = string(&marker, "plan_id");
        let candidate = PathBuf::from(string(&marker, "candidate"));
        let expected = string(&marker, "candidate_sha256");
        finish_update(root, plan, &candidate, expected);
        return;
    }
    let old = root.join("bin").join(binary_name());
    let preview = json_result(&invoke(&old, root, &["update"]), "Update preview");
    if preview["status"] == "up_to_date" {
        println!("{preview}");
        return;
    }
    if preview["status"] != "confirmation_required" {
        fail("The managed update preview was not recognized.");
    }
    let plan = string(&preview, "plan_id").to_owned();
    let prepared = json_result(
        &invoke(&old, root, &["update", "--confirm-update", &plan]),
        "Update preparation",
    );
    if prepared["status"] != "handoff_ready" || prepared["plan_id"] != plan {
        fail("The managed update handoff does not match its plan.");
    }
    let candidate = PathBuf::from(string(&prepared, "candidate"));
    let expected = string(&prepared, "candidate_sha256");
    finish_update(root, &plan, &candidate, expected);
}

fn main() {
    let root = root();
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["update"] {
        update(&root);
        return;
    }
    let runtime = root.join("bin").join(binary_name());
    let status = Command::new(runtime)
        .args(&args)
        .env("AGENTLAW_HOME", root.join("state"))
        .status()
        .unwrap_or_else(|_| fail("Cannot run the managed Agentlaw executable."));
    std::process::exit(status.code().unwrap_or(1));
}
