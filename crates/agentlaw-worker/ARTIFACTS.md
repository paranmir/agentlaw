# Verified local CPU artifacts — 2026-09-26

## Portable QDQ release build

The official pinned repository contains FP32 and AVX2-specific INT8 ONNX files,
not a portable QDQ release file. `tools/build_portable_qdq.py` converts
the official FP32 file in the isolated ignored build-only Python environment.
Installed Rust runtime/startup never runs Python, conversion, or quantization.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| official `model.onnx` | 1247170481 | `75f9f258bf5013f5fe8a4dad61dd0fd16ac0cbaa7a106e3d3f41c2d04a42d541` |
| `model_portable_qdq.onnx` | 313378022 | `f9defdceaaae9d4d4007ad601b05b6e436375e7293e52b74ed1f7a3933c5b26a` |

Recipe: ONNX 1.18.0, ORT 1.22.0, NumPy 1.26.4, tokenizers 0.21.4;
static QDQ signed INT8 symmetric per-channel MatMul/Gemm/Gather, no
architecture-specific optimization. Twelve actual design-document windows of
64 tokens provide calibration. Their source SHA-256s, offsets and exact tensor
snapshot are checked in at `tools/portable-qdq-calibration.json`.
This corpus is conversion calibration, not synthetic or real retrieval-quality
evaluation. A second conversion from the checked-in tensor snapshot reproduced
the identical model SHA-256. The emitted adjacent build manifest records inputs,
versions and checks. ONNX checker, Q/DQ graph presence and single-file tensor
storage passed. Official source is the pinned IBM revision linked below.

The portable file passed the real Rust daemon test in 18.73 seconds: fixed
64-valid-token warmup, normalized Korean 256-vector, durable source publication
through real background inference and vector-index acknowledgement, semantic
recall, and cancellation while awaiting another fence. This verifies local CPU
execution only, not semantic quality, Arm compatibility, or actual GPU execution.

Reproduction uses the build-only requirements and checked-in calibration:

```text
python tools/build_portable_qdq.py --model model.onnx --tokenizer tokenizer.json --output model_portable_qdq.onnx --calibration tools/portable-qdq-calibration.json
```

## Earlier AVX2 comparison smoke (not the portable default)

Only this local validation explicitly downloaded artifacts. Product startup never
downloads or selects alternative models. Files are in ignored `.tools/models`.
IBM revision: `44399559930365213510b1ee2eb15ded83374f0e`.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `granite-r2/model_quint8_avx2.onnx` | 313421909 | `f1fdd44e7e1ac51f12ab7957c7bd092e064d596c288513bf9d326842f669edee` |
| `granite-r2/tokenizer.json` | 33384821 | `0087c868b33bad550a78a08d19798cfd7f713cde4f020803b8f51f405503e15f` |
| `onnxruntime-win-x64-1.22.0.zip` | 72368545 | `174c616efc0271194488642a72f1a514e01487da4dfe84c49296d66e40ebe0da` |

Model/tokenizer digests match Hugging Face's published LFS SHA-256 values. ORT's
ZIP digest is locally measured, not independently authenticated by a signed manifest.
The extracted CPU runtime is `ort-1.22.0/onnxruntime-win-x64-1.22.0/lib/onnxruntime.dll`.
The Microsoft archive also contains large debugging symbols; disk usage exceeds
download size. The model file is self-contained, with no external tensor file.

Official download sources:

- [IBM INT8 ONNX](https://huggingface.co/ibm-granite/granite-embedding-311m-multilingual-r2/resolve/44399559930365213510b1ee2eb15ded83374f0e/onnx/model_quint8_avx2.onnx)
- [IBM tokenizer](https://huggingface.co/ibm-granite/granite-embedding-311m-multilingual-r2/resolve/44399559930365213510b1ee2eb15ded83374f0e/tokenizer.json)
- [Microsoft CPU runtime](https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip)

`cargo run -p agentlaw-worker --example onnx_smoke -- MODEL TOKENIZER DLL`
ran the actual Rust tokenizer + ORT CPU session. Observed output:

```text
model_digest=granite-r2-256:acef2335c22528f7980a059162e57925c39d1781cea96465b3e573f83cd2c22a
dimensions=256
query_norm_squared=0.9999999939610413
related_cosine=0.9046802276411323
unrelated_cosine=0.7395122028839192
inference_elapsed_ms=75
```

The duration covers three warm inference calls, excluding model load and process
startup. This Korean/English smoke is not a semantic quality benchmark or SLA.
CPU only: GPU execution, million-record throughput, total process RAM, and all
platform compatibility are not validated by it.

The separately run `real_onnx_model_over_daemon_ipc` integration test passed in
14.64 seconds in the final warmup-enabled verification (including startup/load;
an earlier pre-warmup run was 10.54 seconds). It used the actual private daemon, waited
for READY, requested two identical Korean inputs, checked deterministic 256-value
normalized results, and cleaned up its own test child. It is ignored in the normal
suite; run with `AGENTLAW_SMOKE_MODEL`, `AGENTLAW_SMOKE_TOKENIZER`,
`AGENTLAW_SMOKE_ORT` and `--ignored` when explicitly provisioned assets exist.

READY now follows a successful fixed internal warmup with exactly 64 unpadded
valid tokens. The same inference path validates the model output shape, finite
values, nonzero norm and normalized 256-vector. No user input is used for warmup;
real query token length is not forced to 64. CPU/GPU comparison and fingerprint
persistence are now implemented, but the provisioned CPU-only DLL cannot validate
actual GPU execution; absence remains an explicit CPU selection diagnostic.
