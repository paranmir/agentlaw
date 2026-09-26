use crate::{EmbeddingError, EmbeddingProvider, SemanticAvailability, DIMENSIONS};
use ort::{session::Session, value::Tensor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::PathBuf, sync::Mutex};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelAssets {
    pub onnx_model: PathBuf,
    pub tokenizer_json: PathBuf,
    pub runtime_library: PathBuf,
}
pub struct OnnxProvider {
    tokenizer: tokenizers::Tokenizer,
    session: Mutex<Session>,
    digest: String,
}
fn fail(e: impl std::fmt::Display) -> EmbeddingError {
    EmbeddingError::Failed(e.to_string())
}
impl OnnxProvider {
    /// CPU is already READY when this background-only method is called. Candidate
    /// preparation never replaces the active session before successful warmup/timing.
    pub fn select_execution(&self, assets: &ModelAssets) -> crate::execution::Selection {
        self.select_execution_cached(assets, None)
    }
    pub fn select_execution_cached(
        &self,
        assets: &ModelAssets,
        cached: Option<&crate::execution::Selection>,
    ) -> crate::execution::Selection {
        use crate::execution::*;
        use ort::execution_providers::{
            CUDAExecutionProvider, DirectMLExecutionProvider, ExecutionProvider,
        };
        let mut report = Selection {
            cacheable: true,
            backend: ExecutionBackend::Cpu,
            cpu_ns: Vec::new(),
            gpu_ns: Vec::new(),
            reason: "no compatible GPU execution provider in provisioned runtime".into(),
            host_resident_bytes: host_resident_bytes(),
            gpu_dedicated_bytes: None,
            gpu_shared_bytes: None,
        };
        let candidate = if CUDAExecutionProvider::default()
            .is_available()
            .unwrap_or(false)
        {
            Some((
                ExecutionBackend::Cuda,
                CUDAExecutionProvider::default().build().error_on_failure(),
            ))
        } else if DirectMLExecutionProvider::default()
            .is_available()
            .unwrap_or(false)
        {
            Some((
                ExecutionBackend::DirectMl,
                DirectMLExecutionProvider::default()
                    .build()
                    .error_on_failure(),
            ))
        } else {
            None
        };
        let Some((backend, ep)) = candidate else {
            return report;
        };
        report.cacheable = false;
        if let Some(cached) = cached {
            if cached.backend == ExecutionBackend::Cpu {
                return cached.clone();
            }
        }
        let result = (|| -> Result<(), EmbeddingError> {
            let mut builder = Session::builder()
                .map_err(fail)?
                .with_intra_threads(1)
                .map_err(fail)?;
            if backend == ExecutionBackend::DirectMl {
                builder = builder
                    .with_memory_pattern(false)
                    .map_err(fail)?
                    .with_parallel_execution(false)
                    .map_err(fail)?;
            }
            let mut gpu = builder
                .with_execution_providers([ep])
                .map_err(fail)?
                .commit_from_file(&assets.onnx_model)
                .map_err(fail)?;
            let encoding = self.fixed_encoding()?;
            Self::infer_session(&mut gpu, &encoding)?;
            report.host_resident_bytes = host_resident_bytes();
            if let Some(cached) = cached {
                if cached.backend == backend {
                    *self.session.lock().map_err(fail)? = gpu;
                    report = cached.clone();
                    report.reason =
                        "fingerprint-matched selection restored after GPU warmup".into();
                    return Ok(());
                }
            }
            for _ in 0..3 {
                let cpu = self.session.try_lock();
                let mut cpu = cpu.map_err(|_| {
                    EmbeddingError::Unavailable(
                        "foreground CPU work active; postpone GPU comparison".into(),
                    )
                })?;
                let start = std::time::Instant::now();
                Self::infer_session(&mut cpu, &encoding)?;
                report
                    .cpu_ns
                    .push(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
                drop(cpu);
                let start = std::time::Instant::now();
                Self::infer_session(&mut gpu, &encoding)?;
                report
                    .gpu_ns
                    .push(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
            }
            if gpu_is_unambiguously_faster(&report.cpu_ns, &report.gpu_ns) {
                *self.session.lock().map_err(fail)? = gpu;
                report.backend = backend;
                report.reason = "GPU timing range strictly below CPU range".into();
            } else {
                report.reason = "CPU retained: GPU timing ranges overlap or CPU is faster".into();
            }
            report.cacheable = true;
            Ok(())
        })();
        if let Err(e) = result {
            report.reason = e.to_string();
        }
        report
    }
    pub fn load(assets: &ModelAssets) -> Result<Self, EmbeddingError> {
        for p in [
            &assets.onnx_model,
            &assets.tokenizer_json,
            &assets.runtime_library,
        ] {
            if !p.is_file() {
                return Err(EmbeddingError::Unavailable(format!(
                    "required model/runtime artifact missing: {}",
                    p.display()
                )));
            }
        }
        let mut hash = Sha256::new();
        let mut buf = [0u8; 65536];
        let mut artifact_hashes = Vec::new();
        for path in [&assets.onnx_model, &assets.tokenizer_json] {
            let mut f = File::open(path).map_err(fail)?;
            let mut artifact_hash = Sha256::new();
            loop {
                let n = f.read(&mut buf).map_err(fail)?;
                if n == 0 {
                    break;
                }
                hash.update(&buf[..n]);
                artifact_hash.update(&buf[..n]);
            }
            artifact_hashes.push(format!("{:x}", artifact_hash.finalize()));
        }
        let supported_models = [
            "f9defdceaaae9d4d4007ad601b05b6e436375e7293e52b74ed1f7a3933c5b26a",
            "f1fdd44e7e1ac51f12ab7957c7bd092e064d596c288513bf9d326842f669edee",
            "75f9f258bf5013f5fe8a4dad61dd0fd16ac0cbaa7a106e3d3f41c2d04a42d541",
        ];
        if !supported_models.contains(&artifact_hashes[0].as_str())
            || artifact_hashes[1]
                != "0087c868b33bad550a78a08d19798cfd7f713cde4f020803b8f51f405503e15f"
        {
            return Err(EmbeddingError::Unavailable("artifacts do not match the release-pinned official Granite R2 model/tokenizer manifest".into()));
        }
        // Runtime must be an explicitly provisioned trusted native library. No download.
        ort::init_from(assets.runtime_library.to_string_lossy())
            .commit()
            .map_err(fail)?;
        let tokenizer = tokenizers::Tokenizer::from_file(&assets.tokenizer_json).map_err(fail)?;
        let session = Session::builder()
            .map_err(fail)?
            .with_intra_threads(1)
            .map_err(fail)?
            .commit_from_file(&assets.onnx_model)
            .map_err(fail)?;
        if session.inputs.iter().any(|i| {
            !matches!(
                i.name.as_str(),
                "input_ids" | "attention_mask" | "token_type_ids"
            )
        }) {
            return Err(EmbeddingError::Unavailable(
                "unsupported ONNX input schema".into(),
            ));
        }
        let provider = Self {
            tokenizer,
            session: Mutex::new(session),
            digest: format!("granite-r2-256:{:x}", hash.finalize()),
        };
        provider.warmup()?;
        Ok(provider)
    }
    /// Internal fixed 64-valid-token tensor; no user text or query-length policy.
    pub fn warmup(&self) -> Result<(), EmbeddingError> {
        let encoding = self.fixed_encoding()?;
        let result = self.infer_encoding(&encoding)?;
        if result.len() != DIMENSIONS || result.iter().any(|v| !v.is_finite()) {
            return Err(EmbeddingError::Failed("warmup output invalid".into()));
        }
        Ok(())
    }
    fn fixed_encoding(&self) -> Result<tokenizers::Encoding, EmbeddingError> {
        const PHRASE: &str =
            "The memory worker verifies normalized vectors before serving recall requests. ";
        let mut encoding = self
            .tokenizer
            .encode(PHRASE.repeat(16), true)
            .map_err(fail)?;
        if encoding.len() < 64 {
            return Err(EmbeddingError::Failed(
                "fixed warmup tokenization unexpectedly short".into(),
            ));
        }
        encoding.truncate(64, 0, tokenizers::TruncationDirection::Right);
        if encoding.get_attention_mask().iter().any(|v| *v != 1) {
            return Err(EmbeddingError::Failed(
                "warmup requires 64 valid unpadded tokens".into(),
            ));
        }
        Ok(encoding)
    }
}
impl EmbeddingProvider for OnnxProvider {
    fn model_digest(&self) -> &str {
        &self.digest
    }
    fn availability(&self) -> SemanticAvailability {
        SemanticAvailability::Ready
    }
    fn embed(&self, input: &str) -> Result<Vec<f32>, EmbeddingError> {
        if input.len() > 4 * 1024 * 1024 {
            return Err(EmbeddingError::Failed(
                "embedding input exceeds bounded profile".into(),
            ));
        }
        let encoding = self.tokenizer.encode(input, true).map_err(fail)?;
        self.infer_encoding(&encoding)
    }
}
impl OnnxProvider {
    fn infer_encoding(&self, encoding: &tokenizers::Encoding) -> Result<Vec<f32>, EmbeddingError> {
        let mut session = self.session.lock().map_err(fail)?;
        Self::infer_session(&mut session, encoding)
    }
    fn infer_session(
        session: &mut Session,
        encoding: &tokenizers::Encoding,
    ) -> Result<Vec<f32>, EmbeddingError> {
        let n = encoding.len();
        if n == 0 || n > 32768 {
            return Err(EmbeddingError::Failed(
                "token input exceeds model context; explicit sectioning required".into(),
            ));
        }
        let mut inputs = Vec::new();
        for field in &session.inputs {
            let values: Vec<i64> = match field.name.as_str() {
                "input_ids" => encoding.get_ids().iter().map(|&v| v as i64).collect(),
                "attention_mask" => encoding
                    .get_attention_mask()
                    .iter()
                    .map(|&v| v as i64)
                    .collect(),
                "token_type_ids" => encoding.get_type_ids().iter().map(|&v| v as i64).collect(),
                _ => return Err(EmbeddingError::Failed("unsupported input".into())),
            };
            inputs.push((
                field.name.clone(),
                Tensor::from_array(([1usize, n], values)).map_err(fail)?,
            ));
        }
        let output = session
            .run(inputs)
            .map_err(|e| EmbeddingError::Retryable(e.to_string()))?;
        let (shape, data) = output[0].try_extract_tensor::<f32>().map_err(fail)?;
        // IBM's official config specifies CLS pooling, never mean pooling.
        if !((shape.len() == 3 && shape[0] == 1 && shape[1] == n as i64 && shape[2] == 768)
            || (shape.len() == 2 && shape[0] == 1 && shape[1] == 768))
        {
            return Err(EmbeddingError::Failed(
                "unsupported Granite output shape".into(),
            ));
        }
        let mut result = data[..DIMENSIONS].to_vec();
        let norm = result
            .iter()
            .map(|v| (*v as f64).powi(2))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return Err(EmbeddingError::Failed("invalid model output".into()));
        }
        for v in &mut result {
            *v = (*v as f64 / norm) as f32;
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_assets_are_unavailable() {
        let p = PathBuf::from("certainly-nonexistent-agentlaw-model.onnx");
        assert!(matches!(
            OnnxProvider::load(&ModelAssets {
                onnx_model: p.clone(),
                tokenizer_json: p.clone(),
                runtime_library: p
            }),
            Err(EmbeddingError::Unavailable(_))
        ));
    }
}
