//! Semantic directory identities shared by writers and managed cleanup.
use super::{generation, SemanticConfig};
use agentlaw_search::vector_file::{VectorFileIdentity, VectorFileKind, VectorFileReadView};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// The initial vector file path; repair generations use the same directory key.
pub fn path(config: &SemanticConfig, identity: &VectorFileIdentity) -> PathBuf {
    let mut hash = Sha256::new();
    let (kind, subject) = match &identity.kind {
        VectorFileKind::Current => ("current", None),
        VectorFileKind::History { subject } => ("history", Some(subject.as_str())),
    };
    for field in [identity.binding.as_str(), kind]
        .into_iter()
        .chain(subject)
        .chain([
            identity.epoch.as_str(),
            identity.model_digest.as_str(),
            identity.config.as_str(),
        ])
    {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hash.update((identity.dimension as u64).to_be_bytes());
    if identity.config != super::INDEX_CONFIG_VERSION {
        // The previous layout included its fixed byte chunk size in its directory key.
        hash.update(4096_u64.to_be_bytes());
    }
    config
        .indices_root
        .join(format!("{:x}", hash.finalize()))
        .join("vector.sqlite")
}

/// Retire a recognized previous layout after the replacement has completed.
pub(super) fn reclaim_previous(config: &SemanticConfig, replacement: &VectorFileIdentity) {
    if replacement.config != super::INDEX_CONFIG_VERSION {
        return;
    }
    let mut previous = replacement.clone();
    previous.config = "v1".into();
    let candidate = path(config, &previous);
    let Some(directory) = candidate.parent() else {
        return;
    };
    let Some(owned) = inspect_directory(config, directory) else {
        return;
    };
    previous.generation = owned.identity.generation.clone();
    if owned.identity != previous {
        return;
    }
    // Ownership inspection rejects unknown files, including symlinks and reparse points.
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if generation::file_id(&entry.file_name().to_string_lossy()).is_some()
            && !matches!(
                generation::remove_files(&entry.path()),
                generation::Removal::Removed
            )
        {
            return;
        }
    }
    let _ = std::fs::remove_file(directory.join("active-generation"));
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            if generation::temporary_id(&entry.file_name().to_string_lossy()).is_some() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let _ = std::fs::remove_dir(directory);
}

pub struct OwnedDirectory {
    pub identity: VectorFileIdentity,
    pub complete: bool,
}

/// Recognize only an ordinary directory whose complete contents and vector metadata
/// agree with the writer's directory key. Unknown or unreadable contents are retained.
pub fn inspect_directory(config: &SemanticConfig, directory: &Path) -> Option<OwnedDirectory> {
    inspect_after_removals(config, directory, &BTreeSet::new())
}

fn ordinary_files(directory: &Path, daemon_layout: bool) -> Option<BTreeSet<String>> {
    let metadata = std::fs::symlink_metadata(directory).ok()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return None;
    }
    #[cfg(windows)]
    if std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 0x400 != 0 {
        return None;
    }
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(directory).ok()? {
        let entry = entry.ok()?;
        let metadata = entry.metadata().ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        let owned_directory = daemon_layout && name == "generations" && metadata.is_dir();
        if (!metadata.is_file() && !owned_directory) || entry.file_type().ok()?.is_symlink() {
            return None;
        }
        #[cfg(windows)]
        if std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 0x400 != 0 {
            return None;
        }
        names.insert(name.to_owned());
    }
    Some(names)
}

/// Reconcile orphan sidecars only with database removals recorded by the pinned plan.
/// Surviving databases must still prove their original identity and directory key.
pub fn inspect_after_removals(
    config: &SemanticConfig,
    directory: &Path,
    removed: &BTreeSet<PathBuf>,
) -> Option<OwnedDirectory> {
    let names = ordinary_files(directory, false)?;
    let mut files = BTreeMap::new();
    for name in &names {
        if removed.contains(Path::new(name)) {
            return None;
        }
        if name == "active-generation" {
            continue;
        }
        if generation::temporary_id(name).is_some() {
            if !generation::valid_id(&std::fs::read_to_string(directory.join(name)).ok()?) {
                return None;
            }
            continue;
        }
        let base = name
            .strip_suffix("-wal")
            .or_else(|| name.strip_suffix("-shm"))
            .unwrap_or(name);
        let id = generation::file_id(base)?;
        if name != base && !names.contains(base) {
            if removed.contains(Path::new(base)) {
                continue;
            }
            return None;
        }
        files.insert(base.to_owned(), id.to_owned());
    }
    let selected = generation::selected(directory).ok()?;
    let mut common: Option<VectorFileIdentity> = None;
    let mut active = None;
    for (name, id) in files {
        let file = directory.join(name);
        let identity = VectorFileIdentity::read(&file).ok()?;
        if identity.generation != id
            || identity.binding.is_empty()
            || identity.epoch.is_empty()
            || identity.model_digest.is_empty()
            || identity.config.is_empty()
            || matches!(&identity.kind, VectorFileKind::History { subject } if uuid::Uuid::parse_str(subject).is_err())
            || path(config, &identity).parent()? != directory
        {
            return None;
        }
        if let Some(common) = &common {
            let mut comparable = identity.clone();
            comparable.generation = common.generation.clone();
            if comparable != *common {
                return None;
            }
        } else {
            common = Some(identity.clone());
        }
        let view = VectorFileReadView::open(&file, &identity).ok()?;
        if id == selected {
            active = Some(OwnedDirectory {
                identity,
                complete: view.progress().complete && view.progress().ack.is_some(),
            });
        }
    }
    active.or_else(|| {
        common.map(|identity| OwnedDirectory {
            identity,
            complete: false,
        })
    })
}

/// Released daemon directories used SHA-256 of this JSON tuple. Their vector
/// metadata omits binding/epoch, so only authoritative pairs can prove ownership.
pub fn inspect_daemon_directory(
    config: &SemanticConfig,
    directory: &Path,
    authorities: &[(String, String)],
) -> bool {
    let Some(names) = ordinary_files(directory, true) else {
        return false;
    };
    if !names.contains("vector.sqlite")
        || names.iter().any(|name| {
            !matches!(
                name.as_str(),
                "vector.sqlite"
                    | "vector.sqlite-wal"
                    | "vector.sqlite-shm"
                    | "writer.lock"
                    | "vector.lock"
                    | "lexical.lock"
                    | "lexical.sqlite"
                    | "lexical.sqlite-wal"
                    | "lexical.sqlite-shm"
                    | "generations"
            )
        })
    {
        return false;
    }
    let Ok((model, index_config, _)) =
        agentlaw_search::ExactVectorIndex::stored_identity(directory.join("vector.sqlite"))
    else {
        return false;
    };
    authorities.iter().any(|(binding, epoch)| {
        let Ok(tuple) = serde_json::to_vec(&(
            binding.as_str(),
            model.as_str(),
            index_config.as_str(),
            epoch.as_str(),
        )) else {
            return false;
        };
        let expected = config
            .indices_root
            .join(format!("{:x}", Sha256::digest(tuple)));
        expected == directory
    })
}
