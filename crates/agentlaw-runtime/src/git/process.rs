//! Owns a Git subprocess tree for the lifetime of a request.
use std::process::{Child, Command};
pub(super) fn prepare(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

#[cfg(windows)]
pub(super) struct Tree {
    _job: std::os::windows::io::OwnedHandle,
}
#[cfg(windows)]
impl Tree {
    pub(super) fn attach(child: &Child) -> std::io::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows::{
            core::PCWSTR,
            Win32::{
                Foundation::HANDLE,
                System::JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                },
            },
        };
        let handle =
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(std::io::Error::other)?;
        let owned = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0 as _) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                HANDLE(owned.as_raw_handle() as isize),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
            .map_err(std::io::Error::other)?;
            AssignProcessToJobObject(
                HANDLE(owned.as_raw_handle() as isize),
                HANDLE(child.as_raw_handle() as isize),
            )
            .map_err(std::io::Error::other)?;
        }
        Ok(Self { _job: owned })
    }
    pub(super) fn terminate(self) {
        drop(self);
    }
}

#[cfg(unix)]
pub(super) struct Tree(u32);
#[cfg(unix)]
impl Tree {
    pub(super) fn attach(child: &Child) -> std::io::Result<Self> {
        Ok(Self(child.id()))
    }
    pub(super) fn terminate(self) {
        drop(self);
    }
}
#[cfg(unix)]
impl Drop for Tree {
    fn drop(&mut self) {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        if let Ok(pid) = i32::try_from(self.0) {
            unsafe {
                kill(-pid, 9);
            }
        }
    }
}

#[cfg(not(any(windows, unix)))]
compile_error!("Git process containment is unsupported on this platform");
