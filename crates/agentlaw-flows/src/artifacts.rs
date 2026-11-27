//! Version-frozen response files have explicit retention and an isolated registry.
//! Cleanup never traverses source, pending, or arbitrary caller-named files.
use crate::{DomainError, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
pub const RETENTION_SECONDS: u64 = 7 * 24 * 60 * 60;
#[derive(Debug, Serialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub format: &'static str,
    pub bytes: u64,
    pub complete: bool,
    pub access: &'static str,
    pub expires_at_unix_seconds: u64,
    pub retention_seconds: u64,
    pub expired_artifacts_removed: usize,
}
fn failure() -> DomainError {
    DomainError::new("result_delivery_failed","Could not retain the complete result artifact. A completed source mutation is not rolled back by a delivery failure.")
}
fn registry_path(directory: &Path) -> PathBuf {
    directory.join("response-registry.sqlite")
}
fn registry_error(directory: &Path, stage: &str, cause: impl std::fmt::Display) -> DomainError {
    DomainError::new(
        "result_delivery_failed",
        format!(
            "Could not {stage} at {}: {cause}",
            registry_path(directory).display()
        ),
    )
}
fn now() -> std::result::Result<u64, std::time::SystemTimeError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
}
fn registry_now(directory: &Path) -> Result<u64> {
    now().map_err(|error| registry_error(directory, "read the clock for artifact expiry", error))
}
fn expiration(directory: &Path, now: u64) -> Result<u64> {
    now.checked_add(RETENTION_SECONDS).ok_or_else(|| {
        registry_error(
            directory,
            "compute artifact expiry",
            "retention deadline exceeds the clock range",
        )
    })
}
fn registry_time(directory: &Path, seconds: u64) -> Result<i64> {
    i64::try_from(seconds)
        .map_err(|error| registry_error(directory, "encode artifact expiry", error))
}
fn registry(directory: &Path) -> Result<Connection> {
    let connection = agentlaw_sqlite::open(registry_path(directory))
        .map_err(|error| registry_error(directory, "open the result registry", error))?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| registry_error(directory, "configure result registry access", error))?;
    connection.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS response_artifacts(id TEXT PRIMARY KEY,expires INTEGER NOT NULL,next_attempt INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS response_expiration ON response_artifacts(expires,next_attempt);")
        .map_err(|error| registry_error(directory, "initialize the result registry", error))?;
    Ok(connection)
}
fn remove_expired_file(directory: &Path, id: &str, extension: &str) -> io::Result<()> {
    let path = directory.join(format!("response-{id}.{extension}"));
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::other("unowned response artifact"));
    }
    let resolved = std::fs::canonicalize(&path)?;
    if resolved.parent() != Some(directory) {
        return Err(io::Error::other(
            "response artifact escaped owned directory",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let exclusive = OpenOptions::new()
            .read(true)
            .access_mode(0x8001_0000)
            .share_mode(0)
            .custom_flags(0x0400_0000)
            .open(&resolved)?;
        drop(exclusive);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(resolved)
    }
}
fn cleanup(directory: &Path, connection: &Connection, now: u64) -> Result<usize> {
    let mut query=connection.prepare("SELECT id FROM response_artifacts WHERE expires<=?1 AND next_attempt<=?1 ORDER BY expires,id LIMIT 32").map_err(|error| registry_error(directory, "prepare the expired result query", error))?;
    let rows = query
        .query_map([registry_time(directory, now)?], |r| r.get::<_, String>(0))
        .map_err(|error| registry_error(directory, "query expired results", error))?;
    let ids = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| registry_error(directory, "read expired result rows", error))?;
    drop(query);
    let mut removed = 0;
    for id in ids {
        let valid = uuid::Uuid::parse_str(&id)
            .is_ok_and(|v| v.to_string() == id && v.get_version_num() == 4);
        let deleted = valid
            && remove_expired_file(directory, &id, "json").is_ok()
            && remove_expired_file(directory, &id, "tmp").is_ok();
        if deleted {
            connection
                .execute("DELETE FROM response_artifacts WHERE id=?1", [id])
                .map_err(|error| {
                    registry_error(directory, "remove an expired result registration", error)
                })?;
            removed += 1;
        } else {
            connection
                .execute(
                    "UPDATE response_artifacts SET next_attempt=?1 WHERE id=?2",
                    params![registry_time(directory, now.saturating_add(3600))?, id],
                )
                .map_err(|error| {
                    registry_error(directory, "defer the next expiry cleanup", error)
                })?;
        }
    }
    Ok(removed)
}
pub struct ArtifactWriter {
    writer: Option<io::BufWriter<File>>,
    directory: PathBuf,
    id: String,
    expires: u64,
    removed: usize,
}
impl ArtifactWriter {
    pub fn create(directory: impl AsRef<Path>) -> Result<Self> {
        let directory = directory.as_ref();
        std::fs::create_dir_all(directory).map_err(|error| {
            artifact_io("create the response-artifact directory", directory, error)
        })?;
        let directory = std::fs::canonicalize(directory).map_err(|error| {
            artifact_io("resolve the response-artifact directory", directory, error)
        })?;
        let connection = registry(&directory)?;
        let current = registry_now(&directory)?;
        let removed = cleanup(&directory, &connection, current)?;
        let expires = expiration(&directory, current)?;
        let id = uuid::Uuid::new_v4().to_string();
        connection
            .execute(
                "INSERT INTO response_artifacts VALUES(?1,?2,?2)",
                params![id, registry_time(&directory, expires)?],
            )
            .map_err(|error| registry_error(&directory, "register the result", error))?;
        let temporary = directory.join(format!("response-{id}.tmp"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| artifact_io("open the temporary result file", &temporary, error))?;
        Ok(Self {
            writer: Some(io::BufWriter::new(file)),
            directory,
            id,
            expires,
            removed,
        })
    }
    pub fn finish(mut self) -> Result<Artifact> {
        self.expires = expiration(&self.directory, registry_now(&self.directory)?)?;
        registry(&self.directory)?
            .execute(
                "UPDATE response_artifacts SET expires=?1,next_attempt=?1 WHERE id=?2",
                params![registry_time(&self.directory, self.expires)?, self.id],
            )
            .map_err(|error| registry_error(&self.directory, "update the result expiry", error))?;
        let temporary = self.directory.join(format!("response-{}.tmp", self.id));
        let mut writer = self.writer.take().expect("artifact writer is open");
        writer
            .flush()
            .map_err(|error| artifact_io("flush the result file", &temporary, error))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| artifact_io("sync the result file", &temporary, error))?;
        let bytes = writer
            .get_ref()
            .metadata()
            .map_err(|error| artifact_io("read the result file metadata", &temporary, error))?
            .len();
        let path = self.directory.join(format!("response-{}.json", self.id));
        drop(writer);
        std::fs::rename(&temporary, &path)
            .map_err(|error| artifact_io("publish the complete result file", &path, error))?;
        Ok(Artifact {
            path,
            format: "json",
            bytes,
            complete: true,
            access: "runtime_host_filesystem",
            expires_at_unix_seconds: self.expires,
            retention_seconds: RETENTION_SECONDS,
            expired_artifacts_removed: self.removed,
        })
    }
}
impl Write for ArtifactWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer
            .as_mut()
            .expect("artifact writer is open")
            .write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer
            .as_mut()
            .expect("artifact writer is open")
            .flush()
    }
}
impl Drop for ArtifactWriter {
    fn drop(&mut self) {
        drop(self.writer.take());
        let _ = std::fs::remove_file(self.directory.join(format!("response-{}.tmp", self.id)));
    }
}

