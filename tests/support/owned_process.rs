//! Tests own their subprocess trees, never an arbitrary PID or user's daemon.
//! On Windows a Job Object closes the orphan-model gap when a test aborts its broker.
//! Frontend commands (CLI and MCP) are not placed in that job: they start a
//! runtime host that must outlive them, and a kill-on-close job would end the
//! host whenever its breakaway is denied. Tests stop those hosts with
//! `RuntimeCleanup`.
use std::process::Child;

pub struct OwnedProcess {
    child: Child,
    #[cfg(windows)]
    job: Option<windows_sys::Win32::Foundation::HANDLE>,
}
impl OwnedProcess {
    #[allow(dead_code)]
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    #[allow(dead_code)]
    pub fn stop(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
    #[allow(dead_code)]
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }
    #[allow(dead_code)]
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait()
    }
    pub fn new(mut child: Child) -> Self {
        #[cfg(windows)]
        unsafe {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = !job.is_null()
                && SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                ) != 0
                && AssignProcessToJobObject(job, child.as_raw_handle()) != 0;
            if !ok {
                let error = std::io::Error::last_os_error();
                let _ = child.kill();
                let _ = child.wait();
                if !job.is_null() {
                    CloseHandle(job);
                }
                panic!("cannot contain owned test process: {error}");
            }
            return Self {
                child,
                job: Some(job),
            };
        }
        #[cfg(not(windows))]
        Self { child }
    }

    /// Owns a CLI or MCP frontend process without a job, so the runtime host
    /// it starts keeps running after the frontend ends.
    #[allow(dead_code)]
    pub fn frontend(child: Child) -> Self {
        Self {
            child,
            #[cfg(windows)]
            job: None,
        }
    }

    #[allow(dead_code)] // Each integration target uses only the process checks it needs.
    pub fn is_running(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("inspect owned process")
            .is_none()
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(job) = self.job {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(job);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
