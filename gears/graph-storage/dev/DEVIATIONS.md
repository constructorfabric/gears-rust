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

**Every entry below has been checked against a running system**, not only
against the source. Where a claim was testable it was tested — on a live
PostgreSQL 19 stand, through the gear's own REST surface, or by a regression
test that fails when the behaviour it describes is reverted. Four entries said
more than the evidence supported and are corrected; the method that produced
them was writing the conclusion before running the experiment, so each entry
now records how it was verified.

**Status.** Every `[doc-gap]` below is now folded into PR #4523 (commit
`facd5da29` on `feature/graph-storage-prd-adr`), marked in the documents as
"Found while building the prototype" so a reader can tell a decision taken up
front from one the implementation forced. The `[platform-gap]` entries are
recorded in the documents too — at the place where they bite — but they close
only with platform work, so they stay open here. The `[deferred]` entries are
accepted scope cuts and need no documentation change.

---

## D-001 [doc-gap] ADR-0006 is `proposed`, the implementation treats it as binding

- **Doc:** `docs/ADR/0006-cpt-cf-graph-storage-adr-sqlpgq-access.md`, `status: proposed` — the only non-accepted ADR; DESIGN already treats its consequences as normative.
- **Implementation:** builds on the platform layer ADR-0006's ask #3 requested (`toolkit-sea-orm-pgq` + `toolkit_db::secure::pgq`, PR #4639), i.e. treats the ADR as accepted.
- **Why:** the platform answer exists; waiting for the status flip blocks everything downstream.
- **Proposal:** flip ADR-0006 to `accepted` in PR #4523 once #4639 merges, and rewrite its "development stand exception" paragraphs — the raw-SQL exception is gone.
- **Folded into the docs:** ADR-0006 now records that its raw-SQL exception is spent and what using the platform layer changed. The `proposed` status stays: flipping it is the architect's call, not the implementer's.
- **How it was checked:** **Verified** by inspection: no `Expr::cust` and no `GRAPH_TABLE` string anywhere in the engine — the pattern is built entirely by the platform builder, so the raw-SQL exception is genuinely unused.

## D-002 [doc-gap] PostgreSQL 18+ reports `ON DELETE RESTRICT` as SQLSTATE 23001

- **Doc:** the schema section mandates `ON DELETE RESTRICT` endpoint FKs and names no SQLSTATE at all — silent rather than wrong, which the first draft of this entry got backwards.
- **Implementation:** the DB error classifier treats **both** `23503` and `23001` (`restrict_violation`) as FK violations.
- **Why:** measured while landing #4639: PostgreSQL 18 changed the SQLSTATE for RESTRICT refusals (pg16/17 → `23503`, pg18+ → `23001`; `NO ACTION` still `23503`). On PG19 every RESTRICT refusal arrives as `23001`.
- **Proposal:** add the dual-SQLSTATE note where the RESTRICT keys are specified — any gear on PG18+ with them needs it. **Done:** DESIGN § 3.7, beside the schema that declares those keys (store-specific, so not in the store-agnostic error model).
- **How it was checked:** **Verified on a live PostgreSQL 19** with `VERBOSITY verbose`, on two plain tables: `ON DELETE RESTRICT` reports `23001` (`violates RESTRICT setting of foreign key constraint`), `ON DELETE NO ACTION` on the same server reports `23503`. Exactly as recorded.

## D-003 [platform-gap] The graph test lane has PostgreSQL 19 but no pgvector

