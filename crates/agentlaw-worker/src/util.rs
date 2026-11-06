//! Small helpers shared by worker modules.
use crate::daemon::Result;
#[cfg(windows)]
use std::process::Command;
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
pub(crate) fn private_state_directory(path: &std::path::Path) -> Result<()> {
    let canonical = fs::canonicalize(path)?;
    if canonical.parent().is_none()
        || std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .and_then(|p| fs::canonicalize(p).ok())
            .as_ref()
            == Some(&canonical)
    {
        return Err("worker state must use a dedicated directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let script="$ErrorActionPreference='Stop'; $taskSid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; $taskAcl=New-Object System.Security.AccessControl.DirectorySecurity; $taskAcl.SetAccessRuleProtection($true,$false); $taskRule=New-Object System.Security.AccessControl.FileSystemAccessRule($taskSid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $taskAcl.AddAccessRule($taskRule); [System.IO.Directory]::SetAccessControl($env:AGENTLAW_PRIVATE_STATE_PATH,$taskAcl)";
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("AGENTLAW_PRIVATE_STATE_PATH", &canonical)
            .creation_flags(0x08000000)
            .output()?;
        if !output.status.success() {
            return Err("unable to restrict worker state directory to current OS user".into());
        }
    }
    Ok(())
}
