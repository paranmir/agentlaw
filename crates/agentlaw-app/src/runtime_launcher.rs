//! Launches an installation runtime candidate.
use agentlaw_runtime::client::{CandidateExit, RuntimeCandidate, RuntimeLauncher};
use std::{
    io,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

pub struct Launcher {
    pub executable: PathBuf,
    pub state: PathBuf,
}

struct Candidate(Child);

impl RuntimeLauncher for Launcher {
    fn launch(&self) -> io::Result<Box<dyn RuntimeCandidate>> {
        let child = Command::new(&self.executable)
            .arg("runtime-host")
            .env("AGENTLAW_HOME", &self.state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(Box::new(Candidate(child)))
    }
}

impl RuntimeCandidate for Candidate {
    fn try_exit(&mut self) -> io::Result<Option<CandidateExit>> {
        let Some(status) = self.0.try_wait()? else {
            return Ok(None);
        };
        Ok(Some(match status.code() {
            Some(3) => CandidateExit::AlreadyOwned,
            Some(4) => {
                use std::io::Read;
                let mut bytes = Vec::new();
                if let Some(stderr) = self.0.stderr.as_mut() {
                    stderr.read_to_end(&mut bytes)?;
                }
                match serde_json::from_slice(&bytes) {
                    Ok(error) => CandidateExit::StartBlocked(error),
                    Err(error) => CandidateExit::Failed(error.to_string()),
                }
            }
            _ => CandidateExit::Failed(format!("candidate exited: {status}")),
        }))
    }
}
