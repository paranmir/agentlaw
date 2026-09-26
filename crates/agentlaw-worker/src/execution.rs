//! Execution-provider selection is a performance choice, never a model-space change.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionBackend {
    Cpu,
    Cuda,
    DirectMl,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub cacheable: bool,
    pub backend: ExecutionBackend,
    pub cpu_ns: Vec<u64>,
    pub gpu_ns: Vec<u64>,
    pub reason: String,
    pub host_resident_bytes: Option<u64>,
    pub gpu_dedicated_bytes: Option<u64>,
    pub gpu_shared_bytes: Option<u64>,
}
pub fn gpu_is_unambiguously_faster(cpu: &[u64], gpu: &[u64]) -> bool {
    cpu.len() == 3
        && gpu.len() == 3
        && cpu.iter().all(|v| *v > 0)
        && gpu.iter().all(|v| *v > 0)
        && gpu.iter().max() < cpu.iter().min()
}
pub fn fingerprint(model: &str, runtime: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut hash = Sha256::new();
    hash.update(model.as_bytes());
    hash.update(std::env::consts::ARCH.as_bytes());
    let mut file = std::fs::File::open(runtime).ok()?;
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let result=std::process::Command::new("powershell.exe").args(["-NoProfile","-NonInteractive","-Command","$ErrorActionPreference='Stop'; Get-CimInstance Win32_Processor | Select-Object Name,ProcessorId | ConvertTo-Json -Compress; Get-CimInstance Win32_VideoController | Select-Object PNPDeviceID,DriverVersion | Sort-Object PNPDeviceID | ConvertTo-Json -Compress"]).creation_flags(0x08000000).output().ok()?;
        if !result.status.success() {
            return None;
        }
        hash.update(result.stdout);
    }
    #[cfg(target_os = "linux")]
    {
        hash.update(std::fs::read("/proc/cpuinfo").ok()?);
        for entry in std::fs::read_dir("/sys/class/drm").ok()? {
            let p = entry.ok()?.path().join("device");
            for name in ["vendor", "device", "driver/module/version"] {
                if let Ok(bytes) = std::fs::read(p.join(name)) {
                    hash.update(bytes);
                }
            }
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        return None;
    }
    Some(format!("{:x}", hash.finalize()))
}
pub fn host_resident_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Counters {
            cb: u32,
            faults: u32,
            peak: usize,
            working: usize,
            quota_peak: usize,
            quota: usize,
            nonpaged_peak: usize,
            nonpaged: usize,
            pagefile: usize,
            peak_pagefile: usize,
        }
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
        }
        #[link(name = "psapi")]
        unsafe extern "system" {
            fn GetProcessMemoryInfo(
                process: *mut std::ffi::c_void,
                counters: *mut Counters,
                size: u32,
            ) -> i32;
        }
        let mut counters: Counters = unsafe { std::mem::zeroed() };
        counters.cb = std::mem::size_of::<Counters>() as u32;
        if unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                std::mem::size_of::<Counters>() as u32,
            )
        } != 0
        {
            return Some(counters.working as u64);
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
        line.split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_ranges_and_noise() {
        assert!(gpu_is_unambiguously_faster(&[11, 12, 13], &[1, 2, 3]));
        assert!(!gpu_is_unambiguously_faster(&[2, 12, 13], &[1, 2, 3]));
        assert!(!gpu_is_unambiguously_faster(&[2, 3], &[1, 1]));
        assert!(!gpu_is_unambiguously_faster(&[3, 4, 5], &[0, 1, 2]));
    }
}
