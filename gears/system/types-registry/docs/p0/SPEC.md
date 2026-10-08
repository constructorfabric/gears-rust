# SPEC — Types Registry P0

Derived from [DESIGN.md](../DESIGN.md), [PRD.md](../PRD.md), [database.sql](../database.sql).
P0 is a scope cut of Product P1, not a separate design: every decision below either
implements a DESIGN clause or records an explicit, named deviation from one.

Status: **approved**, including the P19/P20 read-scope revisions, the P22 out-of-process
revision (D11, D15–D17; decision and rationale in [`plan.md`](./plan.md) P22) and the P23
publication-ordering revision (D18–D22, C12–C13; rationale in [`plan.md`](./plan.md) P23)
and the P25 one-plane-per-route revision (D17, D20; rationale in [`plan.md`](./plan.md) P25),
plus the P26 early-client scheduling revision (accepted and consolidated 2026-10-07: T26–T42, D11's dependent configured entities, §8.4's early handoff).
The live execution
artifacts are [`plan.md`](./plan.md) and [`todo.md`](./todo.md); where this document and
the plan disagree on *ordering*, the plan
wins (it supersedes §15 — see `plan.md` P1). Where they disagree on a *decision*, that is a
bug in this document: report it rather than picking one.

---

## 1. Objective

Turn Types Registry from an in-memory registry into a durable one, and replace its
SDK with a contract that survives the move.

Three deliverables:

1. **Persistence** — admitted entities live in the platform database, survive restart,
   and are visible to every pod.
2. **Registration API (Rust SDK + REST) for global entities** — no tenant ownership.
   The current SDK trait cannot express the new write path and is replaced. The new trait
   is a toolkit contract served in process and over REST, so it works unchanged when
   types-registry runs as its own binary in its own pod (D15, §8.4).
3. **Materialized effective artifacts** — `resolved_schema`, `effective_traits`,
   `effective_traits_schema`, `resolution_fingerprint` are computed at admission and
   stored, so the P1 read path needs no later backfill.

**Users:** platform and domain gears (via `ClientHub`), operators and CI (via REST).

**Success looks like:** a gear registers its Type Schemas at startup, whether or not it runs
in the registry's process; the process is restarted, and every entity — with its resolved
artifacts — is still there, identical, without re-registration.

---

## 2. Scope

### In

| Capability | DESIGN reference |
|---|---|
| Durable storage of managed entities, revisions, current-state projections | §3.7 |
| Asynchronous admission: `202` + operation + `Idempotency-Key` + outbox + worker | ADR-0012, §3.2 *Acceptance path* |
| Dependency-aware partial admission (topological order over an acyclic relation) | §3.2 |
| Optimistic concurrency on the logical entity (`resource_version`) | ADR-0005, ADR-0006 |
| Immutable retained revisions | ADR-0005, ADR-0006 |
| Version families: ownership row, kind/shape/contiguity rules | ADR-0004 |
| Dependency edges (`$ref`, derivation, instance_of) — written **and** read | §3.2 *Dependency Graph* |
| **Upgrade `gts-rust` 0.11.0 → 0.12.0** across the workspace (§7) | `constraint-gts-implementation` |
| BACKWARD compatibility against one baseline, tri-state verdict, undecided **rejected** | ADR-0003, `principle-fail-closed` |
| Per-level content-model classification as a compatibility input, reported by Dry Run | ADR-0003 |
| Derivation-chain validation, Draft-07 dialect gate, ADR-0015 major-0 quarantine | ADR-0014, ADR-0015 |
| Dependent revalidation + effective-artifact refresh via reverse worklist | §3.2, §4 |
| Deletion with `expected_resource_version`, blocked by direct registered dependents | §3.2 |
| Lifecycle `ACTIVE` / `DELETED`, tombstones retained | ADR-0008 |
| Dry Run as a mode of registration and deletion | `fr-dry-run` |
| New SDK trait + REST surface for the above | §3.3 |
| `$select` field projection on exact read, `batchGet` and discovery; one document-free default | §3.3 *Field selection*, plan P19 |
| Discovery filtering by maximum GTS chain `depth` and entity `kind`, composed with `pattern` before pagination | §3.3 *GET /entities*, plan P20 |
| SDK reconciliation of explicitly supplied documents, and per-gear publication of declared GTS entities after wiring | §3.3 *Inventory and startup reconciliation*, D11, D16 |
| Out-of-process operation: the SDK trait as a toolkit contract, a hand-written REST client resolved through the directory, and `PlatformSecurityContext` on every method | toolkit-oop ADR-0001/0002/0005/0006, D15 |
| Per-crate GTS declaration collectors replacing the process-global inventory; a toolkit post-wiring hook and generic readiness contribution | D16, plan P22 |
| One plane per route: the full operation set on platform routes under `/types-registry/platform/v1/`, which require a validated internal token on every host, and the entity reads on tenant routes under `/types-registry/v1/`, which require a validated bearer | toolkit-oop ADR-0006/0008, D17, D20, C8 |
| Publication ordering: a per-entity `publisher_name` + `publisher_version` stamp that stops an older release of the publisher from overwriting what a newer one published, checked at commit, with `publisher` required on every mutation | D18, plan P23 |
| Startup that does not depend on a published or reachable registry: every startup phase audited, publication supervised, `Required` readiness from publication | D21, plan P23 |
| Three backends: SQLite, PostgreSQL, MySQL | `constraint-multi-backend` |

### Out — deferred, not redesigned

| Deferred | Why out |
|---|---|
| Tenant ownership, visibility, tenant plane | User decision. Columns are kept, never populated with scope=2 |
| `publisher_name` bound to the caller's identity, and exposed on reads | P1 [#4628](https://github.com/constructorfabric/gears-rust/issues/4628), through registry-side workload policy over the validated `PlatformIdentity` (ADR-0006/0008; the platform plane bypasses the tenant PDP). P0 takes `publisher` on the wire as **cooperative** publication input from the platform plane or the local client only (D18), and does not bind it to the authenticated workload (C3) |
| A separate platform listener | Toolkit/api-gateway work (ADR-0006/0008). Per-workload narrowing on the platform plane is registry-side workload policy, never a PDP (C3). P0 validates the platform identity per route — on the gear's own listener and at the gateway (D17, D20, C8) — and records no principal (C2) |
| gRPC transport for the SDK | REST is the toolkit default for OoP (ADR-0002) and was chosen (D15) |
| Attested release authority, an owner-wide fence across identifiers, and a `publisher_version` override | D18 orders publications by the version the publisher declares; it does not attest it (C11), it orders identifiers one at a time (C12), and a newer release is the only way to supersede an older one |
| Release-pinned schema reads: revision-addressed reads or pinned resolved snapshots | P0 reads the current revision only; a minor-bearing identifier is immutable as authored, but its resolution closes over major-only references that still move (D22) |
| `ReportOnly` publication readiness, a runtime readiness override, `#[consumes(readiness = optional)]` | No P0 gear needs to serve without the registry; `ReportOnly` alone is useless while a required consumed dependency still holds the process (plan P23) |
| A persistent read cache, stale-if-error and startup without a reachable registry | The SDK cache is in memory and fails closed after its window (§8.3, C13). Profile 3 requires a highly available registry instead (§8.4) |
| PDP / `PolicyEnforcer`, read & write grants, declared permissions | Depends on the deferred identity-to-permission binding (§4 DESIGN) |
| Federation: `source_claim`, the `routing` coordination state, Registry Source Plugins, Control-Plane Validator | Whole subsystem |
| Availability Evaluator, `tenant-resolver` dependency | Needs tenancy |
| Validator inputs that only a tenant or external read has: subject visibility-chain version, Context Tenant availability-chain version, routing generation, `external_revision` | Each is `tenant plane only`, availability-conditional or external, so **none participates in a platform-plane read** — the validator itself is in P0, see §8.5 |
| Availability, reason and Context Tenant ownership-view fields in `$select` | Those values need the tenant availability and visibility work deferred to P1. P0 still supports caller-chosen selection over its managed field allowlist (§10.2, plan P19) |
| `expand_type_filter` | Its DESIGN definition *is* `$select=gts_uuid&availability=available` (plus explicit `lifecycle_status=active`), with the availability filter fixed by the method rather than supplied by the caller. Availability is out of P0 (needs tenancy), so a P0 method of that name would report retired contracts as usable — a same-named different meaning, which is worse than absence. Paging `list_entities` directly is available to any caller that wants the traversal |
| Operator purge job, operation-retention sweep | ADR-0013, §3.2 — no P0 consumer |
| Aliases, Validation Hooks, casting, tenant enablement | P2 in DESIGN |

### Explicitly *not* simplified

Input validation at the admission boundary, idempotency uniqueness as a database
constraint, and compare-and-swap on every update stay in full. They are the
correctness core, not scope.

---

## 3. Decisions locked in review

| # | Decision | Consequence |
|---|---|---|
| D1 | **Async write path per DESIGN** — `202` + operation UUID + polling | `operation` / `operation_item` tables, `toolkit-db` outbox, admission worker. Kept as a contract that does not break when revalidation later becomes genuinely unbounded, even though the P0 worker completes in milliseconds |
| D2 | **A transient `gts-rust` store per admission unit**, built from the database; reads are served from the database | Resolution, compat, chain validation and derivation need a `GtsStore`, so one is built from the unit's dependency closure and dropped after the unit. Reads need rows, not a store (§8.2), so nothing is held between units. Commit re-verifies under locks |
| D3 | **Materialize effective artifacts** | `type_schema` current-state row is populated at admission. Read path shape identical to P1, no later backfill of `resolution_fingerprint` |
| D4 | **Multi-pod** | Commit transaction re-reads `resource_version` of the candidate and the revision vector of everything consumed, and re-derives the reverse-impact set from the database; a difference rolls back and revalidates within `worker.max_revalidation_attempts`. The comparison is **not** taken under locks on the compared rows — §8.1 step 4.2 argues why locking the vector is the wrong tool and what serializes those rows instead. Ordering is provided by the **`entity_write_order` row** of `types_registry__coordination_state`, advanced as each commit transaction's first statement: one commit at a time per installation, which is what lets the in-transaction scan and guard see each other's work. A row rather than an advisory lock because advisory keys live on a separate session that can be lost while the transaction carries on. Every writer of entity state must claim it — deletion at T20, the purge job under ADR-0013 |
| D5 | **Reverse impact by one scoped recursive CTE, refresh by an iterative loop** | `DependencyRepo::reverse_impact` is a single `WITH RECURSIVE` over `dependency` through `SecureCteSelect` (ADR-0001, `toolkit-db`) — no raw SQL — depth-capped at `limits.activation_write_set`, which is also the refusal threshold for the set it returns. The refresh stays a domain loop because the fingerprint-stability stop decides the write set by recomputation, which no closure query can express. See [§D5](#d5-splits-the-traversal-from-the-refresh) |
| D6 | **The old `TypesRegistryClient` is removed in P0**; every consumer migrates inside this effort | ~50 call sites across 20+ gears move. Forced by two facts: async admission makes the old synchronous `register()` a lie in its own signature, and the old models' `Arc`-linked object graphs cannot cross a wire, so keeping them keeps an out-of-process blocker. Migration is split by gear group — see `plan.md` P5. Under P23 (variant C′) the API move is one mechanical change inside the cutover (T31), and the behavioural startup move is per gear (T35–T37) |
| D7 | `operation.plane = 1` (platform), `tenant_id = NULL`, `principal_id` a hardcoded constant with a `TODO` | Idempotency scope becomes global — see §9 ceiling C2 |
| D8 | **`gts`, `gts-id` and `gts-macros` are pinned at 0.12.0 and move together** | A split pin puts the identifier crate and the semantics crate on different specifications. `gts-dylint` / `gts-macros-cli` must not lag either — see §7 |
| D9 | **Use `toolkit-db/preview-outbox`** | Closes DESIGN §4's outbox sign-off for this gear |
| D10 | **`POST /entities` breaks**: `200` + results becomes `202` + operation | No compatibility path on that route. The gear's REST stability is `unstable`; the break is called out in the changelog |
| D11 | **Each owning gear publishes its declarations; registry pull ends in P0** | T31–T37 temporarily seed linked inventory and `cfg.entities` through the outbox; every inline seed must succeed or be unchanged before client publication. T37 retains only registry types, `toolkit-gts` bases and independent configured entities. A configured entity with a dependency outside the inline seed set is published by types-registry after wiring, gating only its consumers, to avoid a cold-database cycle (D21, T34); while the pull lasts an embedded host's inline set covers such items, so they stay inline there. Invalid configuration envelopes still fail boot synchronously. Reconciliation is T24; attribution remains C3. |
| D12 | **`GET /entities` becomes a bounded page with a cursor, document-free by default** | The old shape returns every match with full `content` in one response. A `limit` without a cursor would make the endpoint incomplete, so both land together. P19 adds `$select` on this page and the two exact-key routes; selected documents are explicit and the discovery cursor binds the normalized selection (§10.2) |
| D13 | **P0 field projection on all three reads** (plan P19) | An absent `$select` means the same document-free managed metadata set on exact read, `batchGet` and discovery. P0 selects only fields it can answer; documents are flat and individually selectable. One normalized set drives SQL retrieval, cursor identity, T22d validators and T28 cache keys (§10.2) |
| D14 | **Discovery filters by `pattern`, `depth`, `kind` and `lifecycle_status`, all exact SQL before `LIMIT`** (plan P20) | `depth` is an inclusive maximum GTS chain length; `kind` is `type_schema` or `instance`; `lifecycle_status` defaults to `active`. Admission materializes `entity.chain_depth` and one `entity_gts_segment` row per parsed segment; the repository compiles the `gts-rust`-parsed pattern into one join per constrained segment and fetches `limit + 1`, so only the last page is short. The cursor binds the filters so continuation cannot splice different result sets (§8.2, §10.2) |
| D15 | **The SDK is an OoP toolkit contract** (P22) | `PlatformTypesRegistryApi` takes `PlatformSecurityContext` on every method; semantic models are serde-free with separate wire DTOs. A hand-written REST client preserves required headers/statuses and uses runtime helpers plus `DirectoryResolvingClient`; the server uses `OperationBuilder`. Every profile resolves the same ClientHub trait (§8.4, §10.1). |
| D16 | **Per-crate declarations, published by the owning gear after wiring** (P22) | `declare_gts_inventory!()` / `gts_declarations()` replace global inventory. `gts(crates = […], publisher = …)` records ownership and gathers declarations in the post-wiring hook. The named publisher reconciles in the background and gates readiness. Toolkit defines the signature and a generic `ReadinessStatus` without naming the SDK; `PublicationStatus`, `PublisherContext` and `PublisherVersion` live in `types-registry-sdk`, which implements `ReadinessStatus` for its status, and `toolkit-gts` stays free of publication types (T23, T29). Linking publishes nothing; collectors avoid GTS version splits (§8.4). |
| D17 | **One plane per route** (P22, P25) | Platform API: full operations under `/types-registry/platform/v1/`, `.platform_authenticated()`, validated internal token on every host. Tenant API: three entity reads under `/types-registry/v1/` (`/v2/` until T32), `.authenticated()`, validated bearer with per-hop revalidation. Every presented credential must be validated; an unavailable plane fails closed. No route accepts either plane. Principal recording and authorization remain C2/C6; listener separation remains C8 (§8.4). |
| D18 | **Commit-time per-entity publisher ordering; required `publisher`** (P23) | Global mutations require `publisher: { name, version }`; missing metadata is `400` before acceptance. Gears supply their own name and `CARGO_PKG_VERSION`, without override. Under `entity_write_order`, before preconditions and on both `unchanged` paths: another name → `publisher_mismatch`; lower SemVer → `superseded`; equal/higher → admission and CAS; higher identical content → metadata-only confirmation. Success claims unclaimed rows (empty version, placeholder name). Content/stamp commit atomically; confirmation changes no revision, resource version, timestamp, artifact or validator. Input comes only from local/platform callers and is cooperative, not attested (C3/C11). Rename, field and guard land in T39–T42 without activation or adoption machinery (§8.4). |
| D19 | **SDK mutations return a read-back operation** (P23) | Both adapters submit then call `get_operation`: receipts lack items, including terminal replays, and cannot represent success. Read-back errors carry `operation_id` for same-key replay. REST handles `Retry-After` internally with an SDK fallback; operation models are unchanged. |
| D20 | **Platform-only mutations on every host; HA in Profile 3** (P23/P24) | Mutations require validated internal tokens, including at the embedded gateway; missing internal auth fails closed. Operators/e2e use platform tokens (Profile 1 configures gateway `internal_auth: shared_secret`). Platform OpenAPI declares `internalToken`; discovery excludes it because the edge strips tokens. Distinct tenant/platform prefixes prevent proxy path overlap. Profile 3 requires two stateless registry replicas and a disruption budget (§8.4). |
| D21 | **Registry-dependent work never fails startup** (P23) | T31 audits `init`, `post_init`, `start`, bootstrap and plugin selection; dependent steps become supervised tasks awaiting prerequisites. Publication readiness is `Required`: pending/rejected/superseded-deleted hold it; admitted and superseded-live satisfy it. Admission is sticky; supersession warns and emits stored/offered versions. Older-reader compatibility is N−1 release work (D22). `ReportOnly`, overrides, optional consumption and stricter supersession are P1 (O6). Readiness remains per process. T34 prevents caching while a locally registered same-contract/vendor instance is invisible (§8.4). |
| D22 | **Current registry content defines the global contract** (P23) | Validation, discovery and plugin selection use current snapshots within cache freshness rules; each pod keeps its own decoder/serializer. Local schemas may validate private APIs only. BACKWARD protects older writers; mixed-version tests must prove older-reader support. Release pinning requires revision-addressed reads or resolved snapshots, outside P0. |


### DESIGN amendments named by P22 and P23

DESIGN and `database.sql` incorporate P22/P23: SDK contracts, collection/publication,
read-back operations, ordering, readiness, schema authority, cache generations and HA.
The remaining deviation is interim listener separation:

| DESIGN | P0 | Why |
|---|---|---|
| Planes are separated by listener; platform callers are authenticated workloads (DESIGN §3.3 *Platform REST contract*, ADR-0006/0008) | One listener per process; the planes are separated per route and by path — platform routes under `/types-registry/platform/v1/`, tenant reads under `/types-registry/v1/` (D17, D20) | No platform listener exists yet (C8); the platform prefix is what moves to it, with no path change |


---

## 4. Two DESIGN prerequisites this closes with numbers

DESIGN §4 lists eight implementation prerequisites. P0 closes two of them and must
record how.

**A per-transaction lock timeout in `toolkit-db`.** The write path serializes on the
`entity_write_order` row (§8.1), so every commit queues on a database row lock whose wait only the
backend bounds — and `PostgreSQL`'s default is unbounded. The gear cannot bound it itself: it
issues no raw SQL outside migrations, and `tokio::time::timeout` around the claim is not a
bound, since cancelling the future leaves the statement running and the rollback queues behind
it. What closes this is `TxConfig` gaining a backend-aware lock timeout — `SET LOCAL
lock_timeout` after `BEGIN` on `PostgreSQL`, the session variable set and restored on `MySQL` —
together with contention classification for `55P03` and `1205` so the refusal keeps a shape a
client can branch on. Until then the bound is a deployment setting and this gear documents it
as one.

**Unbounded activation write set.** DESIGN asked for either a permitted
transaction-size/timeout profile for that atomic write or a generation/staging protocol,
"short transaction" alone not being a bound. P0 answers with the first, and DESIGN §3.2
now carries the bound as deployment configuration while §4 keeps only the staging upgrade
path open.

Measured over every chained GTS identifier declared in the repository, the largest
reverse-impact set of any base type is **27** (`gts.cf.core.events.event_type.v1~`; next:
`topic.v1~` 21, `event.v1~` 18, `errors.err.v1~` 14). A revision of the hottest base type in
the platform therefore refreshes ≤ 28 `type_schema` rows in one transaction. The bound below
is set against **27**, not against the size of the declared population — a re-count of the
latter does not move it.

**P0 profile: single transaction, no staging.** Configured bound
`limits.activation_write_set` = **512** entities in the reverse-impact set — the set the
walk returns, before the fingerprint filter decides which of them are rewritten. The
written set is a subset of it, so one number bounds both, and it is the walked set the
bound is *checked* against because that is the number the commit knows before it writes.
Exceeding it fails the candidate with a structured reason rather than committing a partial
refresh. Upgrade path when the bound is reached: the generation/staging protocol DESIGN
describes. Not built.

**Parameterized recursive CTE in `sea-query`.** **Closed, and by verification rather
than by dissolution.** `toolkit-db`'s `SecureCteSelect::recursive_cte` (ADR-0001) builds a
scoped `WITH RECURSIVE` through the typed builder with no raw SQL, and the emitted shape
executes on all three backends — `toolkit-db`'s own `cte_shapes_are_valid_sql_{sqlite,
postgres,mysql}` plus this gear's `reverse_impact_walks_back_up_a_chain`, which runs in
the `PostgreSQL` and `MySQL` container suites.

Two of DESIGN's constraints on the query are honoured as stated and one is not. `UNION`
rather than `UNION ALL`: kept, and it is the builder's default. No per-row accumulator
that would defeat deduplication: **not** kept — the builder emits a mandatory depth
column, because a cap has to be expressible as a predicate on the recursive member. The
consequence is that a node reached at two depths is expanded twice, which bounds
re-expansion at *(rows × depth)* rather than by path count, and the cap is what makes it
finite. `database.sql`'s early stop is unaffected and still sanctions the refresh loop:
*"The traversal reaches the subject anyway, recomputes it, finds an identical digest,
and stops there."*

The `toolkit-db/preview-outbox` sign-off is closed: outbox support is unconditional in
`toolkit-db` 0.12.0. Still open and inherited: worker liveness bounds. The ADR-0015
quarantine preflight is **satisfied by construction** — see §17, O4.

---

## 5. Tech stack

Rust edition 2024, toolchain ≥ 1.95.0. One new third-party dependency: `lru`, for the
store-build cache. Everything else comes from the workspace.

| Concern | Choice |
|---|---|
| GTS semantics | `gts` / `gts-id` / `gts-macros` **0.12.0** — **sole** source, no local approximation (`constraint-gts-implementation`). Upgrade from 0.11.0 is part of this task, §7. Discovery's SQL pattern compiler mirrors the `gts-id` matcher under differential tests (D14) |
| Persistence | SeaORM via `toolkit-db` `DBProvider`, `sea-orm-migration` |
| Async dispatch | `toolkit-db` outbox, leased mode, table prefix `types_registry__outbox` |
| REST | Axum via `OperationBuilder`, utoipa, RFC-9457 problem details |
| Errors | `toolkit-canonical-errors` `CanonicalError`, one `From<DomainError>` ladder |
| Shared state | none held between admissions — reads go to the database, the `gts-rust` store is transient per admission unit (§8.2, D2) |

New dependencies for `cf-gears-types-registry`: `toolkit-db`, `sea-orm`,
`sea-orm-migration`, `time`, `aws-lc-rs` (the FIPS-validated SHA-256 the platform
installs at bootstrap, in place of the DE0708-banned `sha2`), and `lru`. The `toolkit`
outbox needs no feature: it stopped being a preview in toolkit-db 0.12.0 and is now
unconditional.

Gear capabilities change from `[system, rest]` to **`[system, db, rest, stateful]`**
(`stateful` for the outbox worker lifecycle). Precedent: `credstore` runs
`[system, db, rest, stateful]`.

---

## 6. Commands

```bash
make dev                 # fmt + clippy autofix + tests — the daily loop
make ci                  # full gate: fmt, clippy, test, deny, dylint, lychee

