use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use agentlaw_worker::{EmbeddingError, DIMENSIONS};

type Key = (String, [u8; 32]);

#[derive(Debug, Default)]
struct Cache {
    entries: HashMap<Key, (Arc<[f32]>, u64)>,
    access: BTreeMap<u64, Key>,
    epoch: u64,
    bytes: usize,
}

#[derive(Debug)]
pub(crate) struct RecentVectors {
    capacity: usize,
    cache: Mutex<Cache>,
}

impl Default for RecentVectors {
    fn default() -> Self {
        Self::with_capacity(8 * 1024 * 1024)
    }
}

impl RecentVectors {
    pub(crate) fn with_capacity(bytes: usize) -> Self {
        Self {
            capacity: bytes,
            cache: Mutex::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn clear(&self) {
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Cache::default();
    }

    pub(crate) fn get(&self, model: &str, digest: &[u8; 32]) -> Option<Arc<[f32]>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.epoch += 1;
        let epoch = cache.epoch;
        let key = (model.to_owned(), *digest);
        let (vector, last) = cache.entries.get_mut(&key)?;
        let vector = vector.clone();
        let previous = std::mem::replace(last, epoch);
        cache.access.remove(&previous);
        cache.access.insert(epoch, key);
        Some(vector)
    }

    pub(crate) fn insert(
        &self,
        model: &str,
        digest: [u8; 32],
        vector: Arc<[f32]>,
    ) -> Result<(), EmbeddingError> {
        if vector.len() != DIMENSIONS
            || vector.iter().any(|v| !v.is_finite())
            || !vector.iter().any(|v| *v != 0.0)
        {
            return Err(EmbeddingError::Failed("invalid embedding vector".into()));
        }
        let size = |model: &str| model.len() + digest.len() + DIMENSIONS * size_of::<f32>();
        let bytes = size(model);
        if bytes > self.capacity {
            return Ok(());
        }
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.epoch += 1;
        let epoch = cache.epoch;
        let key = (model.to_owned(), digest);
        if let Some((_, last)) = cache.entries.insert(key.clone(), (vector, epoch)) {
            cache.access.remove(&last);
        } else {
            cache.bytes += bytes;
        }
        cache.access.insert(epoch, key);
        while cache.bytes > self.capacity {
            let (_, key) = cache.access.pop_first().unwrap();
            cache.entries.remove(&key);
            cache.bytes -= size(&key.0);
        }
        Ok(())
    }
}
