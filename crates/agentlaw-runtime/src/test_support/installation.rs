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
        crate::host::prepare_test_selection(self.state(), &path, !path.join("format.md").exists())
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
mod tests;
