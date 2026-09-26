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
fn now() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| failure())
}
fn registry(directory: &Path) -> Result<Connection> {
    let connection =
        Connection::open(directory.join("response-registry.sqlite")).map_err(|_| failure())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| failure())?;
    connection.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS response_artifacts(id TEXT PRIMARY KEY,expires INTEGER NOT NULL,next_attempt INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS response_expiration ON response_artifacts(expires,next_attempt);").map_err(|_|failure())?;
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
    let mut query=connection.prepare("SELECT id FROM response_artifacts WHERE expires<=?1 AND next_attempt<=?1 ORDER BY expires,id LIMIT 32").map_err(|_|failure())?;
    let rows = query
        .query_map([i64::try_from(now).map_err(|_| failure())?], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|_| failure())?;
    let ids = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| failure())?;
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
                .map_err(|_| failure())?;
            removed += 1;
        } else {
            connection
                .execute(
                    "UPDATE response_artifacts SET next_attempt=?1 WHERE id=?2",
                    params![
                        i64::try_from(now.saturating_add(3600)).map_err(|_| failure())?,
                        id
                    ],
                )
                .map_err(|_| failure())?;
        }
    }
    Ok(removed)
}
pub struct ArtifactWriter {
    writer: io::BufWriter<File>,
    directory: PathBuf,
    id: String,
    expires: u64,
    removed: usize,
}
impl ArtifactWriter {
    pub fn create(directory: impl AsRef<Path>) -> Result<Self> {
        std::fs::create_dir_all(directory.as_ref()).map_err(|_| failure())?;
        let directory = std::fs::canonicalize(directory).map_err(|_| failure())?;
        let connection = registry(&directory)?;
        let current = now()?;
        let removed = cleanup(&directory, &connection, current)?;
        let expires = current.checked_add(RETENTION_SECONDS).ok_or_else(failure)?;
        let id = uuid::Uuid::new_v4().to_string();
        connection
            .execute(
                "INSERT INTO response_artifacts VALUES(?1,?2,?2)",
                params![id, i64::try_from(expires).map_err(|_| failure())?],
            )
            .map_err(|_| failure())?;
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.join(format!("response-{id}.tmp")))
            .map_err(|_| failure())?;
        Ok(Self {
            writer: io::BufWriter::new(file),
            directory,
            id,
            expires,
            removed,
        })
    }
    pub fn finish(mut self) -> Result<Artifact> {
        self.expires = now()?.checked_add(RETENTION_SECONDS).ok_or_else(failure)?;
        registry(&self.directory)?
            .execute(
                "UPDATE response_artifacts SET expires=?1,next_attempt=?1 WHERE id=?2",
                params![i64::try_from(self.expires).map_err(|_| failure())?, self.id],
            )
            .map_err(|_| failure())?;
        self.writer.flush().map_err(|_| failure())?;
        self.writer.get_ref().sync_all().map_err(|_| failure())?;
        let bytes = self
            .writer
            .get_ref()
            .metadata()
            .map_err(|_| failure())?
            .len();
        let path = self.directory.join(format!("response-{}.json", self.id));
        drop(self.writer);
        std::fs::rename(
            self.directory.join(format!("response-{}.tmp", self.id)),
            &path,
        )
        .map_err(|_| failure())?;
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
        self.writer.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
pub fn create_from_reader(directory: impl AsRef<Path>, reader: &mut impl Read) -> Result<Artifact> {
    let mut writer = ArtifactWriter::create(directory)?;
    io::copy(reader, &mut writer).map_err(|_| failure())?;
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_expired_registered_owned_files_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = create_from_reader(tmp.path(), &mut b"{\"value\":1}".as_slice()).unwrap();
        assert!(artifact.expires_at_unix_seconds > now().unwrap());
        let unregistered = tmp.path().join("user-data.json");
        std::fs::write(&unregistered, "preserve").unwrap();
        let connection = registry(tmp.path()).unwrap();
        connection
            .execute("UPDATE response_artifacts SET expires=0,next_attempt=0", [])
            .unwrap();
        let newer = create_from_reader(tmp.path(), &mut b"{}".as_slice()).unwrap();
        assert_eq!(newer.expired_artifacts_removed, 1);
        assert!(!artifact.path.exists());
        assert!(unregistered.exists());
        assert!(newer.path.exists());
    }
    #[test]
    fn registered_traversal_cannot_delete_a_user_file() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.json");
        std::fs::write(&outside, "preserve").unwrap();
        let directory = tmp.path().join("responses");
        std::fs::create_dir(&directory).unwrap();
        let connection = registry(&directory).unwrap();
        connection
            .execute(
                "INSERT INTO response_artifacts VALUES('../outside',0,0)",
                [],
            )
            .unwrap();
        let artifact = create_from_reader(&directory, &mut b"{}".as_slice()).unwrap();
        assert_eq!(artifact.expired_artifacts_removed, 0);
        assert!(outside.exists());
    }
}
