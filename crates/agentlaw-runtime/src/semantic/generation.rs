//! Durable current-vector generation selection and retired file reclamation.
use agentlaw_storage::persistence::{Persistence, PlatformPersistence};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) enum Publication {
    Replaced(agentlaw_storage::Durability),
    ReplacedButUnsynced(String),
}

#[derive(Debug)]
pub(super) enum Removal {
    Removed,
    InUse,
    Failed { path: PathBuf, reason: String },
}

pub(super) fn sync(dir: &Path) -> Result<agentlaw_storage::Durability, String> {
    PlatformPersistence
        .sync_namespace(dir)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
pub(super) fn crash_point(point: &str) {
    if std::env::var("AGENTLAW_SEMANTIC_CRASH_POINT")
        .ok()
        .as_deref()
        == Some(point)
    {
        let marker = std::env::var_os("AGENTLAW_SEMANTIC_CRASH_MARKER").unwrap();
        std::fs::write(marker, point).unwrap();
        if let Some(resume) = std::env::var_os("AGENTLAW_SEMANTIC_RESUME_MARKER") {
            while !std::path::Path::new(&resume).exists() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            return;
        }
        loop {
            std::thread::park();
        }
    }
}

pub(super) fn valid_id(id: &str) -> bool {
    id == "initial" || uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id)
}

pub(super) fn file_id(name: &str) -> Option<&str> {
    if name == "vector.sqlite" {
        return Some("initial");
    }
    name.strip_prefix("vector-")
        .and_then(|s| s.strip_suffix(".sqlite"))
        .filter(|id| *id != "initial" && valid_id(id))
}

pub(super) fn temporary_id(name: &str) -> Option<&str> {
    name.strip_prefix(".generation-")
        .and_then(|s| s.strip_suffix(".tmp"))
        .filter(|id| *id != "initial" && valid_id(id))
}

pub(super) fn file(dir: &Path, id: &str) -> PathBuf {
    if id == "initial" {
        dir.join("vector.sqlite")
    } else {
        dir.join(format!("vector-{id}.sqlite"))
    }
}

pub(super) fn selected(dir: &Path) -> Result<String, String> {
    match std::fs::read_to_string(dir.join("active-generation")) {
        Ok(id) if valid_id(&id) => Ok(id),
        Ok(_) => Err("invalid semantic generation pointer".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("initial".into()),
        Err(e) => Err(e.to_string()),
    }
}

pub(super) fn publish(dir: &Path, id: &str) -> Result<Publication, String> {
    let temp = dir.join(format!(".generation-{}.tmp", uuid::Uuid::new_v4()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let written = f.write_all(id.as_bytes());
    #[cfg(test)]
    crash_point("before_flush");
    if let Err(e) = written.and_then(|_| f.sync_all()) {
        drop(f);
        return Err(temp_failure(&temp, e.to_string()));
    }
    #[cfg(test)]
    crash_point("after_flush");
    drop(f);
    #[cfg(test)]
    crash_point("before_rename");
    if let Err(e) = PlatformPersistence.install_replace(&temp, &dir.join("active-generation")) {
        return Err(temp_failure(&temp, e.to_string()));
    }
    #[cfg(test)]
    crash_point("after_rename");
    Ok(match PlatformPersistence.sync_namespace(dir) {
        Ok(level) => Publication::Replaced(level),
        Err(e) => Publication::ReplacedButUnsynced(e.to_string()),
    })
}

fn temp_failure(path: &Path, reason: String) -> String {
    match remove_one(path) {
        Removal::Removed => reason,
        Removal::InUse => format!("{reason}; temporary file {} is in use", path.display()),
        Removal::Failed {
            path,
            reason: cleanup,
        } => format!("{reason}; cleanup {}: {cleanup}", path.display()),
    }
}

fn remove_one(path: &Path) -> Removal {
    match std::fs::remove_file(path) {
        Ok(()) => Removal::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Removal::Removed,
        #[cfg(windows)]
        Err(e) if matches!(e.raw_os_error(), Some(32 | 33)) => Removal::InUse,
        Err(e) => Removal::Failed {
            path: path.to_path_buf(),
            reason: e.to_string(),
        },
    }
}

pub(super) fn remove_files(path: &Path) -> Removal {
    match remove_one(path) {
        Removal::Removed => {}
        other => return other,
    }
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        match remove_one(&PathBuf::from(sidecar)) {
            Removal::Removed => {}
            other => return other,
        }
    }
    Removal::Removed
}

pub(super) fn cleanup(dir: &Path, active: &str, protected: &[PathBuf]) -> Result<bool, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e.to_string()),
    };
    let mut candidates = std::collections::BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if temporary_id(name).is_some() {
            candidates.insert(entry.path());
            continue;
        }
        let base = name
            .strip_suffix("-wal")
            .or_else(|| name.strip_suffix("-shm"))
            .unwrap_or(name);
        let id = file_id(base);
        if id.is_some_and(|id| id != active) {
            candidates.insert(dir.join(base));
        }
    }
    let mut complete = true;
    for path in candidates {
        if protected.contains(&path) {
            continue;
        }
        match remove_files(&path) {
            Removal::Removed => {}
            Removal::InUse => complete = false,
            Removal::Failed { path, reason } => {
                return Err(format!("cleanup {}: {reason}", path.display()))
            }
        }
    }
    Ok(complete)
}
