//! Retire older completed reviews only after their services and readers close.
use super::*;
use crate::git::imports::{ImportStatus, ProgressRegistry};
use std::{path::Path, time::Duration};

fn no_pending(stage: &crate::git::git_ops::ImportStageContext) -> bool {
    let db = stage.runtime_path.join("control.sqlite");
    if !db.exists() {
        return !stage.runtime_path.join("control-initialized.json").exists();
    }
    let Ok(conn) = agentlaw_sqlite::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    conn.query_row("SELECT EXISTS(SELECT 1 FROM pending WHERE status='pending') OR EXISTS(SELECT 1 FROM authoring WHERE used=0) OR EXISTS(SELECT 1 FROM authoring_executions WHERE status NOT IN ('published','discarded')) OR EXISTS(SELECT 1 FROM project_creations WHERE done=0)", [], |row| row.get::<_, bool>(0)).is_ok_and(|pending| !pending)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CleanupPlan {
    binding: String,
    #[serde(default)]
    active_binding: String,
    indices: Vec<String>,
}

fn artifacts_expired(stage: &crate::git::git_ops::ImportStageContext) -> bool {
    let dir = stage.runtime_path.join("response-artifacts");
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let now = std::time::SystemTime::now();
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("response-") || !(name.ends_with(".json") || name.ends_with(".tmp")) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            return false;
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            return false;
        }
        let Ok(modified) = meta.modified() else {
            return false;
        };
        if !now.duration_since(modified).is_ok_and(|age| {
            age >= Duration::from_secs(agentlaw_flows::artifacts::RETENTION_SECONDS)
        }) {
            return false;
        }
    }
    true
}

fn find_semantic_indices(root: &Path, binding: &str) -> std::io::Result<Vec<String>> {
    if let Ok(metadata) = std::fs::symlink_metadata(root) {
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("Index root is a link."));
        }
    }
    let directories = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut owned_directories = Vec::new();
    for entry in directories {
        let entry = entry?;
        let kind = entry.file_type()?;
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        let directory = entry.path();
        let mut owned = false;
        let mut other = false;
        for file in std::fs::read_dir(&directory)? {
            let file = file?;
            let kind = file.file_type()?;
            if kind.is_symlink() || kind.is_dir() {
                other = true;
                break;
            }
            let path = file.path();
            let name = file.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("vector") || path.extension().is_none_or(|ext| ext != "sqlite") {
                continue;
            }
            let db =
                agentlaw_sqlite::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(std::io::Error::other)?;
            let owner: String = db
                .query_row(
                    "SELECT binding FROM binding_vector_meta WHERE singleton=1",
                    [],
                    |row| row.get(0),
                )
                .map_err(std::io::Error::other)?;
            if owner == binding {
                owned = true;
            } else {
                other = true;
            }
        }
        if owned && !other {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| std::io::Error::other("Invalid index directory name."))?;
            if name.len() != 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(std::io::Error::other("Invalid index directory name."));
            }
            owned_directories.push(name);
        }
    }
    Ok(owned_directories)
}

