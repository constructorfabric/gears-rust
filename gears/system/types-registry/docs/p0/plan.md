# Implementation Plan: Types Registry P0

Spec: [`SPEC.md`](./SPEC.md)
Task list: [`todo.md`](./todo.md)

> **Location note.** These three artifacts live with the gear they describe, in
> `gears/system/types-registry/docs/p0/`, not in a repository-root `tasks/`. This is
> deliberate: the monorepo holds many gears, and a shared root `tasks/` would collide
> across concurrent work. Downstream commands that default to `tasks/todo.md` — including
> `/agent-skills:build` — must be pointed at `gears/system/types-registry/docs/p0/todo.md`.

## Overview

Make Types Registry durable: entities move from a process-local `gts-rust` store into the
platform database, admission becomes an asynchronous operation-based protocol, effective
artifacts are materialized, registration moves from a registry-side inventory pull to a
per-gear push, and a new SDK trait replaces the old one outright.

Global entities only — no tenant ownership, no `PlatformSecurityContext`, no PDP, no
federation.

30 tasks in 8 phases with 8 review checkpoints. Twenty-seven are S or M; three are **L** and say
why in their own entry — T25 and T26 (consumer migration across twenty-plus gears) and T28
(e2e migration across seven files), each split by gear or by file rather than landing as one
commit. Two tasks exceed the ~5 file guideline, flagged with the reason where they occur.

## Decisions taken during planning

Eleven decisions were made here rather than in the spec, because all of them are consequences
of task ordering or of facts about the runtime that only surface once the work is sliced.
P1–P5 were taken before implementation started; P6–P10 came out of reviewing Phase 1 on its way
in, and the spec has been updated to match all five. P11 is a later housekeeping close-out
(O1 resolved), not a scope decision.

### P1. The spec's §15 build order is replaced by vertical slices

§15 orders work horizontally — schema, then repositories, then "synchronous admission
core", then operations. Its slice 3 explicitly builds a synchronous admission path that
slice 6 then converts to the asynchronous one. That is the same work twice and throws away
slice 3's tests, so it is not followed.

Instead each phase from 1 onward delivers **one complete registration path**, async-shaped
from the first commit. Phase 1 registers a single dependency-free global Type Schema
end to end: migration → entity → repository → transient store → acceptance → worker → REST.
Later phases widen that path without reshaping it.

No decision from SPEC §3 changes. Only the order does.

### P2. Types-registry seeds itself by invoking the admission worker directly

types-registry owns the `toolkit-gts` base types and its own control-plane types (DESIGN:
*"`toolkit-gts` base types default to `types-registry` ownership"*). It cannot register
those through a client — it *is* the registry, same process, same database.

It resolves itself from §8.1: the worker is a plain function of `(operation_id, runner)`,
not a task. So `init()` creates the operations and calls the worker **inline**, with no
outbox and no barrier. This is not a privileged second path; it is the same function the
outbox handler calls. Seeding is deterministic and complete before the client is published.

This is also what makes the §13 no-polling rule satisfiable, so it is the same property
being exploited twice.

### P3. The outbox worker starts at the end of types-registry's `init()`

Verified phase order (`libs/toolkit/src/runtime/host_runtime.rs:6-15`):

```
pre_init (system) → DB migrations → init (all gears) → proxy-wiring
→ post_init (system) → REST wiring → gRPC → start/stop (stateful)
```

`init` of **every** gear precedes `start` of **any**. A worker living in the stateful
`start` entry would therefore not exist while consumers are initializing — and ~13 existing
call sites already register from their own `init()` and block on the result. Those would
hang.

That is avoidable. `GearCtx` exposes `cancellation_token()`
(`libs/toolkit/src/context.rs:166`) and `OutboxHandle::stop()` is an ordinary async
shutdown (`libs/toolkit-db/src/outbox/manager.rs:620`), so the worker starts from `init()`
with correct cancellation wiring and stops from the stateful `stop`. `init` order is
topological and types-registry is a declared dependency of its consumers, so its worker is
live before any consumer's `init()` body runs.

**Order inside types-registry's `init()`:** repositories → inline seeding of
its own types (P2) → start the outbox worker → publish the client. Seeding precedes the
worker start and enqueues nothing, so seed operations cannot be leased concurrently. There is
no snapshot-load step: per P6 seeding builds its own transient store like any other
admission, and reads go to the database.

