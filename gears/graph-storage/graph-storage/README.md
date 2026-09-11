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
- Known gaps between these documents and the code: [below](#known-limitations)

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

## Known limitations

What the documents require and this iteration does not yet deliver, so a
reader is not left to discover it. Each is marked in the documents where it
bites ("Found while building the prototype").

**Deferred features** (the API and schema leave room; nothing is built): content
chunking and heavy-content offload; labels; change events; the admission layer
beyond per-request bounds (per-tenant and global concurrency, queues, reserved
connections, aggregate response bounds); tenant offboarding and deletion
monotonicity; the analytics topology role and metric annotation; the
index-activation lifecycle and per-path index DDL; the re-embedding lifecycle
that opens a new embedding epoch; observability counters; the retained
type-revision history.

**Narrower than documented** (built, with a stated gap):

- *Producer identity is not carried into the store.* The idempotency key is
  tenant-scoped rather than tenant-and-producer-scoped, and a scope's
  "owning producer" is not recorded, so any writer in the tenant may replace
  any scope; ordinary ingests do not take the shared scope lock the ingest
  protocol describes. Source-namespace ownership (`fr-source-ownership`) *is*
  enforced.
- *Neighborhood projection truncates by arrival order, not by degree*, and
  traversal takes explicit seed keys only (not search hits) and does not echo
  the admitted seeds.
- *Hybrid search fails, rather than degrading to its lexical arm,* when the
  embedding provider is unavailable; lexical hits carry no snippets.
- *Compound reads on the built-in store are not one snapshot* (the platform
  offers no caller-held transaction), and the service opens a snapshot for
  traversal only. Search and projection responses still report the revision
  they observed.
- *The traversal edge-scan budget is per hop, and a hop that reaches it is
  trimmed without a truncation reason.*
- *Reason codes for `not_found`, `unimplemented`, `deadline_exceeded`,
  `cancelled`, `unavailable`, `data_loss` and `unknown` are not on the wire*:
  the platform's builders for those categories carry no reason slot.
- *The in-process `GraphStorageClientV1` is narrower than REST*: no edge read,
  no compatibility dry run, no registration options or migrations, no
  source-namespace operations. Widening it is a `ClientV2` question.
- *Readiness does not state the active provider identity and dimension*, and
  five matrix rows report `not_implemented`.
- *The `source_epoch` is minted once and never rotates*; the snapshot-identity
  contract holds for idempotency receipts only.
- *Endpoint-constraint validation runs inside the ingest transaction but not
  under row locks* — the platform's secure ORM exposes no locking surface.
- *Deleting an already-tombstoned row answers `404`* rather than succeeding as
  a no-op, and re-ingesting a tombstoned edge revives it.
- *Base-ontology schemas are published once per tenant and have no update
  path*: an edit to a base schema does not reach a database that already
  published it.
- *The PostgreSQL lane needs an image with both PostgreSQL 19 and pgvector*,
  which `test-containers` does not publish yet (`GEARS_TEST_PG_GRAPH_IMAGE`);
  PostgreSQL 16, the documented baseline, has no lane.
- *The `remote` embedding provider has no per-tenant egress policy in front of
  it* (ADR-0004 asks for one); it is off by default and sends every tenant's
  node and query text to the one configured endpoint when selected.
