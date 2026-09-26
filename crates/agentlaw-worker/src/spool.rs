//! Authenticated IPC carries opaque worker-owned spool IDs, never canonical paths.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid or modified worker-owned body spool")]
    Invalid,
    #[error("body is not UTF-8")]
    Utf8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpoolBody {
    pub spool_id: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpoolBodyRef {
    pub batch_index: usize,
    pub document_index: usize,
    pub body: SpoolBody,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedSpoolPage {
    pub page: crate::derived::PublishedPage,
    pub bodies: Vec<SpoolBodyRef>,
}
struct TemporaryBody {
    file: Option<File>,
    path: PathBuf,
}
impl Drop for TemporaryBody {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}
pub fn stage_body(state_dir: &Path, reader: &mut impl Read) -> Result<SpoolBody> {
    let directory = state_dir.join("source-ingress");
    std::fs::create_dir_all(&directory)?;
    let id = uuid::Uuid::new_v4().to_string();
    let temporary = directory.join(format!("{id}.tmp"));
    let final_path = directory.join(format!("{id}.body"));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let mut temporary_body = TemporaryBody {
        file: Some(file),
        path: temporary.clone(),
    };
    let mut bytes = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if fs2::available_space(&directory)? < n as u64 {
            return Err(
                std::io::Error::other("insufficient disk capacity for source ingress").into(),
            );
        }
        bytes = bytes.checked_add(n as u64).ok_or(Error::Invalid)?;
        digest.update(&buffer[..n]);
        temporary_body
            .file
            .as_mut()
            .ok_or(Error::Invalid)?
            .write_all(&buffer[..n])?;
    }
    temporary_body
        .file
        .as_ref()
        .ok_or(Error::Invalid)?
        .sync_all()?;
    drop(temporary_body.file.take());
    std::fs::rename(temporary, final_path)?;
    Ok(SpoolBody {
        spool_id: id,
        bytes,
        sha256: format!("{:x}", digest.finalize()),
    })
}
pub fn open_body(state_dir: &Path, body: &SpoolBody) -> Result<File> {
    let id = uuid::Uuid::parse_str(&body.spool_id).map_err(|_| Error::Invalid)?;
    if id.to_string() != body.spool_id
        || id.get_version_num() != 4
        || body.sha256.len() != 64
        || !body
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid);
    }
    let directory = std::fs::canonicalize(state_dir.join("source-ingress"))?;
    let path = std::fs::canonicalize(directory.join(format!("{}.body", body.spool_id)))?;
    if path.parent() != Some(directory.as_path()) {
        return Err(Error::Invalid);
    }
    let mut file = File::open(path)?;
    if file.metadata()?.len() != body.bytes {
        return Err(Error::Invalid);
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    if format!("{:x}", digest.finalize()) != body.sha256 {
        return Err(Error::Invalid);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}
/// Call only after durable ingestion has copied every section and recorded its
/// replay receipt. This removes one authenticated owned input, never source data.
pub fn release_body(state_dir: &Path, body: &SpoolBody) -> Result<()> {
    let verified = open_body(state_dir, body)?;
    drop(verified);
    let directory = std::fs::canonicalize(state_dir.join("source-ingress"))?;
    let path = std::fs::canonicalize(directory.join(format!("{}.body", body.spool_id)))?;
    if path.parent() != Some(directory.as_path()) {
        return Err(Error::Invalid);
    }
    std::fs::remove_file(path)?;
    Ok(())
}
#[derive(Debug)]
pub struct BodySection {
    pub index: u64,
    pub text: String,
}
pub struct Sections {
    reader: File,
    pending: Vec<u8>,
    eof: bool,
    index: u64,
    failed: bool,
}
impl Sections {
    pub fn open(state_dir: &Path, body: &SpoolBody) -> Result<Self> {
        Ok(Self {
            reader: open_body(state_dir, body)?,
            pending: Vec::with_capacity(8192),
            eof: false,
            index: 0,
            failed: false,
        })
    }
}
impl Iterator for Sections {
    type Item = Result<BodySection>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let result = (|| {
            while self.pending.len() < 4096 && !self.eof {
                let mut buffer = [0u8; 4096];
                let n = self.reader.read(&mut buffer)?;
                self.eof = n == 0;
                self.pending.extend_from_slice(&buffer[..n]);
            }
            if self.pending.is_empty() {
                if self.index == 0 {
                    self.index = 1;
                    return Ok(Some(BodySection {
                        index: 0,
                        text: String::new(),
                    }));
                }
                return Ok(None);
            }
            let end = self.pending.len().min(4096);
            let valid = match std::str::from_utf8(&self.pending[..end]) {
                Ok(s) => s,
                Err(e) if e.error_len().is_none() && (!self.eof || end < self.pending.len()) => {
                    std::str::from_utf8(&self.pending[..e.valid_up_to()])
                        .map_err(|_| Error::Utf8)?
                }
                Err(_) => return Err(Error::Utf8),
            };
            let boundary = if end < self.pending.len() || !self.eof {
                valid
                    .char_indices()
                    .filter(|(_, c)| c.is_whitespace())
                    .map(|(i, c)| i + c.len_utf8())
                    .last()
                    .filter(|n| *n >= 2048)
                    .unwrap_or(valid.len())
            } else {
                valid.len()
            };
            if boundary == 0 {
                return Err(Error::Utf8);
            }
            let text = String::from_utf8(self.pending.drain(..boundary).collect())
                .map_err(|_| Error::Utf8)?;
            let index = self.index;
            self.index = self.index.checked_add(1).ok_or(Error::Invalid)?;
            Ok(Some(BodySection { index, text }))
        })();
        match result {
            Ok(Some(section)) => Some(Ok(section)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("agentlaw-spool-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn failed_reader_does_not_leave_partial_ingress() {
        struct Fails(bool);
        impl Read for Fails {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("test failure"));
                }
                self.0 = true;
                out[0] = b'x';
                Ok(1)
            }
        }
        let path = directory();
        assert!(stage_body(&path, &mut Fails(false)).is_err());
        assert_eq!(
            std::fs::read_dir(path.join("source-ingress"))
                .unwrap()
                .count(),
            0
        );
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn complete_unicode_body_roundtrips_bounded_sections() {
        let path = directory();
        let text = "한글 and words\r\n".repeat(3000);
        let body = stage_body(&path, &mut text.as_bytes()).unwrap();
        let sections = Sections::open(&path, &body)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(sections.iter().all(|s| s.text.len() <= 4096));
        assert_eq!(
            sections.into_iter().map(|s| s.text).collect::<String>(),
            text
        );
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn tamper_and_path_escape_are_rejected() {
        let path = directory();
        let mut body = stage_body(&path, &mut "text".as_bytes()).unwrap();
        body.spool_id = "../../outside".into();
        assert!(open_body(&path, &body).is_err());
        let body = stage_body(&path, &mut "body".as_bytes()).unwrap();
        std::fs::write(
            path.join("source-ingress")
                .join(format!("{}.body", body.spool_id)),
            "edit",
        )
        .unwrap();
        assert!(open_body(&path, &body).is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
}
