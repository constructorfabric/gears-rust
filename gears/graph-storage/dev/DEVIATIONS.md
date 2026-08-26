# Graph Storage v2 — deviations from the documentation

Every entry records a place where the implementation and the documentation
(PR #4523: `docs/PRD.md`, `docs/DESIGN.md`, `docs/ADR/*`) disagree, in either
direction. The list is walked after the implementation lands; each entry ends
in one of three outcomes: the implementation changes, the documentation
changes (a gap or an error found by building), or the deferral is recorded as
an accepted scope cut.

Entry format:

- **Doc:** section (file + heading) and what it requires.
- **Implementation:** what was built instead.
- **Why:** the reason for the divergence.
- **Proposal:** fix the code / fix the doc / accept the cut.

Categories: `[deferred]` conscious scope cut for this iteration,
`[doc-gap]` the documentation is silent or wrong, `[platform-gap]` the
platform lacks an API the documentation assumes.

---

## D-001 [doc-gap] ADR-0006 is `proposed`, the implementation treats it as binding

- **Doc:** `docs/ADR/0006-cpt-cf-graph-storage-adr-sqlpgq-access.md`, `status: proposed` — the only non-accepted ADR; DESIGN already treats its consequences as normative.
- **Implementation:** builds on the platform layer ADR-0006's ask #3 requested (`toolkit-sea-orm-pgq` + `toolkit_db::secure::pgq`, PR #4639), i.e. treats the ADR as accepted.
- **Why:** the platform answer exists; waiting for the status flip blocks everything downstream.
- **Proposal:** flip ADR-0006 to `accepted` in PR #4523 once #4639 merges, and rewrite its "development stand exception" paragraphs — the raw-SQL exception is gone.

## D-002 [doc-gap] PostgreSQL 18+ reports `ON DELETE RESTRICT` as SQLSTATE 23001

- **Doc:** DESIGN error model maps FK violations from `23503` (`foreign_key_violation`); the schema section mandates `ON DELETE RESTRICT` endpoint FKs.
- **Implementation:** the DB error classifier treats **both** `23503` and `23001` (`restrict_violation`) as FK violations.
- **Why:** measured while landing #4639: PostgreSQL 18 changed the SQLSTATE for RESTRICT refusals (pg16/17 → `23503`, pg18+ → `23001`; `NO ACTION` still `23503`). On PG19 every RESTRICT refusal arrives as `23001`.
- **Proposal:** add the dual-SQLSTATE note to DESIGN's error model — any gear on PG18+ with RESTRICT FKs needs it.

## D-003 [platform-gap] The graph test lane has PostgreSQL 19 but no pgvector

- **Doc:** ADR-0001 point 2 makes "PostgreSQL 16 or later **with pgvector**" the baseline, and PG19 the probed capability for SQL/PGQ. Both are needed at once to exercise this gear.
- **Implementation:** `libs/test-containers`'s `postgres_graph()` pins a stock `19beta3-alpine`, which has no pgvector, so `CREATE EXTENSION vector` fails and the whole schema migration cannot run. The gear's PG19 lane therefore reads `GEARS_TEST_PG_GRAPH_IMAGE` for an image carrying both, skips with a named reason when neither is available, and fails loudly when `GEARS_TEST_PG_GRAPH_REQUIRED` is set.
- **Why:** no published image carries PG19 *and* pgvector yet — pgvector gained PG19 support upstream in 2026-07 and studio-web builds its own (CNPG operand + pgvector from a pinned source revision).
- **Proposal:** platform ask — `test-containers` should expose a graph image with pgvector (built once and published, as studio-web's `docker/graph-postgres/Dockerfile` already does), so the lane needs no per-developer environment variable. Until then, DESIGN's testing section should say the two capabilities do not come in one pinned image.

## D-004 [platform-gap] A gear cannot probe the server major

- **Doc:** DESIGN § Readiness Matrix and ADR-0001 point 2 both describe SQL/PGQ as *probed*: readiness reports it unavailable on an older server, and an operator who explicitly configures it there gets a failure naming the required major.
- **Implementation:** the migration probes the major (it must, to decide whether to emit the property-graph DDL), but the running gear cannot: the sealed `DBRunner` exposes no statement API, deliberately, so there is no way to issue `SELECT current_setting('server_version_num')` or read `pg_catalog`. The engine assumes the capability and falls back per request — with a logged reason — when a pattern refuses.
- **Why:** the seal is the point of the secure ORM; a catalog query is exactly the escape hatch it exists to remove.
- **Proposal:** platform ask — a narrow, read-only server-capability surface on the DB provider (`server_version()`, `has_extension(..)`), or a capability record the migration runner leaves behind for the gear. Related to ADR-0006 ask #4 (statement rendering), which is still open for the same reason.

## D-005 [doc-gap] The platform page envelope has no revision slot

- **Doc:** PRD `fr-read-consistency` and DESIGN § Read Consistency Contract: every compound read reports the observed `(source_epoch, graph_revision)`, and continuation tokens are "the platform `CursorV1` **extended with the observed graph revision** — not a second token format".
- **Implementation:** the projection returns `toolkit_odata::Page<T>`, which carries `items` and `page_info { next_cursor, prev_cursor, limit }` and nothing else; `CursorV1` has no revision field a gear can populate. So the tabular projection is the one read path that does **not** report the revision. Search, traversal, node read and ingest all do.
- **Why:** using the platform binding is itself mandated (PRD `fr-tabular-projection`, and the DE0802/DE0803 lints enforce it), so a gear-local page envelope would violate a different rule.
- **Proposal:** platform ask — a revision (or opaque snapshot-identity) slot on `CursorV1`/`PageInfo`. Until then DESIGN should say which surfaces carry the revision and which cannot.

## D-006 [doc-gap] `GraphStoreV1::end_read` is missing from the trait

- **Doc:** DESIGN § 3.3 lists `begin_read` and makes "one snapshot across every arm of one read" an obligation, but the trait has no way to *close* a snapshot.
- **Implementation:** added `end_read(ReadSnapshot)`. Without it the fake leaks a full copy of the tenant's rows per compound read, and a real store holding a transaction would leak the transaction.
- **Proposal:** add `end_read` to the trait signature in DESIGN.

## D-007 [doc-gap] The built-in store cannot honour the one-snapshot obligation

- **Doc:** DESIGN § 3.3, obligation 5: two arms of one search and a hydration after it observe one revision.
- **Implementation:** the built-in PostgreSQL store declares `StoreCapabilities::snapshots = false`. A true repeatable-read snapshot needs one transaction held across several calls; `Db::transaction_ref_mapped` owns its transaction for the duration of a single closure, and the sealed runner offers no way to keep one alive across trait calls. `begin_read` returns the revision observed when the read began, and the arms are not isolated from a concurrent commit. The in-memory fake **does** honour it (copy-on-open), so the conformance case still has a passing implementation and the asymmetry is visible rather than assumed.
- **Proposal:** either a platform API for a caller-held transaction/snapshot handle, or DESIGN should mark obligation 5 as one an implementation may legitimately decline (it already has the `StoreCapabilities` mechanism for exactly that).

## D-008 [doc-gap] The attribute base has no `family`, so two ontology rules need an exception

- **Doc:** DESIGN § 3.1 authoring rules: `family` is required with no default, which is "the enforcement that stops derivation straight from a base"; and every concrete type resolves a `family`.
- **Implementation:** both rules are enforced for node and edge types only. The attribute base declares no `family` trait at all (attributes are payload fragments, not storable rows), and `provenance` derives from it directly and is concrete.
- **Proposal:** DESIGN should say the two rules are node/edge rules, or the attribute base should declare a family of its own.

## D-009 [doc-gap] The phantom and provenance types are concrete, and the docs read as if every base and family is abstract

- **Doc:** DESIGN § 3.1 describes three abstract bases plus six family types, with the chain `base → family → producer type`.
- **Implementation:** two of the nine are concrete by design — `phantom_node` (the gear instantiates it; nobody derives from it, and the implementation refuses derivation) and `provenance` (producers embed it in analysis-edge payloads). The test that walks the base ontology asserts exactly this split.
- **Proposal:** name the two concrete ones in DESIGN so a reader does not infer that all nine are abstract.

## D-010 [doc-gap] `websearch_to_tsquery` takes a `regconfig`, which cannot be a bound parameter

- **Doc:** DESIGN § 3.7 requires the lexical index and the query predicate to be built on the same expression, and the gear's rules forbid caller data reaching SQL text.
- **Implementation:** the text-search configuration name is inlined into the SQL (`websearch_to_tsquery('simple', $1)`), and only the caller's query text is bound. Binding the configuration as `$1` fails at runtime: `function websearch_to_tsquery(text, text) does not exist`.
- **Why:** the configuration is a compile-time constant shared with the index migration, not caller data — so inlining it is not an injection surface, and it keeps predicate and index on one expression.
- **Proposal:** note it in DESIGN beside the FTS-configuration rule; the next gear to build a lexical arm will hit it.

## D-011 [doc-gap] `$filter` is not the right binding for the type-catalog pattern

- **Doc:** DESIGN § 3.3: "type listing binds `$filter` and `$top`", where `$filter` carries the GTS identifier pattern.
- **Implementation:** the type list takes `pattern` and `limit` as plain query parameters. Binding a GTS pattern as `$filter` is refused by the platform lints (DE0802/DE0803) because `$filter` means an OData filter expression over declared columns — which a GTS pattern is not — and the OData extractor would try to parse it as one.
- **Proposal:** correct DESIGN: the type catalog is not an OData collection; only the node projection is.

## D-012 [doc-gap] A producer type cannot be free-form, and its ids change shape

- **Doc:** DESIGN § 3.1 says a producer derives from a family, never from a base directly, and that `family` is required with no default. It does not say what that means for a producer that already writes types.
- **Implementation:** every consumer type id gains its family prefix — `gts.cf.studio.kg.file.v1~` becomes `gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.owned_node.v1~cf.studio.kg.file.v1~` — and each schema grows an `allOf` `$ref` to its family. The v1 gear accepted free-form types and interned them by name, so this is a breaking change for every existing producer, and it is invisible until registration fails.
- **Why:** without a chain there is nothing to validate an instance against, which is the whole point of the base ontology.
- **Proposal:** DESIGN should carry a short migration note for producers coming from a free-form registry: the id changes, the schema needs `allOf`, and the searchable paths move from a producer-supplied `search_text` to a `full_text_search` trait.

## D-013 [doc-gap] The base ontology has no stated publication moment

- **Doc:** DESIGN § Base Ontology Publication says the schemas are published, not when or by whom.
- **Implementation:** published per tenant on first type registration, prepended to the caller's batch. Not at boot: a tenant that never touches the graph gets no rows, and a tenant created later still finds its ancestors.
- **Why:** without it the very first registration fails on an ancestor nobody registered — the failure a producer sees, not the gear.
- **Proposal:** state the moment in DESIGN; the conformance suite now supplies only producer types, so the behaviour is pinned either way.

## D-014 [doc-gap] An undirected walk meets each edge twice

- **Doc:** `fr-graph-traversal` says edges are treated as undirected for reachability, and DESIGN's `ExpandResponse` carries `edges`. Neither says whether an edge reachable from both endpoints is reported once or twice.
- **Implementation:** deduplicated across hops. A two-hop walk expands the seed, reaches the neighbour, and then expanding *that* meets the very edge that led there — once as outgoing, once as incoming. Reported twice, a caller drawing or counting the result is wrong.
- **Proposal:** say it in DESIGN beside the one-hop primitive; it is not obvious from the trait.

## D-015 [platform-gap] A local `[patch]` and the container image build are mutually exclusive

- **Doc:** the quickstart path is `docker compose up --build`.
- **Implementation:** studio-web reaches the SQL/PGQ layer through a `[patch]` block pointing at the sibling `gears-rust` checkout, which is outside the image build context — so the backend image cannot be built while the patch is in place. The stand runs the binary natively against the compose PostgreSQL instead (`config/local-stand.yaml`).
- **Why:** `toolkit-sea-orm-pgq` is unpublished and lives on a branch.
- **Proposal:** none needed — it resolves itself when PR #4639 merges and the git dependencies move back to `main`. Recorded so the next person does not spend the afternoon finding it.

---

# Deferred scope (agreed before implementation started)

Each of these is a `[deferred]` entry: the docs require it, this iteration
does not ship it, and the API/schema leave room for it.

## D-100 [deferred] Content chunking and heavy-content offload
`fr-content-chunking`, `fr-heavy-content-offload` (PRD §5.3); tables `chunk` + file-storage adapter. Search folding code treats "node hits only" as the degenerate case of chunk folding so the seam exists.

## D-101 [deferred] Labels
`fr-labels` (PRD §5.2); tables `label`, `label_assignment`; label routes; per-hop label filters in traversal (`ExpandRequest.labels` stays in the plugin API, built-in engine returns `CAPABILITY_UNSUPPORTED`).

## D-102 [deferred] Change events via transactional outbox
`fr-change-events` (PRD §5.2). The `emit_events` trait is stored with effective traits; nothing is published.

## D-103 [deferred] Embedding pipeline and built-in providers
`fr-embedding-pipeline` (PRD §5.4), ADR-0005 (ONNX default, remote plugin, model-change lifecycle). This iteration: embeddings are producer-supplied, dimension-guarded on ingest (`fr-embedding-dim-guard` partially: dimension check yes, embedding-space identity registry no — table `embedding_space` deferred). `EmbeddingProviderV1` ships as a trait only.

## D-104 [deferred] Index-activation lifecycle and dynamic index DDL
`fr-index-admission` (PRD §5.1), ADR-0003 point 5 (`requested → building → active`, `CREATE INDEX CONCURRENTLY` worker, DDL queue keys). This iteration: `index` trait paths are stored and validated; `$filter` over them is admitted only for paths covered by the static migration-time indexes; no runtime DDL.

## D-105 [deferred] Full admission layer (fairness, queues, reserved connections)
`nfr-tenant-fairness`, parts of the Capacity contract (`tenant_max_*`, `global_max_*`, `interactive_reserved_connections`). This iteration ships the per-request bounds subset (sizes, depths, budgets, page caps) enforced in the domain admission layer.

## D-106 [deferred] Tenant offboarding / deletion monotonicity
`fr-tenant-offboarding` (PRD §5.8), the six-step external-ledger protocol.

## D-107 [deferred] Analytics topology role and metric annotation
`fr-analytics-topology`, `fr-metric-annotation` (PRD §5.7), ADR-0007 grants. The `graph_revision` signal (`fr-revision-signal`) IS in scope.

## D-108 [deferred] PG16 configuration matrix
ADR-0001 point 2 makes PG16+ the baseline and demands CI on both PG16 and PG19. This iteration pins the test lane to PG19 (`postgres_graph()`, `19beta3-alpine`); the server-major probe and conditional property-graph DDL are implemented, but the PG16 lane is not exercised.

## D-109 [deferred] Full readiness matrix
`fr-readiness`: DESIGN's 14-row matrix is normative. This iteration ships per-capability healthy/degraded/unhealthy (DB, SQL/PGQ availability, vector dimension check) without the full matrix semantics.

## D-110 [deferred] Observability contract
`fr-observability` deny-by-default telemetry allowlist: followed in spirit (no payload/query text in logs), but the metric/counter surface (saturation counters, high-watermark gauges per limit) is not built.