The resulting rule has no phase caveat: **after types-registry's `init()`, submit and await
work from anywhere.** A consumer registering during its own `init()` must declare
`deps = [types_registry]`, or `init` ordering is not guaranteed.

**Who admits, and when.** Acceptance and admission are separate moments with separate
executors:

| Moment | Accepts | Admits |
|---|---|---|
| types-registry `init()` — its own types | types-registry | itself, inline, no outbox (P2) |
| A gear reconciles its inventory | registry code in the **caller's** task (local client in-process; a gRPC client out-of-process) | the outbox worker |
| REST at runtime | types-registry's Axum handler | the outbox worker |

Acceptance is always synchronous, in the caller's task. Admission is performed by exactly
one outbox worker owned by types-registry — one in the system for a single-binary
deployment.

### P4. Registration moves from pull to push, in P0

A gear does not know whether it runs in-process or out of process, and its code must not
depend on that. The registry-side **pull** violates this in the worst way: a gear's code is
unchanged, but its types silently vanish if it moves out of process, because
`all_inventory_type_schemas()` only sees `inventory` records linked into *this* binary.

The platform already runs the transparent mechanism for half of this. Roughly eleven plugin
gears already **push** their well-known Instances by calling `registry.register(...)` from
their own `init()` through the ClientHub-resolved SDK trait — in-process that is the local
client, out of process it would be a gRPC client, and the gear's code is identical either
way. Only inventory-declared *schemas* still travel by pull.

P0 therefore moves schemas onto the same path, as DESIGN already specifies: *"The SDK
filters records by `owning_gear` and reconciles them, replacing the current registry-side
process-wide pull with a per-gear push that works across processes."*

- types-registry seeds **only what it owns** — `toolkit-gts` base types and its own
  control-plane types — inline, per P2.
- Every other gear reconciles its own declarations with **one SDK call**. The five-step
  reconciliation workflow of DESIGN §3.3 lives in the SDK, not in each gear, so no gear
  hand-rolls batching, idempotency or retry.
- `InventoryTypeSchema` / `InventoryInstance` gain `owning_gear`, derived from the declaring
  crate's gear name, so the SDK can filter and attribution stops being a constant.

**This is where registrant-side retry becomes real** — and it lives in the SDK helper. The
earlier answer that no retry was needed was conditional on keeping pull.

It also simplifies the cutover. Seeding no longer has to topologically order ~200 entities
across every gear and detect cycles at startup; types-registry seeds its own small set, and
cross-gear ordering is handled by the retry DESIGN sanctions: *"acyclic dependencies can
converge through retry."*

Cost: a `toolkit-gts` and macro change, plus one line in roughly fifteen gears. Benefit:
ceiling C3 disappears, out-of-process operation is unblocked, and the transparency
requirement actually holds.

### P5. The old SDK trait is removed in P0, not deprecated alongside the new one

Taken here first; SPEC **D6 now records it**, so the two agree. The version of D6 this
replaced kept both traits and deferred consumer migration to a separate commit.

Two facts forced the change. First, async admission makes the old synchronous
`register(Vec<Value>) -> Vec<RegisterResult>` unrepresentable: thirteen call sites call
`RegisterResult::ensure_all_ok(&results)` immediately and would start reading `pending` as
success. Keeping the old trait means keeping a blocking submit-then-await adapter behind a
signature that no longer describes what happens. Second, the old models cannot cross a wire
at all — `GtsTypeSchema.parent: Option<Arc<GtsTypeSchema>>` and
`GtsInstance.type_schema: Arc<GtsTypeSchema>` are in-process object graphs — so retaining
them retains an out-of-process blocker that P4 exists to remove.

So the old trait goes, and every consumer migrates inside P0. The real surface is larger
than the thirteen register sites: reads dominate (`list_instances` ~59 references,
`get_type_schema_by_uuid` ~31, `get_type_schemas_by_uuid` ~28), for roughly fifty files
across twenty-plus gears. Migration is split by gear group across two tasks so no single
task carries it all.

### P6. No store is held between admissions: transient store, reads from the database

This replaces the original SPEC D2 and §8.2, both of which have been rewritten. It changes
T5 and T8; nothing else in the graph moves.

The original shape was one immutable `ArcSwap` snapshot of the `gts-rust` store, loaded from
the whole entity table at init and rebuilt after every successful admission unit, serving
both semantic evaluation and consumer reads. Reviewing T5 before building it surfaced that
this conflates two needs with different answers.

