#[path = "../../../tests/support/owned_process.rs"]
mod owned_process;
use owned_process::OwnedProcess as StopChild;
use std::{
    process::{Command, Stdio},
    time::Duration,
};
#[test]
fn compatibility_placeholder_explains_it_is_unused_and_exits_unsuccessfully() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentlaw-worker"));
    command
        .env("AGENTLAW_HOME", dir.path())
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut process = command.spawn().unwrap();
    let mut output = process.stdout.take().unwrap();
    let mut errors = process.stderr.take().unwrap();
    let mut child = StopChild::new(process);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "placeholder did not exit"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    use std::io::Read;
    output.read_to_string(&mut stdout).unwrap();
    errors.read_to_string(&mut stderr).unwrap();
    assert!(!status.success(), "placeholder unexpectedly succeeded");
    let message = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        message.contains("no longer used"),
        "placeholder must explain retirement; observed stdout={stdout:?}, stderr={stderr:?}"
    );
}
