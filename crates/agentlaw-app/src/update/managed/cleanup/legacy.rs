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

const STATE_FILES: &[&str] = &["update-check.json", "update-check.lock"];
const WORKER_FILES: &[&str] = &[
    "daemon.lock",
    "launch.lock",
    "endpoint.json",
    "source-ingress",
];
const BINDING_DATABASES: &[&str] = &[
    "bootstrap.sqlite",
    "background-status.sqlite",
    "exact-maintenance.sqlite",
];
const BINDING_DIRECTORIES: &[&str] = &["canonical/acquisitions", "canonical/history-cache"];

fn component(name: &str) -> bool {
    name.starts_with("component-") && name.ends_with(".json")
}
fn sqlite_base(name: &str) -> &str {
    name.strip_suffix("-wal")
        .or_else(|| name.strip_suffix("-shm"))
        .unwrap_or(name)
}

fn add(targets: &mut BTreeSet<PathBuf>, path: PathBuf) -> Result<()> {
    match exists_exact(&path) {
        Ok(true) => {
            targets.insert(path);
        }
        Err(problem) if problem.code == "cleanup_deferred" => {
            targets.insert(path);
        }
        Err(problem) => return Err(problem),
        Ok(false) => {}
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

fn bindings(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let state = root.join("state");
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
    Ok(bindings)
}

pub(super) fn inventory(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let state = root.join("state");
    let mut targets = BTreeSet::new();
    if !ordinary_directory(&state)? {
        return Ok(targets);
    }
    for name in STATE_FILES {
        add(&mut targets, state.join(name))?;
    }
    let worker = state.join("worker");
    if ordinary_directory(&worker)? {
        sqlite(&mut targets, worker.join("broker.sqlite"))?;
        for name in WORKER_FILES {
            add(&mut targets, worker.join(name))?;
        }
        for path in children(&worker)? {
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(component)
            {
                add(&mut targets, path)?;
            }
        }
        let indices = worker.join("indices");
        if ordinary_directory(&indices)? {
            let context = IndexContext::read(root);
            for index in children(&indices)? {
                if directory_child(&index)? {
                    if context
                        .as_ref()
                        .is_some_and(|context| context.obsolete(&index))
                    {
                        targets.insert(index);
                        continue;
                    }
                    add(&mut targets, index.join("writer.lock"))?;
                    sqlite(&mut targets, index.join("lexical.sqlite"))?;
                    add(&mut targets, index.join("generations"))?;
                }
            }
        }
    }
    for binding in bindings(root)? {
        if !ordinary_directory(&binding)? {
            continue;
        }
        for name in BINDING_DATABASES {
            sqlite(&mut targets, binding.join(name))?;
        }
        if ordinary_directory(&binding.join("canonical"))? {
            for name in BINDING_DIRECTORIES {
                add(&mut targets, binding.join(name))?;
            }
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

/// A live model lock, or a directory containing it, is never a cleanup target,
/// including when replaying a plan written by an older release.
pub(super) fn protected(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let parts: Vec<_> = relative
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let live = ["state", "worker", "model.lock"];
    parts
        .iter()
        .zip(live)
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

pub(super) fn index_directory(root: &Path, path: &Path) -> bool {
    path.parent() == Some(root.join("state/worker/indices").as_path())
        && path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| {
                name.len() == 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
}

/// Revalidate only the recorded path. Startup never enumerates new cleanup targets.
pub(super) fn eligible(root: &Path, path: &Path) -> Result<bool> {
    if protected(root, path) {
        return Ok(false);
    }
    let state = root.join("state");
    let Ok(relative) = path.strip_prefix(&state) else {
        return Ok(false);
    };
    let Some(name) = relative.to_str() else {
        return Ok(false);
    };
    let name = name.replace('\\', "/");
    if STATE_FILES.contains(&name.as_str())
        || sqlite_base(&name) == "artifacts/response-registry.sqlite"
    {
        return Ok(true);
    }
    if let Some(worker) = name.strip_prefix("worker/") {
        if WORKER_FILES.contains(&worker)
            || sqlite_base(worker) == "broker.sqlite"
            || (!worker.contains('/') && component(worker))
        {
            return Ok(true);
        }
        if let Some(index) = worker.strip_prefix("indices/") {
            if let Some((_, child)) = index.split_once('/') {
                return Ok(child == "writer.lock"
                    || child == "generations"
                    || sqlite_base(child) == "lexical.sqlite");
            }
            return Ok(IndexContext::read(root).is_some_and(|context| context.obsolete(path)));
        }
    }
    let local = if let Some(local) = name.strip_prefix("runtime/") {
        local
    } else if let Some(bindings) = name.strip_prefix("bindings/") {
        let Some((_, local)) = bindings.split_once('/') else {
            return Ok(false);
        };
        local
    } else {
        return Ok(false);
    };
    if BINDING_DATABASES.contains(&sqlite_base(local))
        || BINDING_DIRECTORIES.contains(&local)
        || sqlite_base(local) == "response-artifacts/response-registry.sqlite"
    {
        return Ok(true);
    }
    Ok(local
        .strip_prefix("history-search/")
        .is_some_and(|name| !name.contains('/') && sqlite_base(name).ends_with("-jobs.sqlite")))
}

struct IndexContext {
    bindings: BTreeSet<String>,
    epochs: Vec<(String, String)>,
    model: Option<String>,
    config: agentlaw_runtime::semantic::SemanticConfig,
}
impl IndexContext {
    fn read(root: &Path) -> Option<Self> {
        let mut pending = bindings(root).ok()?;
        let mut seen = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut epochs = Vec::new();
        while let Some(local) = pending.pop_first() {
            if !seen.insert(local.clone()) || !ordinary_directory(&local).ok()? {
                continue;
            }
            let database = local.join("control.sqlite");
            let marker = local.join("control-initialized.json");
            if exists_exact(&database).ok()? || exists_exact(&marker).ok()? {
                let reader = agentlaw_flows::control::ControlReader::open(&local).ok()?;
                let id = reader.binding_id().ok()?;
                let marker = agentlaw_contracts::validation::decode_unique(
                    &fs::read_to_string(marker).ok()?,
                )
                .ok()?;
                if marker["binding_id"] != id || uuid::Uuid::parse_str(&id).is_err() {
                    return None;
                }
                if let Ok(epoch) =
                    agentlaw_storage::Store::read_source_epoch(local.join("canonical"))
                {
                    epochs.push((id.clone(), epoch));
                }
                ids.insert(id);
            }
            let imports = local.join("git/imports");
            if ordinary_directory(&imports).ok()? {
                for import in children(&imports).ok()? {
                    if directory_child(&import).ok()?
                        && ordinary_directory(&import.join("runtime")).ok()?
                    {
                        pending.insert(import.join("runtime"));
                    }
                }
            }
        }
        let state = root.join("state");
        let model = crate::installed::model_assets(&state)
            .ok()
            .flatten()
            .and_then(|assets| assets.model_digest().ok());
        Some(Self {
            bindings: ids,
            epochs,
            model,
            config: agentlaw_runtime::semantic::SemanticConfig::new(
                state.join("worker/indices"),
                agentlaw_runtime::semantic::INDEX_CONFIG_VERSION.into(),
            ),
        })
    }
    fn obsolete(&self, path: &Path) -> bool {
        self.obsolete_after_removals(path, &BTreeSet::new())
    }
    fn obsolete_after_removals(&self, path: &Path, removed: &BTreeSet<PathBuf>) -> bool {
        use agentlaw_runtime::semantic::identity;
        let Some(owned) = identity::inspect_after_removals(&self.config, path, removed) else {
            return identity::inspect_daemon_directory(&self.config, path, &self.epochs);
        };
        if !self.bindings.contains(&owned.identity.binding) {
            return true;
        }
        let mut expected = owned.identity.clone();
        expected.model_digest = self
            .model
            .clone()
            .unwrap_or_else(|| expected.model_digest.clone());
        expected.config = self.config.config_id.clone();
        expected.dimension = self.config.dimension;
        expected.generation = "initial".into();
        let current = identity::path(&self.config, &expected);
        if current.parent() == Some(path) {
            return false;
        }
        // A still-retained binding loses an old model/configuration directory only
        // after its replacement has a verified completed generation.
        identity::inspect_directory(&self.config, current.parent().unwrap())
            .is_some_and(|replacement| replacement.complete)
    }
}

pub(super) fn eligible_index_after_removals(
    root: &Path,
    path: &Path,
    removed: &BTreeSet<PathBuf>,
) -> bool {
    index_directory(root, path)
        && IndexContext::read(root)
            .is_some_and(|context| context.obsolete_after_removals(path, removed))
}
