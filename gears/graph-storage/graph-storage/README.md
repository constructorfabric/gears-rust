# cf-gears-graph-storage

The Graph Storage gear: a typed, multi-tenant knowledge graph with bounded
traversal and hybrid (lexical + vector) retrieval, over PostgreSQL 19 with
SQL/PGQ and pgvector.

The gear is a stateless gateway over a pluggable store. Its public API is the
`cf-gears-graph-storage-sdk` crate: the `GraphStorageClientV1` trait for
in-process consumers, the REST surface under `/graph-storage/v1`, and the
plugin contracts (`GraphStoreV1`, `GraphEngineV1`, `EmbeddingProviderV1`).

- [PRD](../docs/PRD.md), [DESIGN](../docs/DESIGN.md), [ADRs](../docs/ADR/)
- Base ontology schemas the gear registers at boot: [`schemas/`](./schemas/)
- Known gaps between these documents and the code are tracked in the gear's
  development notes, which live with the implementation rather than in the
  published set

## Requirements

PostgreSQL 19 with the `vector` extension. `CREATE PROPERTY GRAPH` is probed at
boot: on a server without it the property-graph migration is skipped and every
traversal hop is served by the portable two-query backend, with a logged reason.

## Configuration

```yaml
graph-storage:
  database:
    server: "pg_graph"          # the PostgreSQL 19 server alias
    dbname: "graph_storage"
  config:
    traversal_hop: pgq          # pgq | two_query
    embedding_dimension: 384    # fixed at migration time
    embedding_provider: onnx    # fake | onnx | remote
    # onnx (feature `onnx`)
    embedding_model_path: /app/models/minilm/model.onnx
    embedding_tokenizer_path: /app/models/minilm/tokenizer.json
    # remote (feature `remote`)
    # embedding_remote_base_url: "https://api.openai.com/v1"
    # embedding_remote_model: "text-embedding-3-small"
    # embedding_remote_api_key_env: "GRAPH_STORAGE_EMBEDDING_API_KEY"
```

One deployment runs one embedding provider. The gear records the provider's
embedding-space identity on first use and, on a later boot with a different
provider, blocks the vector arm until the graph is re-embedded — lexical search,
traversal and ingest keep working.

## Features

| feature  | what it links                                                  |
| -------- | -------------------------------------------------------------- |
| `onnx`   | in-process ONNX provider (`cf-gears-graph-storage-onnx-embedding-plugin`) |
| `remote` | OpenAI-compatible endpoint provider (`cf-gears-graph-storage-remote-embedding-plugin`) |

Both are off by default: a deployment links the provider it selected.

## Testing

```sh
# Unit and fake-store tests
cargo test -p cf-gears-graph-storage
# Against a real PostgreSQL 19 + pgvector image
GEARS_TEST_PG_GRAPH_IMAGE=<image> GEARS_TEST_PG_GRAPH_REQUIRED=1 \
  cargo test -p cf-gears-graph-storage
```
