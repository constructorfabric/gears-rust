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
# Everything that needs no database: unit tests, the conformance suite against
# the in-memory store, the domain service, the REST surface.
make test-graph-storage

# The same conformance suite against a real PostgreSQL 19, plus the cases only
# a server can answer (the SQL/PGQ hop, its parity with the fallback, the
# cross-tenant trap). It needs a server with SQL/PGQ *and* pgvector, which no
# published image carries yet, so build the one this gear pins:
docker build -f gears/graph-storage/docker/pg19-pgvector.Dockerfile \
  -t pg19-pgvector:latest gears/graph-storage/docker
GEARS_TEST_PG_GRAPH_IMAGE=pg19-pgvector:latest make test-graph-storage-pg
```

Without `GEARS_TEST_PG_GRAPH_IMAGE` the PostgreSQL lane skips with a named
reason and starts nothing; `GEARS_TEST_PG_GRAPH_REQUIRED=1` (which the Make
target sets) turns that skip into a failure, so CI cannot pass by running
nothing.

Every case in that lane gets its own server — two of them are operator surgery
on server-wide state — so it runs at a bounded concurrency
(`GRAPH_PG_TEST_THREADS`, default 2, and `GEARS_TEST_PG_GRAPH_STANDS` for the
in-process runner). More than that on an ordinary machine and the connection
pools start timing out, which reads as a flaky gear and is a busy host.

A case that panics can leave its container behind — the removal is
asynchronous, and nothing awaits it — so after a failed run
`docker ps --filter ancestor=pg19-pgvector:latest` is worth a glance; a
handful of forgotten servers is enough to make the next run look flaky too.

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

- *Ordinary ingests do not take the shared scope lock the ingest protocol
  describes* — a scope replacement fences on a monotonic generation instead,
  which is the only serialization the secure ORM's surface allows. Producer
  identity *is* carried into the store: an idempotency receipt is keyed by
  `(tenant, producer, idempotency_key)`, and a scope records its owning
  producer and refuses a replacement submitted by anyone else. Source-namespace
  ownership (`fr-source-ownership`) *is* enforced.
- *Traversal takes explicit seed keys only*, not search hits. Retention under
  a neighborhood budget *is* degree-ordered, and a traversal *does* echo the
  seeds it admitted.
- *Hybrid search fails, rather than degrading to its lexical arm,* when the
  embedding provider is unavailable; lexical hits carry no snippets.
- *Compound reads on the built-in store are not one snapshot* (the platform
  offers no caller-held transaction), and the service opens a snapshot for
  traversal only. Search and projection responses still report the revision
  they observed.
- *The traversal edge-scan budget is per hop*, not per walk: a walk can scan
  the per-hop ceiling at every depth. A hop that reaches it does say so
  (`EdgeScanCap`), and the ceiling covers the whole hop -- a neighbourhood
  asking for degree-ordered retention reads its degrees out of what the
  incidence scan left, rather than out of a second allowance of the same size.
- *Every error names the node resource*, including the ones the type surface
  raises: `DomainError` does not carry which resource it is about, and one
  conversion serves every operation. A consumer switching on `resource_type`
  cannot tell a rejected type registration from a rejected ingest.
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
- *Re-ingesting a tombstoned edge revives it* rather than refusing, so a
  delete is undone by the next batch that names the same edge. Deleting an
  already-tombstoned row settles as a no-op; only a row that never existed is
  `404`.
- *Base-ontology schemas are published once per tenant and have no update
  path*: an edit to a base schema does not reach a database that already
  published it.
- *No published image carries both PostgreSQL 19 and pgvector*, so the lane
  builds its own from [`docker/pg19-pgvector.Dockerfile`](../docker/pg19-pgvector.Dockerfile);
  `test-containers` should publish one. PostgreSQL 16, the documented
  baseline, has no lane at all.
- *Vector search needs two session parameters the gear cannot set for itself.*
  HNSW is an approximate index and pgvector applies filters **after** the
  approximate scan, while every query this gear issues carries a tenant scope,
  a `deleted_at` predicate, an embedding epoch and optionally a type set. At
  the default `hnsw.ef_search` of 40 a filter admitting a tenth of the rows
  leaves about four candidates, so a small tenant sharing an index with a large
  one can get an empty page while its own matching vectors are in the table --
  and an empty page is indistinguishable from an empty graph. A deployment
  serving vector search must therefore set, in its database configuration:

  ```yaml
  params:
    hnsw.iterative_scan: relaxed_order   # `strict_order` if exact ordering matters
    hnsw.ef_search: "200"                # tune against measured selectivity
  ```

  toolkit-db forwards unrecognized `params` keys to `PostgreSQL` as runtime
  parameters, which is the only route available: `DBRunner` exposes no
  statement surface, so the gear cannot issue `SET LOCAL` per query
  (gears-rust #4871) — and per query is the granularity this actually wants,
  since an unfiltered search should not pay for iterative scanning.
  `a_filtered_vector_search_under_returns_without_iterative_scan` demonstrates
  the collapse and the fix against a live server.
- *The `remote` embedding provider has no per-tenant egress policy in front of
  it* (ADR-0004 asks for one); it is off by default and sends every tenant's
  node and query text to the one configured endpoint when selected. It also
  builds its own HTTP client rather than going through the `oagw` gear, so the
  centralized egress policy, credential injection and audit trail that gear
  provides are not in this path. Routing it through `oagw` is the intended
  remediation and the natural place to put the per-tenant policy; it is not
  done here because the plugin is a reference implementation of the provider
  port, and which gear owns egress is a platform decision rather than this
  gear's.
