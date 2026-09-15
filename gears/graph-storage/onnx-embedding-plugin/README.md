# cf-gears-graph-storage-onnx-embedding-plugin

The in-process embedding provider of the graph-storage gear: the default
[ADR-0004](../docs/ADR/0004-cpt-cf-graph-storage-adr-embedding-provider.md)
chooses. A MiniLM-class sentence-embedding model runs through ONNX Runtime in
the gear's own process, so a small deployment needs no inference service to use
vector search.

## Artifacts are supplied, never fetched

The model and tokenizer are read from operator-configured paths; the plugin
downloads nothing. The embedding-space identity is the SHA-256 of the bytes
actually loaded, so two deployments claiming one space either agree on that
hash or are visibly different.

ONNX Runtime is loaded with `dlopen` at first use (`ORT_DYLIB_PATH`), never
linked at build time. Building needs no runtime headers; running needs the
shared library. The pinned runtime version follows the workspace's `ort` crate.

```yaml
graph-storage:
  config:
    embedding_provider: onnx
    embedding_dimension: 384
    embedding_model_path: /app/models/minilm/model.onnx
    embedding_tokenizer_path: /app/models/minilm/tokenizer.json
```

## Testing

The contract lane needs the runtime and the artifacts, and skips with a named
reason without them:

```sh
ORT_DYLIB_PATH=/path/to/libonnxruntime.so \
GRAPH_STORAGE_ONNX_MODEL=/path/to/model.onnx \
GRAPH_STORAGE_ONNX_TOKENIZER=/path/to/tokenizer.json \
  cargo test -p cf-gears-graph-storage-onnx-embedding-plugin
```