pub(super) fn completed(
    stores: &Stores,
    snapshot: &Arc<ConfigSnapshot>,
    active: &crate::store::service::StoreService,
    progress: &ProgressRegistry,
    control: &RequestControl,
) {
    let local = snapshot
        .config
        .runtime_root(stores.installation())
        .join("git");
    let Ok(entries) = crate::git::imports::list(&local, progress) else {
        return;
    };
    let root = local.join("imports");
    let Ok(binding_root) =
        std::fs::canonicalize(snapshot.config.runtime_root(stores.installation()))
    else {
        return;
    };
    let Ok(canonical_root) = std::fs::canonicalize(&root) else {
        return;
    };
    if !canonical_root.starts_with(&binding_root) {
        return;
    }
    let mut complete: Vec<(std::time::SystemTime, String)> = entries
        .into_iter()
        .filter(|entry| entry.status == ImportStatus::Completed)
        .filter_map(|entry| {
            std::fs::metadata(root.join(&entry.import_ref).join("completed.json"))
                .ok()?
                .modified()
                .ok()
                .map(|modified| (modified, entry.import_ref))
        })
        .collect();
    complete.sort();
    complete.pop(); // Keep the newest completion receipt for idempotent replay.
    for (_, reference) in complete {
        if control.check().is_err() {
            return;
        }
        let Some(_lifetime) = progress.try_begin(&reference) else {
            continue;
        };
        let folder = root.join(&reference);
        let Ok(kind) = std::fs::symlink_metadata(&folder) else {
            continue;
        };
        if !kind.file_type().is_dir() || kind.file_type().is_symlink() {
            continue;
        }
        let Ok(canonical_folder) = std::fs::canonicalize(&folder) else {
            continue;
        };
        if canonical_folder.parent() != Some(canonical_root.as_path()) {
            continue;
        }
        let plan_file = folder.join("cleanup-owned.json");
        if !plan_file.exists()
            && std::fs::read_dir(&folder).ok().is_some_and(|entries| {
                let names: Vec<_> = entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
                    .collect();
                names.len() == 1 && names[0] == "completed.json"
            })
        {
            if std::fs::remove_file(folder.join("completed.json")).is_ok() {
                let _ = std::fs::remove_dir(&folder);
            }
            continue;
        }
        let indices = stores.installation().join("worker/indices");
        let plan: CleanupPlan = if plan_file.exists() {
            let Ok(bytes) = std::fs::read(&plan_file) else {
                continue;
            };
            let Ok(plan) = serde_json::from_slice::<CleanupPlan>(&bytes) else {
                continue;
            };
            let stage = crate::git::git_ops::ImportStageContext {
                store_path: folder.join("source"),
                runtime_path: folder.join("runtime"),
            };
            let active_identity = crate::store::identity::resolve(stores.installation(), snapshot);
            if !active_identity.is_ok_and(|identity| identity.binding_id == plan.active_binding) {
                continue;
            }
            if stage.runtime_path.join("control.sqlite").exists()
                || stage.runtime_path.join("control-initialized.json").exists()
            {
                let Ok(identity) = crate::store::identity::resolve_import_stage(
                    stores.installation(),
                    snapshot,
                    &stage,
                ) else {
                    continue;
                };
                if identity.binding_id != plan.binding {
                    continue;
                }
            }
            if !artifacts_expired(&stage) {
                continue;
            }
            plan
        } else {
            let Ok(stage) = active.import_stage(&reference, true) else {
                continue;
            };
            let Ok(expected_root) = std::fs::canonicalize(folder.join("source")) else {
                continue;
            };
            let Ok(actual_root) = std::fs::canonicalize(&stage.store_path) else {
                continue;
            };
            let Ok(expected_local) = std::fs::canonicalize(folder.join("runtime")) else {
                continue;
            };
            let Ok(actual_local) = std::fs::canonicalize(&stage.runtime_path) else {
                continue;
            };
            if expected_root != actual_root
                || expected_local != actual_local
                || !actual_root.starts_with(&canonical_folder)
                || !actual_local.starts_with(&canonical_folder)
            {
                continue;
            }
            if stores.close_stage(snapshot, &stage, control).is_err() || !no_pending(&stage) {
                continue;
            }
            if !artifacts_expired(&stage) {
                continue;
            }
            let Ok(identity) = crate::store::identity::resolve_import_stage(
                stores.installation(),
                snapshot,
                &stage,
            ) else {
                continue;
            };
            let Ok(owned) = find_semantic_indices(&indices, &identity.binding_id) else {
                continue;
            };
            let plan = CleanupPlan {
                binding: identity.binding_id,
                active_binding: match crate::store::identity::resolve(
                    stores.installation(),
                    snapshot,
                ) {
                    Ok(identity) => identity.binding_id,
                    Err(_) => continue,
                },
                indices: owned,
            };
            let Ok(mut temporary) = tempfile::NamedTempFile::new_in(&folder) else {
                continue;
            };
            if serde_json::to_writer(&mut temporary, &plan).is_err()
                || temporary.as_file().sync_all().is_err()
                || temporary.persist_noclobber(&plan_file).is_err()
            {
                continue;
            }
            plan
        };
        let mut index_failure = false;
        if indices.exists() {
            let safe = std::fs::symlink_metadata(&indices)
                .ok()
                .is_some_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
                && std::fs::canonicalize(&indices)
                    .ok()
                    .zip(std::fs::canonicalize(stores.installation()).ok())
                    .is_some_and(|(indices, state)| indices.starts_with(state));
            if !safe {
                continue;
            }
        }
        for name in &plan.indices {
            if name.len() != 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                index_failure = true;
                break;
            }
            let dir = indices.join(name);
            match std::fs::symlink_metadata(&dir) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                    if std::fs::remove_dir_all(dir).is_err() {
                        index_failure = true;
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => {
                    index_failure = true;
                    break;
                }
            }
        }
        if index_failure {
            continue;
        }
        let Ok(children) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut success = true;
        for child in children {
            let Ok(child) = child else {
                success = false;
                break;
            };
            if child.file_name() == "completed.json" || child.file_name() == "cleanup-owned.json" {
                continue;
            }
            let result = match child.file_type() {
                Ok(kind) if kind.is_dir() && !kind.is_symlink() => {
                    std::fs::remove_dir_all(child.path())
                }
                Ok(_) => std::fs::remove_file(child.path()),
                Err(error) => Err(error),
            };
            if result.is_err() {
                success = false;
                break;
            }
        }
        if success
            && std::fs::remove_file(&plan_file).is_ok()
            && std::fs::remove_file(folder.join("completed.json")).is_ok()
        {
            let _ = std::fs::remove_dir(folder);
        }
    }
}