fn artifact_io(stage: &str, path: &Path, error: io::Error) -> DomainError {
    DomainError::new(
        "result_delivery_failed",
        format!("Could not {stage} at {}: {error}", path.display()),
    )
}
pub fn create_from_reader(directory: impl AsRef<Path>, reader: &mut impl Read) -> Result<Artifact> {
    let mut writer = ArtifactWriter::create(directory)?;
    io::copy(reader, &mut writer).map_err(|_| failure())?;
    writer.finish()
}

#[cfg(test)]
mod tests;

/// Remove only expired, owned read-delivery files. Call from owner maintenance, not reads.
pub fn cleanup_read_artifacts(
    directory: impl AsRef<Path>,
    as_of: std::time::SystemTime,
) -> Result<usize> {
    let directory = match std::fs::canonicalize(directory.as_ref()) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(_) => return Err(failure()),
    };
    let mut removed = 0;
    for entry in std::fs::read_dir(&directory).map_err(|_| failure())? {
        let entry = entry.map_err(|_| failure())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(tail) = name.strip_prefix("response-") else {
            continue;
        };
        let Some((id, extension)) = tail.rsplit_once('.') else {
            continue;
        };
        if !matches!(extension, "json" | "tmp")
            || !uuid::Uuid::parse_str(id)
                .is_ok_and(|u| u.get_version_num() == 4 && u.to_string() == id)
        {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|_| failure())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let modified = metadata.modified().map_err(|_| failure())?;
        if as_of
            .duration_since(modified)
            .is_ok_and(|age| age >= std::time::Duration::from_secs(RETENTION_SECONDS))
        {
            remove_expired_file(&directory, id, extension).map_err(|_| failure())?;
            removed += 1;
        }
    }
    Ok(removed)
}
pub(crate) fn read_delivery_failure(reason: &str, kind: io::ErrorKind) -> DomainError {
    DomainError::new("result_delivery_failed", format!("{reason} ({kind:?})."))
}