**What actually needs a `GtsStore`** is semantic computation over related documents —
resolution, `compare_documents`, derivation chains, instance validation. All of it happens
inside admission, over one candidate and what that candidate consumes. That set is exactly
the dependency closure, which the `dependency` table already supplies (D5).

**What reads need is rows.** Verified rather than assumed:
`InMemoryGtsRepository::list` (`in_memory_repo.rs:241-274`) iterates the store as a plain row
container and filters with `GtsIdPattern`, which is a pure function in `gts-id` over the
identifier string — it never asks the store a semantic question. Exact reads are keyed
lookups. And D3 already materializes `resolved_schema` / `effective_traits` /
`effective_traits_schema` on the current-state row. So a read is a `SELECT` plus, for a
pattern query, `GtsIdPattern::matches` in Rust. Nothing about GTS semantics is
reimplemented, so `constraint-gts-implementation` is untouched.

**And the snapshot was not merely unnecessary for reads, it was wrong for them.** SPEC §13
requires *"two pods, commit on A, B's first post-commit read sees it"*
(`nfr-multi-pod-correctness`). P0 has no cross-pod invalidation — no pub/sub, and the outbox
belongs to the committing pod — so B would serve its stale snapshot indefinitely. Admission
survives stale input because of the commit-time revision-vector guard (D4, T15); reads have
no guard, so for them staleness is simply incorrect. This would have shipped as a passing
implementation of a failing criterion.

Consequences, including the cost:

- Reads become a database round trip, which is what the SDK client cache is for. **The
  earlier decision to delete that cache is reversed** — see P7.
- Ceilings **C1** (whole entity set in memory) and **C4** (startup reads the whole table) are
  retired rather than deferred: there is no warm-up read and no process-lifetime store.
- `Mutex<GtsOps>` disappears instead of being replaced. A store owned by one worker
  invocation is never shared, so `GtsOps` not being `Sync` stops being a design constraint.
- The transient store is *cheaper* than what it replaces: the snapshot cost a full rebuild
  after every successful unit; the closure is bounded by the candidate's own dependencies.

### P7. The SDK client cache is kept, not deleted — DESIGN requires it

This reverses a removal both earlier revisions of the spec carried, and it adds **T30**.
SPEC §8.3 is new and records the contract.

DESIGN requires a client cache outright: `cpt-cf-types-registry-fr-client-cache`, *"bounded
per-client representation cache with batched conditional revalidation and fail-closed expiry
handling"*, with the full contract in DESIGN §3.3. The removal rested on two claims, and
neither holds.

The first was ours and expired with P6: *"an LRU in front of an in-memory snapshot is pure
overhead plus a staleness window."* True while reads came from memory; once reads are a
database round trip, a cache buys what it costs.

The second was a misreading, and worth naming precisely because it nearly shipped.
`nfr-cache-correctness` forbids *"an invalidated result accepted as current after the client
observes the mutation"* — it does not forbid a freshness window, and DESIGN §3.3 says so in
as many words: *"a remote mutation not yet observed may produce a stale snapshot within the
bounded window but is not described as an invalidated entry accepted as current."* The window
is the sanctioned trade. And it is a different NFR from `nfr-multi-pod-correctness`, which
governs the **registry's** reads — *"no process-local authority"* — and which P6 satisfies.
Two NFRs about two different sides of the wire were treated as one.

So P0 builds DESIGN's cache minus what needs absent inputs: bounded store, freshness window,
`fresh` bypass, invalidation on an observed terminal outcome, dual identifier/UUID indexing,
and DESIGN's list of what is never cached. Deferred with tenancy: batched conditional
revalidation against freshness validators, and the projection / visibility / Context-Tenant
key dimensions. Recorded as ceiling **C7** rather than left implicit.

Two things this changes beyond the decision:

- **The bound becomes bytes.** Today's default is `capacity: 1024` entries while §3.2 caps
  one resolved document at 1 MB, so the configured bound permits ~1 GB. DESIGN makes the
  same argument and picks 64 MB; adopted. This is a live bug in the current defaults, not a
  new requirement.
- **The cache cannot be carried over as-is.** It is typed on `GtsTypeSchema` / `GtsInstance`,
  which P5 deletes, so it is ported onto `EntitySnapshot`.

**Ordering.** T30 lands last, after the cutover rather than with it, and that is deliberate:
the cache is an optimization over a read path that must be correct first, and T24 is already
the largest task in the plan. The cost is one window — T24 through T28 — where reads are
uncached. Checkpoint 7 gates on T30 being done, so P0 does not finish without it.

