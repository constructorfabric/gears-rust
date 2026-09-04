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

The vector-search work has its own companion report,
[`VECTOR-SEARCH.md`](./VECTOR-SEARCH.md): what was chosen and why, the
difficulties it hit, and what a live PostgreSQL 19 stand proved about it.

Categories: `[deferred]` conscious scope cut for this iteration,
`[doc-gap]` the documentation is silent or wrong, `[platform-gap]` the
platform lacks an API the documentation assumes, `[impl-gap]` the
implementation ships something the documentation does not sanction — the
category D-019 needed, since a mode the docs do not describe is neither a
silent document nor an agreed cut.

**Every entry below has been checked against a running system**, not only
against the source. Where a claim was testable it was tested — on a live
PostgreSQL 19 stand, through the gear's own REST surface, or by a regression
test that fails when the behaviour it describes is reverted. Four entries said
more than the evidence supported and are corrected; the method that produced
them was writing the conclusion before running the experiment, so each entry
now records how it was verified.

**Status.** Every `[doc-gap]` below **up to D-016** is folded into PR #4523
(commit `facd5da29` on `feature/graph-storage-prd-adr`), marked in the
documents as "Found while building the prototype" so a reader can tell a
decision taken up front from one the implementation forced. **D-017 through
D-024 are not**: D-017 and D-018 came out of building vector search after that
sweep, and D-020 through D-024 out of implementing the element envelope and
adapting to PR #4639. Doc edits are agreed before they are published. The `[platform-gap]` entries are
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
- **Found later, and larger than the entry it sits under: publication is also the only moment.** The base ontology has no update path. `with_base_ontology` prepends a base schema only when the type is absent (`if store.get_type(...).is_ok() { continue; }`), and registration converges only on a byte-identical schema — anything else under the same identifier is a conflict. So a base schema edited in the repository reaches a database that has already published it *never*: not at boot, which skips it, and not by re-registration, which refuses it. **Verified** on the stand: posting the stored base node schema back unchanged returns `200`; posting it with a single `description` string changed returns `409 CAS_CONFLICT`, "already registered with a different schema".
- **Why it matters beyond the text:** the schema is served to clients (`/types` returns `type_schema`), so the same type identifier can carry different documentation on two deployments — a fresh one and one provisioned earlier — with nothing to reconcile them. Today that is only descriptive drift, because nothing in the trait resolution reads a `description`. It stops being only descriptive the first time a base schema needs a real correction: a trait shape, an enum value, a constraint. There is no version, no migration and no override for base types, so that correction has no delivery mechanism at all.
- **Proposal:** decide the base-ontology upgrade path before the ontology-registration API freezes — a version on base types, or an explicit ontology migration that is allowed to replace them. Until it exists, treat every base schema edit as unreleasable on existing deployments, editorial ones included.

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

## D-017 [doc-gap] The four vector states are named, not encoded
`fr-embedding-pipeline` fixes the states a vector may be in after an upsert — embedded and current, absent, preserved, stale — and says similarity search must consider only current ones. It does not say how a store distinguishes them, and the columns DESIGN gives (`embedding`, `embedding_epoch`, `embedding_input_hash`) admit more than one encoding.

**Chosen:** `embedding_input_hash` describes the *stored vector's* input, never the node's current text, which is what lets a later ingest tell a preserved vector from a stale one. `embedding_epoch` carries currency: the active epoch for a current vector, `NULL` for one that is absent or stale. The arm then reads `embedding_epoch = <active>`, so "only current vectors rank" is one equality rather than a rule every query has to remember, and the HNSW index is partial on `embedding_epoch IS NOT NULL` so a stale row does not occupy a slot in the candidate set.

**Why it is worth stating:** the encoding is a store-visible contract. An external `GraphStoreV1` plugin has to arrive at the same distinctions, and the FR alone does not tell it how. The decision itself lives in `domain::embedding::decide_vector`, shared by both implementations, so the two cannot drift — the lesson of the endpoint-constraint entry above.

