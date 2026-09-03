# cf-gears-graph-storage-remote-embedding-plugin

The remote embedding provider of the graph-storage gear: the alternative plugin
[ADR-0004](../docs/ADR/0004-cpt-cf-graph-storage-adr-embedding-provider.md)
names beside the in-process ONNX default. It implements
`graph_storage_sdk::plugin_api::EmbeddingProviderV1` over the OpenAI-compatible
`POST /embeddings` protocol, which OpenAI, Azure OpenAI, Groq, Together, Ollama,
vLLM and most self-hosted inference servers expose.

Use it where the gear's process cannot host a model — a memory ceiling, no CPU
budget for inference, or a platform that already runs an inference service.

## Enabling it

The gear links the plugin behind its `remote` feature and selects it by
configuration:

```yaml
graph-storage:
  config:
    embedding_provider: remote
    embedding_dimension: 384
    embedding_remote_base_url: "https://api.openai.com/v1"
    embedding_remote_model: "text-embedding-3-small"
    # Name of the environment variable holding the bearer credential.
    embedding_remote_api_key_env: "GRAPH_STORAGE_EMBEDDING_API_KEY"
    # Send the `dimensions` request field (Matryoshka models). Turn off for a
    # fixed-width model and set embedding_dimension to its native width.
    embedding_remote_request_dimensions: true
```

One deployment runs one provider. Switching providers over a populated graph
changes the embedding-space identity; the gear then blocks the vector arm until
the graph is re-embedded, and every other path keeps working.

## What the identity promises

The space is named by *model at endpoint at width*. Two deployments pointing
one model name at one host share a space; a different host or requested width
does not. A vendor silently changing the weights behind a stable model name is
not detectable from this side — ADR-0004 places that under model governance and
treats remote embedding as governed data egress.

Vectors are L2-normalized before storage, because the gear's index serves
cosine similarity and not every compatible endpoint returns unit vectors.

## Testing

```sh
cargo test -p cf-gears-graph-storage-remote-embedding-plugin
```

The suite runs the SDK's executable provider contract against a mock endpoint
(`wiremock`), plus batching, alignment, width, credential and deadline cases.
No network and no credential are needed.