### P8. P0 ships the platform-plane API on the business listener; the plane is contract-deep, not transport-deep

SPEC §8.4 is rewritten and ceiling **C8** is added. No task moves; T9 and T27 gain criteria.

Registering a global entity is a platform-level operation, and P0 already treats it as one in
the data — §8.1 writes `plane = 1`, `tenant_id = NULL` on every operation record. The earlier
§8.4 nonetheless marked "Tenant REST" as the P0 surface and deferred everything platform,
which left the spec claiming a tenant-plane transport for platform-plane rows. So: **the P0
REST surface and SDK are the platform-plane API for global entities** — async registration and
reads, no tenant ownership — and that is what the e2e suites exercise.

The plane is not enforced by the transport, because the platform offers an in-process gear no
way to do it. Verified in the code, not assumed:

- `internal_auth_middleware` — inbound platform plane over HTTP — is installed only in
  `libs/toolkit/src/runtime/oop_serve.rs:390`, the per-gear server for a gear running *out of*
  process. api-gateway's `internal_auth` is an outgoing gRPC credential for DirectoryService
  (`gears/system/api-gateway/src/gear.rs:823-826`), not an inbound validator.
- api-gateway has one API listener; its only second listener is for health probes. ADR-0006/0008
  ask for a separate platform listener, which nothing implements.
- `OperationBuilder` has no `.platform()`, and the middleware is permissive — a missing token
  passes with no `PlatformSecurityContext` (`toolkit-http-middleware/src/auth.rs:220`).

**The routes therefore keep the authentication they have.** Switching them to `.anonymous()`
to signal "not tenant traffic" would be a security regression, not progress: with no platform
identity available it would let anything reaching the gateway register a global type. Same
shape as the PDP deviation — authenticated, not authorized, gap named (C6, now C8).

One trap avoided: advice to serve platform routes `.anonymous()` and **not** `.exposed()` is
written for a gear's own OoP listener. `exposed` defaults to `false` (internal-only), so
copying it here would make the routes unreachable for the e2e suites that must call them.

**The later gRPC move is expected, not contingent.** A REST contract method taking
`&PlatformSecurityContext` first is rejected at compile time —  *"generated client cannot
source the internal token… serve over gRPC or write a manual client"*
(`toolkit-contract-macros/src/rest_contract_parse.rs:337-347`, UI test
`rest_platform_secctx_rejected.rs`). P0's client is REST-generatable precisely because it
carries no platform identity; adding identity closes REST codegen and leaves gRPC or a manual
client over `attach_internal_token_http`, which today has zero call sites in the repository.
Recorded in §8.4 so the P1 decision is a consequence someone already wrote down.

### P9. Freshness validators are in P0 — the deferral rested on a misread input table

SPEC §8.5 is new, ceiling C7 shrinks, and **T29** is added; T23 and T30 gain criteria.

Both earlier revisions listed *"freshness validators, `ETag` / `If-None-Match`, conditional
reads"* as out of P0 because they *"need the validator inputs tenancy supplies."* That reason
does not survive DESIGN §3.3's own input table
(`cpt-cf-types-registry-tech-freshness-validator`):

| Validator input | Managed | In P0 |
|---|---|---|
| `entity.resource_version` | ✓ | yes — CAS is already P0 (T11) |
| `type_schema.resolution_fingerprint` | ✓, Type Schemas only | yes — materialized by D3 (T8) |
| subject visibility-chain version | ✓ **tenant plane only** | **not applicable** — DESIGN: *"a platform read has no subject visibility chain"*, and every P0 read is platform-plane (P8) |
| Context Tenant availability-chain version | ✓, only when availability is selected | not applicable — availability is out of P0 |
| routing generation | — external only | not applicable — federation is out of P0 |
| `external_revision`, `content_hash` | — external only | not applicable — Externally Managed Entities are out of P0 |
| normalized projection | ✓ | yes — no `$select` in P0, and DESIGN says absent `$select` *equals an explicit default set*, so it is a constant marker |

The tenant inputs are not missing in P0; they **do not participate** in a platform-plane read.
So a P0 validator is fully computable: a versioned digest over `resource_version`,
`resolution_fingerprint` and a projection marker. DESIGN even fixes the wire form — base64url
of a versioned JSON object, 128-bit managed digest, ~48 characters.

The framework is not a blocker either. `OperationBuilder::no_content_response` takes an
arbitrary status, so `304` is declarable, and `file-storage` already returns
`StatusCode::NOT_MODIFIED` with headers by hand
(`gears/file-storage/file-storage/src/api/rest/handlers.rs:212`). There is no ETag helper in
the toolkit — this is manual work with a working precedent, not missing capability.