**Proposal:** DESIGN's `node`/`chunk` table notes should say what `embedding_epoch = NULL` means beside a non-NULL `embedding`, since that is the only state the column names cannot be read off.

## D-018 [doc-gap] `embedding_space` is deployment-wide, and every runtime read is scoped
DESIGN's `embedding_space` table has `epoch` as its primary key and no tenant column: it is deployment-wide, correctly. But every runtime read in this gear goes through the secure ORM, which needs a scopable entity, and there is no unscoped read API — by design.

**Implementation:** the table carries a `tenant_id` holding the nil UUID, and boot reads it under a nil-tenant scope. This is the gear's existing device rather than a new one: `graph_meta` already holds deployment-level keys the same way, and `probe_pgq` already reads under a nil-tenant scope. Nothing reads the table per request — the active epoch is resolved once at boot and carried in memory — so the column costs one write at first boot and nothing thereafter.

**Proposal:** either DESIGN notes the column for stores built on a scope-enforcing ORM, or the platform offers an explicit deployment-scope read. The second is the better shape; the first is what the prototype could do.

## D-019 [impl-gap] The gear's default embedding provider carries no semantics
A deployment that does not configure `graph-storage.embedding_provider = onnx` gets the deterministic fake, and vector search then answers with rankings that mean nothing — reproducible, well-formed, and semantically arbitrary. Boot says so at `warn` level and that is all.

ADR-0005 has no such mode: its three providers are ONNX, remote and "a deterministic fake for CI". Shipping the CI fake as the *runtime default* is this prototype's choice, made so the write path, the epoch bookkeeping and the four vector states could be exercised before a deployment has model artifacts. It is defensible for a prototype and wrong for a release: the failure it produces is a quiet quality loss, which is the exact failure mode ADR-0005 is written to prevent.

**Proposal:** before release, either make `onnx` the compiled-in default with no fallback, or make an unconfigured provider a boot failure. A warning in a log is not a guard.

## D-020 [doc-gap] The envelope's subject id is a `Uuid`, and DESIGN types it `TEXT`

DESIGN § 3.7 gives the audit columns as `created_by_subject_id / created_by_subject_type | TEXT`. The vocabulary that column is pointing at is `SecurityContext`, whose `subject_id()` returns a **`Uuid`** — `subject_type()` is the `&str` half, a GTS type identifier.

**Implementation:** `UUID` for the id, `TEXT` for the type (migration `m0004`). Writing a `Uuid` into `TEXT` widens the domain for nothing, costs 20 bytes a row against 16, and makes an equality join against any other subject column a text comparison.

**Proposal:** fix the doc — one word in two table rows. This is not a design disagreement, it is a table sketch written before the platform type was looked up.

**How it was checked:** **Verified on the live stand.** `\d node` on the PostgreSQL 19 instance shows `created_by_subject_id | uuid | not null`, and a node read back over REST carries `"subject_id": "00000000-0000-0000-0000-00000000a001"` with `"subject_type": "gts.cf.core.security.subject_user.v1~"` — both halves populated from the request's own security context.

## D-021 [doc-gap] `graph_revision` on the element is what lets the projection report one at all

PRD § `fr-tabular-projection` carries a "Found while building the prototype" note: the continuation token cannot be `CursorV1` *extended with the observed graph revision*, because `CursorV1` has no revision member and the platform page envelope carries only items and cursors. It concludes that tabular projection is **the one read path that does not report `(source_epoch, graph_revision)`**, and that closing it needs a platform slot.

**It does not.** DESIGN's envelope table already lists `graph_revision` as an envelope member — "the revision the read observed" — and the envelope rides on the *element*, not on the response. `toolkit_odata::Page` constrains the wrapper; it does not constrain the items, which are this gear's own DTO. So the projection reports its observed revision on every row, and the gap closes with no platform change.

