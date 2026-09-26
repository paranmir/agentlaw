//! Cooperative source admission. Already-decided redo never consults new budgets.
use super::*;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
}
#[derive(Serialize)]
struct Reservation<'a> {
    operation_id: &'a str,
    memory_bytes: u64,
    disk_bytes: u64,
    scope: &'static str,
}
fn checked(bytes: u64, multiple: u64) -> Result<u64> {
    bytes
        .checked_mul(multiple)
        .and_then(|n| n.checked_add(64 * 1024 * 1024))
        .ok_or(Error::InsufficientResource {
            resource: "checked reservation arithmetic".into(),
            required: u64::MAX,
            available: 0,
        })
}
pub(super) fn serialized_bytes(value: &impl Serialize) -> Result<u64> {
    struct Counter(u64);
    impl Write for Counter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(b.len() as u64)
                .ok_or_else(|| std::io::Error::other("reservation overflow"))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(&mut count, value)?;
    Ok(count.0)
}
impl Store {
    pub(super) fn admission_lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.local.join("resource-admission.lock"))?;
        file.lock_exclusive()?;
        Ok(file)
    }
    pub(super) fn admit_mutations(
        &self,
        op: &str,
        mutations: &[Mutation],
        limits: Option<&ResourceLimits>,
    ) -> Result<()> {
        let mut bytes = serialized_bytes(&mutations)?;
        for m in mutations {
            let path = self.safe_path(&Store::relative(&m.unit))?;
            if path.exists() {
                bytes = bytes
                    .checked_add(path.metadata()?.len())
                    .ok_or(Error::Capacity)?;
            }
        }
        self.admit_bytes(op, bytes, limits)
    }
    pub(super) fn admit_bytes(
        &self,
        op: &str,
        bytes: u64,
        limits: Option<&ResourceLimits>,
    ) -> Result<()> {
        let memory = checked(bytes, 12)?;
        let disk = checked(bytes, 20)?;
        self.admit_estimate(op, memory, disk, limits)
    }
    pub(super) fn admit_stream(&self, op: &str, bytes: u64, files: u64) -> Result<()> {
        let memory = files
            .checked_mul(2048)
            .and_then(|n| n.checked_add(256 * 1024 * 1024))
            .ok_or(Error::Capacity)?;
        self.admit_estimate(op, memory, checked(bytes, 4)?, None)
    }
    fn admit_estimate(
        &self,
        op: &str,
        memory: u64,
        disk: u64,
        limits: Option<&ResourceLimits>,
    ) -> Result<()> {
        let available =
            memory_available()?.min(limits.and_then(|l| l.memory_bytes).unwrap_or(u64::MAX));
        if memory > available {
            return Err(Error::InsufficientResource {
                resource: "memory".into(),
                required: memory,
                available,
            });
        }
        self.check_disk(disk, limits)?;
        self.record(&self.local.join("resource-admission"),&Reservation{operation_id:op,memory_bytes:memory,disk_bytes:disk,scope:"serialized cooperative source operation; external process consumption is not reserved"})
    }
    pub(super) fn check_decision_capacity(
        &self,
        dir: &Path,
        targets: &[Target],
        limits: Option<&ResourceLimits>,
    ) -> Result<()> {
        let mut bytes = 4u64 * 1024 * 1024;
        for t in targets {
            bytes = bytes
                .checked_add(dir.join(&t.image).metadata()?.len())
                .ok_or(Error::Capacity)?;
        }
        self.check_disk(bytes, limits)
    }
    fn check_disk(&self, bytes: u64, limits: Option<&ResourceLimits>) -> Result<()> {
        for (label, path) in [
            ("source disk", &self.root),
            ("local recovery disk", &self.local),
        ] {
            let available = fs2::available_space(path)
                .map_err(|_| Error::ResourceUnknown(label.into()))?
                .min(limits.and_then(|l| l.disk_bytes).unwrap_or(u64::MAX));
            if bytes > available {
                return Err(Error::InsufficientResource {
                    resource: label.into(),
                    required: bytes,
                    available,
                });
            }
        }
        Ok(())
    }
}
#[cfg(windows)]
fn memory_available() -> Result<u64> {
    #[repr(C)]
    struct Status {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page: u64,
        available_page: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
    }
    let mut status = Status {
        length: std::mem::size_of::<Status>() as u32,
        load: 0,
        total_physical: 0,
        available_physical: 0,
        total_page: 0,
        available_page: 0,
        total_virtual: 0,
        available_virtual: 0,
        available_extended: 0,
    };
    // Windows fills exactly the repr(C) structure whose size is supplied above.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(Error::ResourceUnknown("physical memory".into()));
    }
    Ok(status.available_physical)
}
#[cfg(not(windows))]
fn memory_available() -> Result<u64> {
    let text = fs::read_to_string("/proc/meminfo")
        .map_err(|_| Error::ResourceUnknown("physical memory".into()))?;
    text.lines()
        .find_map(|line| {
            line.strip_prefix("MemAvailable:")
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<u64>().ok())
        })
        .and_then(|n| n.checked_mul(1024))
        .ok_or_else(|| Error::ResourceUnknown("physical memory".into()))
}