**What made this urgent rather than merely available.** The validator has to be in the SDK
models from **T23**. Adding it afterwards is a breaking change to a contract that ~50 call
sites across twenty-plus gears will already have moved onto (T25, T26). Deferring validators
would therefore not have been a neutral scope cut — it would have bought a second migration.

Consequences:

- Ceiling **C7** shrinks from *"the cache expires rather than revalidates"* to just the
  missing key dimensions: T30's cache now does DESIGN's batched conditional revalidation and
  fail-closed expiry, which is what `fr-client-cache` actually requires.
- A `304` replaces a resolved document of up to 1 MB on the hot read path, which is the
  cheapest thing available for `nfr-lookup-latency` after D3.
- The digest must carry the projection marker from day one; otherwise a P1 `$select` token
  produces a false `unchanged` (RFC 9110 §8.8.3). The versioned wire form is the escape hatch,
  but paying one field now is cheaper than relying on it.

**Ordering.** T23 (field in the models) → T27 (routes exist) → **T29** (computation, `ETag`,
`304`, per-key batch validators) → T30 (cache revalidates against them). T30 was renumbered
from T29 to keep task numbers in dependency order.

### P10. Discovery is paged and content-free in P0; `$select` and expansion stay out

SPEC gains decision **D12** and a rewritten §10.2; §2's row is split. T4, T23, T27 and T28
gain criteria; no task is added.

`Discovery cursors, $select projections, OData pagination, expand_type_filter` were listed out
of P0 with the reason *"P0 keeps the current flat list"* — a restatement of the decision, not a
reason for it. Examined item by item, the four are not one decision:

**Pagination and cursors are in.** Three facts settled it. SPEC §10.1's own trait already
returns `EntityPage`, so deferring the cursor left a page that is a page in name only — the
spec contradicted itself. DESIGN specifies the route as *"`200` with one page and a cursor"*
over *"content-free discovery"*. And the cursor's inputs degenerate exactly as the validator's
did: of the seven DESIGN binds into it — query, subject visibility context, Context Tenant,
authorization scope, routing generation, per-source position, running item count — P0 keeps
**two**, query and position, because the rest are tenant-plane, PDP, or federation. Position is
free: T27 already required ordering by canonical identifier, so the cursor is a keyset over a
unique immutable column, and `toolkit-odata` (`page.rs`, `pagination.rs`) already encodes
cursors as versioned base64url that refuse an unknown version.

**The current shape is also a live problem, not only a spec gap.** `GET /entities` returns
every match in one array with each item's full `content`; with artifacts materialized (D3)
that is *entity count* × up to 1 MB, and after the pull→push cutover the count is every gear's
declarations. A `limit` alone would not have fixed it — without a cursor the bound makes the
endpoint incomplete rather than large, which is why D12 lands both together.

**The default projection is in; arbitrary `$select` is out.** The default field set is what
makes a page content-free, so it is not optional. Caller-chosen sets need optional fields
across the models plus a normalized field-set digest inside the validator, and buy nothing
while there is a single representation to select from — that half stays in ceiling C7.

**`expand_type_filter` is genuinely blocked**, and this is the one item whose original
placement was right for the wrong reason. Its DESIGN definition *is*
`$select=gts_uuid&availability=available`, with the availability filter fixed by the method
rather than supplied by the caller. Availability (ADR-0010) needs tenancy and is out of P0, so
a P0 method under that name would report retired contracts as usable. A same-named different
meaning is worse than absence; a caller wanting the traversal pages `list_entities` itself.

**The consequence to plan for, because it lands in consumer code.** `list_instances` and
`list_type_schemas` are helpers over `list_entities`, and their call sites read payloads from
the result. Against a content-free page the helpers hydrate through `batchGet`: one extra round
trip per page, absorbed by the client cache on repeat, complete with respect to the traversal
rather than to an instant. That is the same trade DESIGN accepts for expansion, but it means
T25/T26 migrate call sites onto a two-step read rather than a renamed one-step read.

### P11. O1 closed — `gts-rust` 0.12.0 published, the local-checkout patch removed

Housekeeping, not a decision about scope: no task is added or renumbered.