**Implementation:** `ElementEnvelope.graph_revision`, resolved from the call's compound-read snapshot when it has one and from `graph_meta` otherwise. Per element rather than per response, which duplicates one value across a page — the cost of the platform wrapper having no place for it.

**Proposal:** replace the PRD note. The finding it records is real (the cursor cannot carry it); the conclusion drawn from it is not.

**How it was checked:** **Verified on the live stand.** `GET /nodes?limit=2` returns each row with `"graph_revision": {"source_epoch": 1, "revision": 5}`, and the conformance obligation `a_projection_row_carries_the_envelope` asserts it against both store implementations.

## D-022 [impl-gap] No read surface returns an edge, so half of `fr-audit-envelope` is unexercised

`fr-audit-envelope` says **every node and edge** returned by any read surface must carry the envelope. The prototype has no surface that returns an edge as an element: `DELETE /edges/{edge_key}` exists, `GET` does not, and an edge appears in a response only as a topology reference — `EdgeRef` in a traversal, `AdjacencyEntry` in a node read — which is a key, a type and two endpoints by design.

**Implementation:** the edge table carries the full envelope and every write populates it (migration `m0004` adds `updated_at` and the three subject pairs), so the data is there and correct. Nothing reads it back.

**Why this is `[impl-gap]` and not `[deferred]`:** a requirement that cannot be observed is a requirement nothing tests, and the columns will drift the first time an ingest path is added that forgets one. The conformance obligation asserts the node half against both stores; the edge half has no assertion because it has no surface to assert through.

**Proposal:** either add the edge read the FR implies, or state in the FR that an edge's envelope is reachable only through a future edge read. The first is small — the columns and the mapping already exist.

## D-023 [doc-gap] "Falling back is never silent" was silent to everything except a human reading logs

DESIGN's traversal section says the pattern backend's decline is "never silent — the reason is logged either way", and the implementation did exactly that: `warn!` on `PatternOutcome::Unavailable`.

**A log line is not observable to a test.** Adapting the gear to PR #4639's anchor opt-in, the fix was removed to see what would fail, and **nothing did**: the pattern statement referenced a relation absent from its `FROM`, PostgreSQL refused it with `missing FROM-clause entry for table "node"`, every traversal was served by the two-query fallback, and the whole PG lane passed. `the_pattern_hop_walks_the_graph` walked the graph without the pattern; `both_hop_backends_return_the_same_answer` compared the fallback with itself and found perfect agreement.

**Implementation:** `ExpandResponse::served_by`, a `HopBackend` of `Pattern` or `TwoQuery`. The two tests now assert which backend answered them.

**Proposal:** DESIGN should say the backend is reported *on the answer*, not only logged. The general form is worth stating once: a fallback that returns the right answer is invisible unless the answer says which path produced it — a property this gear has now met twice, here and in D-004.

**How it was checked:** **Verified by control experiment on a live PostgreSQL 19 stand.** With `correlate_with_anchor()` removed, exactly those two tests fail on exactly those assertions and the other 22 pass; restored, all 24 pass and the stand's own log records zero fallbacks across every traversal served over REST.

## D-024 [doc-gap] The envelope's `key` repeats a node's `node_key`

DESIGN's envelope table gives `key` as an envelope member on both element kinds — "echoes the producer's id" for a node, the derived hash for an edge — while the node base type declares `node_key` as producer-authored. A node read therefore carries the same string twice, once in the body and once in the envelope.

**Kept, deliberately.** Dropping `node_key` from the body would break the round-trip the envelope contract asks for ("a document read from the API can be sent back unchanged"): ingest addresses a node by a top-level `node_key`. Dropping `key` from the envelope would make the envelope a different shape for nodes and for edges, which is the one property it is defined by.

**Proposal:** none — but DESIGN should say the repetition is intended, because the alternative reading is that one of the two is a mistake.

## D-025 [impl] The remote embedding provider is built, without the egress policy ADR-0004 puts in front of it