cargo test -p cf-gears-types-registry
cargo test -p cf-gears-types-registry-sdk
make test-types-registry-db  # this gear's PostgreSQL + MySQL container suites

make dylint              # architecture lints — DE01xx/DE02xx/DE08xx apply here
make e2e-local           # REST surface end to end
```

Definition of done for any slice: `make ci` green, the plain gear tests green on SQLite,
and `make test-types-registry-db` green on PostgreSQL and MySQL. SQLite-only is not
sufficient — `constraint-multi-backend` is a correctness requirement, and the lock and CAS
paths differ per backend.

---

## 7. The `gts-rust` 0.12.0 upgrade

Published `gts` 0.11.0 was missing three capabilities DESIGN §4 requires, and
`constraint-gts-implementation` is categorical that a missing behaviour is *"a change
request against `gts-rust`, not a local approximation."* **0.12.0 supplies all three**,
so the upgrade is a scope item of this task rather than a follow-up, and no deviation
has to be recorded.

Verified against `gts-rust` 0.12.0:

| DESIGN §4 prerequisite | 0.11.0 | 0.12.0 |
|---|---|---|
| 1. **Tri-state verdict**, undecided distinct from incompatible | ❌ bare `bool` | ✅ `CompatibilityVerdict::{Compatible, Incompatible, Unknown}` |
| 2. **Per-level content-model classification** after resolution | ❌ absent | ✅ `ContentModel::{Open, Closed, Partial}`, `classify_object_levels() -> Vec<ObjectLevel>` |
| 3. **Partially open level reported as such**, not forced into a verdict | ❌ absent | ✅ `ContentModel::Partial`; `is_evolvable_in_place()` is `false` for it *"rather than guessed"* |
| 4. **Per-content-model property add/remove**, both directions | ❌ absent | ✅ `CompatibilityFinding::{PropertyAdded, PropertyRemoved, …}`, directional `check_backward_diagnostics` / `check_forward_diagnostics` |
| 5. **Checker spec + impl versions**, persisted on admission | ⚠️ | ✅ `GTS_SPECIFICATION_VERSION = "0.13"`; impl version is the crate version |
| 6. **Document-level comparison that resolves both sides** | ❌ compared unresolved | ✅ `GtsStore::compare_documents() -> SchemaComparison` resolves both, then compares |
| 7. **Registration-policy matching properties** | ✅ | ✅ pinned by the three `gts-id` tests DESIGN cites by name |
| 8. Pattern containment for Source Claim overlap | — | out of P0 scope (federation) |

Three things this settles.

**`SchemaComparison` is the entry point.** DESIGN §3.2 asks the Compatibility policy to
*"compare resolved effective schemas through the document-level compatibility entry
point and reject an indeterminate verdict."* That is `compare_documents` exactly:
it calls `resolve_schema_refs` on both sides and returns `backward_diagnostics`,
`forward_diagnostics` and `candidate_object_levels`, with verdicts recomputed from their
evidence rather than stored beside it.

**`Unknown` must be rejected.** 0.12.0's own doc comment says *"The caller, not this
library, decides how that affects admission."* P0 decides: `Unknown` fails the candidate
with a distinct structured reason, separate from `Incompatible`. That is
`principle-fail-closed` — *"undecidable compatibility is rejected"* — and it is now
implementable rather than a ceiling.

**`candidate_object_levels` is what Dry Run reports.** ADR-0003 wants the level that
prevents admission, and `ObjectLevel` carries the path (`$`, `$.payload`, `$.items[]`).
One document-wide flag would not do — as 0.12.0 notes, in the closed-envelope shape the
deciding level sits inside an extension container, not at the root.

**Do not use `GtsStore::is_minor_compatible`.** In 0.11.0 it compared `old_ent.content` /
`new_ent.content` — the *unresolved* authored documents, violating prerequisite 6. Use
`compare_documents`.

### Upgrade risk: the API is additive, the semantics are not

The `pub use` surface is additive — `schema_evolution` and `SchemaComparison` are added,
nothing is removed. The symbols this monorepo actually uses are a narrow, stable set
(`GtsId::new`/`try_new`/`to_uuid`, `GtsTypeId`, `GtsInstanceId`, `GtsIdPattern::try_new`,
`GtsConfig`, `GtsOps::add_entity`/`validate_entity`, `validate_schema`, `try_narrow`),
and nothing in the repository calls the two moved/deprecated compatibility helpers. The
mechanical part of the upgrade is therefore small.

The behavioural part is not. Between 0.11.0 and 0.12.0: *"fix: Align schema compatibility
with JSON Schema semantics"*, *"fix: correct directional schema compatibility checks"*,
*"fix: localize unprovable schema intersections"*, *"fix(traits): Stop materializing
const values"*, *"fix(macros): Preserve explicit additional properties models"*,
*"fix(gts-id): distinguish v0 from wildcard versions"*.

Two of these reach beyond Types Registry. The macro fix changes generated schema
documents, so `#[gts_type_schema]` output may differ; the traits fix changes what
`x-gts-traits` materializes. **Every declared GTS identifier in the repository must be
re-validated under 0.12.0 before the upgrade lands** — this is T1, and it is the one task
whose failure mode is other gears rather than this one. The `gts-id` v0 fix
works in our favour: it makes the ADR-0015 major-0 quarantine (§8.1 step 7) exact.

`gts`, `gts-id` and `gts-macros` move together — the workspace pins all three, and
`gts-dylint` / `gts-macros-cli` must not lag.

### Sourcing it (D8)

The workspace pins all three crates from crates.io:

```toml
# Cargo.toml
gts-id     = "0.12.0"
gts        = "0.12.0"
gts-macros = "0.12.0"
```

**Verification of the upgrade:**

| Check | Result |
|---|---|
| `cargo check --workspace` | clean, no errors |
| `cargo test -p cf-gears-types-registry` | 209 passed, 0 failed |
| `cargo test --workspace` | **10260 passed, 0 failed, 611 ignored** (most ignores need Docker/testcontainers) |
| `make gts-docs` | 798 files, 0 errors |
| `Cargo.lock` | `num-cmp` is the one transitive dependency 0.12.0 introduces |