T1 consumed 0.12.0 from `~/dev/gts-rust` via `[patch.crates-io]` because the crate wasn't
published yet, and every plan doc tracked that as O1 — a merge blocker. On 2026-08-18 the
crates went live on crates.io (`gts`, `gts-id`, `gts-macros`, all `0.12.0`, confirmed
against the crates.io API), so O1's precondition is met and it closes.

Closing it out: the `[patch.crates-io]` block and its `DO NOT MERGE` comment are deleted
from `Cargo.toml`; the three `"0.12.0"` requirements are untouched, since they already
named the target version rather than the local checkout's declared one (SPEC §7 explains
why that mattered). `Cargo.lock` was re-resolved (`cargo update -p gts -p gts-id -p
gts-macros`) and now carries a registry `source` and checksum for all three instead of a
path override — it is committable for the first time on this branch.

Re-verified against the published crate rather than assumed unchanged: `cargo check
--workspace` clean, `cargo test -p cf-gears-types-registry` 209 passed / 0 failed (T1's
208 + the 6 `gts_012_semantics_tests.rs` tests), `make gts-docs` 798 files / 0 errors.
Full detail in [`t1-gts-0.12.0-upgrade-report.md`](./t1-gts-0.12.0-upgrade-report.md) §6.

## Dependency graph

```
T1 gts-rust 0.12.0  ─────────────────────────────────┐  (blocks all: semantics change)
                                                     ▼
T2 migration ──► T3 entities ──► T4 repositories ──► T5 transient store ──┐
                                        │                                 │
T6 config ──────────────────────────────┴──► T7 acceptance ──► T8 worker (single candidate)
                                                                   │
                                                        T9 REST: POST, GET op, GET entity
                                                                   │
                     ┌─────────────────────────────────────────────┤
                     ▼                                             ▼
        T10 instances                                  T11 revisions + CAS
                     │                                             │
                     └──────────────► T12 family rules ◄───────────┘
                                             │
                              T13 dependency edges
                                             │
                     ┌───────────────────────┼───────────────────────┐
                     ▼                       ▼                       ▼
        T14 reverse impact      T15 revision-vector guard    T16 observability
                     │                       │
                     └───────────┬───────────┘
                                 ▼
                     T17 compatibility ──► T18 derivation + quarantine
                                 │
                     T19 partial admission ──► T20 delete + dry run
                                 │
        ┌────────────────────────┼────────────────────────┐
        ▼                        ▼                        ▼
   T21 outbox        T22 toolkit-gts owning_gear    (T19 enables T24)
        │                        │
        └──────────┬─────────────┘
                   ▼
        T23 new SDK trait + reconciliation helper
                   │
        T24 CUTOVER: registry seeds only its own; ready mode + in-memory repo out
                   │
        ┌──────────┴──────────┐
        ▼                     ▼
   T25 migrate system    T26 migrate domain gears,
   gears + plugins       delete the old trait
        └──────────┬──────────┘
                   ▼
        T27 REST completion + OpenAPI + QUICKSTART
                   │
        T28 e2e suites move to submit-then-poll

        T29 validators + conditional reads (needs T23 + T27)
                   │
        T30 SDK client cache on EntitySnapshot — revalidates against T29 (P7)
```

Foundation order (T2→T5) is unavoidably layered: nothing can be registered before a table
exists. From T7 onward the graph is vertical.

## Task index

### Phase 0 — Upgrade (fail fast)
- T1: Upgrade to `gts-rust` 0.12.0 via `[patch.crates-io]`, re-validate all declared identifiers

**Checkpoint 0**

### Phase 1 — One global Type Schema, persisted, async, end to end (fixtures only)
- T2: Migration for the 9 tables
- T3: SeaORM entities for the core six
- T4: Repositories on `DBRunner`
- T5: Transient `gts-rust` store built from database rows
- T6: Typed configuration
- T7: Acceptance path and operation records
- T8: Admission worker — one dependency-free candidate
- T9: REST — `POST /entities`, `GET /operations/{id}`, `GET /entities/{key}`

**Checkpoint 1** ← proves the architecture

### Phase 2 — Instances, revisions, concurrency
- T10: Registered Instances
- T11: Content revisions and compare-and-swap
- T12: Version-family kind, shape and contiguity rules

**Checkpoint 2**

### Phase 3 — Dependencies and materialization
- T13: Dependency edge extraction and writes
- T14: Reverse-impact worklist and artifact refresh
- T15: Revision-vector guard and bounded retry
- T16: Observability for the admission path

**Checkpoint 3**

### Phase 4 — Compatibility
- T17: Compatibility against one baseline
- T18: Derivation chain and major-0 quarantine