D-103 deferred the remote plugin with the reason that *"building the plugin without that policy would be building the part that is easy to get wrong"*. It is now built anyway (`gears/graph-storage/remote-embedding-plugin`, behind the gear's `remote` feature), and the reason is a deployment fact rather than a change of mind: the reference assembly has to run where a model in the gear's process is not affordable — a memory ceiling, no CPU budget for inference — and a deployment that cannot embed at all is the option ADR-0004 rejects first.

- **What is built:** the `OpenAI`-compatible `POST /embeddings` protocol, batched, aligned by the response `index`, width-checked per vector, L2-normalized on this side; the credential named by environment variable rather than carried in configuration; the SDK's executable provider contract passing against a mock endpoint, as ADR-0004 asks ("remote via mock server").
- **What the identity can promise, and cannot:** the space is named by *model at endpoint at width*. That is the only identity a remote endpoint offers, and it cannot see a vendor changing weights behind a stable model name. The ONNX provider's content hash can; this one relies on model governance.
- **What is still not built:** the default-deny per-tenant egress policy (vendor, endpoint, region, data classes, vectorized fields, retention terms). Until it exists, selecting `remote` means every tenant's node text and query text leaves the deployment for the one configured endpoint, and the configuration file is the only record that it does. **A prototype trade, not a release posture.**
- **Proposal:** the egress policy is a platform concern shared with the LLM gateway (OAGW already models approved upstreams per tenant); the plugin should route through it once a per-tenant upstream can be resolved for embeddings, and the direct client here becomes the no-gateway fallback.

## D-026 [impl] The base-ontology schemas moved into the crate, because `cargo package` ships nothing outside it

The nine base schemas lived under `docs/schemas/` and reached the binary through `include_str!("../../../docs/schemas/...")`. That compiles from a checkout and fails to package: a crate's archive contains only its own directory, so the first `cargo publish` — or the first consumer building the gear as a git dependency — would have found no schemas. They now live in `gears/graph-storage/graph-storage/schemas/` (with the `acme` examples beside them) and DESIGN points there. The gear registers the same bytes; only the path changed.

## D-027 [was impl-gap, now built] Unchanged nodes were re-embedded on every ingest

`EmbeddingCoordinator::plan` composed and embedded every node of a batch before `decide_vector` compared the input hash with the stored one, so the *preserved* state was decided correctly but paid for as if it were *embedded*: a byte-identical re-sync of an 824-node repository through the reference consumer took 25 s with the in-process ONNX provider and changed nothing.

**Built.** `GraphStoreV1` gains `embedding_state(keys)` — the stored input hash and the epoch the stored vector is current under, index-aligned, unknown and unauthorized keys reading alike as `None`. The domain service reads it before planning and the coordinator embeds only the inputs whose hash or epoch differs, in one provider call; the rest are planned as `skipped`, which the store's `decide_vector` resolves to *preserved* as before. Covered three ways: unit tests on the coordinator with a counting provider, a conformance case (`an_unchanged_re_ingest_embeds_nothing`) run against both stores, and the studio-web stand.

**What the read outside the transaction costs.** A node changed by a concurrent writer between the state read and the write is planned as skipped and lands *stale* (vector kept, not rankable) until the next ingest touches it — the same state `embed: false` produces on purpose. The alternative, embedding inside the transaction, would hold row locks across a provider round trip. Accepted for the prototype; the readiness surface (D-109) should count stale rows when it exists.

## D-028 [impl-gap] Lexical search cannot find identifiers inside file names

`compose_search_text` joins the declared paths and the store indexes them with PostgreSQL's default text-search configuration, whose parser emits `README.md` and `rust-watch.Dockerfile` as single `file` tokens. On the stand, `Dockerfile` matched and `README` and `rust` did not, although both name files. The searchable text should also carry the punctuation-split tokens of a name (or the store should index a second, `simple`-configuration vector for identifiers). Producer-side, the reference consumer could add a `name_tokens` payload member, but the gap is in the composition every producer inherits.

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
filtering and the depth-3 neighborhood answer. Hybrid narrowing answers with
*meaning* only since the gear started computing its own vectors (D-103): while
they arrived from the producer, the vector arm ranked whatever it was handed
against whatever it was handed, and nothing tied the two to one model. The criteria table answers its
*alternative* flow (a filter on an unindexed attribute is refused, naming the
alternatives) but not its main flow, which is filtering by payload attributes
— see D-104. And **no scenario was timed against § 6.1 at all**: the stand
carries five nodes, not the seeded reference graph the criterion names, so the
thresholds are untested rather than met.

**4. Chain violations, endpoint-constraint violations and wrong-width vectors
rejected with structured per-item errors — *met*, after a fix.** Chain
violations come back as per-item field violations addressed by JSON pointer,
every violation in one response. The wrong-width vector this criterion names
is no longer a producer's to send: vectors are computed by the gear, and the
width check moved to what the provider returns, per batch, where the FR puts
it. A provider whose declared width is not the migrated one fails the boot
instead, which is the earlier and better place for a configuration error. Endpoint constraints were **not enforced at all** when this record was
first written: `src_types` and `dst_types` were resolved into the type's
effective traits and then never read, in either the validation or the write
path, so an edge between endpoints its type forbids committed. Fixed in
`101ffaa79`, which runs the check where DESIGN puts it — inside the ingest
transaction, endpoint rows locked — and defers it for a phantom endpoint to
materialization, per rule 3 of the Phantom Materialization Contract. Two
conformance cases, one per half, pass against both implementations, and both
halves were then exercised on the live stand: the forbidden edge comes back
`400` with `edges[0]/dst_node_key` and `SCHEMA_VIOLATION`, naming the
endpoint's actual type and the patterns the edge type accepts; materializing a
phantom under a type its accumulated edge forbids comes back naming that edge.

The stand also corrected the test. The phantom case first materialized under a
key the reference-node identity rule already refuses, so the store answered
before the revalidation ran — and both refusals are node-family item errors,
so the assertion accepted the wrong one. Caught only because the live run
printed the message. The case now uses a key the identity rule accepts and
asserts on the edge named in the message; removing the revalidation call makes
it fail, which is the check that the first version would have survived.

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

## Gaps this sweep found that no entry recorded

- **Scope replacement removes nothing** (criterion 2 above). The fencing is
  real; the replacement is not. *Open.*
- **Endpoint constraints were never enforced** (criterion 4 above). Parsed,
  stored, unread. *Fixed in `101ffaa79`.*
- **The `vector_search` trait was published and unread.** Resolved across the
  derivation chain, stored with the type's effective traits and served through
  `GET /types` with the description "Paths composed into the embedding
  input" — while nothing composed anything from it. A producer reading the
  catalogue was told the paths it declared did something. They now do; found
  while answering a question about how vectorization worked, which is to say
  by looking rather than by any test. *Fixed in `2b74dfef5`.*
- **`embedding_epoch` and `embedding_input_hash` were written `NULL` on every
  path.** Both columns existed from the initial schema and neither was ever
  populated, so the states `fr-embedding-pipeline` requires had nowhere to
  live. *Fixed in `2b74dfef5`.*
- **The conformance suite's vector width was its own invention.** It picked 8,
  the fake accepted it, and every case failed on the first real server with
  `expected 384 dimensions, not 8`. The suite now takes the width the schema
  was migrated with. Two implementations caught this; inspection had not.
  *Fixed in `c1da5fde3`.*
- **Two ONNX provider tests measured nothing.** `MiniLM`'s tokenizer pads to a
  fixed 128, so a same-batch comparison is padded identically either way and
  held with mean pooling ignoring the attention mask entirely — and so did
  `related > unrelated`, by 0.85 to 0.52, because 122 padding vectors make
  everything resemble everything. Masked, the same pair scores 0.72 to 0.02.
  Found by deliberately breaking the mask and watching the tests pass.
  *Fixed in `5c809f83b`.*
- **The domain service's own wiring was covered by nothing.** The service and
  the conformance suite each resolved a type's declared vector paths, and
  while they resolved it separately a service that passed no paths at all
  would have left every test green. *Fixed in `f3c781270`.*
- **Phantom creation in the PostgreSQL store worked only by accident.** It
  resolved the phantom node type from the types the batch itself named — and a
  producer never names it: the type is `x-gts-final` and authored only by the
  gear. Every phantom that ever appeared did so because some node in the same
  batch happened to carry that type. Found because the suite had no phantom
  case at all, which is itself the point: the fake and the store had diverged
  on a documented contract and nothing was watching. *Fixed in `101ffaa79`.*

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

## D-103 [was deferred, now built] Embedding pipeline and the in-process ONNX provider
`fr-embedding-pipeline` (PRD §5.4), `fr-embedding-dim-guard`, `fr-vector-search`, ADR-0005.

**What this entry used to record.** Embeddings were producer-supplied and dimension-guarded on ingest; `EmbeddingProviderV1` shipped as a trait with no implementation anywhere in the repository; the `embedding_space` table was deferred, so the identity half of `fr-embedding-dim-guard` was absent. That is no longer the state, and the entry is kept rather than deleted because what it was hiding is worth reading.

**Producer-supplied vectors were not a small deferral.** They are option **C** of ADR-0005 — the option it rejects, in the words *"nothing enforces that all producers and the query side use the same model — mixed-model vector spaces silently break similarity ranking, and the gear cannot embed query text at all without a model"*. The prototype had exactly that shape: `NodeSpec.embedding` and `SearchRequest.query_vector`, width-checked and otherwise unexamined. Two producers with different models would have ranked against each other with nothing to detect it.

**Built (`99e3c87bf`, `2b74dfef5`, `5c809f83b`):**
- The Embedding Coordinator (`domain/embedding.rs`), composing each node's input from its name plus the payload paths its type declares in `vector_search`, hashing it canonically, and calling the provider once per batch. Composition and embedding happen **before** the transaction, which is where DESIGN's ingest sequence puts them (step 5, ahead of step 6) and costs nothing: validation has already resolved every type record.
- The `embedding_space` table, and with it the identity half of `fr-embedding-dim-guard`. Boot compares the active provider's identity against the recorded one; on a mismatch the vector arm refuses (`EMBEDDING_SPACE_MISMATCH`, `failed_precondition`) instead of ranking across two spaces. Every other path is untouched, because only vectors are incomparable.
- The four vector states, on the `embedding_epoch` / `embedding_input_hash` columns that were previously written `NULL` unconditionally.
- Both producer-facing vector fields removed; `options.embed` added in their place.
- The in-process ONNX provider (`gears/graph-storage/onnx-embedding-plugin`), behind the gear's off-by-default `onnx` feature, verified against `all-MiniLM-L6-v2` rather than only compiled.

**Still deferred, and now the whole of what is left:**
- **The model-change lifecycle.** `requested → scanning → embedding → validating → cutover → complete`, its administrative API, the resumable backfill and per-tenant progress. Nothing opens a second epoch: a boot that finds a different identity reports it and blocks the arm, which is the safe half of the lifecycle without the recovery half. The gear has no background task of any kind, so this is a new capability rather than a missing branch.
- **The remote provider and its egress policy.** ADR-0005 requires a default-deny per-tenant policy over vendor, endpoint, region, data classes and vectorized fields before node text or user queries may leave a deployment. Building the plugin without that policy would be building the part that is easy to get wrong. *Superseded by D-025: the plugin is built; the policy is not.*
- **Chunk embeddings.** The `chunk` table is deferred (D-100), so "and every content chunk" has nothing to embed, and the "bounded content prefix" of the composed text is a prefix of name-plus-attributes only.
- **The readiness surface** that should report the active identity and dimension. The *behaviour* the FR asks for is enforced — at boot, and per request on the vector arm — but there is nowhere to read it from (D-109).

## D-104 [deferred] Index-activation lifecycle, dynamic index DDL — and payload filtering entirely
`fr-index-admission` (PRD §5.1), ADR-0003 point 5 (`requested → building → active`, `CREATE INDEX CONCURRENTLY` worker, DDL queue keys). No runtime DDL, as recorded.

**Corrected after checking.** The first draft said `$filter` over `index`-trait paths "is admitted only for paths covered by the static migration-time indexes", which implies payload filtering works for some paths. It works for none: the filterable-field schema declares `node_key`, `name`, `created_at` and `updated_at`, and nothing else, so `$filter=payload/severity eq 'critical'` is refused outright. Verified through the API. `index` trait paths *are* stored with the type's resolved traits — they are simply not wired to the filter surface, which is a larger gap than "no runtime DDL" and worth stating as its own deferral: **payload attributes are stored but not filterable at all**.

Checking this also found the rejection to be misclassified as `out_of_range`/`LIMIT_EXCEEDED`; fixed in `05695faa0`.

**Why it is not merely deferred: the platform binding blocks it regardless of what this gear builds.** Wiring the `index` trait to the filter surface is impossible in the platform OData binding as it stands, and no amount of index work changes that. Two places. `FilterField` declares its members as `const FIELDS: &'static [Self]` (`libs/toolkit-odata/src/filter.rs`), so the filterable set is fixed when the gear compiles — while a declared path belongs to a tenant's ontology and a type version, and its admissibility depends further on whether its index is active. And `FieldToColumn::map_field` returns a `Column`, with the predicate assembled as `Expr::col(column)` (`libs/toolkit-db/src/odata/sea_orm_filter.rs`), so a field must *be* a column; an extraction expression over one has nowhere to go. Both would be additive to fix: a field set resolved per request carrying its field kind, and a `map_field_expr(F) -> SimpleExpr` defaulting to today's behaviour. This entry is therefore part deferral, part platform gap.

**Two things the ADR left to the reader, and the projection cannot be built without either.**
- *A declared path needs a B-tree over its extraction expression, not a GIN over the payload.* Both are "a JSONB index", which is why it is easy to miss. The projection orders and paginates by keyset, so `$filter`, `$orderby` and the cursor all need a total order over the filtered field; GIN answers containment and existence. One GIN covers equality over every path and ordering over none — so the v1 prototype's single GIN was not a cheaper form of the decision but a much weaker one, admitting `eq` and leaving every ordered projection to a full scan and a sort with nothing to say so.
- *A declared path needs a resolved scalar type.* The trait is a list of pointers carrying no type, which is right — the pointer already points into the type's own schema. But the resolution must be explicit, because the index expression needs the cast (`->>` yields `text`), comparison needs the semantics (`'10' < '9'`), the cursor codec needs the field kind, and something must happen when a payload holds an object where the path was declared scalar. Registration should reject a path that does not land on a scalar rather than build an index nothing will use.

- **Folded into the docs:** ADR-0003 now names the index kind in the decision itself, carries both findings as consequences marked "Found while building the prototype", and gains a "What payload filtering needs from the platform" section in the form ADR-0006 uses — two numbered asks, recorded as *unraised*, because the lifecycle they would serve is unimplemented and there is nothing yet to measure a proposed signature against. DESIGN's trait table row now says B-tree over the path expression, `$filter` **and** `$orderby`, scalar required. Published in `215380db9`.
- **Not changed, deliberately:** the `index` trait's own `description` in the base node schema still says "backed by a JSONB index and admissible in `$filter`". Correcting it is a base-schema edit, which has no delivery path on an existing database — see D-013.

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
