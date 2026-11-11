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

struct Candidate {
    child: Child,
    state: PathBuf,
}

impl RuntimeLauncher for Launcher {
    fn launch(&self) -> io::Result<Box<dyn RuntimeCandidate>> {
        let spawn = |breakaway: bool| {
            let mut command = Command::new(&self.executable);
            command
                .arg("runtime-host")
                .env("AGENTLAW_HOME", &self.state)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(
                    0x08000000 | 0x00000200 | if breakaway { 0x01000000 } else { 0 },
                );
            }
            #[cfg(not(windows))]
            let _ = breakaway;
            command.spawn()
        };
        let child = match spawn(true) {
            Err(error) if cfg!(windows) && error.raw_os_error() == Some(5) => spawn(false)?,
            other => other?,
        };
        Ok(Box::new(Candidate {
            child,
            state: self.state.clone(),
        }))
    }
}

impl RuntimeCandidate for Candidate {
    fn try_exit(&mut self) -> io::Result<Option<CandidateExit>> {
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        Ok(Some(match status.code() {
            Some(3) => CandidateExit::AlreadyOwned,
            Some(4) => {
                let error = crate::update::managed::check_frontend_start(&self.state)
                    .err()
                    .unwrap_or_else(|| {
                        agentlaw_contracts::DomainError::new(
                            "update_maintenance",
                            "An update was in progress; retry the request.",
                        )
                    });
                CandidateExit::StartBlocked(error)
            }
            _ => CandidateExit::Failed(format!("candidate exited: {status}")),
        }))
    }
}
