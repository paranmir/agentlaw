//! Tests own their subprocess trees, never an arbitrary PID or user's daemon.
//! On Windows a Job Object closes the orphan-model gap when a test aborts its broker.
use std::process::Child;

pub struct OwnedProcess {
    child: Child,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}
impl OwnedProcess {
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
            return Self { child, job };
        }
        #[cfg(not(windows))]
        Self { child }
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
