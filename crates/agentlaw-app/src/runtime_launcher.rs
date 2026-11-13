//! Launches an installation runtime candidate.
use agentlaw_runtime::client::{CandidateExit, RuntimeCandidate, RuntimeLauncher};
#[cfg(not(windows))]
use std::process::{Child, Command, Stdio};
use std::{io, path::PathBuf};

pub struct Launcher {
    pub executable: PathBuf,
    pub state: PathBuf,
}

struct Candidate {
    #[cfg(not(windows))]
    child: Child,
    #[cfg(windows)]
    child: WindowsChild,
    state: PathBuf,
}

impl RuntimeLauncher for Launcher {
    fn launch(&self) -> io::Result<Box<dyn RuntimeCandidate>> {
        #[cfg(windows)]
        let child = match WindowsChild::spawn(self, true) {
            Err(e) if e.raw_os_error() == Some(5) => WindowsChild::spawn(self, false)?,
            other => other?,
        };
        #[cfg(not(windows))]
        let child = {
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
            match spawn(true) {
                Err(error) if cfg!(windows) && error.raw_os_error() == Some(5) => spawn(false)?,
                other => other?,
            }
        };
        Ok(Box::new(Candidate {
            child,
            state: self.state.clone(),
        }))
    }
}

impl RuntimeCandidate for Candidate {
    fn try_exit(&mut self) -> io::Result<Option<CandidateExit>> {
        #[cfg(windows)]
        let code = match self.child.try_exit()? {
            Some(code) => code,
            None => return Ok(None),
        };
        #[cfg(not(windows))]
        let code = {
            let Some(status) = self.child.try_wait()? else {
                return Ok(None);
            };
            status.code().unwrap_or(-1)
        };
        Ok(Some(match code {
            3 => CandidateExit::AlreadyOwned,
            4 => {
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
            _ => CandidateExit::Failed(format!("candidate exited: {code}")),
        }))
    }
}

#[cfg(windows)]
struct WindowsChild(std::os::windows::io::OwnedHandle);
#[cfg(windows)]
impl WindowsChild {
    fn spawn(launcher: &Launcher, breakaway: bool) -> io::Result<Self> {
        use std::{
            ffi::OsString,
            os::windows::{ffi::OsStrExt, io::FromRawHandle},
            ptr,
        };
        #[repr(C)]
        struct Startup {
            size: u32,
            reserved: *mut u16,
            desktop: *mut u16,
            title: *mut u16,
            x: u32,
            y: u32,
            xs: u32,
            ys: u32,
            xc: u32,
            yc: u32,
            fill: u32,
            flags: u32,
            show: u16,
            reserved2: u16,
            reserved_ptr: *mut u8,
            stdin: *mut std::ffi::c_void,
            stdout: *mut std::ffi::c_void,
            stderr: *mut std::ffi::c_void,
        }
        #[repr(C)]
        struct Process {
            process: *mut std::ffi::c_void,
            thread: *mut std::ffi::c_void,
            pid: u32,
            tid: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateProcessW(
                app: *const u16,
                cmd: *mut u16,
                pa: *mut std::ffi::c_void,
                ta: *mut std::ffi::c_void,
                inherit: i32,
                flags: u32,
                env: *mut std::ffi::c_void,
                dir: *const u16,
                startup: *mut Startup,
                process: *mut Process,
            ) -> i32;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        let app: Vec<u16> = launcher
            .executable
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut command: Vec<u16> = OsString::from(format!(
            "\"{}\" runtime-host",
            launcher.executable.display()
        ))
        .encode_wide()
        .chain(Some(0))
        .collect();
        let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(k, _)| !k.to_string_lossy().eq_ignore_ascii_case("AGENTLAW_HOME"))
            .collect();
        vars.push((
            OsString::from("AGENTLAW_HOME"),
            launcher.state.as_os_str().to_owned(),
        ));
        vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
        let mut env: Vec<u16> = Vec::new();
        for (key, value) in vars {
            env.extend(key.encode_wide());
            env.push(b'=' as u16);
            env.extend(value.encode_wide());
            env.push(0);
        }
        env.push(0);
        let mut startup: Startup = unsafe { std::mem::zeroed() };
        startup.size = std::mem::size_of::<Startup>() as u32;
        let mut process: Process = unsafe { std::mem::zeroed() };
        let flags = 0x08000000 | 0x00000200 | 0x00000400 | if breakaway { 0x01000000 } else { 0 };
        if unsafe {
            CreateProcessW(
                app.as_ptr(),
                command.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                flags,
                env.as_mut_ptr().cast(),
                ptr::null(),
                &mut startup,
                &mut process,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            CloseHandle(process.thread);
            Ok(Self(std::os::windows::io::OwnedHandle::from_raw_handle(
                process.process,
            )))
        }
    }
    fn try_exit(&self) -> io::Result<Option<i32>> {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
        }
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.0.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((code != 259).then_some(code as i32))
    }
}