**Checkpoint 4**

### Phase 5 — Batching, deletion, dry run
- T19: Dependency-aware partial admission
- T20: Deletion and Dry Run

**Checkpoint 5**

### Phase 6 — Dispatch and the new contract
- T21: Outbox dispatch wiring
- T22: `toolkit-gts` — `owning_gear` on inventory records
- T23: New SDK trait and the reconciliation helper

**Checkpoint 6**

### Phase 7 — Cutover and migration
- T24: **Cutover** — registry seeds only what it owns; ready mode and in-memory repository out
- T25: Migrate system gears and plugins onto the new trait
- T26: Migrate domain gears; delete the old trait
- T27: REST completion, OpenAPI, QUICKSTART
- T28: Update e2e suites for the `202` contract
- T29: Freshness validators and conditional reads (`ETag` / `304`, batch validators)
- T30: SDK client cache — window, byte bound, `fresh`, conditional revalidation

**Checkpoint 7 — ready for review**

## Checkpoints

Each checkpoint is a human review gate. Do not proceed past a failing one.

**Checkpoint 0** — `make ci` green; every declared GTS identifier still admits under
0.12.0; every difference in generated schema documents accounted for. This gate protects
other gears, so it is reviewed before any registry code is written.

**Checkpoint 1** — a fixture Type Schema registers over REST, the operation reaches
`completed`, the entity and its resolved artifacts are readable, and both survive a process
restart. Consumers are untouched: the old trait is still served from its existing in-memory
repository, while the new path reads from the database and holds no store between admissions
(P6). `make test-db` green on three backends. This checkpoint proves the architecture.

**Checkpoint 2** — Instances register; equal content reports `unchanged` without a revision;
a stale `expected_resource_version` fails `precondition_failed`; family shape and contiguity
refusals hold under concurrency.

**Checkpoint 3** — a revision of a base type refreshes every dependent's artifacts in one
transaction; an identical recomputation moves no `resource_version`; the activation bound
refuses rather than partially committing; admission emits spans and metrics.

**Checkpoint 4** — the compatibility matrix passes, including `Unknown` rejected with its
own reason; provenance is persisted on every revision.

**Checkpoint 5** — a batch with a failing dependency commits independent branches and
blocks the dependent; a cycle with one invalid member commits nothing; Dry Run writes
nothing.

**Checkpoint 6** — an operation submitted through the outbox reaches `completed` without a
direct worker call; inventory records carry `owning_gear`; the new trait and its
reconciliation helper work against a mock consumer. Nothing has been cut over yet.

