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
    traversal_hop: auto         # auto (default) | pgq | two_query -- `pgq` is a
                                # demand: without SQL/PGQ the gear is not ready
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
docker build -f gears/graph-storage/deploy/docker/pg19-pgvector.Dockerfile \
  -t pg19-pgvector:latest gears/graph-storage/deploy/docker
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

## Using it from a producer

Answers to the questions producers have asked while building on the gear.
Each is the shipped behaviour, with the contract that fixes it.

- **Registering an ontology is one batch, atomically** (`fr-type-registration`):
  one type that conflicts refuses the whole batch and registers nothing, and
  the refusal names the type. A type that drifted compatibly is updated in
  place with `options.on_existing: update`, and `POST /types/compatibility`
  says beforehand what a change would cost; an incompatible change needs a new
  major or `options.revalidate`. What is compatible is GTS's verdict, not a
  rule of thumb: a description is; **an added optional property is only where
  the level it is added to is closed** (`additionalProperties: false`). Under
  an open `payload` -- the shape of every example here -- declaring a property
  narrows what the level accepts (a value that was any type must now match the
  declaration), so the same edit is backward-incompatible and `update` refuses
  it; close the leaf's payload, restating inherited properties, to make it
  compatible. A type an earlier build stored converges when it is offered
  again byte-identical, even where this build's analysis would refuse it (an
  upgrade does not fail the producer's unchanged run); a changed schema is
  analyzed by this build. Registering one type at a time is a valid fallback,
  not a workaround.
- **`expected_version` is the one conditional write.** A stored version is 1 or
  more and advances on every update; `Some(n)` requires exactly `n` and is a
  conflict otherwise, including when no node is stored under the key.
  `Some(0)` means "there must be none": two writers claiming one key with it
  get exactly one success, which is how a version number is claimed across
  replicas without a lock. No read returns the version yet (README § Known
  limitations), so a read-then-update is last-writer-wins until it does. Edges
  carry no version: their identity is derived from their endpoints.
- **Scope replacement removes what carries the scope.** `replace_scope:
  {attribute, value, generation}` removes, after the batch's own writes, the
  nodes of `scope_managed` types whose payload field `attribute` equals
  `value` and that the batch did not re-supply, and the `static` edges the
  scope declared (edges record the scope that declared them). A node whose
  payload does not carry the attribute is not the scope's and is never removed
  -- a replacement that "does nothing" is a payload without the field. The
  generation is monotonic per scope; an equal generation with different
  content is a conflict, and an older one is `STALE_GENERATION`, whose
  `recorded_generation` violation carries the recorded generation as its
  whole description. **An empty batch is the scope's erasure**: it removes
  every node and static edge of the scope -- a static edge incident to a
  removed node goes whichever scope declared it -- except a node a live
  analysis edge still references; the idempotency receipts of earlier
  requests stay. A replacement, like every write, touches
  the caller's own tenant only, whatever subtree its reads may see.
- **A tombstoned node key is not reusable before purge**, and purge is not
  built; a tombstoned edge is revived by the next upsert that names it
  (DESIGN § Soft Delete Contract, rules 4 and 6).
- **Adjacency on a node read is bounded per direction** by
  `node_read_max_adjacency` (100 by default, at most 1 000): outgoing and
  incoming are cut separately, so one read carries up to twice that many
  entries, with a truncation flag and no cursor. To list every edge of a hub,
  walk it: `POST /graph/traverse` with the node as the seed and depth 1 returns
  its edges, up to `traversal_max_nodes` (1 000 by default, at most 10 000)
  neighbours.
