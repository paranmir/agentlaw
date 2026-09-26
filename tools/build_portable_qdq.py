"""Release-only conversion. Never imported or installed by the Rust product.

Use the isolated .tools/quantize-venv environment with pinned requirements below.
Calibration consists only of actual repository design documents, not generated
retrieval examples. Passing this build validates the artifact, not search quality.
"""
import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path

import numpy as np
import onnx
from onnxruntime.quantization import CalibrationDataReader, QuantFormat, QuantType, quantize_static
from tokenizers import Tokenizer

MODEL_SHA = "75f9f258bf5013f5fe8a4dad61dd0fd16ac0cbaa7a106e3d3f41c2d04a42d541"
TOKENIZER_SHA = "0087c868b33bad550a78a08d19798cfd7f713cde4f020803b8f51f405503e15f"
SOURCE = "https://huggingface.co/ibm-granite/granite-embedding-311m-multilingual-r2/resolve/44399559930365213510b1ee2eb15ded83374f0e/onnx/model.onnx"

def sha(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()

class Corpus(CalibrationDataReader):
    def __init__(self, tokenizer, inputs, documents):
        self.samples = []
        self.sources = []
        for path in documents:
            text = path.read_text(encoding="utf-8")
            ids = tokenizer.encode(text).ids
            offsets = sorted(set([0, len(ids)//4, len(ids)//2, 3*len(ids)//4]))
            windows = []
            for start in offsets:
                sample = ids[start:start+64]
                if len(sample) != 64:
                    continue
                windows.append({"token_offset": start, "token_count": 64, "ids_sha256": hashlib.sha256(np.array(sample,dtype="<i8").tobytes()).hexdigest()})
                values = {"input_ids": sample, "attention_mask": [1]*64, "token_type_ids": [0]*64}
                self.samples.append({name: np.array([values[name]],dtype=np.int64) for name in inputs})
            self.sources.append({"path": str(path), "sha256": sha(path), "windows": windows})
        self.iterator = iter(self.samples)

    def get_next(self):
        return next(self.iterator, None)

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--model",type=Path,required=True)
    parser.add_argument("--tokenizer",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--document",type=Path,action="append",default=[])
    parser.add_argument("--calibration",type=Path)
    parser.add_argument("--calibration-output",type=Path)
    args=parser.parse_args()
    assert sha(args.model)==MODEL_SHA, "unverified model source"
    assert sha(args.tokenizer)==TOKENIZER_SHA, "unverified tokenizer source"
    model=onnx.load(str(args.model))
    inputs=[item.name for item in model.graph.input]
    del model
    corpus=Corpus(Tokenizer.from_file(str(args.tokenizer)), inputs, args.document)
    if args.calibration:
        snapshot=json.loads(args.calibration.read_text(encoding="utf-8"))
        corpus.sources=snapshot["sources"]
        corpus.samples=[{name:np.array(value,dtype=np.int64) for name,value in sample.items()} for sample in snapshot["samples"]]
        corpus.iterator=iter(corpus.samples)
    assert corpus.samples, "real calibration corpus required"
    if args.calibration_output:
        args.calibration_output.write_text(json.dumps({"sources":corpus.sources,"samples":[{name:value.tolist() for name,value in sample.items()} for sample in corpus.samples]},ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
        return
    assert not args.output.exists(), "refuse to overwrite existing release artifact"
    quantize_static(str(args.model),str(args.output),corpus,quant_format=QuantFormat.QDQ,
                    activation_type=QuantType.QInt8,weight_type=QuantType.QInt8,
                    per_channel=True,reduce_range=False,op_types_to_quantize=["MatMul","Gemm","Gather"],
                    extra_options={"ActivationSymmetric":True,"WeightSymmetric":True})
    model=onnx.load(str(args.output))
    onnx.checker.check_model(model)
    kinds={node.op_type for node in model.graph.node}
    assert {"QuantizeLinear","DequantizeLinear"}<=kinds, "not a QDQ model"
    assert not any(t.data_location==onnx.TensorProto.EXTERNAL for t in model.graph.initializer), "model must be one file"
    manifest={"format":"agentlaw-portable-qdq-build-v1","source_url":SOURCE,"source_sha256":MODEL_SHA,
              "tokenizer_sha256":TOKENIZER_SHA,"model_sha256":sha(args.output),"model_bytes":args.output.stat().st_size,
              "dependencies":{name:importlib.metadata.version(name) for name in ["onnx","onnxruntime","numpy","tokenizers"]},
              "recipe":"static QDQ signed INT8 symmetric per-channel MatMul/Gemm/Gather; MinMax; no architecture-specific optimization",
              "calibration_sources":corpus.sources,"calibration_samples":len(corpus.samples),
              "validation":"ONNX checker and graph format only; actual Rust CPU warmup required separately; no retrieval quality claim"}
    args.output.with_suffix(".manifest.json").write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
    print(json.dumps({"model_sha256":manifest["model_sha256"],"model_bytes":manifest["model_bytes"]}))

if __name__=="__main__":
    main()