**Checkpoint 7** — every gear reconciles its own declarations and gates its own readiness;
the platform boots; the old trait is gone and no consumer references it. The SDK client cache
is in place on the new models, with its window, byte bound and `fresh` bypass (P7) — P0 does
not finish with an uncached read path. All 15 success criteria of SPEC §16; `make ci`,
`make test-db`, `make e2e-local`, `make dylint` green; O1 resolved (P11).

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| 0.12.0 semantics reject a currently-admitted schema in another gear | **High** — breaks unrelated gears | T1 is first and is its own commit; full re-validation sweep before any registry code |
| Pull→push cutover regresses platform boot | **High** — every gear now gates on its own registration | T24 lands after the helper is proven against a mock (Checkpoint 6); migration split across T25/T26 by gear group; each verified by booting the example server |
| Removing the old trait breaks ~50 call sites in 20+ gears | **High** | Split by gear group; new trait exists and is tested (T23) before the first consumer moves; `cargo test --workspace` gates each migration task |
| `[patch.crates-io]` on an absolute local path | **High** — CI cannot build, `Cargo.lock` poisoned | **Resolved 2026-08-18 (P11):** never committed with the patch in place; O1 closed by publishing `gts` 0.12.0 to crates.io and deleting the patch |
| A gear's registration fails at startup and it gates readiness on it | Medium | This is DESIGN-intended (*"Each gear gates only its own readiness"*), but it is a real behavioural change; the SDK helper retries, and failures name the gear and identifier |
| Dual path (in-memory + DB) live through phases 1–5 | Medium | The DB path has no consumer until T24; no dual-write, no reconciliation between them. P6 keeps them from converging by accident: the new path holds no persistent store, so there is no second copy of entity state that could drift from the old repository |
| Read latency regresses at T24, when reads move from memory to the database | Medium | Correctness first, then the cache: D3 already materializes what a read returns, so a read is one keyed `SELECT`, and T30 restores caching with DESIGN's contract (P7). The exposure is the T24–T28 window, which is why Checkpoint 7 gates on T30 |
| A cached entry can be stale inside its freshness window | Low | DESIGN §3.3's sanctioned trade, and now bounded further: T29's validators let T30 revalidate rather than guess, `fresh` gives an authoritative read, `0s` disables the window, and invalidation is immediate on an observed terminal outcome |
| The validator field reaches the SDK models after consumers have migrated | **High** — a second migration across 20+ gears | T23 carries the field from the start, before T25/T26 move any consumer; T29 only fills it in (P9) |
| A materialized `effective_*` value differs from the deleted client-side computation | Medium — reads as a regression, invites a "fix" back to the old wrong answer | 12 call sites in `account-management`, `resource-group`, `credstore` consume those methods today. The old ones resolved only the parent `$ref` and approximated trait defaults (`TODO(#1723)`), so `gts-rust` is authoritative; T25/T26 carry an explicit criterion to accept the new value, and SPEC §13 pins the outside-the-chain `$ref` case as a test |
| Content-free discovery turns one-step list reads into list + `batchGet` at ~87 call sites | Medium | The SDK helpers hydrate internally, so call shapes survive (P10); the client cache absorbs the second trip; T23 fixes the helper shape before T25/T26 touch a consumer |
| `GET /entities` shape change reaches e2e alongside the `POST` break | Medium | Both are the same migration in T28, behind the one shared helper it already owns; the route's declared stability is `unstable` |
| Concurrency protocol wrong under the least-tested backend (MySQL) | Medium | `make test-db` on all three at every checkpoint |
| The `POST /entities` 202 break reaches other gears' e2e suites | Medium | Confirmed surface: 6 types-registry e2e files (~95 references to `/entities`) plus `account_management/conftest.py`, whose registration helper is another suite's setup. T28 owns the migration behind one shared polling helper, not open-coded loops |
| Activation write set exceeds the measured 27 in a future deployment | Low | Configured bound 512, refuses rather than partially commits (T14) |

## Parallelization

- **Parallel:** T16 with T14/T15. T22 with T21. T25 and T26 are independent gear groups once
  T24 lands. T30 with T27/T28 — it needs the new models (T26) and the database read path
  (T24), and nothing in the REST or e2e tasks touches the client cache.
- **Sequential:** T2→T5 (foundation), T7→T8, T13→T14→T15, T22→T23→T24.
- **Contract first, then parallel:** T23's trait shape is fixed by SPEC §10.1, so it can
  start as soon as Checkpoint 4 passes.

## Open questions

- **O1** — publish `gts` 0.12.0. Blocks merge, not development (T1 uses the patch).
- **O2** — `principal_id` constant value. Needed by T7; spec recommends a deterministic
  UUIDv5 from `"types-registry.p0.principal"`.
- **O4** — ADR-0015 quarantine preflight as a migration-time assertion. Needed by T18.

**Pre-existing finding, not P0 scope.** `testing/e2e/gears/resource_group/conftest.py::create_type` posts to `/types-registry/v1/types` expecting `201`, but types-registry registers no `/types` route — only `/entities`. The fixture is referenced 36 times in that suite, so either those tests are skipped or already failing. Worth reporting to the resource-group owners; this plan does not take it on, and T28 does not touch it.

Closed in review: no new ADRs for P0 (the no-PDP deviation is SPEC §9 C6, the wire break is
§10.2); registration moves to push in P0 (P4), which retires O3 and ceiling C3; the old SDK
trait is removed in P0 rather than deprecated (P5), which supersedes SPEC D6; the persistent
`ArcSwap` snapshot is replaced by a transient per-unit store with reads from the database
(P6), which rewrites SPEC D2 and §8.2 and retires ceilings C1 and C4; the SDK client cache
DESIGN requires is kept rather than deleted (P7), which adds SPEC §8.3, ceiling C7 and T30; the
P0 REST surface and SDK **are** the platform-plane API for global entities, with the plane
contract-deep rather than transport-deep (P8), which rewrites SPEC §8.4 and adds ceiling C8;
freshness validators and conditional reads are **in** P0 (P9), because the inputs a platform
read needs all exist — which adds SPEC §8.5 and T29, and shrinks C7; discovery becomes a
bounded content-free page with a cursor (P10), which adds SPEC D12 and rewrites §10.2, while
arbitrary `$select` and `expand_type_filter` stay out with corrected reasons.