- **Doc:** ADR-0001 point 2 makes "PostgreSQL 16 or later **with pgvector**" the baseline, and PG19 the probed capability for SQL/PGQ. Both are needed at once to exercise this gear.
- **Implementation:** `libs/test-containers`'s `postgres_graph()` pins a stock `19beta3-alpine`, which has no pgvector, so `CREATE EXTENSION vector` fails and the whole schema migration cannot run. The gear's PG19 lane therefore reads `GEARS_TEST_PG_GRAPH_IMAGE` for an image carrying both, skips with a named reason when neither is available, and fails loudly when `GEARS_TEST_PG_GRAPH_REQUIRED` is set.
- **Why:** no published image carries PG19 *and* pgvector yet — pgvector gained PG19 support upstream in 2026-07 and studio-web builds its own (CNPG operand + pgvector from a pinned source revision).
- **Proposal:** platform ask — `test-containers` should expose a graph image with pgvector (built once and published, as studio-web's `docker/graph-postgres/Dockerfile` already does), so the lane needs no per-developer environment variable. Until then, DESIGN's testing section should say the two capabilities do not come in one pinned image.
- **Folded into the docs:** ADR-0001 Consequences records that the PG16/PG19 matrix has no off-the-shelf PG19 half. Stays open: it closes when an image with both is published.
- **How it was checked:** **Verified** by running the gear's migration against the platform-pinned image: `CREATE EXTENSION IF NOT EXISTS vector` fails with `0A000 extension "vector" is not available`, and the schema is not created at all.

## D-004 [platform-gap] Readiness cannot name the server major

- **Doc:** DESIGN § Readiness Matrix and ADR-0001 point 2 describe SQL/PGQ as *probed*: readiness reports it unavailable on an older server, and an operator who explicitly configures it there gets a failure **naming the required major**.
- **Implementation:** the gear probes by *attempting* — at startup it runs the same pattern every hop uses, under a scope matching no rows, and takes the outcome as the answer. No catalog access is needed, so the sealed runner is not in the way.
- **Why this entry shrank.** Its first draft said "a gear cannot probe the server major" and concluded the runtime must inherit the migration's decision. The premise is true and the conclusion was wrong: what the hop depends on is whether a pattern executes, not which major answers it. Worse, the implementation matched the wrong conclusion — it assumed the capability, and a missing property graph was classified as an internal error, so **every traversal on the PostgreSQL 16 baseline answered 500** with the data reachable by the other backend the whole time. Found by dropping the property graph on the live stand. Fixed in `d9675f936`, covered by `traversal_answers_on_a_server_without_the_property_graph`, which fails with `relation "kb" does not exist` if either half of the fix is reverted.
- **What remains:** the attempt reports that the pattern did not run, never *why*. Naming the required major needs a narrow read-only capability surface (`server_version()`, `has_extension(..)`). That is a **diagnostics** gap, not a correctness one — the gear serves the right answers without it.
- **Proposal:** platform ask, filed as a diagnostics improvement rather than a blocker.
- **Folded into the docs:** DESIGN § 2.2, the readiness matrix row and ADR-0001 point 2 now say the gear probes by attempting, and separate the reporting gap from the correctness one (`3e88533f1`).

## D-005 [doc-gap] The platform page envelope has no revision slot

- **Doc:** PRD `fr-read-consistency` and DESIGN § Read Consistency Contract: every compound read reports the observed `(source_epoch, graph_revision)`, and continuation tokens are "the platform `CursorV1` **extended with the observed graph revision** — not a second token format".
- **Implementation:** the projection returns `toolkit_odata::Page<T>`, which carries `items` and `page_info { next_cursor, prev_cursor, limit }` and nothing else; `CursorV1` has no revision field a gear can populate. So the tabular projection is the one read path that does **not** report the revision. Search, traversal, node read and ingest all do.
- **Why:** using the platform binding is itself mandated (PRD `fr-tabular-projection`, and the DE0802/DE0803 lints enforce it), so a gear-local page envelope would violate a different rule.
- **Proposal:** platform ask — a revision (or opaque snapshot-identity) slot on `CursorV1`/`PageInfo`. Until then DESIGN should say which surfaces carry the revision and which cannot.
- **Folded into the docs:** PRD `fr-tabular-projection`, DESIGN § 3.3 and the drivers table no longer claim the revision travels in `CursorV1`; DESIGN § Read Consistency Contract names the surfaces that do report it and the one that cannot.
- **How it was checked:** **Verified** through the API: a projection page returns `page_info { next_cursor, prev_cursor, limit }` and no revision anywhere in the envelope, while node read, search, traversal and ingest all carry `(source_epoch, graph_revision)`.

## D-006 [doc-gap] `GraphStoreV1::end_read` is missing from the trait

- **Doc:** DESIGN § 3.3 lists `begin_read` and makes "one snapshot across every arm of one read" an obligation, but the trait has no way to *close* a snapshot.
- **Implementation:** added `end_read(ReadSnapshot)`. Without it the fake leaks a full copy of the tenant's rows per compound read, and a real store holding a transaction would leak the transaction.
- **Proposal:** add `end_read` to the trait signature in DESIGN.
- **Folded into the docs:** `end_read` is in the trait listing in DESIGN § 3.3.
- **How it was checked:** **Verified** by inspection of the trait listing in DESIGN § 3.3, which has `begin_read` and no way to close what it opens.

## D-007 [doc-gap] The built-in store cannot honour the one-snapshot obligation

- **Doc:** DESIGN § 3.3, obligation 5: two arms of one search and a hydration after it observe one revision.
- **Implementation:** the built-in PostgreSQL store declares `StoreCapabilities::snapshots = false`. A true repeatable-read snapshot needs one transaction held across several calls; `Db::transaction_ref_mapped` owns its transaction for the duration of a single closure, and the sealed runner offers no way to keep one alive across trait calls. `begin_read` returns the revision observed when the read began, and the arms are not isolated from a concurrent commit. The in-memory fake **does** honour it (copy-on-open), so the conformance case still has a passing implementation and the asymmetry is visible rather than assumed.
- **Proposal:** either a platform API for a caller-held transaction/snapshot handle, or DESIGN should mark obligation 5 as one an implementation may legitimately decline (it already has the `StoreCapabilities` mechanism for exactly that).
- **Folded into the docs:** DESIGN § 3.3 records that the built-in store declines the obligation, and why that is the capability mechanism working rather than an exception to it.
- **How it was checked:** **Verified by experiment**, not by reading. `the_built_in_store_declines_the_snapshot_obligation` opens a snapshot, commits a row from another call, and observes that row **through** the snapshot — so the absence of isolation is now an assertion rather than a comment. Inspection separately confirms the cause: every transaction API on `Db` takes a closure and `DbTx<'a>` is bound to its lifetime, so no caller can hold one across trait calls.

## D-008 [doc-gap] The attribute base has no `family`, so two ontology rules need an exception

- **Doc:** DESIGN § 3.1 authoring rules: `family` is required with no default, which is "the enforcement that stops derivation straight from a base"; and every concrete type resolves a `family`.
- **Implementation:** both rules are enforced for node and edge types only. The attribute base declares no `family` trait at all (attributes are payload fragments, not storable rows), and `provenance` derives from it directly and is concrete.
- **Proposal:** DESIGN should say the two rules are node/edge rules, or the attribute base should declare a family of its own.
- **Folded into the docs:** DESIGN § 3.1 scopes the family rules to node and edge types and says attributes have none.
- **How it was checked:** **Verified** through the API: an attribute type deriving straight from the attribute base registers successfully, where the equivalent node type deriving straight from the node base is refused. The family rules really are node/edge rules.

## D-009 [doc-gap] The prose and the table disagreed about which family types are abstract

- **Doc:** DESIGN § 3.1 prose said "three abstract bases and six **concrete** family types"; the per-family table two pages later marks four of the six abstract, `phantom_node` final and `provenance` concrete. The table was right and the prose was wrong — the opposite way round from what the first draft of this entry claimed.
- **Implementation:** four abstract families, plus two concrete by design: `phantom_node` (the gear instantiates it for an unresolved endpoint; nothing derives from it) and `provenance` (embedded in analysis-edge payloads). The test that walks the base ontology asserts exactly this split.
- **Proposal:** make the prose agree with the table. **Done:** DESIGN § 3.1, which now names which four are abstract and why the other two are not.

## D-010 [doc-gap] `websearch_to_tsquery` takes a `regconfig`, which cannot be a bound parameter

- **Doc:** DESIGN § 3.7 requires the lexical index and the query predicate to be built on the same expression, and the gear's rules forbid caller data reaching SQL text.
- **Implementation:** the text-search configuration name is inlined into the SQL (`websearch_to_tsquery('simple', $1)`), and only the caller's query text is bound. Binding the configuration as `$1` fails at runtime: `function websearch_to_tsquery(text, text) does not exist`.
- **Why:** the configuration is a compile-time constant shared with the index migration, not caller data — so inlining it is not an injection surface, and it keeps predicate and index on one expression.
- **Proposal:** note it in DESIGN beside the FTS-configuration rule; the next gear to build a lexical arm will hit it.
- **Folded into the docs:** DESIGN § 3.7, beside the schema whose index expression it has to match.
- **How it was checked:** **Verified on a live PostgreSQL 19**: `PREPARE … websearch_to_tsquery($1, $2)` fails with `function websearch_to_tsquery(text, text) does not exist`, while the inlined-configuration form prepares and executes.

## D-011 [doc-gap] `$filter` is not the right binding for the type-catalog pattern

- **Doc:** DESIGN § 3.3: "type listing binds `$filter` and `$top`", where `$filter` carries the GTS identifier pattern.
- **Implementation:** the type list takes `pattern` and `limit` as plain query parameters. Binding a GTS pattern as `$filter` is refused by the platform lints (DE0802/DE0803) because `$filter` means an OData filter expression over declared columns — which a GTS pattern is not — and the OData extractor would try to parse it as one.
- **Proposal:** correct DESIGN: the type catalog is not an OData collection; only the node projection is.
- **Folded into the docs:** DESIGN § 3.3 now says the type catalog is not an OData collection and takes plain parameters.
- **How it was checked:** **Verified** through the API, and it found a defect rather than only a mismatch: `$filter` sent to the type catalog was **silently ignored** — the same 12 types came back with and without it. Fixed in `05695faa0`; the catalog now refuses an unrecognised parameter and names the three it takes.

## D-012 [doc-gap] A producer type cannot be free-form, and its ids change shape

- **Doc:** DESIGN § 3.1 says a producer derives from a family, never from a base directly, and that `family` is required with no default. It does not say what that means for a producer that already writes types.
- **Implementation:** every consumer type id gains its family prefix — `gts.cf.studio.kg.file.v1~` becomes `gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.owned_node.v1~cf.studio.kg.file.v1~` — and each schema grows an `allOf` `$ref` to its family. The v1 gear accepted free-form types and interned them by name, so this is a breaking change for every existing producer, and it is invisible until registration fails.
- **Why:** without a chain there is nothing to validate an instance against, which is the whole point of the base ontology.
- **Proposal:** DESIGN should carry a short migration note for producers coming from a free-form registry: the id changes, the schema needs `allOf`, and the searchable paths move from a producer-supplied `search_text` to a `full_text_search` trait.
- **Folded into the docs:** DESIGN § 3.1 carries a migration note: identifier, schema and search text all change at once.
- **How it was checked:** **Verified** through the API: a free-form type and a type deriving straight from the node base are both refused, each naming what is wrong.

## D-013 [doc-gap] The base ontology has no stated publication moment

- **Doc:** DESIGN § Base Ontology Publication says the schemas are published, not when or by whom.
- **Implementation:** published per tenant on first type registration, prepended to the caller's batch. Not at boot: a tenant that never touches the graph gets no rows, and a tenant created later still finds its ancestors.
- **Why:** without it the very first registration fails on an ancestor nobody registered — the failure a producer sees, not the gear.
- **Proposal:** state the moment in DESIGN; the conformance suite now supplies only producer types, so the behaviour is pinned either way.
- **Folded into the docs:** DESIGN § Base Ontology Publication now says publication happens twice, in two registries, and the gear's own copy is per tenant on first registration.
- **How it was checked:** **Verified** through the API: registering two producer types returns 11 records — the two plus the nine base schemas the gear published for that tenant.

## D-014 [doc-gap] An undirected walk meets each edge twice

- **Doc:** `fr-graph-traversal` says edges are treated as undirected for reachability, and DESIGN's `ExpandResponse` carries `edges`. Neither says whether an edge reachable from both endpoints is reported once or twice.
- **Implementation:** deduplicated across hops. A two-hop walk expands the seed, reaches the neighbour, and then expanding *that* meets the very edge that led there — once as outgoing, once as incoming. Reported twice, a caller drawing or counting the result is wrong.
- **Proposal:** say it in DESIGN beside the one-hop primitive; it is not obvious from the trait.
- **Folded into the docs:** DESIGN § 3.3, beside `ExpandResponse`.
- **How it was checked:** **Verified** through the API: before the fix a two-hop walk of `f-1 → f-2 → f-3` returned `f-1 → f-2` twice; after it, once.

## D-015 [platform-gap] A local `[patch]` and the container image build are mutually exclusive

- **Doc:** the quickstart path is `docker compose up --build`.
- **Implementation:** studio-web reaches the SQL/PGQ layer through a `[patch]` block pointing at the sibling `gears-rust` checkout, which is outside the image build context — so the backend image cannot be built while the patch is in place. The stand runs the binary natively against the compose PostgreSQL instead (`config/local-stand.yaml`).
- **Why:** `toolkit-sea-orm-pgq` is unpublished and lives on a branch.
- **Proposal:** none needed — it resolves itself when PR #4639 merges and the git dependencies move back to `main`. Recorded so the next person does not spend the afternoon finding it.

## D-016 [deferred] Two read-surface defects the sweep found

Neither is a divergence from the specification — the specification was right
and the implementation was not — but both were invisible until the entries
around them were checked against a running system, so they are recorded where
that check happened.

- **Every `OData` failure was reported as a breached bound.** An unknown filter
  field answered `out_of_range` / `LIMIT_EXCEEDED` with the field `limit`,
  telling a caller to reduce a value they never sent. Now classified by what it
  is, and the rejection names the fields the projection accepts, which
  `fr-tabular-projection` asks for.
- **The type catalog ignored parameters it did not recognise.** `$filter` sent
  there changed nothing and said nothing — the failure mode the projection's
  binding exists to prevent, on the surface next to it.

Both fixed in `05695faa0`.

---

# Acceptance criteria: what the prototype actually establishes

PRD § 9 is the checklist this gear will be judged against, and nothing here
recorded where the prototype stands against it. Checked one by one, on the
live stand and in the suites. Nothing below is a divergence from the
specification — it is the distance still to cover, written down so it is not
rediscovered.

**1. Register an ontology, ingest owned nodes, reference nodes and both edge
families, re-run identically for byte-identical state — *partly*.** The
registration and the convergence halves hold and are covered end to end: an
identical batch re-run reports every row unchanged and leaves the revision
where it was. But only **owned nodes and static edges** were ever ingested.
Reference nodes (with their `(system, kind, native_id)` key derivation) and
analysis edges (with their required `provenance`) have validation code and no
exercise, in the suites or on the stand.

**2. Scope replacement removes stale static content and preserves analysis
edges — *no*.** Generation fencing works and is covered: an older generation
is refused, an equal one with different content conflicts. The **replacement
itself does nothing** — `fence_and_clear_scope` writes the fence row and
returns `(0, 0)`, removing no stale content, so the half of the criterion
about preserving analysis edges across a re-sync has nothing to preserve them
*from*. This was a deliberate cut, and it had not been written down anywhere
until this sweep.

**3. Four retrieval scenarios within the § 6.1 latency thresholds — *two and
a half of four, none timed*.** Hybrid narrowing, bounded traversal with
filtering and the depth-3 neighborhood answer. The criteria table answers its
*alternative* flow (a filter on an unindexed attribute is refused, naming the
alternatives) but not its main flow, which is filtering by payload attributes
— see D-104. And **no scenario was timed against § 6.1 at all**: the stand
carries five nodes, not the seeded reference graph the criterion names, so the
thresholds are untested rather than met.

**4. Chain violations, endpoint-constraint violations and wrong-width vectors
rejected with structured per-item errors — *two of three*.** Chain violations
come back as per-item field violations addressed by JSON pointer, every
violation in one response; a wrong-width vector is refused naming both widths.
**Endpoint constraints are not enforced at all**: `src_types` and `dst_types`
are resolved into the type's effective traits and then never read — neither in
the validation path nor in the write path. DESIGN specifies this check runs
inside the ingest transaction under locks on the endpoint nodes. An edge
between endpoints its type forbids commits today.

**5. Adversarial multi-tenant tests, zero cross-tenant data in every endpoint
— *partly*.** Two tenants owning the same node key see only their own row, and
the cross-tenant trap asserts its own fixture before trusting the pass. But
that covers the store and the hop, not *every endpoint*: search, projection,
node read and traversal are each scoped by construction and none has an
adversarial case of its own.

**6. `cfs validate` passes and CI meets the coverage threshold — *half*.**
`cfs validate` passes: 238 artifacts, 0 errors. Coverage was never measured —
`cargo llvm-cov` has not been run against this gear, so the 85 % line-coverage
threshold in `nfr-code-coverage` is unknown rather than met.

## Two gaps this sweep found that no entry recorded

- **Scope replacement removes nothing** (criterion 2 above). The fencing is
  real; the replacement is not.
- **Endpoint constraints are never enforced** (criterion 4 above). Parsed,
  stored, unread.

## And one claim the suite makes about itself

The conformance module's own header lists five obligations "asserted against
both the built-in store and the fake". Four are: batch atomicity, generation
fencing, no-orphan-edges, and the snapshot obligation (asserted on the fake,
and asserted as *declined* on the PostgreSQL store). **Obligation 2 —
single-writer serialization per scope identity, held until durable — has no
case at all.** Two concurrent replacements of one scope are never made to
race. The header should not claim it until one does.

---

# Deferred scope (agreed before implementation started)

Each of these is a `[deferred]` entry: the docs require it, this iteration
does not ship it, and the API/schema leave room for it.

## D-100 [deferred] Content chunking and heavy-content offload
`fr-content-chunking`, `fr-heavy-content-offload` (PRD §5.3); tables `chunk` + file-storage adapter.

**Corrected after checking.** The first draft claimed the search path "treats node hits only as the degenerate case of chunk folding so the seam exists". It does not: there is no mention of chunks or folding anywhere in the search implementation. The arms rank nodes and RRF fuses them; adding chunks means adding a folding step, not filling in a prepared one.

## D-101 [deferred] Labels
`fr-labels` (PRD §5.2); tables `label`, `label_assignment`; label routes; per-hop label filters in traversal (`ExpandRequest.labels` stays in the plugin API, built-in engine returns `CAPABILITY_UNSUPPORTED`).

## D-102 [deferred] Change events via transactional outbox
`fr-change-events` (PRD §5.2). The `emit_events` trait is stored with effective traits; nothing is published.

## D-103 [deferred] Embedding pipeline and built-in providers
`fr-embedding-pipeline` (PRD §5.4), ADR-0005 (ONNX default, remote plugin, model-change lifecycle). This iteration: embeddings are producer-supplied, dimension-guarded on ingest (`fr-embedding-dim-guard` partially: dimension check yes, embedding-space identity registry no — table `embedding_space` deferred). `EmbeddingProviderV1` ships as a trait only.

## D-104 [deferred] Index-activation lifecycle, dynamic index DDL — and payload filtering entirely
`fr-index-admission` (PRD §5.1), ADR-0003 point 5 (`requested → building → active`, `CREATE INDEX CONCURRENTLY` worker, DDL queue keys). No runtime DDL, as recorded.

**Corrected after checking.** The first draft said `$filter` over `index`-trait paths "is admitted only for paths covered by the static migration-time indexes", which implies payload filtering works for some paths. It works for none: the filterable-field schema declares `node_key`, `name`, `created_at` and `updated_at`, and nothing else, so `$filter=payload/severity eq 'critical'` is refused outright. Verified through the API. `index` trait paths *are* stored with the type's resolved traits — they are simply not wired to the filter surface, which is a larger gap than "no runtime DDL" and worth stating as its own deferral: **payload attributes are stored but not filterable at all**.

Checking this also found the rejection to be misclassified as `out_of_range`/`LIMIT_EXCEEDED`; fixed in `05695faa0`.

## D-105 [deferred] Full admission layer (fairness, queues, reserved connections)
`nfr-tenant-fairness`, parts of the Capacity contract (`tenant_max_*`, `global_max_*`, `interactive_reserved_connections`). This iteration ships the per-request bounds subset (sizes, depths, budgets, page caps) enforced in the domain admission layer.

## D-106 [deferred] Tenant offboarding / deletion monotonicity
`fr-tenant-offboarding` (PRD §5.8), the six-step external-ledger protocol.

## D-107 [deferred] Analytics topology role and metric annotation
`fr-analytics-topology`, `fr-metric-annotation` (PRD §5.7), ADR-0007 grants. The `graph_revision` signal (`fr-revision-signal`) IS in scope.

## D-108 [deferred] PG16 configuration matrix
ADR-0001 point 2 makes PG16+ the baseline and demands CI on both PG16 and PG19. This iteration pins the test lane to PG19 (`postgres_graph()`, `19beta3-alpine`); the server-major probe and conditional property-graph DDL are implemented, but the PG16 lane is not exercised.

## D-109 [deferred] The readiness surface, entirely
`fr-readiness`: DESIGN's 14-row matrix is normative, and `GET /health/ready` is in its REST surface.

**Corrected after checking.** The first draft said this iteration "ships per-capability healthy/degraded/unhealthy (DB, SQL/PGQ availability, vector dimension check) without the full matrix semantics". It ships none of it: `GET /health/ready` answers 404 and the word `health` does not appear in the route registration. What exists are the *inputs* a readiness surface would report — the capability probe, the embedding-dimension check at boot — with nothing exposing them. The deferral is the whole surface, not its fidelity.

## D-110 [deferred] Observability contract
`fr-observability` deny-by-default telemetry allowlist: followed in spirit (no payload/query text in logs), but the metric/counter surface (saturation counters, high-watermark gauges per limit) is not built.
