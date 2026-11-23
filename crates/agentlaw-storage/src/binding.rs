use std::{fs, path::Path};

use crate::Result;

/// Normalized identity of one store and its local state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingKey {
    store: String,
    local: String,
}

impl BindingKey {
    pub fn store_identity(&self) -> &str {
        &self.store
    }

    pub fn local_identity(&self) -> &str {
        &self.local
    }
}

pub fn binding_identity(root: &Path, local: &Path) -> Result<BindingKey> {
    Ok(BindingKey {
        store: path_identity(&fs::canonicalize(root)?),
        local: path_identity(&fs::canonicalize(local)?),
    })
}

pub(crate) fn path_identity(path: &Path) -> String {
    let identity = path.to_string_lossy().to_string();
    #[cfg(windows)]
    let identity = identity.to_lowercase();
    identity
}

#[cfg(test)]
mod tests;
