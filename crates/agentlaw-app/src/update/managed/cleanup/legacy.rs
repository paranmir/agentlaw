//! Exact legacy paths from the managed-install cleanup inventory.
use super::*;

pub(super) fn children(path: &Path) -> Result<Vec<PathBuf>> {
    if !ordinary_directory(path)? {
        return Ok(Vec::new());
    }
    match fs::read_dir(path) {
        Ok(entries) => entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|_| error("cleanup_unknown", "Cannot enumerate a cleanup directory."))
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(error(
            "cleanup_unknown",
            "Cannot inspect a cleanup directory.",
        )),
    }
}

pub(super) fn ordinary_directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(windows)]
            if std::os::windows::fs::MetadataExt::file_attributes(&meta) & 0x400 != 0 {
                return Err(error(
                    "cleanup_scope",
                    "A cleanup path contains a reparse point.",
                ));
            }
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(error(
                    "cleanup_scope",
                    "A cleanup path is not an ordinary directory.",
                ));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(error(
            "cleanup_unknown",
            "Cannot inspect a cleanup directory.",
        )),
    }
}

fn add(targets: &mut BTreeSet<PathBuf>, path: PathBuf) -> Result<()> {
    if exists_exact(&path)? {
        targets.insert(path);
    }
    Ok(())
}

fn sqlite(targets: &mut BTreeSet<PathBuf>, path: PathBuf) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        add(targets, PathBuf::from(name))?;
    }
    Ok(())
}

fn directory_child(path: &Path) -> Result<bool> {
    let meta = fs::symlink_metadata(path)
        .map_err(|_| error("cleanup_unknown", "Cannot inspect an inventory entry."))?;
    if meta.file_type().is_symlink() {
        return Err(error("cleanup_scope", "An inventory entry is a link."));
    }
    #[cfg(windows)]
    if std::os::windows::fs::MetadataExt::file_attributes(&meta) & 0x400 != 0 {
        return Err(error(
            "cleanup_scope",
            "An inventory entry is a reparse point.",
        ));
    }
    Ok(meta.is_dir())
}

pub(super) fn inventory(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let state = root.join("state");
    let mut targets = BTreeSet::new();
    if !ordinary_directory(&state)? {
        return Ok(targets);
    }
    for name in ["update-check.json", "update-check.lock"] {
        add(&mut targets, state.join(name))?;
    }
    let worker = state.join("worker");
    if ordinary_directory(&worker)? {
        sqlite(&mut targets, worker.join("broker.sqlite"))?;
        for name in [
            "daemon.lock",
            "launch.lock",
            "model.lock",
            "endpoint.json",
            "source-ingress",
        ] {
            add(&mut targets, worker.join(name))?;
        }
        for path in children(&worker)? {
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("component-") && s.ends_with(".json"))
            {
                add(&mut targets, path)?;
            }
        }
        let indices = worker.join("indices");
        if ordinary_directory(&indices)? {
            for index in children(&indices)? {
                if directory_child(&index)? {
                    add(&mut targets, index.join("writer.lock"))?;
                    sqlite(&mut targets, index.join("lexical.sqlite"))?;
                    add(&mut targets, index.join("generations"))?;
                }
            }
        }
    }
    let mut bindings = BTreeSet::from([state.join("runtime")]);
    let binding_dir = state.join("bindings");
    if ordinary_directory(&binding_dir)? {
        for binding in children(&binding_dir)? {
            if directory_child(&binding)? {
                bindings.insert(binding);
            }
        }
    }
    match fs::read_to_string(state.join("config.json")) {
        Ok(raw) => {
            let value = agentlaw_contracts::validation::decode_unique(&raw)?;
            let selected: config::Config = serde_json::from_value(value).map_err(|_| {
                error(
                    "cleanup_unknown",
                    "The selected binding configuration is invalid.",
                )
            })?;
            if let Some(path) = selected.runtime_path {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if !path.is_absolute()
                    || name.len() != 64
                    || !name.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(error(
                        "cleanup_unknown",
                        "The selected binding path is invalid.",
                    ));
                }
                match fs::canonicalize(&path) {
                    Ok(path) => {
                        if path.parent() != Some(binding_dir.as_path()) {
                            return Err(error(
                                "cleanup_unknown",
                                "The selected binding is outside this installation.",
                            ));
                        }
                        bindings.insert(path);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(error(
                            "cleanup_unknown",
                            "Cannot inspect the selected binding.",
                        ))
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(error(
                "cleanup_unknown",
                "Cannot read the binding configuration.",
            ))
        }
    }
    for binding in bindings {
        if !ordinary_directory(&binding)? {
            continue;
        }
        for name in [
            "bootstrap.sqlite",
            "background-status.sqlite",
            "exact-maintenance.sqlite",
        ] {
            sqlite(&mut targets, binding.join(name))?;
        }
        let canonical = binding.join("canonical");
        if ordinary_directory(&canonical)? {
            add(&mut targets, canonical.join("acquisitions"))?;
            add(&mut targets, canonical.join("history-cache"))?;
        }
        let history = binding.join("history-search");
        if ordinary_directory(&history)? {
            for path in children(&history)? {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let base = name
                    .strip_suffix("-wal")
                    .or_else(|| name.strip_suffix("-shm"))
                    .unwrap_or(name);
                if base.ends_with("-jobs.sqlite") {
                    sqlite(&mut targets, history.join(base))?;
                }
            }
        }
        let artifacts = binding.join("response-artifacts");
        if ordinary_directory(&artifacts)? {
            sqlite(&mut targets, artifacts.join("response-registry.sqlite"))?;
        }
    }
    let artifacts = state.join("artifacts");
    if ordinary_directory(&artifacts)? {
        sqlite(&mut targets, artifacts.join("response-registry.sqlite"))?;
    }
    Ok(targets)
}
