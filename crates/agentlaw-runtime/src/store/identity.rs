//! Store identity construction from an installation snapshot.

use crate::{
    installation::registry::ConfigSnapshot,
    ports::{RuntimeError, RuntimeResult},
};
use agentlaw_flows::runtime::StoreIdentity;
use std::path::Path;

pub fn resolve(_installation: &Path, _snapshot: &ConfigSnapshot) -> RuntimeResult<StoreIdentity> {
    Err(RuntimeError::Unavailable(
        "store identity is not implemented".into(),
    ))
}
