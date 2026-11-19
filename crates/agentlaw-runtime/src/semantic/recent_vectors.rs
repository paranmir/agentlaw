use std::sync::Arc;

use agentlaw_worker::EmbeddingError;

#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct RecentVectors {
    _private: (),
}

#[allow(dead_code)]
impl RecentVectors {
    pub(crate) fn with_capacity(_bytes: usize) -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn clear(&self) {
        unimplemented!("clearing the installation cache for a test")
    }

    pub(crate) fn get(&self, _model: &str, _digest: &[u8; 32]) -> Option<Arc<[f32]>> {
        unimplemented!("recent vector lookup")
    }

    pub(crate) fn insert(
        &self,
        _model: &str,
        _digest: [u8; 32],
        _vector: Arc<[f32]>,
    ) -> Result<(), EmbeddingError> {
        Err(EmbeddingError::Failed(
            "recent vector insertion is not implemented".into(),
        ))
    }
}