- **`$filter` and `$orderby` reach payload attributes the type declares** in
  its `index` trait (`payload/severity`). Equality on a string, boolean or
  date-time value is served by one GIN over the payload; equality on a number,
  every range comparison and ordering over a payload path are admitted and
  unindexed, which is a scan of the type's rows; `contains` is a `LIKE` and
  scans too. A page carries no total and there is no offset: the platform
  pages by cursor and refuses `$count` and `$skip` by design
  (`toolkit::api::odata`), so a listing that needs either walks the pages
  (#5108 records what a consumer asked for beyond this).
  A type that declares no `index` paths can be filtered by `node_key`, `name`,
  `created_at` and `updated_at` only, which is a scan of the tenant. The audit
  envelope's `updated_at` is when the gear last wrote the row, not when the
  object changed; an object's own time belongs in its payload.
- **A cursor continues the listing that minted it, and only with its query.**
  `next_cursor` carries the ordering and a fingerprint of the `$filter` and of
  the types its first page ran under; it does not carry `$top`. Send the next
  page as `cursor` **plus the same `type_pattern`, `$filter` and `$top`**
  (`$orderby` is refused next to a cursor, which already names it). A cursor
  replayed without its filter, with another one, or with one added is refused
  with `400`, and so is one replayed with a `type_pattern` that selects a
  different set of types -- both are part of which listing this is. Since the
  fingerprint is of the *selected* types, registering a type that the pattern
  matches invalidates the cursors in flight under it; start the listing
  again. The tenant is not part of it: a cursor is a position in key order,
  and presented under another tenant it continues that tenant's listing from
  the same key. Without `type_pattern` a payload ordering is refused, since the
  admitted paths come from the selected types. Without `$top` the page falls
  back to `projection_max_page`.
- **Readiness for a probe is the platform's `/readyz`**, which runs this gear's
  healthcheck and takes the pod out of traffic when a fatal component is
  unhealthy -- every row of the matrix but `embedding_space`: an embedding
  identity mismatch is reported as degraded and the pod stays in rotation for
  ingest, reads, traversal and lexical search, with the vector arm failing
  (`ComponentReadiness::fatal`); the gear's own
  `GET /graph-storage/v1/health/ready` is the detailed state
  document and answers `200` whatever the state, the same split the platform
  makes between `/readyz` and `/health`. Point a probe at the gateway's
  `/readyz`, not at the gear's route.
- **Nodes and edges in one batch** commit or fail together
  (`fr-bulk-ingest`); an importer that writes nodes in one call and edges in
  another has, between the two, a graph with nodes and no edges.
- **Migrations are additive** from `v0.1.0`: no shipped migration's DDL has
  changed since it shipped (later tags add migrations), so a database created
  by any tag upgrades in place. Only a database from the vendored prototype
  needs to start empty.

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
type-revision history; runtime plugin registration and selection (the built-in
store, engine and embedding provider are constructed directly, and no
independently developed implementation can reach the service yet; #4873).

**Narrower than documented** (built, with a stated gap):

- *Ordinary ingests do not take the shared scope lock the ingest protocol
  describes* — a scope replacement fences on a monotonic generation instead,
  which is the only serialization the secure ORM's surface allows. What
  protects an ordinary write from a concurrent replacement is that the
  replacement re-checks scope membership in the statement that removes a
  node, so a node an ingest has just moved out of the scope is not deleted.
  One residual remains: a new edge written to a node a replacement is
  removing at that moment can be lost (DESIGN § Concurrent Ingest Protocol). Producer
  identity *is* carried into the store: an idempotency receipt is keyed by
  `(tenant, producer, idempotency_key)`, and a scope records its owning
  producer and refuses a replacement submitted by anyone else. Source-namespace
  ownership (`fr-source-ownership`) *is* enforced.
- *Traversal takes explicit seed keys only*, not search hits. Retention under
  a neighborhood budget *is* degree-ordered, and a traversal *does* echo the
  seeds it admitted.
- *Traversal has two hop backends*, the SQL/PGQ pattern and the two-query hop
  (`traversal_hop: auto | pgq | two_query`); the iterative-CTE hop of ADR-0001 was
  built on the development stand and is not shipped.
- *Four specified tables are not created*: `chunk`, `label`, `label_assignment`
  and `ingest_audit` (chunking, labels and the audit record are deferred).
- *Hybrid search fails, rather than degrading to its lexical arm,* when the
  embedding provider is unavailable; lexical hits carry no snippets.
- *A re-validating or migrating type update is a claim about the rows it
  read.* The scan and a concurrent ingest of the same type are separate
  transactions at the server's default isolation, and the platform offers no
  lock or isolation level to fence them (#4871): a row committed after the
  scan passed its position was validated against the schema in force when it
  was written, not by the update. Evolve a type with its ingest quiesced, as
  for any schema migration (ADR-0006 § 3); the fence is a follow-up (#5012).
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
- *Idempotency receipts never expire*: `idempotency_retention_days` is
  validated and read by nothing, because an expiry needs a protocol before a
  cleanup (a deleted receipt makes a late retry look new; #4874).
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
  builds its own from [`deploy/docker/pg19-pgvector.Dockerfile`](../deploy/docker/pg19-pgvector.Dockerfile);
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
  it.* ADR-0004 requires one; it is not implemented. The provider sends every
  tenant's node and query text to the one configured endpoint, and builds its
  own HTTP client rather than going through the `oagw` gear, so that gear's
  centralized egress policy, credential injection from `credstore`, rate
  limiting and audit trail are not in this path. Tracked as
  [#4877](https://github.com/constructorfabric/gears-rust/issues/4877);
  `mini-chat`'s OpenAI provider routes through `ServiceGatewayClientV1` and is
  the reference for the fix.

  **This limitation is reachable only by a deployment that opts in.** `remote`
  is a Cargo feature that is off by default: a binary built without
  `--features remote` does not link the plugin at all, and naming `remote` in
  such a build fails at boot with a message saying so rather than falling back
  to another provider. A deployment built with `onnx` alone therefore has no
  external embedding egress path, and #4877 neither applies to it nor blocks
  it. Shipping `onnx`-only is the supported way to run this gear before #4877
  lands.
- *A migration cannot rebuild the vector index without blocking the `node`
  table.* The platform's migration runner wraps every migration in a
  transaction and ignores `use_transaction()`, and `PostgreSQL` refuses
  `CREATE INDEX CONCURRENTLY` inside one, so an index change can only be
  written the way m0003 writes it: `DROP INDEX` and `CREATE INDEX` in one
  transaction. That holds an `ACCESS EXCLUSIVE` lock on `node` from the drop
  until the commit, so reads and writes both wait for the whole HNSW build (a
  `CREATE INDEX` alone would hold a `SHARE` lock, which still blocks writes).
  m0003 itself costs nothing: every migration of this gear ships in the same
  release, so it runs on an empty table at first install. A later migration
  that changes the index on a populated table needs a maintenance window, or
  has to be run as an operator step outside the runner, until the runner can
  run a migration outside a transaction
  ([#5011](https://github.com/constructorfabric/gears-rust/issues/5011)).
