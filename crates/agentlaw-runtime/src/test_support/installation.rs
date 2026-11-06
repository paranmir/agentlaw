use agentlaw_contracts::DomainError;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

fn valid_store_name(name: &str) -> bool {
    matches!(
        (
            Path::new(name).components().next(),
            Path::new(name).components().nth(1)
        ),
        (Some(Component::Normal(_)), None)
    ) && !name.contains(['/', '\\'])
}

pub struct TestInstallation {
    root: tempfile::TempDir,
    state: PathBuf,
}
impl TestInstallation {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary installation");
        let state = root.path().join("state");
        fs::create_dir_all(&state).expect("create state");
        Self { root, state }
    }
    pub fn state(&self) -> &Path {
        &self.state
    }
    pub fn store_path(&self, name: &str) -> PathBuf {
        assert!(
            valid_store_name(name),
            "Store name must be a single path component."
        );
        self.root.path().join("stores").join(name)
    }
    pub fn select_store(&self, name: &str) -> agentlaw_contracts::Result<serde_json::Value> {
        if !valid_store_name(name) {
            return Err(DomainError::new(
                "invalid_store_location",
                "Store name must be a single path component.",
            ));
        }
        let path = self.store_path(name);
        crate::installation::setup::connect(
            self.state(),
            &path,
            !path.join("format.md").exists(),
            |state| {
                Ok(agentlaw_worker::RuntimeConfig {
                    state_dir: state.into(),
                    executable: std::env::current_exe()
                        .map_err(|e| DomainError::new("configuration_io", e.to_string()))?,
                    model: None,
                })
            },
        )
    }
    pub fn config(
        &self,
    ) -> agentlaw_contracts::Result<Option<crate::installation::config::Config>> {
        crate::installation::config::load(self.state())
    }
    pub fn binding_key(&self) -> agentlaw_contracts::Result<agentlaw_storage::BindingKey> {
        let config = self
            .config()?
            .ok_or_else(|| DomainError::new("connection_required", "No selected store."))?;
        let local = config.runtime_root(self.state()).join("canonical");
        agentlaw_storage::binding_identity(&config.memory_store_path, &local)
            .map_err(|e| DomainError::new("store_unavailable", e.to_string()))
    }
    pub fn binding_ref(&self) -> agentlaw_contracts::Result<crate::protocol::BindingRef> {
        let config = self
            .config()?
            .ok_or_else(|| DomainError::new("connection_required", "No selected store."))?;
        Ok(crate::protocol::BindingRef::new(
            &self.binding_key()?,
            &config.user_id,
        ))
    }
}
impl Default for TestInstallation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_names_must_be_single_components() {
        let fixture = TestInstallation::new();
        let absolute = fixture
            .root
            .path()
            .join("absolute")
            .to_string_lossy()
            .into_owned();
        for name in [absolute.as_str(), "a/b", "..", "."] {
            assert!(std::panic::catch_unwind(|| fixture.store_path(name)).is_err());
            assert!(fixture.select_store(name).is_err());
        }
        assert!(fixture.store_path("a").starts_with(fixture.root.path()));
    }

    #[test]
    fn installation_selection_is_isolated_and_stable() {
        let fixture = TestInstallation::new();
        assert!(fixture.config().unwrap().is_none());
        fixture.select_store("a").unwrap();
        assert!(fixture.store_path("a").join("format.md").exists());
        assert_eq!(
            fixture.config().unwrap().unwrap().memory_store_path,
            fs::canonicalize(fixture.store_path("a")).unwrap()
        );
        let a = fixture.binding_ref().unwrap();
        assert_eq!(a, fixture.binding_ref().unwrap());
        fixture.select_store("b").unwrap();
        assert_ne!(a, fixture.binding_ref().unwrap());
        assert!(fixture
            .config()
            .unwrap()
            .unwrap()
            .memory_store_path
            .starts_with(fs::canonicalize(fixture.root.path()).unwrap()));
        fixture.select_store("a").unwrap();
        assert_eq!(a, fixture.binding_ref().unwrap());
        assert!(fixture.select_store("../escape").is_err());
    }
}
