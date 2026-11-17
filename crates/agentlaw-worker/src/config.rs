//! Configuration for the runtime-owned model child.
use crate::ModelAssets;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub state_dir: PathBuf,
    pub executable: PathBuf,
    pub model: Option<ModelAssets>,
}
