//! Model child execution and supervision for the Agentlaw runtime.
mod config;
pub mod execution;
mod model_child;
pub mod onnx;
pub mod supervisor;
pub(crate) mod util;
pub use config::RuntimeConfig;
pub use model_child::run_model_child;
pub use onnx::{ModelAssets, OnnxProvider};
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub const MODEL_ID: &str = "ibm-granite/granite-embedding-311m-multilingual-r2";
pub const DIMENSIONS: usize = 256;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkerState {
    Stopped,
    Starting,
    Loading,
    Ready,
    Failed,
    Stopping,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SemanticAvailability {
    Ready,
    Loading,
    Unavailable,
    Failed,
}
#[derive(Clone, Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmbeddingError {
    #[error("embedding model loading")]
    Loading,
    #[error("embedding inference unavailable: {0}")]
    Unavailable(String),
    #[error("embedding inference failed: {0}")]
    Failed(String),
    #[error("retryable embedding inference failure: {0}")]
    Retryable(String),
    #[error("request cancelled")]
    Cancelled,
}
pub trait EmbeddingProvider: Send + Sync {
    fn model_digest(&self) -> &str;
    fn availability(&self) -> SemanticAvailability;
    fn embed(&self, input: &str) -> std::result::Result<Vec<f32>, EmbeddingError>;
}
pub struct UnavailableProvider;
impl EmbeddingProvider for UnavailableProvider {
    fn model_digest(&self) -> &str {
        MODEL_ID
    }
    fn availability(&self) -> SemanticAvailability {
        SemanticAvailability::Unavailable
    }
    fn embed(&self, _: &str) -> std::result::Result<Vec<f32>, EmbeddingError> {
        Err(EmbeddingError::Unavailable(
            "ONNX runtime/model assets are not installed; no synthetic embedding substituted"
                .into(),
        ))
    }
}