/// Writes an exceptional read result without a registry transaction or fsync.
pub(crate) struct ReadArtifactWriter {
    writer: Option<io::BufWriter<File>>,
    temporary: PathBuf,
    final_path: PathBuf,
}
impl ReadArtifactWriter {
    pub(crate) fn create(directory: impl AsRef<Path>) -> Result<Self> {
        std::fs::create_dir_all(directory.as_ref()).map_err(|error| {
            read_delivery_failure(
                "The response-artifact directory could not be created",
                error.kind(),
            )
        })?;
        let directory = std::fs::canonicalize(directory).map_err(|error| {
            read_delivery_failure(
                "The response-artifact directory could not be resolved",
                error.kind(),
            )
        })?;
        let id = uuid::Uuid::new_v4();
        let temporary = directory.join(format!("response-{id}.tmp"));
        let final_path = directory.join(format!("response-{id}.json"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                read_delivery_failure(
                    "The temporary response file could not be opened",
                    error.kind(),
                )
            })?;
        Ok(Self {
            writer: Some(io::BufWriter::new(file)),
            temporary,
            final_path,
        })
    }
    pub(crate) fn finish(mut self) -> Result<Artifact> {
        let mut writer = self
            .writer
            .take()
            .expect("response writer is open before finish");
        writer.flush().map_err(|error| {
            read_delivery_failure(
                "The temporary response file could not be flushed",
                error.kind(),
            )
        })?;
        let metadata = writer.get_ref().metadata().map_err(|error| {
            read_delivery_failure(
                "The temporary response file metadata could not be read",
                error.kind(),
            )
        })?;
        let modified = metadata.modified().map_err(|error| {
            read_delivery_failure(
                "The response file modification time could not be read",
                error.kind(),
            )
        })?;
        let expires = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| elapsed.as_secs().checked_add(RETENTION_SECONDS))
            .ok_or_else(|| {
                DomainError::new(
                    "result_delivery_failed",
                    "The response file timestamp does not support a retention deadline.",
                )
            })?;
        drop(writer);
        std::fs::rename(&self.temporary, &self.final_path).map_err(|error| {
            read_delivery_failure(
                "The complete response file could not be published",
                error.kind(),
            )
        })?;
        Ok(Artifact {
            path: self.final_path.clone(),
            format: "json",
            bytes: metadata.len(),
            complete: true,
            access: "runtime_host_filesystem",
            expires_at_unix_seconds: expires,
            retention_seconds: RETENTION_SECONDS,
            expired_artifacts_removed: 0,
        })
    }
}
impl Write for ReadArtifactWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer
            .as_mut()
            .expect("response writer is open during write")
            .write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer
            .as_mut()
            .expect("response writer is open during flush")
            .flush()
    }
}
impl Drop for ReadArtifactWriter {
    fn drop(&mut self) {
        drop(self.writer.take());
        let _ = std::fs::remove_file(&self.temporary);
    }
}