Toolchain is not a problem: `gts-rust` requires 1.95.0 and this workspace is already on
1.97.0 with `rust-version = "1.95.0"`. (CLAUDE.md's "minimum toolchain 1.92.0" is stale.)

---

## 8. Architecture

### 8.1 Write path

```
POST → acceptance (synchronous, reads no entity state)
     → one transaction: insert operation + operation_item(s) + enqueue outbox(operation UUID)
     → 202 + operation UUID + Location + Retry-After
                                            ↓
                          leased outbox → admission worker
                                            ↓
     transient store from the unit's dependency closure → evaluate (outside any transaction)
                                            ↓
        per admission unit: short transaction with database rechecks + writes
```

**Acceptance checks, in this order** (DESIGN §3.2; steps 4 and the tenant half of 3 are
out of scope):

1. Envelope and batch size — refuse > `limits.batch_candidates` (100).
2. Candidate identifiers — refuse non-canonical GTS identifier, or duplicate within batch.
3. Registration policy — for a declared creation, the candidate's last-segment vendor
   must be admitted for its region (`allowed_vendors`; the region's `tenant_ownable`
   parameter is inert in P0 — §10.3). Closed by default; global `cf` is implicitly
   admitted. Revisions and deletions bypass this gate.
4. Managed identifier profile — refuse an explicit UUID tail (ADR-0001); refuse a minor
   or major 0 in the **last** segment of an Instance identifier (ADR-0004, ADR-0015).
   A minor on a Type Schema identifier is admissible under any prefix.
5. Declared identity and dialect, Type Schema candidates — the item's `gts_id` is the
   entity's identity, and the document must agree with it: top-level `$id` is exactly
   `gts://<gts_id>`, with no trimming, case folding or bare `gts.` spelling. An absent
   or non-string `$id` is refused as `missing_schema_id`; any other string, malformed or
   naming another entity, as `schema_id_mismatch`, without echoing the declared value.
   Both are `400` field violations on `entity` with reason `VALIDATION_FAILED`, before
   any operation exists, so one such candidate refuses its whole batch. Instances are
   not checked: their identity is the item's `gts_id` alone. Then top-level `$schema`
   present and in the closed Draft-07 spelling set; any `$schema` below the root must not
   differ (ADR-0014).
6. `force` per candidate — require `allow_compatibility_force` and a waivable
   cross-minor baseline from `compat::select_baseline`. Intra-entity revisions are
   never waivable. Store the request on `operation_item.compat_forced`; each worker
   pass rechecks the deployment setting and baseline eligibility. The revision's
   `compat_forced` records the effective waiver.
7. ADR-0015 quarantine — the worker refuses stable candidates whose immediate base,
   `$ref` target, or conforming type is major 0. `x-gts-ref` is exempt. See below.
8. Canonicalize through `gts-rust`, compute the request fingerprint, resolve the
   mandatory `Idempotency-Key`.

Ordering invariant that must not be reordered: step 3 precedes any existence lookup, so
a refusal cannot probe the namespace. Steps 4 and 6 are request-static; family shape and
whether a waived comparison would fail remain worker decisions inside the commit transaction.

**Worker checks.** Step 7 uses the outgoing edges already extracted in
`unit::evaluate` for the dependency graph. This keeps quarantine and stored edges
consistent, preserves worker-side `invalid_schema` refusals for malformed `$ref`s,
and records quarantine failures as admission-stage `AdmissionFailureReason` values
(P16). It reads only the request and runs before loading stored rows.

The ADR-0014 dialect pin also runs in the worker: step 5 checks admissibility,
while the pin needs the baseline document. It runs immediately before comparison
and reports `dialect_changed`, distinct from `compatibility_undecidable`
(§16.12, `principle-fail-closed`).

Replay of a matching fingerprint under the same key returns the stored operation —
`202` while active, `200` when terminal. A different fingerprint under the same key
returns `409`.

**The worker is a plain function:** `(operation_id, runner)` performs one pass;
the outbox only maps its result to `Ok`, `Retry` or `Reject`. Domain tests call
the worker directly (§13).

**Admission and delivery failures.** Candidate problems, including missing external
dependencies, become terminal item outcomes and acknowledge delivery. In-batch
dependencies are ordered; cross-request dependencies require the caller to await the
prerequisite.

Retry is reserved for failures that may clear. Permanent system failures and an
exhausted attempt budget mark undecided items `system_failure` and acknowledge
the message. Diagnostics contain only stable `error_code`, `operation_id` and
allowlisted `cause_kind` values. If terminalization fails, redeliver — past the
budget too, because nothing else re-drives a non-terminal operation. Dead-lettering
is reserved for envelopes that name no operation.

The default budget is eight admission attempts. `operation_timeout` bounds each leased
handler, and delivery `N + 1` resolves stored status without running admission again.

**Partitioning.** Eight persisted partitions route by the operation UUID's last two
bytes modulo eight. Partitions run concurrently; `entity_write_order` still serializes
entity commits. Retries block only their partition. Changing the count requires recreating
the disposable dev/test outbox; live repartitioning is unsupported.

**Where it runs.** Startup order is repositories → outbox worker → seeding → await
the seed operations → client publication. The outbox starts first because acceptance
enqueues inside its own transaction and there is nowhere to enqueue before it is
bound. Terminal is not enough to publish: `system_failure` and a refused candidate
are terminal too, so startup requires every seed item to be `succeeded` or
`unchanged`, and any `failed` item fails boot. The stateful entry point stops the
retained `OutboxHandle` on runtime cancellation. Starting during `init()` means the
worker is live before any gear's post-wiring publication runs, in process or not (D16).

**Two seed sources, one pass.** Until T37, seeding covers (1) all process-linked toolkit-gts
inventory, including other gears' declarations, and (2) the operator-configured
`cfg.entities` from the deployment YAML — identities whose GTS identifiers are
deployment-specific and cannot be expressed as gear-owned inventory items (e.g. the
platform-root tenant type whose identity is chosen by the operator). Both sources are submitted
together in a single pass; an invalid or oversized combined seed set fails startup
loudly. The combined set must fit `limits.batch_candidates` and the other admission limits;
there is no silent truncation or split that could separate a candidate from its dependency.
Admission orders the combined candidate graph. T31 verifies the real deployment inventory
and the over-limit refusal before publishing the client. This startup set is collected from
the binary, not by scanning the entity table; C4 remains closed.

Process-wide collection is transitional (D11). Each declaring gear publishes its own
crates after wiring (D16), and once all of them do, T37 narrows source (1) to the registry's
own control-plane types and the `toolkit-gts` base types, and removes the process-global
inventory. Until then both paths submit identical content and one of them reports
`unchanged`. `cfg.entities` remains deployment configuration admitted on behalf of the
platform operator, outside per-gear publication. Repeated startup is idempotent; no
ready-mode barrier is restored.

Acceptance and admission therefore have different executors. Acceptance is always
synchronous, in the caller's task, inside registry code: the REST handler for API traffic, or
the local client for an in-process SDK caller. Admission is performed by the types-registry
outbox processors, with a database lease per partition shared across pods. Seeding takes
the same path: an accepted operation always carries a durable message, so there is no
composition in which one is committed with no driver.

**Worker, per admission unit:**

1. Build the candidate graph. Two edge sets, and they are not the same set:
   - the **ordering** graph is authored `$ref`s between candidates, each candidate's
     identifier-derived immediate derivation base, its Instance conformance target, and the
     implicit `vM.(n-1)~ → vM.n~` edge (not stored in `dependency`). Conformance cannot close
     a cycle and is still needed here: an Instance must not commit ahead of the Type Schema
     that may then be refused;
   - the **cycle-bearing** graph is `$ref` and derivation. Those are the two an effective form
     inlines, so those are the two a cycle can be built from.
2. Process in topological order; one candidate is one unit. The *admitted* relation is
   acyclic (ADR-0012): derivation strictly shortens the `~`-chain, nothing references an
   Instance, and every cycle over the combined `$ref`-and-derivation edge set is refused
   because it has no resolved form. The in-batch graph is not acyclic by construction — the
   overlay lets candidates see each other — so the ordering detects a cycle and fails its
   members with `invalid_schema`. Past that refusal there is no condensation step and no
   atomic group. If predecessor edges still prevent ordering, identify the actual cyclic
   components in the remaining ordering graph and refuse only their members. Candidates
   downstream of either kind of cycle remain ordered and receive `blocked_by_dependency`
   or `blocked_by_predecessor` according to their failed blocker; they are not cycle members.
3. Build the unit's transient `gts-rust` store (D2): the candidates, plus the transitive
   closure of what they consume, read `gts_id`-sorted from the database. Evaluate outside
   any transaction against it: resolution, compat vs
   baseline, derivation, references, dependent revalidation. Record the target revision,
   the reverse-impact identifier set, and a revision vector (`resource_version`, plus
   `resolution_fingerprint` where effective content was consumed) — together with the
   **closure roots** the reads were driven from, so step 4.3 re-derives the same question
   rather than merely re-reading the same rows. The vector's reads run in the **same**
   transaction as the store build, so the state it records is the state the documents
   being validated came from; only the validation itself is outside a transaction.
   For an intra-entity compatibility baseline, require its snapshot
   `entity.resource_version` to equal the accepted `expected_resource_version` before
   comparison; a mismatch is terminal `precondition_failed`. The commit's precondition
   then protects the version actually compared. Checking only at commit would let a
   future expected version become valid after evaluation against an older baseline.
   Once the dependency store has loaded, this baseline read refuses an absent entity
   with `precondition_failed`, and a tombstone with `entity_deleted` **before** checking
   its version. These refusals and a version mismatch precede candidate schema
   validation and compatibility comparison; a tombstone must never suggest retrying
   the revision with a newer version. Deleted cross-minor predecessors remain valid
   comparison baselines. **None of these refusals, nor a compatibility refusal, is final
   until the stamp check of step 1a has run** (D18): the
   refusal — including a dependency refusal (`dependency_not_found`, `dependency_deleted`,
   `blocked_by_dependency`), which would otherwise retry forever — is recorded only in a
   transaction that claims `entity_write_order` and finds the same `publisher_name` (or an
   unclaimed row) and an equal or higher `publisher_version`; otherwise the item is `failed`
   with reason `publisher_mismatch` or `superseded` instead. A stamp read during evaluation is only a shortcut,
   because a concurrent publication can overtake it. So an older release offering a
   narrower schema after a newer one widened it learns `superseded`, not a compatibility
   refusal.
4. Commit transaction:
   1. **claim the `entity_write_order` row.** This is the transaction's first statement and
      nothing may precede it, reads included: every step below is an answer about
      committed state, and a read taken before the claim is an answer about state that
      can still move. It is what orders commits, and the order is total only because it
      comes first. No other lock is taken — T15 retired the family advisory locks, and
      the reasoning is below;
   1a. **check the publisher stamp** (D18; numbered so the existing step references hold).
      It reads the committed stamp after the claim and **before** the caller precondition,
      so an older publisher with a stale `expected_resource_version` gets `superseded`
      rather than `precondition_failed` and a re-read loop. With the candidate's durable
      `publisher` (stored at acceptance, never derived by the worker): a different
      `publisher_name` on a claimed row terminalizes the item as `failed` with reason
      `publisher_mismatch`; a lower `publisher_version` terminalizes it as `failed` with
      reason `superseded`; neither writes anything. An equal or higher version, or an
      unclaimed row, continues to step 2, and a successful commit claims an unclaimed row
      with the offered name and version. A dry run evaluates this step and persists
      nothing;
   2. enforce the caller precondition — creation requires the identifier absent, update
      requires `entity.resource_version == expected_resource_version`. An update whose
      authored content equals the current revision's is **`unchanged`** and terminates
      here: it writes no revision, moves no version and refreshes no dependent, so the
      guard below does not apply to it and is not asked. When step 1a saw a **higher**
      `publisher_version` or an unclaimed row, `unchanged` also advances or claims the stamp
      as a metadata-only confirmation: no revision, `resource_version`, `updated_at`, artifact or validator
      moves, and for a minor-bearing Type Schema this is never a content revision
      (ADR-0004). The `PreparedUnit::Unchanged` shortcut taken before the transaction
      re-enters step 1a under the claim rather than trusting its earlier read. A
      successful content commit writes the new stamp in the same transaction; a failed,
      refused or dry-run item never moves it;
   3. re-derive the revision vector from the database — the dependency closure from the
      same roots evaluation used, and the reverse-impact set (D5) — and compare
      **membership and every column**: `resource_version` throughout, plus
      `resolution_fingerprint` for each dependent, whose artifacts a refresh moves
      without moving its version. Any difference rolls the transaction back and
      revalidates from scratch, bounded by `worker.max_revalidation_attempts`;
      exhaustion terminalizes the item as `failed` with reason
      `revalidation_exhausted`;
   4. re-test predecessor existence for each minor-bearing candidate. Before revising
      a Type Schema whose own document declares `x-gts-abstract: true`, require no
      active direct `InstanceOf` dependant; otherwise refuse with `dependent_invalid`.
      Deleted Instances and Instances of concrete derived types do not block this
      revision. The presence check runs under the same `entity_write_order` claim as
      Instance creation, before writing any revision or moving any version. If the
      abstract revision commits first, the Instance's vector guard instead forces
      revalidation against the abstract type, which refuses the Instance;
   5. insert the immutable revision, replace the current-state projection, replace the
      entity's outgoing dependency edges;
   6. refresh affected current effective schemas (bounded by `limits.activation_write_set`);
   7. increment `resource_version`, record the outcome and resulting version.

**Why step 4.2 takes no row locks over the revision vector.** The step was written as
*"then lock candidate and revision-vector entity/current rows in canonical identifier
order"*, and P0 does not do that. Not because it cannot — though it also cannot, see
below — but because locking the vector is the wrong tool for what the vector is:

- A lock guarantees only that nothing moves **after** it is taken. Movement between
  evaluation and the moment of locking is precisely what step 4.3 exists to catch — the
  phantom dependent appears before any lock could be held — so the comparison is required
  either way and the lock is purely additive. What it would buy is that a contended
  admission *waits* rather than rolling back: liveness, not correctness.
- It costs one round trip per vector member inside the commit transaction, on a set
  `activation_write_set` allows to reach 512 — the cost T14 restructured the reverse read
  into a single CTE to avoid. And it is the sole reason the canonical ordering in step 4.2
  has to extend past families at all: order matters because the locks are many.
- It is the wrong shape for this design, which is optimistic throughout —
  `resource_version` compare-and-swap, a transient store per unit, validation outside any
  transaction. Registrations are rare against reads, which is the regime optimistic
  detection is for.
- Only then, the platform fact: the secure query API exposes no `FOR UPDATE`
  (`toolkit-db/src/secure/select.rs`) and `SQLite` has no row locking at all, so the
  portable primitive would be the advisory lock. This is corroboration, not the reason —
  the argument above would stand if `FOR UPDATE` were available tomorrow.

**What holds instead, and why it is sufficient.** The candidate's own row is serialized by
the compare-and-swap that writes it: the precondition travels in the statement's `WHERE`,
so there is no gap between checking the version and moving it. A **dependency** that moves
is serialized by the refresh its mover owes the dependants (D5): that refresh writes each
affected dependant's `type_schema` row — the same row this commit writes — so the two
block on one another in the database. The block orders them; what makes the loser *notice*
is the compare-and-swap described next, not the block. Where the mover's change leaves a
dependant's `resolution_fingerprint` unmoved it writes nothing and no conflict arises, and
that equality is itself the proof that nothing this commit consumed went stale.

**The row conflict orders the writes; it does not recompute the loser.** That is worth
stating precisely, because the earlier form of this paragraph claimed it did. Artifacts are
computed in Rust and only then written: the row lock makes the second writer wait, and the
wait re-evaluates the predicate, not the payload. A write that lost the race would therefore
apply artifacts computed before the winner landed — and pair them with a `revision_no` read
afterwards, which is exactly the row `update_current`'s contract forbids.

So **every artifact write carries a compare-and-swap** on `(revision_no,
resolution_fingerprint)`, and a miss is drift (`current_projection_moved`): the evaluation is
void and the retry redoes it, under `worker.max_revalidation_attempts` like every other drift.
What the token *is* differs between the two writing paths, and the difference matters:

- **The refresh** recomputes each dependent inside this transaction, so its token is the
  projection state those artifacts were computed against — taken from the read that selected
  the document, never from a later one, because a later read would adopt whatever moved in
  between and confirm it.
- **The candidate's own revision** computed its artifacts at *evaluation*, outside any
  transaction, so no token could be their input. Its token is read inside the commit, before
  step 4.3, and is a **post-guard sentinel**: the guard is what establishes that the
  evaluation's view still holds, and the sentinel is what establishes that nothing wrote this
  row between the guard and the write. Composition is the argument — token read, then guard,
  then swap — not a claim about where the artifacts came from. It is needed because the entity
  compare-and-swap does not cover this row: a refresh owed by a *transitive* dependency shares
  no lock with this commit and writes exactly here.

Only a write whose artifacts cannot be stale goes unconditional, which in P0 is an Instance,
having no artifact row at all.

**Both compare-and-swaps are unreachable while commits are serialized**, and are kept anyway.
Nothing can move a row under a commit when no second commit is in flight, so neither swap can
lose. They are depth, not the mechanism: they are what a narrower ordering protocol would rely
on if the global lock is ever replaced, and they cost one predicate on a statement that is
issued regardless. Their known limit belongs with them — the swap is
`(revision_no, resolution_fingerprint)` rather than a monotonic version, so an `ABA` return to
an identical fingerprint would defeat it. Under the lock that is unreachable; a protocol that
relaxes the lock has to introduce the monotonic version with it.

What step 4.3 buys on top of the lock is that an evaluation is never committed against a state
it did not see, which is what makes the transient-store design (D2) safe.

Canonical identifier order is kept where it is observable. P0 takes no family locks, so what
remains observable is the vector: it is `(gts_id, role)`-sorted on both sides, which is what
makes the comparison one merge walk and the drift it reports deterministic rather than row-order
dependent. `domain::family::lock_order` survives for the writer that needs it next — DESIGN's
per-family ordering, which deletion and purge inherit.

**The liveness cost is real and named.** A dependant of a base that is being revised
constantly can exhaust `worker.max_revalidation_attempts` and terminalize as
`revalidation_exhausted` where a lock-holding commit would have waited and succeeded. The
answer to that is the attempt budget and, if it is ever observed, backoff between attempts
— not locks over the vector. P0 does not observe it: the only writer is the registry's own
seeding, and the retry arrives under the caller's `Idempotency-Key`. The commit ordering below
adds a second cost of the same kind: a unit whose commit is queued behind another waits on the
database, and if the backend's own lock timeout cuts that wait, the result is a transient
storage error — contention, never a verdict on the candidate, recovered by a replay under the
caller's `Idempotency-Key` until T21 wires the outbox and by redelivery after it.

**The write path is serialized, and that is a correctness requirement.** The optimistic
mechanisms above cover everything that meets on a row: a dependant that already exists is
reached by the mover's refresh and one of the two compare-and-swaps loses. What they cannot
cover is **reachability the mover's reverse scan did not see** — an edge committed after that
scan, whether by an entity that did not exist when it ran or by an existing one adding a
reference. Adding an edge writes only `dependency` and moves no `resource_version`, so the two
commits write no row in common and no swap can lose. Both pass their own guards, and the
dependant keeps an artifact inlined from a revision that is no longer current, carrying a
`resolution_fingerprint` that matches that artifact and therefore reports no drift. Nothing
later repairs it.

The mechanism that closes it is a **serialized write path**: every commit claims the
`entity_write_order` row of `types_registry__coordination_state` as the **first statement** of
its transaction, so one commit runs
at a time per installation. Both the reverse-impact scan and the vector guard run *inside* the
transaction, so that ordering gives them a total order, and the argument is two cases with no
third:

- **The edge commits first.** It is in `dependency` when the mover claims the row, so the
  mover's scan — running after that claim — finds the dependant and refreshes it.
- **The mover commits first.** The unit writing the edge has committed nothing yet. When it
  does, its own guard re-derives its vector after its own claim and sees the mover's new
  `resource_version` on a member of its closure: drift, rollback, re-evaluation against the
  new revision.

Neither side needs to know about the other. Without the total order both halves can be in
flight at once, each blind to the other, which is exactly the schedule above.

**A row, and not an advisory lock.** `toolkit-db` holds advisory keys on a session separate
from the commit transaction's connection, so losing that session releases the key server-side
while the transaction carries on — mutual exclusion would lapse, and silently, since the
generation epoch is only consulted at release. A row lock ends with `COMMIT` or `ROLLBACK` and
with nothing else, which is the property this rests on.

**First statement, not merely early.** Claimed after any read, the row would order the writes
and leave those reads outside the serialized region — which is the interleaving it exists to
prevent.

**Why the guard is still required, and is not redundant.** Evaluation runs *outside* the
transaction, so a commit that landed between this unit's evaluation and its own claim leaves the
evaluation stale. That is what the vector catches, and it stays the only thing that can:
`a_dependency_mutated_between_evaluation_and_commit_costs_one_rollback_and_one_retry` holds a
pass at its evaluation closure read for exactly that reason. The claim closes what the guard
could not see; the guard closes what the claim does not cover.

**Why the dependency side of the vector needs no fingerprint.** An artifact is a pure function
of the **authored** documents of the closure — `load_unit_store` and the refresh both read
`current_documents`, never materialized artifacts, because those are outputs of the resolution
the store performs (D3). An authored document changes only with a new revision, which moves
`resource_version`. So `resource_version` per closure member is a complete staleness detector
for dependencies, and `resolution_fingerprint` is recorded only for *dependents*, whose
artifacts the refresh consumes directly.

**What it costs, and where it lands.** Admission throughput, and only that: evaluation —
parsing, resolution, compatibility, derivation — stays outside the transaction. What is
serialized is the commit transaction, for its whole length, for **every** outcome: an
`unchanged` re-submission claims the row and writes its item outcome, and a creation writes an
entity, a revision and a current row. Unrelated registrations queue behind both.

**The queue wait is the database's, and the gear states no bound on it.** `lock_timeout` on
`PostgreSQL`, `innodb_lock_wait_timeout` on `MySQL`, `busy_timeout` on `SQLite` — deployment
settings this gear does not own, and an operator running an installation with concurrent
registrants should set the first, since `PostgreSQL` waits forever by default. Exceeding one
surfaces as an ordinary database error, which leaves the operation non-terminal — recovered by
a replay under the same `Idempotency-Key` until T21 wires the outbox, and by redelivery after
it.

A gear-side budget was tried and removed, and the reason is worth keeping: wrapping the claim
in `tokio::time::timeout` bounds nothing. Cancelling the future leaves the statement running on
the connection and the rollback queues behind it, so the caller waits exactly as long and only
gets a different error at the end — the container test written against that version deadlocked
instead of reporting a refusal. A per-transaction lock timeout on `toolkit-db`'s `TxConfig`
(`SET LOCAL lock_timeout` after `BEGIN`) is what would give the gear a bound it can state; §4
records it.

What varies is the expensive part, the dependents' re-materialization the commit performs in a
blocking task while the transaction is open, bounded by `limits.activation_write_set`. That
part *is* zero for `unchanged` and for a creation — the first refreshes nothing, the second has
no dependants yet — so the serialized section is short except when revising something already
depended on, whose measured maximum in the repository is 27 dependents. If the serialized
section is ever measured to matter, the upgrade path is a graph generation compare-and-swapped
at commit, which restores parallelism and subsumes this lock; it is not built, and nothing
depends on it being built.

**Every writer of entity state must claim the row**, or the order it provides is not total.
Today that is this path alone. Deletion (T20) and the operator purge job (ADR-0013) join it,
and each is a correctness obligation of its own task rather than an optimization.
`a_creation_claims_the_entity_write_order_row_exactly_once` and its revision and
`unchanged` siblings are what makes an omission visible: the row is a
monotone count of committed admissions, so a writer that skipped it shows up as a count that
did not move.

**P0 takes no family advisory locks, and this is a deviation from DESIGN §3.7 worth naming.**
DESIGN has the commit lock or create every candidate family in canonical order; T12 implemented
that with advisory keys held across the transaction. The claim retires them for three reasons,
in order of weight:

- They are **redundant**: their whole window — `create_or_get`, the three family rules, the
  entity insert — is inside the commit transaction, which the claim makes exclusive. The
  check-then-act they serialized is already serialized.
- They **invert an order**. Admission took them *before* its transaction, while ADR-0013's purge
  claims the row and then takes family locks. Two writers acquiring the same two things in
  opposite orders is a deadlock, and keeping a redundant lock is a poor reason to have one.
- They **turn a wait into a refusal**. Taken before the transaction, two passes could collide on
  a family key while neither had claimed the row, and the loser answered `503` where it would
  otherwise have queued and succeeded.

What DESIGN describes stays right for the design it describes: a narrower ordering protocol,
one that does not serialize every commit, needs per-family ordering again and would reintroduce
them — together with the entity and routing tiers DESIGN orders after them.

**The write-set bound is asked twice.** `limits.activation_write_set` bounds the
reverse-impact read in the vector as well as in the refresh, so an over-bound candidate
is refused with `activation_write_set_exceeded` at **evaluation**, before a transaction
has written anything, and the refresh's own refusal (step 4.6) remains as the backstop
for a set that grew in between.

**Resolution budgets apply to each document.** Before `gts-rust` resolves a candidate,
`limits.resolution_closure` counts the candidate itself and the distinct documents reached
through `$ref`, derivation and Instance conformance in the candidate-overlaid store.
Converging paths count once; `x-gts-ref`, removed outgoing references and unrelated documents
loaded for other refresh subjects do not count. Exceeding the budget yields
`resolution_closure_exceeded`. Each canonical effective artifact must also fit
`limits.resolved_document` UTF-8 bytes; exceeding it yields `resolved_document_too_large`.
An Instance's conforming schema is subject to the same byte limit. Both checks also apply
to each refreshed dependent, and a refresh refusal rolls back the candidate revision and
all dependent writes. Zero is invalid for either configuration setting.

These are composition and output bounds. The repository's separate 512-entity store-build
guard still bounds database loading, and `gts-rust` constructs the resolved value before its
canonical byte size can be checked; the byte limit is not an allocator-level memory cap.

Deletion has its own short protocol, and it opens the same way: a transaction whose **first
statement** claims the `entity_write_order` row, then the positive `expected_resource_version`
precondition, the recheck that the target is `ACTIVE` at that version with no direct
registered dependents, `DELETED`, the version increment, the outcome. The claim is not
optional and it is not merely early — a deletion whose recheck runs before it is a
check-then-act on state that can still move, which is the failure the recheck exists to
prevent. After the claim and before the precondition, deletion runs the same stamp check
as step 1a (D18): another name is `publisher_mismatch`, a lower `publisher_version` is
`superseded`, and an equal or higher one — or any on an unclaimed row — proceeds and writes
the tombstone and the new stamp in one transaction. Nothing deletes an entity because a newer
manifest no longer declares it.

**Dry Run predicts the whole batch without entity-state writes.** Run the same checks in
dependency order over one read-only snapshot plus earlier successful candidates' virtual
changes. Merge each tentative layer on success; discard it on refusal or error.

Do not write `version_family`, `entity`, revision, current-pointer or `dependency` rows,
or claim `entity_write_order`: a prediction must not serialize real writers behind a batch.
Verify this with an adapter that rejects write attempts; unchanged tables alone permit rollback.
Operation, outcome, idempotency and dispatch records remain durable. Publish outcomes and
completion atomically after releasing the snapshot, preserving payloads for redelivery on failure.

### 8.2 Read path, and why no store is held between admissions

**Reads are served from the database.** Nothing persistent lives in process memory.

This is not the obvious choice, and it rests on separating two questions that look like
one: *what needs a `GtsStore`* and *what needs entity rows*. They have different answers.

A `GtsStore` is needed only where GTS semantics are computed over a set of related
documents — `resolve_schema_refs`, `compare_documents`, derivation-chain validation,
instance validation against a type. All of that happens **inside** admission, on one
candidate plus what it consumes.

Reads need rows. The exact-read primitive is a keyed lookup, and the list primitive is
identifier matching, a pure function of the parsed identifier — no store question. And
by D3 the effective artifacts a reader wants (`resolved_schema`, `effective_traits`,
`effective_traits_schema`) are already materialized on the current-state row. So a read is
a selected-column lookup (with a kind-selected current-revision join that checks the
pointer and reads `content` or `provenance` when selected). Discovery (D14) decides every
filter in one statement: `kind` and `lifecycle_status` on stored columns, `depth` on the
materialized `chain_depth`, and `pattern` as joins on `entity_gts_segment`, the segments
`GtsId::segments()` produced at admission. `gts-rust` parses both sides; the repository
only compiles the parsed pattern (`repo/segment_filter.rs`), and differential tests pin it
to `GtsId::matches_pattern` on every backend. T22b keeps document columns out of
metadata-only reads.

**Why the process-local snapshot was rejected.** A snapshot rebuilt after each local
admission unit cannot satisfy the multi-pod read criterion of §13 — *"two pods, commit on
A, B's first post-commit read sees it"* (`nfr-multi-pod-correctness`). P0 has no
invalidation channel between pods: no pub/sub, and the shared admission outbox distributes
work through partition leases rather than broadcasting commits to every pod.
Pod B would serve its stale snapshot indefinitely. Admission is protected against exactly
this by the commit-time revision-vector guard (D4, §8.1 step 4.3), which makes evaluation
against possibly-stale data safe; **reads have no such guard**, so for them staleness is
simply wrong. Serving reads from the database makes the criterion true by construction
rather than by a mechanism P0 does not have.

Two consequences worth naming, because they are the price:

- A read is a database round trip where it used to be a memory read. That is what the SDK
  client cache of §8.3 is for, and it is the reason that cache is kept rather than removed:
  a bounded freshness window on the *client* is a trade DESIGN §3.3 sanctions, whereas a
  process-local store treated as authority on the *registry* side is not.
- Startup no longer reads the whole table, and no `GtsOps` is held for the process
  lifetime. That retires two ceilings rather than deferring them (§9, C1 and C4).

**The transient store, per admission unit.** The worker builds a `GtsStore` from the
database rows the unit needs: the candidates, plus the transitive closure of what they
consume, obtained from the `dependency` table (which D5 already writes and reads). It is
dropped when the unit ends.

The closure is seeded from **both** the candidates' identifiers and their documents. The
identifier supplies the `~`-chain — every derivation base and an Instance's conforming type
— and the document supplies its `$ref` targets, which no identifier implies. An
`x-gts-ref` target is seeded by neither, because validating that keyword never reads the
target document. The document half is not an optimization: a candidate's own edge rows are written
at commit (step 4.5 above), so during the read that validates it they either do not exist
yet, on a first admission, or still describe the previous revision. A reference target that
no entity carries is reported apart from a candidate that has no row yet — the second is
the ordinary state of every creation, the first is a fact about the registry. Rows are loaded `gts_id`-sorted, so a derived schema never
loads before its base — lexicographic order on GTS chain identifiers already implies
parent-before-child, since a base identifier is a strict prefix of every identifier
derived from it, as the existing `switch_to_ready` documents.

Building from the closure rather than from every row is what keeps this cheaper than the
snapshot it replaces: the snapshot cost a full rebuild after every successful unit,
whereas the closure is bounded by the candidate's own dependencies.

`Mutex<GtsOps>` disappears with it. The current code holds one because *"`GtsOps` contains
a `Box<dyn GtsReader>` which is not `Sync`"*; a store owned by one worker invocation and
never shared needs no lock at all, so the non-`Sync` field stops being a design
constraint instead of being worked around.

`temporary` / `persistent` two-phase storage and **ready mode are removed.** They existed
only because there was no durable source of truth and startup ordering was unknown; with
a database supplying entities on demand, neither has a job. `SystemCapability::post_init`
and `switch_to_ready` go away, which also settles DESIGN's objection that the `post_init`
barrier conflicts with `constraint-boot-path`.

---

### 8.3 The SDK client cache stays

DESIGN requires a client cache — `cpt-cf-types-registry-fr-client-cache`, *"bounded
per-client representation cache with batched conditional revalidation and fail-closed
expiry handling"* — and under D2 it earns its keep: a read is now a database round trip,
not a memory read, so the cache buys what it costs elsewhere.

Its bounded staleness window is sanctioned, not a correctness violation.
`cpt-cf-types-registry-nfr-cache-correctness` is *"no invalidated result accepted as
current after the client observes the mutation"*, and DESIGN §3.3 draws the line
explicitly: *"a remote mutation not yet observed may produce a stale snapshot within the
bounded window but is not described as an invalidated entry accepted as current."* That is
also a different NFR from `nfr-multi-pod-correctness`, which constrains the **registry's**
own reads — *"no process-local authority"* — and is satisfied by D2. A client's freshness
window and registry-side authority are not the same property.

What P0 builds is DESIGN's cache minus what needs inputs P0 does not have:

| DESIGN §3.3 | P0 |
|---|---|
| Bounded store, LRU eviction | ✅ — bound is **bytes**, not entries: §3.2 caps one resolved document at 1 MB, so today's `capacity: 1024` bounds memory to nothing useful. DESIGN's argument, adopted |
| Freshness window, `0` meaningful and supported | ✅ — DESIGN's 30 s default replaces today's 1 min |
| `fresh` per-call bypass | ✅ — T22d's validator makes the call revalidate unconditionally against the source, even within the freshness window |
| Invalidation on an observed terminal mutation, across identifier and UUID keys | ✅ — a client observes a mutation when a poll or the reconciliation helper returns a terminal successful outcome, **not** when the `POST` is accepted |
| Entries indexed by both identifier and UUID | ✅ — already true of the current cache |
| `NotFound`, `Failed`, discovery pages and operation resources never cached | ✅ — all four are expressible in P0 |
| Key includes visibility context, Context Tenant, normalized projection | ✅ for the selected-field set from T22b; visibility context and Context Tenant remain fixed P0 markers. A narrow entry never answers a wider selection |
| Batched conditional revalidation against validators, fail-closed | ✅ — §8.5 puts validators in P0, so expiry sends expired keys and their validators in one conditional `batchGet` and keeps an `unchanged` entry. A failed revalidation propagates and never extends the window |
| No invalidated result accepted as current after the mutation is observed | ✅ — per-key generation advances on observed terminal outcomes or `fresh` validator changes; reads started earlier cannot refill the cache (P23) |

The cache is not carried over as-is: it is typed on `GtsTypeSchema` / `GtsInstance`, which
D6 deletes, so it is ported onto `EntitySnapshot` when the new trait lands. It also moves out
of the registry's local client into the SDK, as a decorator over any `dyn PlatformTypesRegistryApi`:
a gear in another process reads through the REST client, and DESIGN's cache is per client,
not per registry (D15). Until then the existing cache keeps serving the old trait untouched.

Ceiling C7 records what is still fixed rather than derived: visibility and Context Tenant key dimensions. Projection is real in P0.

The byte bound measures canonical payload bytes of the cached documents, not the process's
retained heap. The cache is in memory: a restart starts empty, and nothing survives an
unreachable registry beyond the window (C13). The publisher stamp of D18 is not part of
`EntitySnapshot` in P0, so a metadata-only confirmation changes no cached representation.

---

### 8.4 Out-of-process operation

In production types-registry runs as its own binary in its own pod (toolkit-oop DESIGN,
Profile 3), and every other gear may run in another process. P0 is built for that (D15–D17).
The gear's code does not depend on the deployment profile (ADR-0001): it resolves
`dyn PlatformTypesRegistryApi` from `ClientHub` and publishes its declarations through one SDK call,
in process and out of process alike.

#### Surfaces

| Surface | Transport | Consumer | In P0 |
|---|---|---|---|
| Local client via `ClientHub` | a direct call | gears in the registry's process | ✅ |
| Hand-written REST client via `ClientHub`, resolved through the directory | HTTP, `X-ToolKit-Internal-Token` | gears in **other processes** | ✅ (D15) |
| Hand-written tenant REST client (`TypesRegistryApi`) via `ClientHub` | HTTP, the caller's `Authorization: Bearer` | gears in other processes serving a tenant's request | ✅ (D17, T27) |
| Platform REST on the gear's listener, `/types-registry/platform/v1/` | HTTP, `X-ToolKit-Internal-Token` | the platform client above, operators, CI, e2e | ✅ — the full operation set, platform plane only (D17, D20) |
| Tenant REST on the gear's listener, `/types-registry/v1/` | HTTP, `Authorization: Bearer` | the tenant client above, tenants through the gateway, e2e | ✅ — entity reads only (D17) |
| A separate platform listener | HTTP | the platform callers, with the plane enforced by transport | ✗ (C8) |
| gRPC | — | — | ✗ — REST is the toolkit default (ADR-0002) |

**Wiring.** types-registry declares `#[provides(transports = [local, rest], rest_client = …)]`;
a consumer declares `#[consumes(contract = PlatformTypesRegistryApi, from = "types-registry",
resolving_client = …)]`. A local implementation wins in Profile 1; otherwise the consumer
registers the resolving client, whose calls fail `service_unavailable` until the registry is
reachable and which rebuilds itself when the registry moves (`DirectoryResolvingClient`,
`toolkit-contract/src/runtime/resolving.rs`). `#[consumes]` adds no `init` ordering by design,
so no consumer may call the registry from its own `init()` — proxy wiring runs after every
`init` (`host_runtime.rs:631`).

**Manual REST client.** Codegen lacks custom headers/status handling required for
`Idempotency-Key`, `Location`, `Retry-After`, replay `200`/`202` and `ETag`/`304`.
Use `runtime::http::{build_request_url, attach_internal_token, map_http_error}` and
`runtime::retry::retry_with_backoff`, plus client spans/RED metrics. Submission and
exact reads bypass `send_unary`, which discards success metadata and rejects `304`.
Contract tests exercise live routes. Platform contexts themselves work in codegen
(`docs/arch/toolkit-contract-binding/{PRD,DESIGN}.md`; plan P22).

**Route authentication.** `OperationSpec` supplies listener and embedded gateway
policy; proxy discovery reads OpenAPI `security` (D17, plan P25).

- Platform routes use `.platform_authenticated()`: require a validated internal token
  and validate every presented credential. Missing authenticators fail closed.
- Tenant reads use `.authenticated()`: require a validated bearer; an internal token
  alone is insufficient. Neither route set is anonymous.
- Platform OpenAPI declares `internalToken`; discovery excludes it because the edge
  strips tokens.

The standalone binary needs `DynBearerAuthenticator` in ClientHub, via
`AuthNResolverBearerAuthenticator` over `AuthNResolverClient`. T27 supplies it and records the production linked/remote topology here;
without it tenant routes must return canonical `401` before dispatch.
Authentication grants no authorization (C2, C6, C8).

**Early client handoff (P26, P27).** T24a–T27a provide both API transports and their resolving
clients, T28 the specified platform cache and T29 the generic post-wiring, supervision and
readiness prerequisites. T30 runs isolated registry and consumer application processes, plus
the master host's `DirectoryService` as an explicit prerequisite process, using real
resolution and the existing publisher. Its development bearer authenticator validates signed
tokens independently of registry plugin discovery and is recorded as development-only. A
`publish = false` harness package owns the feature-gated pilot bins and test, and
`make test-types-registry-pilot` runs inside `make ci`; no gear crate depends on the harness.
The pilot registry seeds registry and base types through admission, never consumer
inventory. Both start orders, late dependencies, invalid declarations, readiness, local/REST
parity and restart must pass before handoff.

**First real gear (P26).** After the atomic cutover (T31/T32), T34 assigns publishers
(including a deployment's child tenant types, which stay registry `cfg.entities`) and adds the
post-wiring path for configured entities whose dependency lies outside the inline seed set.
T35 moves real Account Management and its static IdP plugin after wiring; T36 migrates AM's
host prerequisites and runs AM against a registry in another process with the production
`AuthNResolverBearerAuthenticator`, in the topology T27 recorded (a linked topology is
verified by T38 after the pull ends). Checkpoint 8A requires an Active root and authenticated
child-tenant create/read in local and remote hosts, delayed prerequisites, configuration
change and restart, and green embedded AM e2e. The root binding stays fatal in AM's `init`;
a registry refusal of the root type holds readiness without failing boot (D21), a pending
registry prerequisite is waited for rather than failed, and business, database or IdP saga
failures keep the existing `bootstrap.strict` policy. RG's sealed,
init-only bootstrap surface is not exposed after wiring. The composition shares production
domain, adapters and runtime and adds no admission fork or feature switch to an existing host.
Automatic collection, the remaining fleet and the publisher guard follow; no mixed-version
deployment guarantee is made before Checkpoint 9.

#### Declarations and publication

A GTS Type Schema or well-known Instance is declared with `#[gts_type_schema]`,
`gts_instance!` or `gts_instance_raw!`. Each declaring crate — a gear crate or an SDK crate —
calls `toolkit_gts::declare_gts_inventory!()` once. That macro defines a crate-local collector and
`gts_declarations()`, which returns plain data (identifier and JSON), never `toolkit-gts` types.
Forgetting it is a compile error.

The **owning gear** declares the crates it publishes in its attribute:

```rust
#[toolkit::gear(
    name = "authz-resolver",
    gts(crates = [crate, authz_resolver_sdk], publisher = types_registry_sdk::publish_gts),
    // …
)]
pub struct AuthzResolver;
```

The macro records ownership for coverage checks, gathers each listed crate’s
declarations after wiring and composes the publisher handle into readiness.

Toolkit defines the publisher signature: ClientHub, gear name/version as text,
declarations and cancellation token → supervised status implementing generic
`ReadinessStatus` (`is_ready`, `is_terminal`). Publication types (`PublisherContext`,
`PublisherVersion`, per-identifier status) live in `types-registry-sdk`; toolkit and
its macros never name that SDK. The SDK depends on toolkit for its contract.

Publication reconciles (§10.1) with bounded passes, backoff across cycles, re-reads
after conflicts, one key per identical submission/new keys per cycle, and
`Retry-After` pacing. Status is pending/admitted/rejected/superseded per identifier.
Panic, early exit and cancellation settle status and are observed/joined.
`Required` readiness follows D21; admission is sticky, startup never waits.
Configuration-built Instances use the same publisher context.

Why per crate:

- **Ownership is explicit.** Merely linking an SDK crate publishes nothing, so a consumer of
  `authz-resolver-sdk` never becomes the publisher of its plugin schema.
- **No version split.** The collector's entry type is local to the declaring crate and is
  iterated inside it, so two semver-incompatible `toolkit-gts` versions in one binary cannot
  hide declarations (gts-rust #130).
- **The expected set is known.** A gear's readiness can check exactly what it declared.

types-registry publishes its own control-plane types, the `toolkit-gts` base types and
`cfg.entities` inline in `init()`, as before (plan P2/P3), with no census of other processes.
Until every declaring gear publishes for itself (T35–T37), it also keeps seeding all linked
inventory. The two paths submit identical content, so one of them reports `unchanged`. T37 ends
that transition. A `cfg.entities` item with a dependency outside the inline seed set — a schema
another gear publishes through this registry — is not seeded inline: types-registry publishes
it after wiring with its own context, the registry's readiness does not wait for it, and only
the consumer bootstrap that needs it waits for its status (D11 as amended, T34).

Cross-gear dependencies converge through partial admission and repeated cycles: B, whose schema
`$ref`s A's, is blocked until A is admitted and becomes ready after it, with no global barrier.

#### Publication ordering (D18)

**Publisher input.** The actual registrant supplies its name and own
`CARGO_PKG_VERSION`; SDK helpers only forward them. Sharing a dependency does not
share ownership: another publisher gets `publisher_mismatch`. Names are opaque
equality keys until P1 workload binding (C3).

Every platform mutation requires request-level `publisher: { name, version }` for
all items; mixed publishers/versions require separate requests. Registry seeds and
configured entities use the registry’s own name/version; operators/e2e supply theirs.
Missing fields or invalid/oversized SemVer yield synchronous `400`; tenant bearers
are refused. Acceptance stores publisher on the operation and in its fingerprint;
a same-key publisher change yields `409`.

**Outcomes per item**, compared by SemVer precedence (prerelease counts, build metadata does
not):

| Incoming `publisher_version` vs stored | Authored content | Outcome |
|---|---|---|
| Another `publisher_name` | any | `failed` with reason `publisher_mismatch`; nothing written |
| Unclaimed row | any | ordinary admission; success or `unchanged` claims the row with the offered stamp |
| Lower | any | `failed` with reason `superseded`; nothing written; publication stops retrying it |
| Equal | identical | `unchanged` |
| Equal | different | ordinary admission, CAS and compatibility; content and stamp commit together |
| Higher | identical | `unchanged` plus a metadata-only stamp confirmation |
| Higher | different | ordinary admission; content and stamp commit together on success |

Equal versions permit configuration changes without a build; differing pod
configurations may overwrite each other during rollout (C11). Per-entity stamps
allow partial admission: A@2 must not block B@2 while B@1 remains stored. Publication
always submits equal documents so the registry can confirm higher versions.

**Rollback.** Lower versions are superseded; reverting content requires a newer
release, including hotfixes on older lines. Superseded-live satisfies readiness with
a warning; superseded-deleted does not (D21). Older readers supporting current
contracts remain the release’s N−1 obligation (D22).

**Introduction (Phase 9).** Rename, stamp, field and check land together; T41 requires
publisher. First publication claims pre-T39 rows; T37 coverage assigns one publishing
gear per declaring crate. P0 ships only after Checkpoint 9. Older writers and
pre-Phase-9 registry binaries must not use the migrated database; rolling the registry
back below Phase 9 is unsupported.

#### Availability (D20)

Every publishing gear depends on the registry at startup, and SDK reads fail closed once
their freshness window passes (§8.3). The registry holds no process-local entity state (D2),
so **Profile 3 runs at least two registry replicas with a disruption budget**; the remaining
dependency is the database. The gear ships that configuration as a deployment chart (T38). P0 does not promote a gear to ready without a reachable registry,
and offers no persistent cache or stale-if-error (C13).

#### What holds under OoP

| Property | Why |
|---|---|
| The async protocol | Submit → operation id → poll; no streaming, callbacks or shared memory |
| The SDK models | Flat, no `Arc`-linked graphs, no trait objects — the old models' `parent: Option<Arc<GtsTypeSchema>>` is exactly what could not cross a wire, and D6 deletes it |
| Multi-pod correctness (D4) | Already assumes several processes against one database |
| Materialized artifacts (D3) | A remote reader selects what it needs in one response; the server holds no per-caller state |
| The client cache (§8.3) | An SDK decorator over any `dyn PlatformTypesRegistryApi`, so a remote client caches exactly as a local one |

**The risk is divergence between two hand-maintained sides.** REST handlers and the REST client
are both written by hand, so a change to one without the other is caught only by the contract
test. Handlers stay thin mapping steps over one domain service (DE0201), and wire DTOs never
reach the semantic models.

---

### 8.5 Freshness validators and conditional reads

**Validators, `ETag` / `If-None-Match` and conditional reads are in P0.** They look like they
need the inputs tenancy supplies; DESIGN §3.3's own input table
(`cpt-cf-types-registry-tech-freshness-validator`) says otherwise:

| Validator input | Managed | Applies to a P0 read |
|---|---|---|
| `entity.resource_version` | ✓ | **yes** — compare-and-swap already maintains it |
| `type_schema.resolution_fingerprint` | ✓, Type Schemas only | **yes** — materialized at admission (D3) |
| subject visibility-chain version | ✓ **tenant plane only** | **no** — DESIGN: *"a platform read has no subject visibility chain"*, and §8.4 establishes every P0 read is platform-plane |
| Context Tenant availability-chain version | ✓, only when availability is selected | **no** — availability is out of scope, and the input is conditional even in P1 |
| routing generation | — external only | **no** — federation is out of scope |
| `external_revision` | — external only | **no** — Externally Managed Entities are out of scope; managed revisions already move `resource_version` |
| normalized projection | ✓ | **yes** — T22b normalizes the actual selected-field set for exact read and `batchGet`. Absent `$select` equals an explicit default set; order and case do not alter the digest. Discovery pages carry no validator |

The tenant inputs are not missing from P0. They **do not participate** in a platform-plane
managed read, by DESIGN's own rule. So the P0 validator is complete rather than approximate:
a versioned digest over `resource_version`, `resolution_fingerprint` where the kind has one,
and the normalized selected-field set. A validator for a narrow representation cannot
match a wider one, even if the underlying entity version is unchanged.

`resolution_fingerprint` is not redundant beside `resource_version`, and this is the case a
simpler digest gets wrong: a dependent's effective schema is refreshed when a base is revised
(§8.1 step 4.6), and §13 requires an identical recomputation to move **no** `resource_version`.
A `resource_version`-only validator would report `unchanged` for a dependent whose resolved
document genuinely changed.

**Computed, never stored** (`principle-derive-not-store`). No column holds a validator, and no
cache holds one as authority.

**Wire form is DESIGN's**: base64url of a version byte followed by the 128-bit digest,
byte-identical in the `ETag` header and in batch bodies. The 23-character payload is quoted as
an HTTP entity-tag; both REST surfaces carry those 25 bytes. The version is also digested, and
it is load-bearing — it is what lets P1 add the chain versions while retaining the projection
digest and refusing to honour a P0 token (ceiling C7).

**Where they apply.** Exact reads carry an `ETag`; a matching `If-None-Match` returns a bodyless
`304`. Batch reads carry validators **beside individual keys**, because one header cannot
represent a batch: any result may be `unchanged`, and the response stays `200` even when all
are. Discovery pages carry no validator and are never conditional — a page is a changing set,
not an exact-key answer.

**Two consequences elsewhere in this spec.** The validator field must be in the SDK models from
the moment the new trait exists, not added afterwards: ~50 call sites across twenty-plus gears
migrate onto that contract, and a later addition buys a second migration. And the client cache
of §8.3 gains the revalidation half of `fr-client-cache` — expiry becomes a batched conditional
`batchGet`, fail-closed on error, instead of dropping the entry.

The toolkit has no `ETag` helper, which is manual work rather than a missing capability:
`OperationBuilder::no_content_response` accepts any status, so `304` is declarable, and
`file-storage` already returns `StatusCode::NOT_MODIFIED` with headers by hand.

### 8.6 Observability of the write path

Admission is asynchronous, so an operator cannot read a decision off the response that triggered
it. What the write path emits is therefore part of the contract, not a by-product:

- **Two spans.** `types_registry.admission.operation` covers one pass over one operation and
  opens before its first read; `types_registry.admission.unit` covers one candidate. Both carry
  `operation_id`, `kind` and `dry_run`, the unit span also `gts_id` and `operation_item_id`.
  Identifiers, selected baselines and dependent counts are **span fields** — per event, not per
  series.
- **Instruments behind a domain port.** `AdmissionMetrics` is a `domain::ports` trait with an
  OpenTelemetry adapter in `infra`, injected at `init`; domain code never names the SDK. The
  rendered names carry a configurable prefix, `_total` on counters and `_seconds` on the duration
  histogram, and they are asserted rather than reviewed.
- **Every label value comes from a closed vocabulary, and no identifier is ever a label.** The
  vocabularies are `status`, `stage`, `reason`, `drift`, `verdict`, `dry_run` and `kind`.
- **Every terminal outcome and every refusal is countable, and the vocabulary is
  compile-enforced.** Acceptance-stage reasons come from an exhaustive match; admission-stage
  reasons come from a `Reason` newtype whose only constructors are named consts, so a refusal
  added later cannot compile without a reason. The one unbounded case — a reason read back off a
  stored `error_payload` — maps to a single `other` label.
- **A series never blends decisions of different kinds.** A dry run performs no write and a
  deletion is not a registration, so `dry_run` and `kind` are labels wherever a counter would
  otherwise merge them. An undecidable compatibility comparison is likewise distinguishable from
  a decided-against one in the metrics, not only in the refusal reason — which is what makes
  §16.12 observable in a deployment.

Refusals persist `{reason, message}`, returned unchanged by operation reads beside the
reason-specific `context` (DESIGN §3.3). `reason`
is the stable machine-readable refusal category; `message` is for humans and is not
a parsing contract. There is no separate `diagnostics` field (PRD, ADR-0003).

For `incompatible_with_baseline` and `compatibility_undecidable`, this build renders
up to 20 backward findings as `finding at path`, in engine order, and counts omitted
findings in the message. Each path retains at most 200 UTF-8 bytes, cut at a character
boundary and followed by `...(truncated)` when shortened. Paths use GTS notation
(`$`, `$.payload`, etc.); the engine's unbounded human `detail` is not copied.
Refusals before comparison, such as `dialect_changed` and `baseline_unresolvable`,
explain their cause without inventing comparison findings. Clients branch on `reason`
and display `message` without depending on its wording or these implementation limits.
Successful admissions carry no compatibility diagnostics.

## 9. Database

`database.sql` is the normative target. P0 creates **11 of its 12 tables**, omitting only
`source_claim` (federation). The exception is `coordination_state`, which arrives in its
own second migration rather than the initial one, because the initial migration is
already applied on every existing installation and would never deliver a new table to it.

- **`coordination_state` exists** — one table for every installation-global
  coordination sequence, named by `state_name`.
- **Only `entity_write_order` is seeded and used in P0**; nothing at runtime seeds or
  addresses any other state.
- **`source_claim` is not created** in P0.
- **The `routing` state row arrives with federation**: its migration seeds `routing`
  together with `source_claim`, and `routing.state_seq` is the routing generation.
- **No standalone `routing_config` table will be created** — in P0 or ever; the
  routing generation lives in the `routing` row above.

**Tenant columns are created and constrained exactly as specified, and never populated
with tenant scope.** Every P0 row carries `ownership_scope = 1`, `owner_tenant_id = NULL`.
This is deliberate: keeping the columns and their CHECK constraints means P1 tenancy
needs no schema migration. `ck_tr_entity_owner` then makes `publisher_name` (`owning_gear`
before T39) NOT NULL for every P0 entity.

| Table | P0 |
|---|---|
| `version_family` | full, global scope only |
| `entity` | full, global scope only; `chain_depth` materialized (D14) |
| `entity_gts_segment` | full — one row per parsed segment, written with the entity (D14) |
| `type_schema_revision` | full |
| `instance_revision` | full |
| `type_schema` | full — artifacts materialized (D3) |
| `instance` | full |
| `dependency` | full — written and read (D5) |
| `operation` | full; `plane = 1`, `tenant_id = NULL` |
| `operation_item` | full |
| `coordination_state` | full — `entity_write_order` seeded at 0; `routing` deferred to federation |
| `source_claim` | **not created** |

Migration notes:

- One initial migration, `m2026NNNN_000001_initial.rs`, mapping identity, UUID, binary,
  boolean, timestamp and binary-collation types per backend. Identifier columns:
  `varchar(1024)`, binary collation, ASCII charset where the default is multi-byte.
- `coordination_state` in its own second migration, `m2026NNNN_000002_coordination_state.rs`,
  seeding `entity_write_order` at sequence zero with a migration timestamp; re-running it
  against a database that already has the table and row preserves both.
- `entity.chain_depth`, `entity_gts_segment` and the discovery indexes in
  `m20260925_000005_entity_gts_segment.rs`. It has no backfill, so it refuses while
  `entity` holds a row; SQLite's `chain_depth` is nullable with a CHECK rejecting NULL,
  because SQLite cannot add a `NOT NULL` column without a default.
- Outbox tables come from `outbox_migrations_with_prefix("types_registry__outbox")`,
  not from this migration.
- `routing` is not seeded, because federation has not landed: its migration will seed
  the `routing` row together with `source_claim`.
- **Publication state (D18, T39)** in its own forward migration, with no backfill and no
  reinterpretation of stored content:
  - `entity.owning_gear` is renamed to `entity.publisher_name` (`RENAME COLUMN` on all three
    backends) and holds the actual publisher once a row is claimed; until then it keeps
    the `"types-registry"` placeholder;
  - `entity.publisher_version` — nullable canonical SemVer text. `NULL` marks an unclaimed
    row. It is compared by a typed SemVer comparator in the domain under the write order,
    never by SQL string order;
  - `operation.publisher_name` / `operation.publisher_version` — both or neither, platform
    plane only; set at acceptance and read by the worker for every candidate. Nullable only
    for operations accepted before the migration: the API requires them (T41), and a
    portable `NOT NULL` would need a backfill of history;
  - no new item status or column: a superseded or mismatched candidate is `failed` with
    reason `superseded` or `publisher_mismatch`, the stored and offered values in the
    existing `error_payload` context, and a metadata-only confirmation is `unchanged`. A
    CHECK admits a stamp only on a global entity with a publisher name. `database.sql`
    carries the target shape.

### Declared ceilings

There is no final compatibility ceiling: `gts-rust` 0.12.0 supplies the tri-state verdict and
the content-model classification, so completed P0 honours `principle-fail-closed` for
compatibility rather than deviating from it (§7). C9 records the implementation window before
that final state and must be struck before the database path is exposed.

C1 and C4 are **struck** — resolved in P0 rather than deferred. C3 is narrowed by D11/D16 to
attribution, which D18 makes cooperative, and remains open until P1. C11 is narrowed by D18;
C12 and C13 are added by P23. The rows are kept because other documents cite the numbers.

| # | Ceiling | Upgrade path |
|---|---|---|
| C1 | **Struck by D2.** Was: the whole entity set held in process memory, so entity count becomes a memory bound | Resolved in P0 — the store is transient per admission unit and bounded by the unit's dependency closure (§8.2) |
| C2 | `idempotency_scope_hash` digests three constants, so the key namespace is **global**: two unrelated callers reusing one key collide with `409`. `principal_id` is `Uuid::nil()`: D17 authenticates the calling workload but P0 records no principal, so neither audit records nor the key scope name it | P1 records the validated platform identity as the operation principal and derives the key scope from it |
| C3 | **Cooperative attribution.** Until Phase 9 `owning_gear = "types-registry"` stays a compatibility placeholder for every admission. After it, a claimed row's `publisher_name` is the actual publisher, taken from the request's required `publisher` (D18) — but it is **declared, not authenticated**: any platform-plane workload or local caller can name any publisher, and nothing binds the internal token to a name. The first publication claims an unclaimed row, so a caller that names the wrong publisher can claim it first. It coordinates cooperating publishers; it is never authority | P1 #4628: registry-side **workload policy** (ADR-0006/0008) — the platform plane bypasses the tenant PDP, so no PDP decides this. A deployment-configured allow-list maps a validated identity to the publisher names it may use, and a request naming any other is refused with `403` before an operation exists. The allow-list keys on the **full** `PlatformIdentity`, not `peer_name()`, which drops the Kubernetes namespace; one process may host several gears under one identity, so the names need not equal the identity's. With a SPIFFE identity, which carries a version, `publisher_version` becomes attested; with a service account it stays declared. The local client gets its name bound when the runtime builds it, not from the caller. Claiming an unclaimed row then needs a permitted name too; correction of existing attribution even when authored content is unchanged; exposure on reads alongside the ownership view (§10.2). No owner is inferred from a GTS namespace |
| C4 | **Struck by D2.** Was: startup reads the whole table on the platform boot path, so startup time is linear in entity count | Resolved in P0 — no warm-up read; startup cost is the seed set, not the table (§8.2) |
| C5 | No operation-retention sweep: terminal operations accumulate | The §3.2 sweep, once volume justifies it |
| C6 | **No PDP.** Access is authenticated but not authorized, contrary to `06`. DESIGN lets an authenticated platform workload bypass the tenant PDP; P0 authenticates every caller, tenant or workload (D17), but binds no permission to either — a tenant bearer cannot mutate on any host, but any platform workload holding a valid internal token can, on the registry's own listener or through the gateway (D20). From T41 `publisher` is required too, and refused from a tenant bearer (D18). `#[secure(unrestricted)]` entities reject tenant-scoped queries. Registration policy covers creations only (§8.1 step 3); callers reaching mutations can revise or tombstone eligible entities, including `cf.core.*`, even in closed regions. Lifecycle, version and dependant checks provide no authority check. Exact, batch and discovery reads are authenticated only too. What `exposed = false` does and does not bound is C8 | P1 epic #4628: identity-to-permission binding first, then owner/principal checks before `unit::commit_revision` and `deletion::commit_deletion`, plus `tenant_col` + `PolicyEnforcer` (§12) |
| C7 | **The validator and cache key have no tenant or visibility dimensions.** P0's validator digests `resource_version`, `resolution_fingerprint` and T22b's normalized selected-field set (§8.5); the SDK cache key carries the same projection and fixed visibility/Context Tenant markers. Correct for managed platform-plane reads, and incomplete once tenant visibility or availability arrives | The wire form leads with a **version** byte, so P1 adds the chain versions under a new version and refuses to honour a P0 token. The cache key gains real visibility/Context Tenant dimensions without changing its projection rule |
| C8 | **`exposed = false` is not an access boundary in Profile 1.** Every P0 operation is platform-plane (`plane = 1`), yet an embedded host registers every REST provider's routes on the gateway router regardless of `exposed` (`libs/toolkit/src/runtime/host_runtime.rs:753`). So in Profile 1 registration and deletion are reachable on the gateway's one listener by any caller holding a valid internal token — the gateway validates it per route (D20), but there is no separate platform listener and no workload policy, so every platform workload is equal (C6). A tenant bearer is refused there. Tenant reads are reachable by any authenticated tenant bearer, platform reads by any valid internal token (D17). `exposed = false` keeps mutations out of the gateway's external routing only when the registry runs out of process, where discovery publishes no platform route (§8.4) | A platform listener — the `/types-registry/platform/v1/` routes move to it unchanged — and registry-side workload policy over the validated `PlatformIdentity` before mutation dispatch (C3) — not a PDP, which the platform plane bypasses (ADR-0006/0008). Only then may mutation routes be exposed through the gateway. This is toolkit/api-gateway work outside this gear, and ADR-0006/0008 already ask for the listener |
| C9 | **Implementation sequencing.** T14 adds reverse-impact refresh; T17 adds compatibility checks and effective waiver provenance, replacing the temporary `force` refusal. ADR-0004 still permanently forbids content revisions of minor-bearing Type Schemas; creation is admissible (§8.1 step 4). C8 keeps mutations internal | Remove this row when Checkpoints 3 and 4 are complete, before the isolated pilot exposes new clients (T30), and before T31 migrates existing consumers. The ADR-0004 restriction remains |
| C10 | **`batchGet` names at most 100 keys, not DESIGN §3.3's 500.** T22b's default and narrow projections reduce ordinary transfer cost, but `$select=content,resolved_schema,effective_traits,effective_traits_schema` can still request large documents for every key. The item limit is retained without claiming that it bounds aggregate response bytes; reconciliation inspecting more than 100 identifiers pages its reads | T24's reconciliation helper pages `batchGet`. A separately designed response-byte budget may justify lifting the key count later; discovery likewise limits items, and callers selecting documents should request smaller pages (§10.2) |
| C11 | **Publication ordering is declared and per release, and arrives with Phase 9.** D18 stops a publisher with a lower `publisher_version` from rewriting a claimed entity, so a rolled-back or restarting older pod no longer restores older content. What remains: the version is the publisher's claim, not an attestation (C3); two pods of **one** version with different content — typically different configuration during a rollout — overwrite each other until the rollout ends; reverting content takes a newer release, since there is no override; a pre-Phase-9 registry binary on the same database bypasses the check, which is why none may write to a migrated database (§8.4); and between T35 and Phase 9 the old behaviour holds: compare-and-swap orders writes, not releases | Identity-bound publisher authority and SPIFFE-attested versions (C3's P1 path); a privileged, audited operator restore once authorization exists |
| C12 | **No release atomicity and no owner-wide fence.** Stamps are per entity: a partially admitted release leaves some identifiers at the new version and some at the old, and an older pod can still **create** an identifier the newer release never published, or that it stopped declaring. Nothing is deleted because a manifest no longer names it | An owner-wide fence with per-entity applied stamps, if a use case needs it; deletion stays explicit |
| C13 | **No startup or reads without a reachable registry.** Publishing gears are not ready until their publication is admitted (`Required`), and SDK reads fail closed after the freshness window (§8.3). The cache is in memory, so a restart starts empty | A highly available registry is the P0 answer in Profile 3 (§8.4). A persistent snapshot store with an explicit per-use stale-if-error policy is an optional later capability, never a publication proof |


Each ceiling gets a `ponytail:`-style source comment naming the bound and the upgrade
path at the point where it bites.

---

## 10. Contracts

### 10.1 New SDK trait

`types-registry-sdk` gains this trait and **loses** `TypesRegistryClient`, which is deleted
in the cutover (D6, T31): every consumer moves onto the new trait mechanically in that same
change, so the two never serve existing embedded consumers side by side and no shim
is written (plan P23/P26). The early T30 pilot uses an isolated registry composition
and database with the new APIs only; it does not migrate an existing embedded consumer.
Registrations stay synchronous inside `init` through the local client — the reconciliation
helper, not bare `register_and_await`, so a changed configuration-built Instance updates rather
than collides — until T35–T37 move them after wiring. Consumers keep `deps = [types_registry]`
until then, because the init order is built from `deps` and `#[consumes]` wiring runs after
every `init`.

Shape follows DESIGN §3.3, minus tenancy, availability and federation. T22b supplies
projection, and T22d supplies validators before the P0 cutover. The trait is a toolkit
contract (D15): the name ends in `Api` — of the suffixes `#[toolkit::contract]` accepts, only
`Api` and `Backend` are remote-capable — every method returns
`Result<_, CanonicalError>` and takes `PlatformSecurityContext` first, and the trait carries no
default methods — a default body would make a contract method `optional`:

```rust
#[toolkit::contract(gear = "types-registry", version = "v1")]
pub trait PlatformTypesRegistryApi: Send + Sync {
    /// The one required exact-read primitive. Single and kind-narrowed reads
    /// are extension functions over it (`PlatformTypesRegistryApiExt`).
    #[idempotency(SafeRead)]
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError>;

    #[idempotency(SafeRead)]
    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError>;

    #[idempotency(IdempotentWrite)]
    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError>;

    #[idempotency(IdempotentWrite)]
    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError>;

    #[idempotency(SafeRead)]
    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError>;
}

/// Blanket-implemented for every `T: PlatformTypesRegistryApi + ?Sized`, including
/// `dyn PlatformTypesRegistryApi`. Not part of the contract, so not part of its IR.
#[async_trait]
pub trait PlatformTypesRegistryApiExt: PlatformTypesRegistryApi {
    /// A one-item `delete_entities`, sent as a one-item `:batchDelete` because the
    /// single-key route carries no publisher.
    async fn delete_entity(&self, ctx: &PlatformSecurityContext, key: IdempotencyKey,
        entity: DeleteItem, publisher: PublisherContext, dry_run: bool)
        -> Result<DeletionOperation, CanonicalError>;

    /// Submits, polls to terminality at the pace the receipt's `Retry-After` sets,
    /// and returns per-identifier outcomes. The async contract made ergonomic; not
    /// a second protocol.
    async fn register_and_await(&self, ctx: &PlatformSecurityContext, key: IdempotencyKey,
        request: RegisterEntitiesRequest, deadline: Duration)
        -> Result<RegistrationOperation, CanonicalError>;

    // get_type_schema, get_instance, get_type_schemas, get_instances, their
    // _by_uuid variants, list_type_schemas, list_instances — see below.
}
```

The mutation and operation types are DESIGN's, not renamed. `register_entities` and
`delete_entities` return the operation **as read**, not as received (D19): each adapter
submits, then calls `get_operation` and returns its result. The receipt — `operation_id`,
`status`, `replayed` — has no items, and a terminal replay (`200`) carries a terminal status
with no items, so no adapter ever builds `RegistrationOperation { items: [] }` from it. If
the read fails after an accepted submit, the error carries the `operation_id` and the caller
retries under the same key, which replays. The REST adapter keeps `Retry-After` as its own
polling hint for `register_and_await` and publication, falling back to an SDK default; the
semantic models carry neither it nor `replayed`. `register_and_await` holds one monotonic
deadline across submit and polling, honours cancellation, and on timeout names the
`operation_id` — a timeout never cancels the accepted write. `IdempotencyKey` is a trait parameter, not a
transport detail: the REST client sends it as the `Idempotency-Key` header, which the v2 route
requires and which widens `toolkit-http`'s POST retry beyond its always-retry triggers such as `429`.

`BatchGetEntitiesRequest` carries one `Projection` for all keys, `ListEntitiesRequest` carries it for discovery,
and the single-read extension helpers pass it to `batch_get_entities`. The SDK exposes typed
field constants and `light()` / `with(&[…])` / `full()` selection builders rather than a
type parameter, preserving object safety. `Default` and an explicit selection of the §10.2
default fields have one normalized identity. An empty or unsupported selection fails before
transport; the REST adapter applies the same rules to wire input.

`ListEntitiesRequest::filter` carries P0 `pattern`, `max_chain_depth`, `kind` and `lifecycle` alongside its
projection and page request. `max_chain_depth` maps to REST `depth`; `kind` uses the
existing `EntityKind` vocabulary. Omitted fields mean no restriction, except `lifecycle`,
which defaults to `Active`; the server
applies the same filter before either projected items or cursors are produced (§10.2).

**Reconciliation takes explicitly supplied desired documents (T24).** It batch-reads
their identifiers with `content` selected, compares authored canonical bytes exactly (there is no
content digest to shortcut it), supplies the read `resource_version` for
updates, and returns `UpToDate` without submitting if nothing differs. Otherwise it submits
bounded batches and polls to terminal outcomes, with bounded retries for missing dependencies
and a deadline. One idempotency key covers retries of an identical submission and its polling;
a new cycle — a re-read that changes candidates or preconditions — takes a new key (ADR-0012).
`already_exists` and `precondition_failed` caused by a concurrent publisher of the same content
lead to a re-read, not a failure. It never discovers inventory or deletes records absent from
the supplied set. `register_and_await` is the submit/poll primitive; reconciliation adds
read/compare above it.

**Publication is reconciliation run for a gear (D16).** `publish_gts` matches the publisher
signature `toolkit` defines (T29), so a gear's `gts(crates = …, publisher = …)` attribute can name
it. It takes the `ClientHub` (from which it resolves `dyn PlatformTypesRegistryApi`), a
`PublisherContext { name, version }` captured in the publishing crate (D18), the documents
gathered from the `gts_declarations()` of the crates the gear lists, and a cancellation token.
It runs reconciliation in a supervised background task —
never inside `init()`, where a remote client is not wired yet — with backoff across cycles, and
returns a status handle: *pending*, *admitted*, *rejected* or *superseded*, with per-identifier
reasons. The gear composes the handle into its readiness under its policy (D21). A permanently
rejected or superseded candidate stops retrying; a new cycle never raises its own version or
reads one from the registry. From T41 reconciliation **always submits**, equal documents
included, so the registry sees and confirms a higher version (D18); the
`UpToDate`/no-`POST` contract above holds only until then. Explicit registration callers and
configuration-built Instances call the same helper directly from `post_wiring` in T35–T37,
passing their own `PublisherContext`; a shared SDK helper forwards a context and never builds
one. Gear names in diagnostics are not identity or authority.

**Convenience read helpers are `PlatformTypesRegistryApiExt` methods** over `batch_get_entities` and
`list_entities`, keeping the contract minimal and object-safe while preserving the call shapes
consumers already use: `get_type_schema`, `get_instance`, `get_type_schemas`, `get_instances`, their
`_by_uuid` variants, `list_type_schemas`, `list_instances`. Kind narrowing costs no round
trip, since the kind is the trailing `~` of the identifier. `EntitySnapshot` likewise exposes the
materialized documents as **plain fields** — `content`, `resolved_schema`,
`effective_traits`, `effective_traits_schema` — plus a small `segments` accessor, so a
consumer that previously called the old models' computed methods reads a field instead.
No accessor is needed to reach inside a group, because outside `provenance` there is no
group to reach inside of. `gts_id`, `gts_uuid`, `kind` and `lifecycle_status` are plain
fields, present on every snapshot; the rest are optional because selection may omit them;
`Some` containing JSON `null` still represents a selected document, while `None` means
unselected or inapplicable. `origin` has only the managed variant in P0 and carries
`resource_version` and timestamps; `provenance` follows §10.2. List
helpers explicitly select the documents they read, on the page or through `batchGet`.

**The old models' client-side `effective_*` methods are deleted, and duplication is the
weakest of three reasons.**

1. **They are wrong for a schema that references outside its derivation chain.**
   `GtsTypeSchema::effective_schema` inlines only the parent's `$ref` and, in its own words,
   leaves *"non-parent `allOf[].$ref` items (mixin references) … as-is"*;
   `effective_properties` / `effective_required` walk `parent` alone. A parent chain is not a
   reference closure, so any `$ref` to a type outside the chain stays unresolved. The server
   resolves through `gts-rust`'s `resolve_schema_refs`, which closes over every resolution-bearing
   reference. An `x-gts-ref` is never inlined and is therefore outside this comparison.
2. **They are a local approximation of GTS semantics**, which
   `constraint-gts-implementation` forbids outright. The code admits it: `effective_traits`
   carries `TODO(#1723): replace with gts-rust's resolve_schema(...).effective_traits once
   that helper is exposed publicly`.
3. **They cannot cross a wire**, because they need the `Arc<GtsTypeSchema>` parent graph —
   the OoP blocker recorded in §8.4.

D3 removes the need entirely: the server materializes `resolved_schema`, `effective_traits`
and `effective_traits_schema` at admission, through `gts-rust`, and a read returns each
when explicitly selected.

**Consequence for the migration, stated because it will look like a regression.** A
materialized value is not always byte-equal to what the old method returned — it differs
exactly where the approximation was wrong, which is the unresolved-reference and
trait-default cases above. Where they differ, `gts-rust` is right by definition. Twelve call
sites across `account-management`, `resource-group` and `credstore` consume these methods
today; an assertion there that fails after the switch is reporting the old bug, and must be
updated to the materialized value rather than restored.

**`PlatformSecurityContext` is the first parameter of every method** (D15), as DESIGN's
platform client specifies and as every remote transport needs; the base contract macro does not
enforce it, so the SDK does. A caller passes
`PlatformSecurityContext::outbound_marker()`: the REST client discards it and attaches the
process's `X-ToolKit-Internal-Token`; the registry re-derives the identity from that token
(D17). The local client passes the context through without validating credentials — an
outbound marker never becomes authority — and P0 records no principal from it (C2). The tenant
plane arrives as a separate trait, `TypesRegistryApi`, not a new parameter: `SecurityContext`
first, the three entity reads only, served by its own routes (D17, plan P25, T27).

Models: `EntityKey`, `EntityLookup` (`Found` / `Unchanged` / `NotFound`; no `Failed`
without federation), `EntitySnapshot`, `EntityKind`, `LifecycleStatus`,
`Origin::Managed`, `Provenance`, `Projection`, `FieldSelection`,
`ListEntitiesRequest`, `ListEntitiesResponse`, `BatchGetEntitiesRequest`, `BatchGetItem`,
`RegisterEntitiesRequest`, `RegisterItem`, `DeleteEntitiesRequest`, `DeleteItem`,
`Operation`, `RegistrationOperation`, `RegistrationItemResult`,
`DeletionOperation`, `DeletionItemResult`,
`OperationStatus`, `CandidateStatus`. Field-for-field the DESIGN §3.3 shapes with the
out-of-scope fields absent — never renamed, so P1 adds rather than rewrites. P23 adds, without
renaming any of them: `PublisherContext { name, version }` with a typed SemVer `version`;
`RegisterEntitiesRequest::publisher` and `DeleteEntitiesRequest::publisher`, both a **required**
`PublisherContext` at request level, with no per-item field — in the models from T24, sent
by the adapters from T39. `CandidateStatus` is unchanged: a superseded or mismatched
candidate is `Failed` with reason `superseded` or `publisher_mismatch`. The publisher stamp is not a field of `EntitySnapshot` in P0: it is
reported through operation outcomes, which are never cached.

**Semantic models and wire types are separate.** The models above carry no serde, no utoipa
and no HTTP types. The REST client's wire DTOs live in their own SDK module behind the
`rest-client` feature, derive serde only, and convert to and from the models. Both directions
are needed on the client, whereas the server's `api_dto` types are one-directional, so the two
sides do not share types and the contract test keeps them aligned. This is a deliberate
exception to gear-creator's "no serde in the contract" rule, confined to the wire layer.

### 10.2 REST

Business listener per DE0801, two route sets, one plane each (D17, plan P25). Platform routes,
under `/types-registry/platform/v1/...`, require a validated `X-ToolKit-Internal-Token` and
refuse a bearer alone, on the registry's listener and through api-gateway (D20). Tenant routes,
under `/types-registry/v1/...`, require a validated bearer and refuse an internal token alone.
Both answer a canonical `401` without the credential they need. What `exposed = false` bounds,
and what it does not, is ceiling C8.

Platform routes (`PlatformTypesRegistryApi`):

| Method | Path | Success |
|---|---|---|
| `POST` | `/types-registry/platform/v1/entities` | `202` + operation; `200` on terminal replay |
| `POST` | `/types-registry/platform/v1/entities:batchDelete` | `202` + operation; `200` on terminal replay |
| `DELETE` | `/types-registry/platform/v1/entities/{entity_key}` | `202` + operation; `200` on terminal replay — removed by T41 (no publisher) |
| `POST` | `/types-registry/platform/v1/entities:batchGet` | `200`, one result per requested key |
| `GET` | `/types-registry/platform/v1/entities/{entity_key}` | `200`; `304` on a current `If-None-Match`; `404` when absent |
| `GET` | `/types-registry/platform/v1/entities` | `200` + one bounded page and a cursor; document-free by default |
| `GET` | `/types-registry/platform/v1/operations/{operation_id}` | `200` |

Tenant routes (`TypesRegistryApi`) — the three entity reads, with the same bodies, statuses and
validators as their platform twins:

| Method | Path | Success |
|---|---|---|
| `POST` | `/types-registry/v1/entities:batchGet` | as above |
| `GET` | `/types-registry/v1/entities/{entity_key}` | as above |
| `GET` | `/types-registry/v1/entities` | as above |

`get_operation` has no tenant route: a tenant cannot submit. Until T32 promotes them into the paths
T31 frees by deleting legacy v1, the tenant routes sit on `/types-registry/v2/...` (T27), since legacy v1 still holds
`GET /entities` and `GET /entities/{gts_id}`.

`Idempotency-Key` is required on every mutation, both deletion spellings included. Every `202` carries operation
`Location` and advisory `Retry-After`. Errors are RFC-9457 via
`modkit::api::problem` with `.standard_errors(openapi)`.

**Publisher context (D18).** `POST /entities` and `POST /entities:batchDelete` require a
top-level `"publisher": { "name": "…", "version": "…" }` beside `items`, applying to
every item. It travels in the body, not a header, because the request fingerprint is computed
over the canonical body, the local client has no HTTP, and the worker reads it back from the
stored operation. When present, both fields are required and `version` must be valid SemVer
within its length bound, and a request without it is `400` (T41); from a tenant bearer it
is refused. The single-key `DELETE /entities/{entity_key}` takes no body, so it cannot carry
a publisher: T41 removes it from the P0 route set, and every deletion goes through
`:batchDelete`; DESIGN keeps it for the tenant plane. A superseded item is `failed` with
reason `superseded` and `stored_version` / `offered_version` in its context, a mismatched
one with reason `publisher_mismatch` and `stored_publisher` / `offered_publisher`; the
operation's own status stays progress only.

`POST /entities:batchGet` names **at most 100 keys** — the write ceiling, not DESIGN §3.3's
500. Its default is small, but explicit document selection can make a response large;
P0 has no aggregate response-byte budget (ceiling C10). Absence is a per-key `not_found`
inside a `200`, never a `404`:
one missing key must not lose the answers for the others.

`POST /entities` **breaks** (D10): its success shape changes from `200` + per-item results
to `202` + operation, on the same path, with no transitional alias. The route's declared
stability is `unstable`, the change is called out in the changelog, and any REST caller
must move to submit-then-poll. `GET /entities/{entity_key}` keeps its route and `200`/`404`
semantics, but T22b changes its default representation as described below.

**The break is withdrawn for the T9a–T31 window** (`plan.md` P12). T9 took it early by
repointing the existing v1 routes at the database, which changed `POST /v1/entities`'s
*request* body and so refused its existing callers with `400` rather than handing them a
`202` they could adapt to. Worse, it left
`oagw` and `account-management` writing to the database while still resolving through the
in-memory `TypesRegistryClient`.

**The two versions name their array differently, on purpose.** v1 takes `entities`, v2 takes
`items`, so the T32 promotion refuses a v1 body outright — `400`, missing field `items` —
rather than accepting the field and failing later on elements that changed shape too. `items`
is also what the operation result, the discovery page and `Page<T>` call their array.

So **T9a restores both v1 routes verbatim from `main`** and registers the async surface under
`/types-registry/v2/` instead. Until T31 the two are separate paths over separate stores: no
v1 handler takes `RegistryService`, no v2 handler takes `TypesRegistryService`, and neither
falls back to the other on a miss — a v1-registered identifier is `404` on v2 and the reverse,
asserted rather than tolerated, because a fallback would report an entity as registered when
the admission meant to persist it never ran.

`/v2/` is interim by construction. T31 deletes v1 with the in-memory repository it reads, and
**T32 promotes the async surface onto the v1 paths**, so P0 still ends on one version and the
break above is reinstated there. Route paths come from one constant per version
(`routes::V1` / `routes::V2`, used by the tests too), so the promotion is a constant change
rather than a sweep. The table above describes the **post-T32** surface, which is the P0
end state.

**The read representation breaks too (D12–D14).** The old `GET /entities` returns every
match in one array, each item carrying full `content`; old exact reads likewise include
documents by default. P0 makes discovery a page and adopts DESIGN §3.3's field selection:

- **A page, not a list.** `limit` defaults to 50 and may not exceed 100; the response
  carries a cursor exactly when another match remains, and a page with a cursor is full
  (the query fetches `limit + 1`). Ordering is by canonical identifier, which is what
  makes the cursor a plain keyset — `gts_id` is unique
  and immutable, so a page boundary cannot drift or duplicate. Cursors come from
  `toolkit-odata`, which already encodes them as versioned base64url and refuses an unknown
  version. `depth`, `kind` and a non-default `lifecycle_status` join the cursor's filter
  hash as terms added only when present, on top of T22b's pattern/`$select` expression:
  no release preceded T22c, so the wire version stays `CursorV1`'s `1`, and naming or
  changing any of them is a `400`. A token resumes only while its canonical `$select`
  is unchanged.
- **Lifecycle filter (discovery only).** `lifecycle_status=active|deleted|all`, default
  `active`: `active` lists live entities, `deleted` only tombstones, `all` both. It is an
  SQL predicate on the stored status, intersected with the other filters before the page
  limit. Unknown, empty or repeated values are a `400` naming `lifecycle_status`.
  Exact reads and `batchGet` are unchanged: they always return tombstones by key.
- **P0 discovery filters.** `pattern` is a GTS pattern parsed by `gts-rust` and matched
  exactly in SQL over the stored segments (D14). `depth` is an optional **inclusive maximum number of
  GTS identifier segments**: a one-segment root has depth 1, and a derived type or
  Instance tail adds one for each segment. The same rule applies with or without
  `pattern`; a filter of `depth=2` includes depth 1 and 2. REST accepts an integer
  `1..=255` (the SDK uses `u8`) and refuses zero, negative, non-integer and overflow
  values with an RFC-9457 `depth` field violation. Count parsed `GtsId::segments()`,
  stored as `entity.chain_depth`; do not count `~` characters or traverse dependency
  edges. `kind` is optional and
  accepts only `type_schema` or `instance`, using the stored `entity.kind` and the
  same enum as read results; unknown values are `400` naming `kind`. The old v1
  `is_schema` spelling is not an alias. A caller may
  combine `pattern`, `depth`, `kind` and `lifecycle_status`; all are intersected
  **before** the page limit and projection. `limit` and `cursor` control
  traversal, while `$select` controls returned fields; none changes the match predicate.
- **One document-free default on all three reads.** Absent `$select` is identical to an
  explicit selection of `gts_id,gts_uuid,kind,origin,lifecycle_status`.
  `origin` has only the `managed` variant in P0 and carries `resource_version`, `created_at`
  and `updated_at`; its `external` variant waits for federation. The P0 default omits
  DESIGN's availability/reason and Context Tenant ownership view, which cannot be answered
  without tenancy. `publisher_name` is internal attribution, not ownership: P0 persists it
  on the entity (§9) but returns it on no read, neither in `provenance` nor as a
  stand-in for the missing ownership view, and defines no ownership group. Exposing it
  is P1 work, designed with that view. The same holds for `publisher_version` (D18). REST `origin` is
  an internally tagged object: `{"type":"managed","resource_version":1,
  "created_at":"...","updated_at":"..."}`; timestamps use RFC 3339. Federation's external
  variant adds `{"type":"external","source":"..."}` without changing the managed shape.
- **P0 selectable fields.** The complete allowlist is the five default fields plus
  `content`, `resolved_schema`, `effective_traits`, `effective_traits_schema` and
  `provenance`. `content` is the whole authored JSON document for either kind. The three
  effective documents apply only to Type Schemas and are absent on Instances. Each document
  is selected independently and appears at the top level; there is no `effective` or
  `authored` wrapper. `provenance` is the one group and contains exactly `gts_spec_version`,
  `gts_impl_version` and `compat_forced` (`null` for Instances). Selecting
  a group returns it whole; selecting a document never projects paths inside its JSON.
- **No content digest.** Authored content has no stored or selectable digest, so no
  digest name is in the allowlist. Reconciliation selects `content` and
  compares canonical authored bytes before deciding `UpToDate`, and admission compares
  them before deciding `unchanged`. The `ETag` inputs above are unaffected.
- **Selection rules.** Field names are case-insensitive with surrounding whitespace
  trimmed, matching ToolKit OData parsing. Normalize them to a sorted, unique canonical
  set; order does not affect identity, while duplicates are rejected by the parser.
  An empty, unknown, unavailable, nested, malformed or excessive selection is `400`
  RFC-9457 with a field violation naming `$select` and reason `INVALID_SELECT`, the
  reason ToolKit's own parser reports. *Unavailable* means a DESIGN §3.3 field P0 cannot
  answer — `availability` and `owned_by_context_tenant` — and *nested* any name
  containing `.` or `/`. The canonical identity is the selected names sorted and
  comma-joined, e.g. `content,gts_id,gts_uuid,kind,lifecycle_status`. In particular, reject empty comma
  segments such as `$select=content,,kind`: ToolKit's parser currently drops those,
  so the gear checks the raw spelling as well. Honor ToolKit's 2048-character and
  100-field parser limits. `gts_id`, `gts_uuid`, `kind` and `lifecycle_status` are
  mandatory on every entity of all three reads, whatever `$select` names, including a
  deleted result, so a narrow projection still says which documents apply;
  they are always in the normalized effective set, so naming them does not change cursor,
  validator or cache identity. Unselected fields
  are omitted, while a selected JSON `null` remains present. `entity_key`, per-key status and
  `etag` are batch result
  envelope metadata outside selection. An exact `ETag` is likewise outside the body.
  REST DTOs and OpenAPI mark the four mandatory fields required and non-nullable and
  every other field optional, omitting an unselected one; selected nullable values
  remain present as JSON `null`.
- **Transport placement.** Exact `GET /entities/{entity_key}` and discovery `GET /entities`
  take `$select` in the query. `POST /entities:batchGet` takes one top-level body
  `"$select"` string applying to every `items[]` key, never a per-item selection or query
  parameter. Its per-item `if_none_match` remains independent. The GET routes register
  ToolKit's OData `$select` parameter and use its extractor; this gear accepts only its
  declared options (`pattern`, `depth`, `kind`, `lifecycle_status`, `limit`/`$top`,
  `cursor`/`$skiptoken` on discovery;
  `$select` on exact read) and rejects unsupported OData options, v1-only filters and
  unknown unprefixed parameters rather than silently ignoring them.
- **Cursor and storage contract.** Discovery's cursor binds the normalized selection in
  addition to `pattern`, `depth`, `kind`, `lifecycle_status` and the keyset position;
  changing any bound filter or the selection on continuation is `400`. Absent
  `depth`/`kind` are distinct from explicit values and must be bound as such; absent
  `lifecycle_status` and explicit `active` are one binding.
  Absent selection and an explicit default set are interchangeable. The hidden canonical
  `gts_id` used for ordering remains available even if the caller does not select it.
  A metadata-only read does not fetch or parse authored/effective JSON. Selected documents
  are fetched in bounded batches within the same database snapshot as identity/current
  state, through SecureORM, without one query per entity. Trimming a fully hydrated DTO
  with `apply_select()` would not satisfy this storage contract.
- **No validator on a page** (§8.5): a page is a changing set, not an exact-key answer.

Why pagination is in P0: the old discovery response is unbounded in entity count, and the
entity count is now every gear's declarations. A `limit` alone would make the endpoint
incomplete without a cursor. The default page is document-free; a caller selecting
documents may ask for a smaller `limit`. P0's fixed item limits and per-document budgets
do not amount to an aggregate response-byte budget (C10).

**Consequence for the SDK, stated because it changes consumer code.** `list_instances` and
`list_type_schemas` are provided helpers over `list_entities` (§10.1), and their ~87 existing
call sites read payloads from the result. The helpers explicitly select those documents,
on the page itself or through an optional `batchGet` for per-key validators and caching (§8.3).
The result is complete with respect to the traversal rather than to an instant, which is the
same trade DESIGN accepts for type-filter expansion.

### 10.3 Configuration

Added at `gears.types-registry.config`, defaults per DESIGN §3.8:

```yaml
gears:
  types-registry:
    config:
      allow_compatibility_force: false
      limits:
        authored_document: 256KB
        resolved_document: 1MB
        resolution_closure: 64
        batch_candidates: 100
        activation_write_set: 512      # DESIGN §3.2; the profile is §4
        page_size_default: 50          # `GET /entities`, DESIGN §3.3
        page_size_max: 100             # at most 100, the batch-read ceiling (C10)
      registration_policy: {}          # closed by default; global `cf` implicit
      worker:
        operation_timeout: 5m          # T21 lease-handler budget; must be > 0. Not
                                       # bounded by stop_timeout: the host hard-stops
                                       # at 35s, so a long pass outlives the drain
        max_revalidation_attempts: 8   # the revalidation loop's bound, §8.1 step 4.3
        max_delivery_attempts: 8       # T21: failed deliveries before the operation
                                       # is terminalized as `system_failure`.
                                       # >0 and <= 32766: the outbox's i16 counter
                                       # less the increment the handler's own
                                       # delivery has already spent
      local_client:
        cache:
          freshness_window: 30s        # DESIGN §3.3; `0s` disables the window
          store_bound: 64MB            # bytes, not entries — see §8.3
```

The cache keys change shape: `local_client.cache.type_schemas.{capacity,ttl}` and
`.instances.{capacity,ttl}` are replaced by one `freshness_window` and one byte `store_bound`
covering both kinds. The four old keys are accepted-and-ignored for one release with a
warning naming their replacements, so an existing deployment neither fails to start nor
silently keeps a setting that no longer applies.

The cache is an SDK decorator (§8.3), so these two settings configure it wherever a client is
built. In the registry's process they come from this config. A remote consumer's resolving
client is built by `#[consumes]` wiring, and T28 decides where its cache settings are read from
without adding a per-consumer key to this gear. Transport wiring itself — local versus REST,
and static endpoint overrides — is toolkit configuration
(`gears.<gear>.config.client_wiring` / `consumer_wiring`), not a types-registry key.

`ctx.config_or_default()` makes absent config and `config: {}` equivalent to these
defaults. Existing keys (`entity_id_fields`, `schema_id_fields`, `entities`) are retained.

Test fixtures need `registration_policy` entries: `acme`, `test`, `vendor` and `x` are the
common fixture vendors, with `fabrikam`, `contoso`, `globex`, `myvendor`, `acme_corp` and
`nonexistent` in narrower ones. **No production gear is affected** — every declaration outside
`gts.cf.*` is in a test file, an inline `#[cfg(test)]` block or a doc comment, and no
`#[gts_type_schema]` in shipped code names another vendor.

#### Registration policy in P0: one of its two parameters

DESIGN §3.2 gives the policy two independent parameters per GTS Identifier Region — which
vendors may appear in the candidate's last segment, and whether an entity there may be
tenant-owned. **`allowed_vendors` is enforced in P0** (§8.1 acceptance step 3);
**`tenant_ownable` has nothing to decide**, because §9 fixes every P0 row to
`ownership_scope = 1`, `owner_tenant_id = NULL`, so no candidate can be tenant-owned in the
first place.

That is not the same as ignoring the key. Entries merge from the platform release and the
deployment, and a P1-ready deployment will carry `tenant_ownable`; refusing it would fail
startup on a valid configuration, while silently dropping it would let an operator believe a
parameter is enforced. So P0 **parses and validates it, records that it is inert, and refuses
any candidate that asks for tenant ownership** rather than quietly globalizing it — which is
unreachable through the P0 request shapes and is therefore a fail-closed assertion, not a
feature.

Everything else about the policy is P0 behaviour as DESIGN specifies it: closed by default,
the implicit global `cf` allowance, per-parameter resolution (longest literal prefix, exact key
beats any pattern, entries omitting a parameter are skipped, closed default otherwise),
`allowed_vendors` replacing rather than extending a less-specific set, the gate running before
any existence lookup, and revisions and deletions bypassing it so closing a region cannot
freeze existing entities.

The four resolution examples of DESIGN §3.2 are the P0 test matrix, reproduced here because
`tenant_ownable` reads differently under P0's global-only rule:

| Entry key | `allowed_vendors` | `tenant_ownable` | P0 effect |
|---|---|---|---|
| `gts.acme.*` | `[acme]` | `true` | `acme` admitted in its own namespace, including derivations; the ownership half is inert |
| `gts.cf.core.rg.type.v1~*` | `[acme]` | `true` | `acme` may derive from the resource-group type; derivations are global in P0 |
| `gts.cf.core.rg.type.v1~` | `[]` | `false` | the base type itself stays closed — the exact key governs only the base, `…~*` governs the subtree |
| `gts.cf.toolkit.plugins.plugin.v1~*` | `["*"]` | `false` | any vendor may register in the plugin region; still global-only |

The last row is what makes third-party plugin and permission Instances registrable without
opening `gts.cf.*` wholesale, and it is the entry a deployment onboarding another vendor needs
in addition to that vendor's own namespace.

---

## 11. Project structure

```
gears/system/types-registry/
├── docs/p0/{SPEC,plan,todo}.md           ← this spec, its plan and task list
├── types-registry-sdk/src/
│   ├── api.rs                            DELETED once consumers migrate (D6)
│   ├── contract.rs                       NEW  PlatformTypesRegistryApi (#[toolkit::contract]) + PlatformTypesRegistryApiExt
│   ├── tenant_contract.rs                NEW  TypesRegistryApi: tenant-plane entity reads (D17, T24a)
│   ├── models.rs                         shrinks: the old models go with the old trait (D6)
│   ├── entity_models.rs                  NEW  P0 models per §10.1 — no serde
│   ├── reconcile.rs                      NEW  reconciliation + `publish_gts`, returning the SDK's publication status
│   ├── cache/                            NEW  client cache as a decorator over dyn PlatformTypesRegistryApi (§8.3)
│   ├── rest_client/                      NEW  behind `rest-client`: hand-written client, wire DTOs,
│   │                                          DirectoryResolvingClient wiring (D15)
│   └── error.rs                          extend: precondition_failed, blocked_by_*
libs/toolkit-gts{,-macros}/               declare_gts_inventory!, per-crate collectors (D16);
                                          the process-global inventory is removed at T37
libs/toolkit/src/                         Gear::post_wiring hook, generic readiness contribution (D16),
                                          `.platform_authenticated()` OperationBuilder auth axis (D17)
└── types-registry/src/
    ├── gear.rs                           capabilities [system, db, rest, stateful]
    ├── config.rs                         extend per §10.3
    ├── domain/
    │   ├── admission/                    NEW  acceptance, worker, unit commit
    │   ├── compat.rs                      NEW  baseline selection + resolved comparison
    │   ├── dependency.rs                  NEW  extraction + reverse worklist
    │   ├── gts_store.rs                   NEW  build a transient GtsStore from rows (D2)
    │   ├── validator.rs                   NEW  freshness validator digest + wire form (§8.5)
    │   ├── ports.rs                       NEW  persistence ports + the row / input types
    │   ├── service.rs                    rewritten
    │   └── repo.rs                       rewritten: async, DB-backed traits
    ├── domain/local_client.rs            PlatformTypesRegistryApi over the domain service; passes ctx through
    ├── infra/
    │   ├── storage/
    │   │   ├── entity/                    NEW  one file per entity, 9 tables (`02`)
    │   │   ├── migrations/                NEW  initial migration + Migrator
    │   │   ├── repo/                      NEW  one file per repository, `runner: &impl DBRunner`,
    │   │   │                                   taking and returning `domain::ports` types
    │   │   ├── store.rs                    NEW  `Repos`: the five ports over the repositories
    │   │   ├── mapper.rs                  NEW  domain row ↔ SDK model `From` impls
    │   │   └── in_memory_repo.rs         DELETED
    │   ├── outbox.rs                      NEW  LeasedMessageHandler + wiring
    │   └── cache/                        MOVED to the SDK (§8.3)
    ├── api/rest/                         extend: operations, batchGet, batchDelete, delete-one;
    │                                          platform routes + tenant read routes, one plane each (D17)
    └── ../QUICKSTART.md                   NEW  per `02` (gear has REST endpoints)

gears/system/types-registry/types-registry/tests/
  common/mod.rs                            extend: test_db(), transient-store helpers
  gts_store_test.rs                        NEW  closure containment, load order
  admission_service_test.rs                NEW
  admission_worker_test.rs                 NEW  worker invoked directly, never polled
  dependency_repo_test.rs                  NEW
  compat_test.rs                           NEW
  operation_idempotency_test.rs            NEW
  api_rest_test.rs                         NEW  Router::oneshot
  validator_test.rs                        NEW  digest inputs, 304, batch `unchanged` (§8.5)
  rest_client_contract_test.rs             NEW  hand-written REST client vs live routes over TCP (D15)
  ready_mode_tests.rs                     DELETED (ready mode is gone)

gears/system/types-registry/types-registry-sdk/tests/
  client_cache_test.rs                     NEW  window, `fresh`, revalidation, byte bound (§8.3)
  publish_test.rs                          NEW  status handle, retry across cycles, key per cycle (D16)
```

**This tree is indicative, not exhaustive.** It fixes layer placement and names the files whose
location is a decision; the concrete module and test split inside each directory is chosen by
the task list, which is finer-grained (`todo.md` names every file it touches). A file appearing
there but not here is not a deviation — a file in the *wrong layer* is.

Layer placement per `02`. Lint coverage is partial in this repo — DE0201 (DTOs under
`api/rest/`), DE0801 (versioned endpoints), DE0309 (`#[domain_model]`) and DE13xx (no
print macros) are active; DE0101/DE0102 are skipped in `Gears.toml` but the rule stands.

---

## 12. Code style

**Authority is [`docs/toolkit_unified_system/`](../../../../../docs/toolkit_unified_system/),
not `guidelines/DNA/languages/RUST.md`** — the latter is outdated and must not be used as
a reference for this work. Relevant parts: `02_gear_layout_and_sdk_pattern.md`,
`04_rest_operation_builder.md`, `05_errors_rfc9457.md`, `06_authn_authz_secure_orm.md`,
`11_database_patterns.md`, `12_unit_testing.md`.

Workspace clippy is pedantic; `unwrap_used` and `expect_used` are denied.

### Rules this task must follow

| Rule | Source |
|---|---|
| **No plain SQL in handlers, services or repos.** Raw SQL only in migration definitions | `11` core invariants |
| Repository methods take `runner: &impl DBRunner`, **not** `&SecureConn` — the same method then works inside and outside a transaction | `11` |
| Multi-step mutations go through `in_transaction_mapped`, which consumes the `SecureConn` and hands the closure a `&SecureTx` | `11` |
| `#[domain_model]` on **every** non-module-private `struct`/`enum` under `domain/` — enforced by lint DE0309. Strictly module-private types are exempt | `02` |
| SeaORM entities: one file per entity under `infra/storage/entity/`; repositories under `infra/storage/repo/`, one file per repository. `02` writes this as a single `infra/storage/repo.rs`; five repositories in one file read past 1200 lines, so the gear follows `mini-chat`'s `infra/db/repo/` shape and keeps `storage::repo::EntityRepo` as the import path through `repo/mod.rs` re-exports | `02` |
| `#[derive(Scopable)]` with an explicit `#[secure(...)]` on every entity — all four dimensions declared, or `unrestricted` | `06` |
| REST DTOs only in `api/rest/dto.rs`, with serde + utoipa; `From` conversions there or in `mapper.rs`. **Its input is a `domain::ports` row, not a `SeaORM` entity** — entities do not leave `infra/storage/repo/`, which maps them at the edge, so an entity ↔ SDK mapper has nothing to take | `02` |
| Local client adapter in `domain/local_client.rs`, implementing the SDK trait and delegating to the domain service | `02` |
| Gear name kebab-case (`types-registry`), endpoints `/{gear}/v{N}/{resource}` | `02`, `04` |
| A gear with REST endpoints SHOULD ship `QUICKSTART.md` | `02` |

Note on lints: `Gears.toml` currently **skips** `de0101_no_serde_in_contract`,
`de0102_no_toschema_in_contract`, `de0504_client_versioning` and
`de1101_tests_in_separate_files` in this repository. The rules still hold as
conventions — this spec requires them — but do not expect the linter to catch a
violation.

### D5 splits the traversal from the refresh

**The traversal is one scoped recursive CTE.** `toolkit-db`'s ADR-0001 supplies
`SecureSelect::with_ctes` / `SecureCteSelect::recursive_cte`: a `WITH RECURSIVE` built
entirely through `sea-query`, with the scope predicate embedded in both the seed and the
recursive member and no raw SQL anywhere, so `11`'s first invariant is satisfied. One
statement replaces two reads per hop, and the read runs inside the commit transaction while
the candidate's row is locked — where round trips are paid for in contention, not latency.

**The refresh is a loop, and could not be anything else.** Which dependents get *written* is
decided by recomputing each one and comparing `resolution_fingerprint` against the stored
digest — a function of the recomputation, not of the graph. A closure query cannot consult
it, so a CTE can only ever return the *candidate* set that a loop then filters. The bound is
therefore stated over that returned set rather than over the rows the loop ends up writing
(§4): the written set is a subset, so the same number bounds it, and only the walked set is
a number the query itself can refuse on. A candidate whose walk is over the bound is refused
even where the filter would have written fewer — deliberately, because the alternative is
running the whole unbounded refresh to find out.

**The depth cap is the bound, and that pairing is load-bearing.** `recursive_cte` requires a
`max_depth`, and a cap truncates *silently* — a dependent missing from the set keeps stale
artifacts marked current, the one failure this read must not have. Passing the write-set
bound as the cap makes truncation unreachable below the refusal: seed rows carry depth `0`,
so a dependent at shortest distance `d` appears at depth `d - 1`; a hidden dependent would
need a path of at least `bound + 2` edges, every one of whose `bound + 1` intermediate
dependents is nearer and therefore already in the set — which puts the set over the bound,
where the refusal has already fired. A returned set is complete; an incomplete walk is an
error.

**The relation is acyclic, and the walk still deduplicates.** Two edge kinds cannot close a
cycle at all (ADR-0012): derivation strictly shortens the `~`-chain, and nothing references an
Instance. What can is `$ref`, alone or combined with derivation — a base that `$ref`s a schema
derived from it — and admission refuses both over the combined edge set, because an effective
form inlines both kinds and neither cycle has a resolved form. `UNION` is nevertheless required
rather than `UNION ALL`, because a DAG's paths converge and `UNION ALL` would enumerate a
fan-in-heavy graph once per path. The depth cap then does double duty: it bounds the
re-expansion `UNION`-with-depth allows, and it keeps a row that contradicted the acyclicity
invariant from hanging the commit transaction.

Repository-owned traversal, domain-free, no raw SQL:

```rust
// infra/storage/repo/dependency_repo.rs
//
// ponytail: one scoped WITH RECURSIVE over `dependency`, depth-capped at
// limits.activation_write_set (512); measured max fan-out in-repo is 27.
// Over the bound is a refusal, never a truncated set.
// Upgrade path if that bound is hit: the generation/staging protocol in DESIGN §4.
pub async fn reverse_impact(
    runner: &impl DBRunner,
    scope: &AccessScope,
    roots: &[i64],
    bound: usize,
) -> Result<ReverseImpact, ScopeError> {
    let walk = RecursiveCte::<dependency::Entity>::new(
        "reverse_impact",
        Condition::all().add(dependency::Column::ToEntityId.is_in(roots.iter().copied())),
        // the next row's `to_entity_id` points at a walked row's `from_entity_id`
        dependency::Column::ToEntityId,
        dependency::Column::FromEntityId,
        u32::try_from(bound).unwrap_or(u32::MAX),
    );
    let rows = entity::Entity::find()
        .secure()
        .scope_with(scope)
        .with_ctes()
        .recursive_cte(walk)
        .join_cte("reverse_impact", /* entity.id = reverse_impact.from_entity_id */)
        .filter(/* entity.id NOT IN roots */)
        .select_only()
        .column(entity::Column::Id)
        .distinct()
        .limit(u64::try_from(bound + 1).unwrap_or(u64::MAX))
        .all_as::<DependentId>(runner)
        .await?;
    // over the bound -> ReverseImpact::OverBound, which the domain words as a
    // candidate refusal; otherwise the full rows, gts_id-sorted.
}
```

A refusal reached *after* the commit transaction began writing travels as
`WorkerError::RefusedAfterWrite(ItemFailure)` rather than in the `Ok(Err(..))` position
every other candidate refusal uses. The reason is the transaction: the `Ok` position
commits, and this refusal exists to prevent exactly the writes it would commit.
`process_item` unwraps it and records the failure in its own transaction, so the
distinction is invisible past the worker.

Structured logging only: `tracing::info!(gts_id = %id, operation_id = %op, "admitted")`.
No print macros (DE13xx).

### Secure ORM without a PDP

`06` requires every entity to declare its scoping dimensions. P0 entities have no active
tenant dimension, so they carry `#[secure(unrestricted)]` — `06`'s own guidance for
*"truly global tables"* — with a comment recording that P1 switches `entity` and
`version_family` to `tenant_col = "owner_tenant_id"` once tenancy lands. The columns
already exist (§9), so that is a code change, not a migration.

This carries one **named deviation** from a toolkit core invariant. `06` states: *"Every
sensitive DB access MUST be covered by a PDP decision (via `PolicyEnforcer`)."* P0 has no
PDP — deferred by decision, because DESIGN's identity-to-permission binding is itself an
unresolved prerequisite. Consequently P0 reads and writes are authenticated but not
authorized, which is what the current in-memory implementation already does, so this is
not a regression. It is ceiling C6 and it closes when the binding lands.

Because `unrestricted` denies any query whose scope carries tenant IDs, a future
tenant-scoped caller cannot silently read these tables — it fails closed. That is the
property that makes the deviation safe to hold.

---

## 13. Testing strategy

Follow `12_unit_testing.md`, with one exception for real outbox-delivery tests:

- **No timers, polling or retries in worker, domain or compatibility tests.** Invoke the
  admission worker directly with `(operation_id, runner)`.
- **Real outbox delivery may wait.** `toolkit-db` exposes no single-pass driver;
  `Outbox::push_dirty()` marks a partition and wakes a sequencer, and `Outbox::flush()`
  wakes one without naming a partition — neither waits for delivery. This exception has
  four bounds:
  1. Use only `tests/common/mod.rs::await_delivery`; no ad-hoc waits.
  2. Read immediately, then use capped exponential backoff under one deadline covering
     reads and waits. Expiry fails the test.
  3. Retry only `pending`/`running` observations, never submissions, assertions or test cases.
     Other responses must be handled immediately.
  4. Use the helper only when delivery is the subject, including outbox-backed router tests.
     Test the handler shell directly without waiting.
- **Whole passing suite under 5 s.** Per-wait failure deadlines do not replace this budget.
- Each test builds its own **SQLite `:memory:`** database and fresh service instances; no
  shared state, parallel-safe. `make test-types-registry-db` on PostgreSQL and MySQL covers the
  backend-specific lock, CAS and range-bound paths.
- Pure logic is `#[test]`, not `#[tokio::test]`.
- **Verify state with direct entity queries**, never only through a service read — a
  scope bug would hide the row.
- Table-driven tests are manual `vec![]` + loop. **Not `rstest`.** Setup helpers are plain
  `async fn` in `tests/common/mod.rs`, not fixtures.
- Naming is `{area}_{scenario}` in snake_case: `admission_update_with_stale_version_fails`,
  `dependency_reverse_impact_reaches_transitive_dependents`.
- Error variants asserted with `assert!(matches!(...), "…got: {err:?}")`.

Target 90%+ coverage. The plain SQLite gear tests plus
`make test-types-registry-db` on PostgreSQL and MySQL are part of done, not a follow-up.

**Unit** — acceptance check ordering, fingerprint computation, family key derivation
(`vM~` / `vM.n~` → one key), shape and contiguity rules, dialect spelling set,
identifier profile refusals, topological order, baseline selection.

**Compatibility semantics** — the tests that pin the 0.12.0 behaviour, from T1 onward:

| Test | Asserts |
|---|---|
| Re-validation sweep over all declared identifiers | every declared identifier still admits under 0.12.0; a failure names the identifier and the finding |
| Generated-schema diff for `#[gts_type_schema]` types | every difference from 0.11.0 output is accounted for, none silent |
| `CompatibilityVerdict::Unknown` from `compare_documents` | candidate fails with a reason **distinct** from `Incompatible`, and nothing is committed |
| Optional property added at a `Closed` level | compatible |
| Same addition at an `Open` level | incompatible — the open level already accepted arbitrary values under that name |
| Same addition at a `Partial` level | `Unknown`, not a guessed verdict |
| Dry Run on an incompatible candidate | reports the offending `ObjectLevel.path`, not just prose |
| Comparison of unresolved documents | never reachable: the code path goes through `compare_documents` only |
| Admitted revision provenance | `gts_spec_version` = `GTS_SPECIFICATION_VERSION`, `gts_impl_version` = crate version |

**Integration, per backend** — these are the tests that would have caught the bugs:

| Test | Asserts |
|---|---|
| Replay of a matching fingerprint | returns the stored operation, `202` non-terminal / `200` terminal, writes nothing new |
| Same key, different fingerprint | `409`, original operation untouched |
| Concurrent acceptance on one key | one winner, loser returns the winner after fingerprint verification |
| Update with stale `expected_resource_version` | terminal item `precondition_failed`, no silent rebase |
| Future `expected_resource_version` reached by another admission after evaluation | terminal `precondition_failed`; never commit a candidate compared against an older baseline |
| Create when identifier exists | terminal item failure, no revision |
| Concurrent first registration of one family | exactly one succeeds; family ownership is single |
| Minor admitted while `vM~` exists | refused on shape |
| `vM.3~` with `vM.2~` absent | refused on contiguity |
| Deleted predecessor | still counts as compatibility baseline |
| Batch with one failing dependency | dependent `failed` with `blocked_by_dependency`, independent branches commit |
| Circular `$ref`, in one batch or closed by a revision | refused as `invalid_schema`; no cyclic edge is ever stored |
| Revision of a base with N dependents | every dependent's `resolved_schema` and `resolution_fingerprint` refreshed in the same transaction |
| Revise a concrete Type Schema to abstract while it has a live direct Instance | terminal `dependent_invalid`; schema revision, artifacts, version and Instance value stay unchanged |
| Revise a base Type Schema to final while it has a live derived Type Schema | refresh refuses with `dependent_invalid`; the base revision is rolled back and both schemas' versions and artifacts stay unchanged |
| Abstract transition with only deleted direct Instances, or Instances of concrete derived or unrelated types | succeeds; those Instances do not prevent abstraction |
| Concurrent abstract transition and direct Instance creation, in either commit order | With one competing commit: Instance first refuses the abstract revision with `dependent_invalid`; abstract revision first makes the Instance revalidate and refuse with `invalid_value`. Further drift is subject to the usual `revalidation_exhausted` bound. Both orders asserted on SQLite, PostgreSQL and MySQL |
| Refresh yielding identical artifacts | fingerprint unchanged, nothing written, `resource_version` not moved |
| Activation set over the bound | candidate fails, no partial refresh committed |
| Duplicate worker invocation on one operation | second invocation is a no-op |
| Worker re-invoked after a rolled-back unit | revalidates from scratch and commits once |
| Dependency moved between evaluation and commit | the guard rolls the commit back; the retry re-resolves against the moved dependency and commits once, so the candidate lands at one new revision |
| Dependent created, or refreshed, after the reverse-impact scan | detected — the first on membership, the second on `resolution_fingerprint` alone, since a refresh moves no `resource_version` |
| `unchanged` re-submission while a dependency moves | still `unchanged`: an outcome that writes nothing is not guarded, so a genuine no-op cannot be turned into a failure |
| Revalidation budget exhausted | item terminal `failed` with reason `revalidation_exhausted`, naming the last drift, and nothing written |
| Two commits in flight at once | impossible: each claims the `entity_write_order` row as its first statement, so the second waits on it until the first ends. Not observable on `SQLite`, which serializes write transactions regardless — **owed as a container-backend test**. What is asserted here is that every commit claims the row at all (`a_creation_claims_the_entity_write_order_row_exactly_once` and its revision / `unchanged` siblings), which is the invariant a future writer can omit |
| Edge committed after a mover's reverse scan | the mover's scan runs after its own claim, so the edge is either already visible to it or belongs to a unit that has not committed — and that unit's own guard sees the mover's revision when it does |
| Dependent's projection moves under an artifact write | the write is a compare-and-swap on the projection state its artifacts were computed against. Unreachable while commits are serialized, since no second commit can move that row; kept as depth for the day a narrower ordering protocol replaces the claim |
| New dependant created while its dependency is being revised | the two serialize on the `entity_write_order` row: whichever commits second sees the first, so either the revision refreshes the new dependant or the dependant revalidates against the new revision. Never two commits and a stale artifact with a matching fingerprint |
| Edge added to a target being deleted concurrently (**T20**) | one of the two refuses; a tombstone never stands over a live registered dependant. The same claim, which deletion makes like every other writer of entity state — the optimistic guard does not cover this either |
| Restart | every entity and its artifacts identical, byte for byte |
| Two pods, commit on A | B's first post-commit read sees it (`nfr-multi-pod-correctness`). Under D2 this holds by construction — B reads the database, and no process-local copy can go stale |
| Second admission after a committed revision | the unit's transient store is rebuilt from the database and sees the new revision without any invalidation step |
| Transient store contents | contains the candidates and their dependency closure, and nothing outside it — a document not reachable from the closure is absent, so an accidental whole-table load fails the test |
| Read inside the freshness window after a direct database change | served from the SDK cache, i.e. stale — asserted deliberately, because this is the trade DESIGN §3.3 sanctions and it must be a decision, not a surprise |
| Same read with `fresh` | bypasses the window and returns the new value |
| Read after a terminal successful registration outcome | the cached entry is gone under **both** its identifier and its UUID key, without waiting for the window |
| `NotFound`, a failed read, a discovery page, an operation resource | never cached, each asserted separately (§8.3) |
| Cache store bound | exceeded by cached bytes, not entry count: one 1 MB document evicts where a thousand small ones do not |
| Expired entry whose content did not change | revalidated to `unchanged` and **kept**, with no full snapshot fetched |
| Two expired keys | one conditional `batchGet`, not two |
| Failed revalidation | error propagates, window is not extended (`principle-fail-closed`) |
| Validator of an unchanged entity | byte-identical across two reads; a revision changes it |
| Validator of a dependent refreshed by a base revision | changes even though its own `resource_version` did not move |
| `If-None-Match` with the current validator | bodyless `304`; with a stale one, `200` and the selected representation |
| `batchGet` with a mix of current and stale validators | `200`, `unchanged` for current keys, snapshots for the rest |
| Instance validator | omits `resolution_fingerprint` and still changes on revision |
| Validator with an unknown version field | rejected, never treated as a match |
| Same key under two selected-field sets | validators differ; a narrow token never answers `unchanged` for a wider selection. Reordered and explicit-default selections yield the same token |
| Type Schema whose `$ref` targets a type **outside** its derivation chain | `resolved_schema` closes over that reference too — the case the deleted client-side `effective_schema` left unresolved (§10.1) |
| `effective_traits` of a chain with a trait default at two levels | matches `gts-rust`, not the deleted client-side merge order |
| `$select` on exact read, `batchGet` and discovery | each applies the same field allowlist and document-free default; exact and batch return the same projected entity for one key; each document can be selected alone |
| Invalid `$select` | empty, empty comma segment, duplicate, unknown, unavailable, nested and over-limit fields return RFC-9457 `400` naming `$select`; unsupported OData options and unknown unprefixed query keys are refused |
| Projected Instance and deleted entity | Type Schema-only fields are absent for an Instance; `$select=resolved_schema` on an Instance still returns exactly the mandatory `gts_id`, `gts_uuid`, `kind: instance`, `lifecycle_status`; a deleted exact read with `$select=content` still includes `kind` and `lifecycle_status: deleted` |
| Metadata-only exact/batch/discovery reads | instrumented storage proves no authored/effective JSON column is fetched or parsed; selected documents are fetched in bounded, snapshot-consistent batches on SQLite, PostgreSQL and MySQL |
| Content equality and reconciliation | no digest field is selectable; `unchanged` and `UpToDate` follow exact canonical authored-byte equality |
| Registration policy, four DESIGN §3.2 entries | each admits and refuses exactly what §10.3's table says, including the exact-key-versus-`~*` split |
| Per-parameter resolution | longest literal prefix wins; an exact key beats any pattern; an entry omitting `allowed_vendors` is skipped so a less-specific entry supplies it; no entry means closed |
| `allowed_vendors` of a more specific entry | replaces, never extends, a less-specific set |
| Region with no entry | first creation refused, naming region **and** parameter |
| Revision or deletion in a closed region | admitted — closing a region must not freeze existing entities |
| Config carrying `tenant_ownable` | parsed and validated, never enforced, and never silently treated as enabling tenant ownership |
| Discovery page | bounded by `limit`, ordered by canonical identifier, deleted entities absent by default; `content` absent by default and present only when selected |
| Discovery `lifecycle_status` | `active` (default), `deleted` and `all` list live, tombstoned and both; composes with `pattern`/`depth`/`kind` across sparse pages; malformed or repeated values are `400`; the cursor binds the normalized value |
| Discovery `depth` and `kind` | a one-segment Type Schema matches `depth=1`, a two-segment derived schema or Instance does not; `depth=2` includes both levels, while `kind=type_schema` and `kind=instance` partition the same active fixture set |
| Combined discovery filters | `pattern`, `depth`, `kind` and `lifecycle_status` intersect in SQL before `LIMIT`; a sparse match set returns full pages, never an empty page with a cursor, and filtering is independent of `$select` |
| Discovery pattern semantics | a generated corpus is discovered under every wildcard cut, bare `~*`, minors pinned in early segments, instance tails and UUID-tail patterns, returning exactly what `GtsId::matches_pattern` accepts on SQLite, PostgreSQL and MySQL; a UUID-tail identifier is refused at storage |
| Invalid discovery filters | `depth=0`, negative, non-integer and overflow values, plus unknown `kind` and legacy `is_schema`, return RFC-9457 `400` with the offending field named |
| `limit` above `page_size_max` | refused, not silently clamped |
| Cursor traversal over a matching set larger than one page, unchanged while it is walked | every matching entity (active by default) appears exactly once across pages |
| Entity admitted mid-traversal | every entity that continues to match throughout the traversal appears exactly once; a newly admitted `gts_id` behind the cursor is skipped, while one ahead of it can appear. The cursor is a keyset over an immutable unique `gts_id`, not a snapshot, so it never repeats an entity but does not freeze membership |
| Cursor with an unknown version | rejected rather than reinterpreted |
| Cursor resumed with another selection or filter | changing `$select`, `pattern`, `depth` or `kind` is rejected with `400`; absent `$select` and the explicit default field set resume interchangeably |
| Filtered cursor traversal | mixed depths and kinds across multiple pages produce each matching entity exactly once, under any `lifecycle_status`; every page with a cursor is full and the last page has none |
| SDK cache under two selections | different normalized sets occupy different entries; reordered/default-equivalent selections reuse one entry |
| SDK cache late fill (P23) | a read started before an observed terminal outcome or a `fresh` validator change does not fill the entry when it completes |
| SDK receipt (D19) | a terminal replay returns the operation read through `get_operation`, never `items: []`; a failed read after an accepted submit surfaces the `operation_id`, and a same-key retry replays |
| `list_instances` helper over a document-free default | selects documents on the page or through `batchGet` and returns payloads, so the call shape consumers use is preserved |
| Two pods, concurrent dependency change | commit-time revision-vector mismatch rolls back and retries |
| Dry Run | full check sequence runs, nothing committed, `resource_version` unmoved |
| Dry Run of a batch | matches real-run statuses/reasons on identical initial state; admits a referrer to an in-batch base and refuses an Instance invalidated by an in-batch revision |
| Dry Run write attempts | instrumented storage observes no entity-state write or `entity_write_order` claim |
| Delete with live direct dependent | refused; count reported without identities |
| Deleted entity | exact read returns it as deleted; list excludes it |

**Publication ordering** (D18) — through the real worker and outbox on a real database, all
three backends for the concurrency cases:

| Test | Asserts |
|---|---|
| Another `publisher_name` on a claimed row | `publisher_mismatch`, nothing written |
| Unclaimed row | the first publication or confirmation claims it with name and version; a second name then mismatches |
| Lower `publisher_version` | `superseded`, no revision and no stamp change; publication stops retrying it |
| Lower version with a stale `expected_resource_version` | `superseded`, not `precondition_failed` — the stamp check precedes the precondition |
| Delayed old operation | 0.1 accepted, 0.2 commits, 0.1's worker reaches commit → `superseded`, newer content intact |
| Equal version, changed content | ordinary admission; content and stamp commit together |
| Higher version, identical content | stamp confirmed; `resource_version`, revision, `updated_at`, artifacts and validator byte-identical; no reverse-impact refresh |
| Higher version, identical minor-bearing Type Schema | metadata-only confirmation; changed content is still refused by ADR-0004 |
| Partial admission | A@2 commits, B@2 is blocked; the next cycle admits B@2 with no conflict caused by A |
| Both `unchanged` shortcuts | `PreparedUnit::Unchanged` and the in-transaction `unchanged` both re-run the stamp check under the claim |
| Request without `publisher` | `400` before an operation exists, on registration and `:batchDelete`; the single-key `DELETE` route is gone |
| Versioned deletion | equal or higher → tombstone and stamp in one transaction; lower → `superseded` |
| Failed, refused and dry-run items | never move a stamp |
| `publisher` from a tenant bearer, or with an invalid SemVer | refused before an operation exists |
| Same key, different `publisher` | `409` |
| SDK publication with every document equal | still submits and is confirmed; no `UpToDate` shortcut |
| Shared SDK helper in a crate with another version | the published version is the calling gear's, not the helper crate's |
| Database populated before the migration | rows keep the placeholder name with no version and are claimed by their first publications |

**Out of process** (D15–D17) — each against real processes or a real listener, not mocks:

| Test | Asserts |
|---|---|
| Hand-written platform REST client vs live platform routes over TCP | submit with `Idempotency-Key`, replay `200` vs `202`, `Location`/`Retry-After`, exact read with `ETag`/`304`, `batchGet` `unchanged`, discovery cursor, Problem → `CanonicalError` without loss |
| Caller authentication per route, through the real api-gateway (with and without its internal authenticator configured) and through `oop_serve` | no credential → `401`; an invalid bearer → `401`; an invalid internal token → rejected; tenant reads: a valid bearer → served on both, a valid internal token alone → `401` on both; platform routes: a valid internal token → served on both (the gateway with its internal authenticator configured), a valid bearer alone → `401` on both |
| Two processes, B's schema `$ref`s A's, B publishes first | B is not ready, then ready after A without manual action; nothing blocks startup |
| Permanently invalid declaration | publication reports it rejected with its reason and stops retrying; the gear stays not ready |
| Two publishers of identical content | both converge to `unchanged`; neither fails on `already_exists` / `precondition_failed` |
| Per-crate collectors in a release build with LTO | every declaration of a crate reached only through `gts_declarations()` survives linking |
| Ownership coverage, from the gears' `gts(crates = …)` metadata against an independent scan | every crate that calls `declare_gts_inventory!()` — test, example and doctest crates exempt unless a shipped binary links them — is listed by exactly one gear in the workspace, and in every binary configuration each enabled gear publishes the crates it lists; linking a crate whose owner runs elsewhere is not an omission |
| Registry unreachable at consumer start | the consumer starts, reports not ready, and becomes ready once the registry is reachable |
| Mutation on the registry's own listener (D20) | a valid internal token is served; a valid bearer alone gets `401`; the tenant reads serve a bearer only |
| Registry-dependent startup step (D21) | with the registry unreachable or a plugin instance not yet published, `post_init`/`start`/bootstrap do not fail boot; the dependent step completes once its prerequisite appears |
| Two plugin instances of one vendor, the worse one published first (D21) | the selector does not cache the worse choice while a locally registered instance is still invisible |
| Readiness (D21) | `Required` holds readiness on pending, rejected and a superseded deleted entity, while a superseded live entity is ready with a warning and a metric; admitted is sticky |
| Supervised publication | a panicking or exiting publication task is observed and its status is not left pending |
| Mixed versions (D18, D22) | 0.1 → 0.2 → restart 0.1: newer content and stamp survive; global validation uses the current schema; a read-modify-write of a newer payload by the older DTO is not reported as safe |
| Cold database, `cfg.entities` depending on a remotely published schema (D11 as amended) | no bootstrap cycle: the registry serves admission, the base schema publishes, then the dependent configuration entity is admitted |

**E2E** (`testing/e2e/`, pytest) — register → poll → read → re-register unchanged →
delete, plus idempotency replay and `409`, against a real server and HTTP client. One run
places types-registry in its own process (T38).
E2E tests verify deployment; Rust outbox tests verify wiring. Only these two test groups
may wait; Rust tests must use the shared helper above.

**Compatibility fixture** — pin representative `GTS Identifier → UUID` mappings, per
`constraint-single-installation`, so a `gts-rust` upgrade cannot silently move
references.

---

## 14. Boundaries

**Always**

- Follow [`docs/toolkit_unified_system/`](../../../../../docs/toolkit_unified_system/) for
  code organisation, layering, DB access and test shape. **Ignore
  `guidelines/DNA/languages/RUST.md` — it is outdated.**
- Take GTS semantics from `gts-rust`. A missing behaviour is an upstream change request,
  never a local approximation (`constraint-gts-implementation`). Discovery's SQL
  compiler is the one mirror, and it stays under differential tests (D14).
- Keep repositories on `runner: &impl DBRunner` and the Secure ORM; raw SQL only in
  migration definitions.
- Validate at the admission boundary before touching storage.
- Enforce idempotency uniqueness and `resource_version` CAS in the database, never in
  process memory.
- Run `make ci`, the plain SQLite gear tests, and `make test-types-registry-db` for
  PostgreSQL/MySQL before calling a slice done.
- Sign commits (`git commit -s`), Conventional Commits format.
- Name every deliberate simplification at the point where it bites, with its bound and
  upgrade path.

**Ask first**

- Any change to `database.sql` — it is the normative P1 target, and a P0 deviation from
  it costs a migration later.
- Adding a dependency, or enabling any `preview-` feature.
- Deviating from the migration order for `TypesRegistryClient`: the new trait must exist and
  be tested before the first consumer moves (D6).
- Widening scope into anything listed Out in §2.
- Renaming an SDK model field that DESIGN §3.3 names.

**Never**

- Populate `ownership_scope = 2` or a non-null `owner_tenant_id` in P0.
- Create `source_claim` or seed the `routing` coordination state.
- Approximate a compatibility, resolution, or matching semantic locally.
- Take an authoritative admission decision from process-local state without the
  commit-time database recheck (D2, D4).
- Hold a `GtsStore`, an entity map or any other entity-derived state beyond the admission
  unit that built it, or serve a **registry-side** read from such state. Under D2 the only
  correct lifetime is one unit, and the only read source is the database — a process-local
  copy has no invalidation channel in P0 and silently breaks the multi-pod read criterion
  of §13. The SDK client cache of §8.3 is not an exception to this: it is on the other side
  of the contract, bounded by a freshness window DESIGN sanctions, and it is never
  consulted by the registry itself.
- Serve a cached entry for an entity whose mutation the client has already observed, or
  extend a freshness window on a failed read (`principle-fail-closed`).
- Commit a partial effective-artifact refresh.
- Remove or weaken a failing test to make a slice pass.
- Reorder the acceptance checks in §8.1 — the ordering is a disclosure boundary.
- Collapse `CompatibilityVerdict::Unknown` into `Incompatible` — they are separate
  outcomes with separate reasons.
- Write raw SQL in a handler, service or repository.
- Add `sleep`, a timer, polling or a retry loop to a unit or integration test, outside
  §13's shared outbox-delivery helper. If a test needs to wait for anything the code could
  have been called for directly, the code under test is shaped wrong.
- Introduce `rstest` or fixture-based setup.
- Call the registry from a consumer's `init()`, or wait on it anywhere during startup —
  `post_init`, `start` and bootstrap sagas included. A remote client is wired only after every
  `init` (D15), and readiness, not startup, reflects admission (D16, D21).
  The one transitional exception is T31–T37: in process, through the local client, the
  registrations T31 migrated stay in `init` until T35–T37 move them; Checkpoint 8 requires none
  to remain.
- Derive a `publisher_version` anywhere but the publishing gear's own crate, raise it to get
  past `superseded`, read it back from the registry, or compare it as a string (D18).
- Synthesize an operation from a receipt (D19).
- List a crate in the `gts(crates = …)` of a gear that does not own it, or submit
  `toolkit-gts` types across a crate boundary where plain data will do (D16).
- Put serde, utoipa or HTTP types on the semantic SDK models; wire DTOs stay in
  `rest_client/` (D15).

---

## 15. Build order

**Superseded by [`plan.md`](./plan.md).** This section originally ordered the work
horizontally — schema, then repositories, then a synchronous admission path a later slice
would have rewritten. `plan.md` P1 replaces it with vertical slices and [`todo.md`](./todo.md)
is the executable task list. P26 orders remaining work in three phases: T26–T32 (both
clients, the isolated handoff at Checkpoint 7A, then every gear on the persistent registry),
T33–T38 (publication after wiring — Account Management first at Checkpoint 8A — and
out-of-process operation), then T39–T42 (the publisher-version guard for mixed-version
rollout). Generic post-wiring/supervision/readiness (T29) precedes the explicit-declaration
pilot and does not depend on per-crate collectors or the gts attribute (T33). Checkpoint 7A
is the development/integration handoff; final P0 deployment still requires Checkpoint 9. The number is kept because
other documents cite it.

---

## 16. Success criteria

1. A gear's Type Schemas and Instances survive a process restart with authored content
   and all three effective artifacts byte-identical.
2. `make ci` and the plain gear tests green on SQLite;
   `make test-types-registry-db` green on PostgreSQL and MySQL.
3. Registration returns `202` + operation; polling reaches a terminal state with one
   outcome per candidate GTS identifier.
4. Replaying an `Idempotency-Key` with the same fingerprint writes nothing and returns
   the stored operation; a different fingerprint returns `409`.
5. An update with a stale `expected_resource_version` fails `precondition_failed` — no
   silent rebase.
6. A batch where one dependency fails commits the independent branches and reports
   `blocked_by_dependency` for everything downstream of it.
7. A revision of a base type refreshes every dependent's `resolved_schema` and
   `resolution_fingerprint` in the same transaction; an identical recomputation moves
   no `resource_version`.
8. Two pods against one database: a commit on one is visible to the other's first
   post-commit read, with no process-local entity state anywhere on the read path (D2).
9. Deleting an entity with a live direct registered dependent is refused; a deleted
   entity is still exact-readable as deleted and absent from lists.
10. No tenant-scoped row and no federation table exists in any P0 deployment.
11. Every ceiling in §9 is a comment in the code at the point it binds.
12. The workspace is on `gts-rust` 0.12.0, every declared identifier re-validates, and
    an undecidable compatibility comparison (`CompatibilityVerdict::Unknown`) is rejected
    with its own reason rather than collapsed into `Incompatible`.
13. The SDK client cache is in place on the new models with a byte-bounded store, a
    freshness window, `fresh` bypass, invalidation on an observed terminal outcome and
    batched conditional revalidation — P0 does not ship an uncached read path (§8.3,
    `cpt-cf-types-registry-fr-client-cache`).
14. An exact read carries a validator and honours `If-None-Match` with a bodyless `304`;
    a batch read reports `unchanged` per key while returning `200`; a dependent refreshed
    by a base revision gets a new validator even though its own `resource_version` did not
    move (§8.5).
15. Exact read, `batchGet` and discovery honor one normalized `$select` contract and
    return document-free managed metadata by default. `GET /entities` is bounded in
    item count; under one `pattern`/`depth`/`kind`/`lifecycle_status` filter and one
    selection its cursor returns no entity twice and returns every entity that matches
    (active by default) for the whole traversal exactly once;
    selected documents are fetched only on request. P0 has no aggregate response-byte
    budget (§10.2, D12–D14, C10).
16. Every admission decision is diagnosable from the emitted signals alone (§8.6): each terminal
    outcome and each refusal is counted under a closed vocabulary, an `Unknown` compatibility
    verdict and a forced waiver are each distinguishable in the metrics, and no series blends a
    dry run with a commit or a deletion with a registration.
17. With types-registry in its own process, a gear in another process resolves
    `dyn PlatformTypesRegistryApi` from `ClientHub` with no code change, publishes its own declarations
    after wiring, reports not ready until they are admitted, and reads them back through the
    same trait. Declarations whose dependencies are published by another process converge
    without a startup barrier (D15, D16).
18. Every route serves exactly one plane, on the registry's own listener and on api-gateway
    alike (D17, D20). Platform routes (`/types-registry/platform/v1/`) serve only a validated
    `X-ToolKit-Internal-Token`; a valid bearer alone gets `401`, and a host without an internal
    authenticator refuses every platform call with `401`. Tenant read routes
    (`/types-registry/v1/`, `/v2/` until T32) serve only a validated bearer; a valid internal
    token alone gets `401`. A caller with neither credential gets `401`. An invalid credential
    beside a valid one is refused wherever that stack validates it: the registry's listener
    validates both, while api-gateway ignores an internal token on a tenant route unless its
    internal authenticator is configured. The registry seeds only its own types, the `toolkit-gts` base types and `cfg.entities`. No process-global GTS inventory
    remains (D11, D17).
19. Every mutation carries a `publisher`, and one with a lower `publisher_version` never
    changes a claimed entity: a rolled-back or delayed older publication reports
    `superseded` and the newer content and stamp survive, including a delayed outbox
    operation and partial admission. A higher version with identical content confirms the
    stamp without moving `resource_version`, the validator or `updated_at`; another
    publisher name is `publisher_mismatch`; a request without `publisher` is refused (D18).
20. No startup phase fails because the registry is unreachable or a declaration is not yet
    published; dependent steps wait for their own prerequisites, and readiness follows each
    gear's publication as `Required` (D21).

---

## 17. Open questions

The P22 questions — rolling-update authority, publisher coverage, wire serialization, header
binding, the trait name and the local client's context — are resolved in `plan.md` P22 and
recorded as D15–D17, C11 and the DESIGN amendments of §3. P23 resolves publication ordering,
the receipt, platform-only mutations, availability, startup and schema authority (D18–D22).
**One remains open for P0**, and one is deferred: O5 is closed by T34 before T35; O6 is
deferred to P1:

**O5 — who publishes bootstrap entities.** The registry's own control-plane types, the
`toolkit-gts` base types and each `cfg.entities` item need a publisher: which gear publishes
each, and from which call. How they are stamped is no longer open — with `publisher`
required (D18), every entity carries the name and version of whoever publishes it, and what
Types Registry publishes, `cfg.entities` included, carries `types-registry` and the
registry crate's version. No owner may be inferred from a GTS namespace or from the
placeholder.

**O6 — a stricter policy for a superseded publication (deferred to P1).** By default a superseded publication
of a live entity is ready with a warning (D21), because an older pod that never restarted is
already serving against the same contract. Whether a gear may opt into holding readiness on
`superseded` — and whether that is worth a per-gear setting at all, given it only affects
restarted pods — is undecided. Neither choice treats `superseded` as proof of compatibility.

Two older answers are load-bearing enough to state rather than merely close:

**O4 — there is no ADR-0015 quarantine preflight scan, and none is needed.** A scan would
establish that no stable schema has a major-0 immediate base or `$ref` target in a registry that
predated the rule, and no such registry exists:
the release that introduces the check is the release that first persists an entity. What
remains is the negative obligation the ADR states — do not enable the rule against a database
populated by a build that had the storage but not the check.

**O2 — `principal_id` is `Uuid::nil()`**, behind the named constant `P0_PRINCIPAL_ID`. A
deterministic UUIDv5 would *look like* a principal a reader could resolve to a subject, and P0
records no platform identity (ceiling C2 — D17 authenticates the caller but stores nothing);
nil is the honest spelling of "no subject". The
constant carries the greppability, and its docstring carries the nil-write warning. Nothing
downstream depends on the value: C2 makes the `Idempotency-Key` namespace global whichever
constant is digested.

---

## 18. Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Reference schema**: [database.sql](../database.sql)
- **Code organisation authority**:
  [`docs/toolkit_unified_system/`](../../../../../docs/toolkit_unified_system/) — `02`
  (layout, SDK pattern, `#[domain_model]`), `04` (OperationBuilder), `05` (RFC-9457),
  `06` (Secure ORM, `#[secure]`), `11` (DBRunner, transactions, migrations), `12`
  (test shape). `guidelines/DNA/languages/RUST.md` is **outdated and not used**
- **ADRs honoured in P0**: 0001, 0003, 0004, 0005, 0006, 0008, 0012, 0014, 0015
- **Toolkit ADRs relied on (P22)**: toolkit-oop 0001 (deployment profiles), 0002 (REST-first
  OoP), 0005 (eventual readiness), 0006 (platform-plane auth); toolkit-contract-binding PRD
  (header injection deferred to a manual `impl`)
- **P22 decision and rationale**: [`plan.md`](./plan.md) P22
- **P26 scheduling and task-ID mapping** (both clients, isolated remote pilot before fleet migration): [`plan.md`](./plan.md) P26
- **P23 decision and rationale** (publication ordering, receipt, platform-only mutations,
  availability, startup, schema authority — D18–D22, C12–C13): [`plan.md`](./plan.md) P23
- **ADRs out of P0 scope**: 0002, 0007, 0009, 0010, 0011, 0013
- **`gts-rust`**: 0.12.0 from crates.io, declaring `GTS_SPECIFICATION_VERSION = "0.13"`
- **Prerequisites closed here**: activation write set (§4), `sea-query` recursive CTE
  (D5 — verified available and *used*, on all three backends), GTS capabilities 1–7 of
  DESIGN §4 via the 0.12.0 upgrade (§7)
- **Prerequisites still open**: worker liveness bounds
  (§10.3 `worker.*` proposes values), benchmark profile. GTS capability 8 (pattern
  containment) is deferred with federation
- **`preview-outbox` reliance**: approved (D9), matching `ledger`, `file-storage`,
  `chat-engine`
