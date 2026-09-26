//! Explicit local artifact smoke; no downloads and no mock vectors.
use agentlaw_worker::{EmbeddingProvider, ModelAssets, OnnxProvider};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: onnx_smoke MODEL TOKENIZER RUNTIME_DLL".into());
    }
    let model = OnnxProvider::load(&ModelAssets {
        onnx_model: args[0].clone().into(),
        tokenizer_json: args[1].clone().into(),
        runtime_library: args[2].clone().into(),
    })?;
    let started = std::time::Instant::now();
    let query = model.embed("기억 저장 후 검색에서 최신 변경을 찾아야 한다.")?;
    let relevant = model
        .embed("After saving memory, retrieval must include the latest published revisions.")?;
    let unrelated = model.embed("A recipe for baking chocolate cake with vanilla frosting.")?;
    let dot = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| *x as f64 * *y as f64)
            .sum::<f64>()
    };
    assert_eq!(query.len(), 256);
    assert!(query.iter().all(|v| v.is_finite()));
    assert!((dot(&query, &query) - 1.0).abs() < 1e-5);
    println!("model_digest={}\ndimensions={}\nquery_norm_squared={}\nrelated_cosine={}\nunrelated_cosine={}\ninference_elapsed_ms={}",model.model_digest(),query.len(),dot(&query,&query),dot(&query,&relevant),dot(&query,&unrelated),started.elapsed().as_millis());
    Ok(())
}
