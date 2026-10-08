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
artifacts are materialized, and a new SDK trait replaces the old one outright. Types Registry
runs in its own process in production, so the new trait is a toolkit contract served in
process and over a hand-written REST client, and each gear publishes its own GTS declarations
after wiring. Registry-side inventory pull ends once every gear does (P22, superseding P18's
deferral).

Global entities only — no tenant ownership, no PDP, no federation. `PlatformSecurityContext`
is in the contract and every route authenticates its caller; authorization and a platform
listener remain P1 (P22).

**Coordination state, stated once for the whole plan.** `types_registry__coordination_state`
exists — it is created and seeded by this P0's second migration. Only its
`entity_write_order` row is seeded and used in P0: the serialization point every commit
that writes entity state advances as its first statement (P15 below). `source_claim` is
not created in P0. The `routing` state row — the future routing generation — arrives with
federation, seeded by that phase's own migration alongside `source_claim`. A standalone
`routing_config` table is never created, in P0 or any later phase.

`limits.resolved_document` and `limits.resolution_closure` are enforced during admission
and dependent refresh. Both must be positive. Closure accounting is per document over the
candidate overlay; the resolved-size budget applies to the canonical bytes of each effective
artifact. Exceeding either refuses the candidate without committing partial state.

Completed task IDs/evidence remain unchanged. P26 consolidates and renumbers only
unfinished work into **T26–T42**: feature-sized tasks, with Phases 1–6 as the granularity
reference. The queue is Phase 7 (both clients, an isolated handoff, every gear on the
persistent registry), Phase 8 (publication after wiring: Account Management first, then
every gear out of process) and Phase 9 (mixed-version rollout, last). Task references in
historical decisions P1–P25 and completed evidence retain the numbering current when
recorded; P26's mapping, as regrouped by P27 (T24a and T27a added), and the active
graph/index/queue below are the execution authority. No future-task renumbering is implicit.

## Decisions taken during planning

These decisions were made here rather than in the spec, because all of them are consequences
of task ordering or of facts about the runtime that only surface once the work is sliced.
P1–P5 were taken before implementation started; P6–P10 came out of reviewing Phase 1 on its way
in, and the spec has been updated to match all five. P12 is a correction: it reverses a change T9
made to the existing v1 REST contract, and adds T9a and T27. P13 is a reordering — Instances
into Phase 1, and `make dylint` per phase instead of per task. P14 defines `x-gts-ref`
independently of the dependency graph. P15 replaces the revision-vector lock with the optimistic
guard and keeps the one thing that survives the argument: a serialized write path — one row,
claimed first by every commit — that orders commits, joined by every writer of entity state:
admission here, deletion at T20, purge under ADR-0013. P16 came out of reviewing Phase 3:
observability is a per-task obligation from T17 onward. P17, revised after T20, splits REST
completion into T20a (mutations, Phase 5) and T22a (reads, Phase 6), and moves T21 outbox
dispatch into Phase 5. P18 supersedes P4’s P0 scope: inventory push moves to P1, while
explicit-document reconciliation remains in P0. (P11 was a housekeeping close-out and is retired; the number is not reused.)
P19 adds `$select` across the three read routes before T23 fixes the SDK shape and T22d
adds validators; it supersedes P10's arbitrary-projection deferral, not P10's bounded
content-free discovery default.
P20 adds `depth` and `kind` to P0 discovery before T23 fixes `ListEntitiesRequest`; it supersedes
T22a's historical filter limit while keeping tenancy, availability and federation deferred.
P21 moves the validator task into Phase 6 as T22d (formerly T29 (retired)) and T23 into Phase 7, so Checkpoint 6 closes the complete P0 REST
contract and the SDK maps a computed validator rather than declaring an unfilled one.
P22 brings out-of-process operation into P0. It supersedes P18's deferral of per-gear push,
P4's `owning_gear` filtering and P8's stale codegen premise, adds T24 and T25, splits Phase 7,
and renumbers the open tasks.

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

### P2. Types-registry seeds itself through its own outbox

types-registry owns the `toolkit-gts` base types and its own control-plane types (DESIGN:
*"`toolkit-gts` base types default to `types-registry` ownership"*). It cannot register
those through a client — it *is* the registry, same process, same database.

`init()` collects one deterministic seed batch and submits it like any other request, so
acceptance and enqueue share a transaction and no seed operation exists without a driver.
It then awaits that operation and requires every item `succeeded` or `unchanged` before
publishing the client; a `failed` item fails boot. Seeding is therefore deterministic and
complete at publication, which is what makes the §13 no-polling rule satisfiable. At T26,
P18 extends the seed set to all process-linked inventory plus `cfg.entities`. P22 makes that
transitional: once every declaring gear publishes for itself, T29 narrows the seed back to the
registry's own types, the `toolkit-gts` base types and `cfg.entities`.

### P3. The outbox worker starts inside types-registry's `init()`, before seeding

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

**Order inside types-registry's `init()`:** repositories → start the outbox worker →
seed the seed set (P2/P18) → await its items → publish the client. The worker starts
first because acceptance enqueues in its own transaction; publication requires every
seed item `succeeded` or `unchanged`, and a `failed` one fails boot. There is
no snapshot-load step: per P6 seeding builds its own transient store like any other
admission, and reads go to the database.

The resulting rule has no phase caveat: **after types-registry's `init()`, submit and await
work from anywhere.** ~~A consumer registering during its own `init()` must declare
`deps = [types_registry]`.~~ **Superseded by P22:** a remote client is wired only after every
`init`, so no consumer calls the registry from `init()`. Consumers declare `#[consumes]`,
which adds no `init` ordering, and publish from the post-wiring hook (T25).

**Who admits, and when.** Acceptance and admission are separate moments with separate
executors:

| Moment | Accepts | Admits |
|---|---|---|
| types-registry `init()` — its seed set: linked inventory + `cfg.entities` until T29, then its own types, the base types and `cfg.entities` (P18, P22) | types-registry, after its worker starts | the outbox worker; `init()` awaits it before publishing the client (P2) |
| A gear publishes its declarations after wiring | registry code in the caller's task (local client), or the registry's REST handler (remote client) | the outbox worker |
| REST at runtime | types-registry's Axum handler | the outbox worker |

Acceptance is always synchronous, in the caller's task. Admission is performed by exactly
one outbox worker owned by types-registry — one in the system for a single-binary
deployment.

### P4. Registration moves from pull to push — P0 scheduling superseded by P18, then by P22

**Historical decision.** The rationale below describes the original plan. P18 moved this
inventory migration to P1; P22 brings push back into P0 in a different form — per-crate
collectors and explicit crate ownership instead of `owning_gear` filtering.

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
- **`cfg.entities` is outside P4's scope.** It carries operator-controlled identities whose
  GTS identifiers are deployment-specific and cannot be expressed as gear-owned inventory items
  (e.g. the platform-root tenant type). These are seeded into the database at startup through
  the same outbox admission path as types-registry's own inventory (T26). They are not
  reconciled through the SDK — no gear owns them; the deployment operator does.

**This is where registrant-side retry becomes real** — and it lives in the SDK helper. The
earlier answer that no retry was needed was conditional on keeping pull.

It also simplifies the cutover. Seeding no longer has to topologically order ~200 entities
across every gear at startup; types-registry seeds its own small set, and cross-gear
ordering is handled by the retry DESIGN sanctions: *"dependencies converge through retry."*

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
them retains an out-of-process blocker. P0 removes that model blocker; inventory push
from P4 is now deferred to P1 by P18.

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

**What reads need is rows.** Pattern matching is a pure function of the parsed
identifier — it never asks a store a semantic question. Exact reads are keyed lookups. And
D3 already materializes `resolved_schema` / `effective_traits` / `effective_traits_schema`
on the current-state row. So a read is a `SELECT`; for discovery, one `SELECT` whose
pattern joins the admission-time parsed segments (P20, SPEC D14).

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
and DESIGN's list of what is never cached. Batched conditional revalidation against freshness
validators was first deferred here with tenancy, but P9 moves it into P0 once the validator
inputs are shown to be platform-plane computable. Deferred with tenancy, then, are only the
projection / visibility / Context-Tenant key dimensions. Recorded as ceiling **C7** rather
than left implicit.

Two things this changes beyond the decision:

- **The bound becomes bytes.** Today's default is `capacity: 1024` entries while §3.2 caps
  one resolved document at 1 MB, so the configured bound permits ~1 GB. DESIGN makes the
  same argument and picks 64 MB; adopted. This is a live bug in the current defaults, not a
  new requirement.
- **The cache cannot be carried over as-is.** It is typed on `GtsTypeSchema` / `GtsInstance`,
  which P5 deletes, so it is ported onto `EntitySnapshot`.

**Ordering.** T30 lands last, after the cutover rather than with it, and that is deliberate:
the cache is an optimization over a read path that must be correct first, and T26 is already
the largest task in the plan. *(Superseded by P23, variant C′: the cache — now T27 — lands **before** the cutover,
on the new SDK, so consumers moved by T26 are cached from their first read and P0 has no
uncached window.)*

### P8. P0 ships the platform-plane API on the business listener; the plane is contract-deep, not transport-deep

SPEC §8.4 is rewritten and ceiling **C8** is added. No task moves; T9, T20a and T22a carry the criteria.

> **Partly superseded by P22.** The macro no longer rejects `&PlatformSecurityContext`, so the
> "later gRPC move" below does not follow; the SDK goes out of process over a hand-written REST
> client. Per-route caller authentication (T24) replaces "the routes keep the authentication
> they have", which would refuse every remote gear's call.

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
*(Superseded by P22: in embedded hosting `exposed` does not remove a route from the gateway
router at all — `host_runtime.rs:753` — so the "internal-only" premise was wrong; see SPEC C8.)*

**The later gRPC move is expected, not contingent.** A REST contract method taking
`&PlatformSecurityContext` first is rejected at compile time —  *"generated client cannot
source the internal token… serve over gRPC or write a manual client"*
(`toolkit-contract-macros/src/rest_contract_parse.rs:337-347`, UI test
`rest_platform_secctx_rejected.rs`). P0's client is REST-generatable precisely because it
carries no platform identity; adding identity closes REST codegen and leaves gRPC or a manual
client over `attach_internal_token_http`, which today has zero call sites in the repository.
Recorded in §8.4 so the P1 decision is a consequence someone already wrote down.

### P9. Freshness validators are in P0 — the deferral rested on a misread input table

SPEC §8.5 is new, ceiling C7 shrinks, and **T22d** (added as T29 (retired), renamed by P21) is added; T23 and T30 gain criteria.

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
| `external_revision` | — external only | not applicable — Externally Managed Entities are out of P0 |
| normalized projection | ✓ | yes — no `$select` in P0, and DESIGN says absent `$select` *equals an explicit default set*, so it is a constant marker |

The tenant inputs are not missing in P0; they **do not participate** in a platform-plane read.
So a P0 validator is fully computable: a versioned digest over `resource_version`,
`resolution_fingerprint` and a projection marker. DESIGN even fixes the wire form — base64url
of a version byte and a 128-bit managed digest, 23 characters.

The framework is not a blocker either. `OperationBuilder::no_content_response` takes an
arbitrary status, so `304` is declarable, and `file-storage` already returns
`StatusCode::NOT_MODIFIED` with headers by hand
(`gears/file-storage/file-storage/src/api/rest/handlers.rs:212`). There is no ETag helper in
the toolkit — this is manual work with a working precedent, not missing capability.

**What made this urgent rather than merely available.** The validator has to be in the SDK
models from **T23**. Adding it afterwards is a breaking change to a contract that ~50 call
sites across twenty-plus gears will already have moved onto (T28, T29). Deferring validators
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

**Ordering.** T22a (read routes, Phase 6 per P17) → T23 (the field in the models) → **T22d**
(computation, `ETag`, `304`, per-key batch validators) → T30 (cache revalidates against them).
**Superseded by P21:** T22d now precedes T23 — T22a → T22b → **T22d** → T23 → T30 — and T23 maps
the value T22d computes. The constraint above, that the field precedes T28/T29, is unchanged.
T30 was renumbered from T29 (retired) to keep task numbers in dependency order; P21 later renamed the
validator task from T29 (retired) to T22d, and the number was retired then. P17 retires T27 (retired)'s
out-of-order identifier by splitting it into T20a and T22a; the later tasks kept their IDs.
P22 renumbers the open tasks sequentially and so reuses both numbers.

### P10. Discovery is paged and content-free in P0; `$select` was deferred here, expansion stays out

SPEC gains decision **D12** and a rewritten §10.2; §2's row is split. T4, T22a, T23 and T31
gain criteria; no task is added.

`Discovery cursors, $select projections, OData pagination, expand_type_filter` were listed out
of P0 with the reason *"P0 keeps the current flat list"* — a restatement of the decision, not a
reason for it. Examined item by item, the four are not one decision:

**Pagination and cursors are in.** Three facts settled it. SPEC §10.1's own trait already
returns `ListEntitiesResponse`, so deferring the cursor left a page that is a page in name only — the
spec contradicted itself. DESIGN specifies the route as *"`200` with one page and a cursor"*
over *"content-free discovery"*. And the cursor's inputs degenerate exactly as the validator's
did: of the six DESIGN binds into it — query, subject visibility context, Context Tenant,
authorization scope, routing generation, per-source position — P0 keeps
**two**, query and position, because the rest are tenant-plane, PDP, or federation. Position is
free: the read route already required ordering by canonical identifier, so the cursor is a
keyset over a unique immutable column, and `toolkit-odata` (`page.rs`, `pagination.rs`) already encodes
cursors as versioned base64url that refuse an unknown version.

**The current shape is also a live problem, not only a spec gap.** `GET /entities` returns
every match in one array with each item's full `content`; with artifacts materialized (D3)
that is *entity count* × up to 1 MB, and after the pull→push cutover the count is every gear's
declarations. A `limit` alone would not have fixed it — without a cursor the bound makes the
endpoint incomplete rather than large, which is why D12 lands both together.

**At this decision, the default projection was in and arbitrary `$select` was out.** The default field set is what
makes a page content-free, so it is not optional. Caller-chosen sets need optional fields
across the models plus a normalized field-set digest inside the validator, and buy nothing
while there is a single representation to select from. P19 later moves that half into P0;
the document-free discovery default remains.

**`expand_type_filter` is genuinely blocked**, and this is the one item whose original
placement was right for the wrong reason. Its DESIGN definition *is*
`$select=gts_uuid&lifecycle_status=active&availability=available`, with the filters fixed by the method
rather than supplied by the caller. Availability (ADR-0010) needs tenancy and is out of P0, so
a P0 method under that name would report retired contracts as usable. A same-named different
meaning is worse than absence; a caller wanting the traversal pages `list_entities` itself.

**The consequence to plan for, because it lands in consumer code.** `list_instances` and
`list_type_schemas` are helpers over `list_entities`, and their call sites read payloads from
the result. The helpers select those documents on the page (P19) or hydrate through an
optional `batchGet`, complete with respect to the traversal rather than to an instant — the
same trade DESIGN accepts for expansion.

### P12. v1 stays intact; the async surface ships as v2 and is promoted at T27

T9 repointed the two existing v1 routes — `POST /entities` and `GET /entities/{gts_id}` — at the
database path instead of adding new ones. That contradicts the invariant in the risk table below
(*"The DB path has no consumer until T26; no dual-write"*), and it costs more than a red e2e
suite:

* the `POST` body shape changed and gained a required `Idempotency-Key`, so existing callers
  get `400`/`422` — not the status-code break T31 was scoped for;
* `testing/e2e/gears/oagw/helpers.py` and `testing/e2e/gears/account_management/conftest.py`
  register over REST and then resolve through `TypesRegistryClient`, so both gears write to the
  database and read from process memory. That is a functional cross-gear regression, and no e2e
  edit repairs it — only T26 does;
* `GET /entities` (list, memory) and `GET /entities/{entity_key}` (database) gave one resource two
  sources of truth.

So the surface becomes **additive**: v1 is restored verbatim from `main` and keeps serving the
in-memory store, while T9's async surface moves to `/types-registry/v2/` (T9a). The two stores
stay unreconciled — no dual-write, no fallback read — which is P6 enforced rather than merely
intended: with a fallback, an admission that never happened would read as success.

**v2 is interim, and its retirement is planned rather than assumed.** T26 deletes the in-memory
repository, so the v1 routes reading it are deleted in that same task, and **T27** promotes v2
onto the v1 paths. **P17 fixes the order at T26 → T27 → T31:** T20a authors deletion in
Phase 5 and T22a completes reads in Phase 6, so every route exists before cutover and T27
promotes all seven at once. T28 follows T26 and T29 follows T28; both can proceed alongside
route promotion. *(P22: e2e migration, now T31, depends on T29 and T30, so it no longer runs
alongside the consumer migration.)* Nothing else in this decision changes — v1 and v2 stay separate stores until T26, and the promotion is still
where the break reaches a v1 caller.

**What the T26–T29 window owes `TypesRegistryClient`.** T28 and T29
migrate consumers off the *Rust trait*, not off `/v2/`, so T27's rename costs them nothing and
their placement after it is right. The gap is one task earlier: T26 deletes the in-memory
repository the old trait's implementation reads, while ~13 `register(...)` sites and every read
site stay on that trait until T28/T29. Its `register` is synchronous, and after T26 the only store
is the asynchronous one. **The decision, rather than a note that one is needed:** the old trait keeps its shape over the
window and its `register` becomes a **submit-then-poll shim** over the one database store, deleted
by T29 with the trait. It is not a dual path in P6's sense — one store, one write path, no
fallback read — and it is what keeps the workspace building while ~15 gears migrate one at a time.
The alternative, folding T28 and T29 into T26, is rejected: T28 is ten gears and their plugins and
T29 five more, which is not work that shares a task with the cutover, and a task that cannot
compile until all of it lands is not a task. T26 carries the criterion; *"repointing it at the
database would be a compatibility shim with no consumer"* (T27) stays true of the **routes** and
was never true of the trait. *(Superseded by P23, variant C′: the SDK now lands before the
cutover, so T26 moves every consumer onto it in the same change and no shim is written. What made
folding unworkable here — the behavioural startup move of T28/T29 — stays separate; only the
mechanical API move joins the cutover.)*

**What this buys, concretely.** `make e2e-local` stays green from here to T26 with no e2e file
edited, and the red window shrinks from ~19 tasks to the T26–T31 stretch, where the wire break is
real and unavoidable. What it does *not* buy is an earlier cutover: the SDK and every consumer
stay on the in-memory store until T26, which is P6's design and not a gap. The earliest honest
cutover needs Instances (T10), revisions (T11), reference and derivation edges (T13) and
dependency-aware batching (T19) — without them the first `register` from `oagw` or
`account-management` fails, since both push batches of derived schemas and instances.

### P13. Instances move into Phase 1; `make dylint` runs per phase, not per task

Two ordering changes, taken after Checkpoint 1's report.

**T10 moves from Phase 2 into Phase 1.** Instances are not a widening of the path — they are
what the platform pushes today. P4 counts *"roughly eleven plugin gears"* already registering
their well-known Instances from their own `init()`, so Instance support is on the critical path
to T26 and is the longest pole in it. T9's surface also accepts an Instance and then fails it in
the **worker** (`StoreBuildError::UnsupportedKind` → `WorkerError::StoreBuild` → opaque `500`, a
retryable class for a final decision); building the feature closes that hole rather than adding a
refusal for it, so no separate task is needed.

The move drags in three companions, listed in T10's own entry. The one worth naming here is the
**identifier-derived closure**, because it corrects a claim Phase 1 committed to code:
`DependencyRepo::closure` walks the `dependency` table only, so nothing until T13 could reach a
candidate's base — and `admission_worker_test.rs` asserts a derived Type Schema *fails*, blaming
T13's missing edges. That is half right. `GtsId::chain_ids()` and `get_type_id()` are pure
functions of the identifier, so a derivation base — and an Instance's conforming type — need no
edge table at all. Seeding the closure worklist with the chain as well as the edges is what makes
T10 cheap, and it admits derived Type Schemas in Phase 1 as a side effect. T13 supplies the
`$ref` targets needed by the forward closure and the direct edge set needed by T14's reverse
walk. `x-gts-ref` is neither resolved nor represented by an edge.

Phase 1 therefore delivers one global entity **of each kind**, and Phase 2 becomes revisions and
concurrency. T12's *kind* rule moves with T10 — a Type Schema `…ns.thing.v1~` and an Instance
`…ns.thing.v1` derive the same `family_key` — while shape and contiguity stay in T12.

**`make dylint` moves from per-task verification to the phase checkpoint.** It builds the whole
workspace, so per task it is the most expensive check on the list and the one that gets skipped;
per phase it is cheap enough to actually run. The exposure is bounded — phases 2 through 7 are two
to four tasks each.

**The per-task standing bar had to move with it.** `todo.md` required *"`make ci` green"* of every
task, and `make ci` ends in `dylint` — so the old bar cancelled this decision on
the line above it, which is why T1–T9 each recorded `make ci` as *partial*. The bar is now
`make fmt`, `make clippy` and gear tests per task; the full `make ci` — `dylint`, `deny`,
`lychee`, `gts-docs` and four container targets — is a checkpoint gate. Bare `make ci` lines are
gone from individual tasks, and where one carried something specific (`lychee` on the two
documentation tasks) that check stayed and only `ci` went.

**And `make test-db` was never the right command for this gear.** It runs `cf-gears-toolkit-db`'s
own suite and never builds `cf-gears-types-registry` — a task could satisfy that line without
executing one line of the gear, which T2, T4, T5, T7 and T8 each noticed separately. The two
container suites now have a target of their own, `make test-types-registry-db`,
in `make ci` beside `test-users-info-pg` and `test-usage-collector-pg`. `todo.md`'s Commands
section is the single definition; the tasks say *gear tests* and point at it.

The counter-evidence is real, and is why this is a decision rather than a convenience: the first
run happened at Checkpoint 1 with 26 violations standing across T1–T9, in three families
(DE0708, DE1302, DE0301). A phase is a far shorter accumulation window than nine tasks, and
layering violations are cheap to fix in bulk because they are mechanical.

**The per-task records go with the requirement.** T1–T9 each carried a `make dylint` line
recording that it had not run; with no per-task requirement there is nothing for those lines to
record, so they are removed rather than left standing as unmet criteria. Nothing observed is lost:
the workspace-wide run at Checkpoint 1 covers every one of those tasks, and it is where the
findings are recorded. Checkpoint 0's gate is ticked from that same run — Phase 0 is one task and
the run included its changes. Phase 1's run covers T1–T9 only, so Checkpoint 1 carries an explicit
**re-run** item for T9a and T10.

### P14. `x-gts-ref` is not a dependency edge

`x-gts-ref` constrains an instance value to match a GTS identifier pattern. It does not resolve
or inline the entity that the value names and therefore creates no dependency edge.

* **Validation.** `gts-rust` enforces the keyword by matching the value string against the
  pattern — `XGtsRefValidator::validate_value_matches_gts_pattern` parses the value, parses the
  pattern, compares. It never consults the store. So the constraint is satisfiable with nothing
  registered under the pattern.
* **Artifact refresh (T14).** An `x-gts-ref` target is not inlined — DESIGN §3.1 excludes it from
  the resolution closure by name. Revising the target therefore cannot change the constraining
  schema's artifacts. Including it in a reverse walk would spend
  `limits.activation_write_set` on branches whose effective artifact cannot change.
* **Deletion safety (T20).** The platform provides no referential-integrity guarantee for the
  keyword: an `x-gts-ref` naming `topic.v1~` will **not** block deleting `topic.v1~`. A value
  naming a deleted entity stays structurally valid, the registry sees no runtime data that would
  make the refusal meaningful. Instance values are likewise not scanned for identifiers they
  contain.
* **Admission policy.** The managed–external boundary classifies entity-naming patterns directly
  from candidate content when federation is introduced. Major-0 quarantine has no such check:
  changing a named v0 entity cannot change the constraining schema's accepted payloads.

The stored edge kinds are `1 schema_ref, 2 derivation, 3 instance_of`, and
`ck_tr_dependency_kind` admits exactly those values. The numbering is append-only after the
first release.

**Renumbering them in the initial migration is safe here, and this is why.** `main` carries
`1 schema_ref, 2 gts_ref, 3 derivation, 4 instance_of` and the same initial migration, so a
deployment on `main` with a database has applied that migration and will not re-apply the
edited one. Nothing is mis-decoded regardless, because **no supported production path in
`main` can have persisted a `dependency` row**: `DependencyRepo::replace_outgoing` has no
caller there outside tests — `main`'s own T13 entry says so — and this branch is where
admission first calls it. So no row exists whose `kind` could be reinterpreted, and the only
residue on an
already-migrated database is a laxer `CHECK` (`IN (1,2,3,4)` where the edited migration writes
`IN (1,2,3)`), which admits a superset of what the code can now produce. Once a release has
persisted an edge, this argument expires and the numbering is append-only, full stop.


### P15. Locking the revision vector is the wrong tool; serializing commits is right

SPEC §8.1 step 4.2 asked for two lock levels: the version family, then *"candidate and
revision-vector entity/current rows in canonical identifier order"*. T15 implements the first and
not the second — and the second is not a shortfall to be made up later. It is the wrong mechanism,
and DESIGN §4 has been corrected rather than deviated from.

**A lock cannot do the guard's job.** It guarantees only that nothing moves *after* it is taken,
and the movement that matters happens between evaluation and the lock — the phantom dependent
appears before any lock could be held. So the vector comparison is required whether or not rows
are locked, and the lock is purely additive: what it buys is that a contended admission waits
instead of rolling back. Liveness, not correctness.

**And it is expensive in exactly the place this design pays attention to.** One round trip per
vector member, inside the commit transaction, on a set `activation_write_set` allows to reach 512
— the cost T14 restructured the reverse read into a single CTE to avoid. It is also the only
reason step 4.2's canonical ordering needs to extend past families: order matters because the
locks are many. Remove them and the requirement disappears with them. Optimism is the shape of
the rest of this design anyway — `resource_version` compare-and-swap, a transient store per unit,
validation outside any transaction — and registrations are rare against reads, which is the
regime optimistic detection is for. That the secure API has no `FOR UPDATE` and `SQLite` has no
row locking is corroboration, not the argument: the argument stands if `FOR UPDATE` arrives
tomorrow.

**What holds instead.** The candidate's own row is serialized by the compare-and-swap that writes
it. A dependency that moves is serialized by the refresh its mover owes the dependants: that
refresh writes each affected dependant's `type_schema` row, the same row this commit writes, so
the two block on one another — which orders them and nothing more, since a refresh computes its
artifacts before it writes. What makes the loser notice is that the refresh's write is a
compare-and-swap on the revision and fingerprint it read; it rolls back and recomputes. Where the
change leaves a dependant's fingerprint unmoved, nothing is written and nothing was stale. SPEC §8.1
step 4.2 records the argument, the one window it leaves, and the liveness cost by name.

**One lock survives the argument, and it orders commits.** The argument covers everything that
meets on a row. What it cannot cover is an edge committed *after* a mover's reverse scan: adding an
edge moves no `resource_version` and writes only `dependency`, so the two commits write no row in
common, both pass their own guards, and the dependant keeps an artifact inlined from a revision
that is no longer current with a fingerprint that matches it. The requirement is therefore a **serialized write
path**: every commit claims the `entity_write_order` row of `types_registry__coordination_state`
as its transaction's first
statement, one commit at a time per installation. It works because the reverse-impact scan and
the vector guard already run inside the commit transaction: either the edge is visible to the
mover's scan, or the unit writing it has not committed and its own guard catches the mover. Two
cases, no third. A row rather than an advisory lock, because advisory keys live on a session
separate from the transaction's connection and losing it would release the key while the
transaction carried on. This is nothing like a lock over the vector, which stays the optimistic
guard for the window between evaluation and the claim. Every writer of entity state claims it:
admission here, deletion at **T20**, the purge job under ADR-0013. DESIGN §3.7 states it.

**The `unchanged` outcome is not guarded, deliberately.** Step 4.3 sits ahead of every write, and
an `unchanged` candidate performs none: no revision, no version move, no refresh. The one thing it
decides — that the authored content already equals the current revision — is decided from rows
read inside its own transaction, so no part of it rests on the evaluation's view. Guarding it
would take a genuine no-op re-submission, make it revalidate because a *neighbour* moved, and
after `worker.max_revalidation_attempts` such moves turn it into a failure. So the guard runs
after that branch, and `an_unchanged_resubmission_is_not_refused_by_a_moved_dependency` is what
would catch the mistake.

**One consequence worth naming:** `limits.activation_write_set` is now asked twice, because the
vector's reverse-impact read is the same read the refresh does. An over-bound candidate is
therefore refused at evaluation, before any transaction has written, under the same
`activation_write_set_exceeded` reason — strictly earlier and cheaper, and invisible to a client.
T14's refusal stays as the backstop for a set that grew in between. Both ask the same question,
because D5 states the bound over the set the walk *returns* rather than over the rows the
fingerprint filter ends up writing — the written set is a subset, and only the walked set is a
number either read has before it writes.

### P16. Observability is a per-task obligation from T17 on, not a second T16

SPEC **§8.6 is new** and records the contract this decision enforces; success criterion **16** is
added, so P0 does not finish with an undiagnosable decision on the write path.

T16 instrumented the admission path *as it stood at the end of Phase 3*. Every decision Phase 4
and Phase 5 add — a compatibility verdict, a forced waiver, a quarantine refusal, a deletion, a
dry run — is one T16's instruments either cannot see or cannot separate from something else.
Checked in the code rather than assumed:

* `AdmissionMetrics` (`domain/ports/metrics.rs`) has five methods and **no `kind` and no
  `dry_run` parameter**. So a deletion's success and a registration's success are one series, and
  a dry run that wrote nothing would increment `candidates_total{status="succeeded"}` beside the
  commits that did. Both spans already carry `kind` and `dry_run`, so the gap is in the metrics
  only — which is why this decision is about labels and not about spans.
* Acceptance-stage refusals are enumerable because `AcceptanceError::reason()` is an exhaustive
  match, and T16's claim that *"a refusal a later task adds cannot compile until it has a
  reason"* is true **of acceptance**. Admission-stage reasons are `ItemFailure::new("literal",
  …)` at ten-odd call sites, and nothing makes a new one appear in any vocabulary. `Unknown`,
  `blocked_by_*`, the quarantine refusals and deletion's dependent check would each be countable
  only if someone remembered.
* `Unknown` is the one verdict SPEC §16.12 requires to be distinguishable, and the only one a
  deployment has reason to alert on. Counted as one `reason` among a dozen it loses exactly what
  makes it special: it is a fail-closed refusal, not a candidate decided against.

**So each task instruments what it adds, in its own commit**, and there is no follow-up
observability task to defer. The rule, stated once here and carried as criteria in T17, T18, T19
and T20:

1. **Every new terminal outcome and every new refusal is countable under a closed vocabulary**,
   with no identifier ever a label. `refusals_total{stage,reason}` carries the refusals; a new
   instrument appears only where a label on an existing one would misreport — which is the case
   for the compatibility verdict, because `compatible` is not a refusal and has nowhere else to
   go.
2. **A series that blends writes with non-writes is wrong.** `dry_run` becomes a label wherever
   a series would otherwise mix a dry run with a commit, and `kind` wherever it would
   mix a deletion with a registration. T20 does that sweep in one commit, across every instrument
   that exists by then, because it is the task that makes both distinctions real.
3. **The admission reason vocabulary has one home, and it is compile-enforced.**
   `ItemFailure::new` takes `AdmissionFailureReason`, defined in `domain::admission::reasons`.
   Each task adds its refusal variants there. Stored and API codes remain strings;
   `ItemFailure::from_payload` restores known variants and preserves unfamiliar codes as
   `Unknown(String)`. Known reasons keep their metric labels after reading from storage;
   unknown codes map to the single `other` label.
4. **The evidence bar is T16's**, because that is what makes a dashboard contract real: rendered
   names, label keys and label *values* asserted against an `InMemoryMetricExporter`; the
   emission asserted end to end through the real `accept` / `run_operation`; and a mutation check
   that stripping the emission fails the tests.

**One correction to T16's record, while it is being extended.** `todo.md`'s T16 entry and its
commit message both argue for a process-global instrument set reached like `tracing`. The code
that shipped does not do that: the instruments are behind
`domain::ports::metrics::AdmissionMetrics`, the OpenTelemetry adapter is in `infra::metrics`, the
handle is injected from `init()` and carried down the call graph as an `Arc`, and the name prefix
is configurable. The port is the better shape — `de0301_no_infra_in_domain` cannot see an
infrastructure type that hides at the crate root — and it is the shape to extend, so the record
is corrected rather than the code. Only `observability.rs`'s two span constructors are free
functions, and its module header states why.

### P17. Complete mutations and dispatch in Phase 5; reads and the SDK contract in Phase 6

T27 (retired) is split into T20a (mutations) and T22a (reads); T21 moves into Phase 5.
The later tasks keep their IDs (until P22 renumbers the open ones).

- **Phase 5: T19 → T20 → T20a → T21 → Checkpoint 5.** T20a exposes single/batch
  deletion with dry run on all mutations (body for registration/batch deletion, query for
  single deletion). T21 adds outbox submission for database-backed mutations; T26 later
  moves startup seeding onto the same path (P3).
- **Phase 6: T22a → T23 → Checkpoint 6.** *(Historical: P19/P20 insert T22b/T22c, and P21
  replaces T23 with T22d and moves T23 into Phase 7 — see P21 for the current order.)* (T22 deferred by P18.) T22a adds `:batchGet` and bounded,
  content-free discovery with cursors and `$select` refusal. REST and SDK follow SPEC
  §10.1/§10.2 (`items`, `key`, `ListEntitiesResponse`).

T20a predates T21. T21 depends on T20; scheduling it after T20a enables
REST-to-outbox tests before Checkpoint 5. T22a needs database reads, v2 routes and T20a's
mutation docs for the seven-route completeness check. T23 needs T4 reads and T21 dispatch
for explicit-document reconciliation, and follows T22a by execution order. T22 is no longer
a P0 dependency (P18). T22d needs T22a and T23 — P21 drops the T23 dependency and moves T22d
into Phase 6 ahead of it.

Checkpoint 5 proves submit → poll → terminal outcome through the router and outbox for
all mutations in both modes, without direct worker calls. Dry runs persist outcomes but
change no entity state, revisions or versions. Checkpoint 6 verifies reads and completes
all seven routes.

T20a documents mutations; T22a completes OpenAPI and quickstart reads. Both use
`routes::V2` and internal-only mutations (C8), with router tests and manual `curl`.
P12 keeps e2e files unchanged and `make e2e-local` green until T26.

Cutover remains **T26 → T27 → T31**, alongside T28 → T29. T27 promotes all seven
routes and owns both v1-breaking changelog entries: one for the write protocol, one for
pagination and document-free defaults on read routes. T31 migrates the Python suites.

### P18. Defer per-gear inventory push to P1; retain explicit-document reconciliation

> **Deferral superseded by P22 (2026-09-30).** Per-gear publication is back in P0, through
> per-crate collectors rather than T22's `owning_gear` filter. T22 stays a transfer note that now
> points at T25. `owning_gear` on the wire, attribution correction and exposure on reads remain
> P1, as below.

**Accepted scope revision (2026-09-15).** Supersedes P4's P0 scheduling and rewrites SPEC
D11. T22 moves to [#4827](https://github.com/constructorfabric/gears-rust/issues/4827)
under [P1 #4628](https://github.com/constructorfabric/gears-rust/issues/4628)
alongside platform-plane authentication and client integration. `owning_gear` is attribution
and a local inventory selector, never authentication or authorization. The reason to group
this work is to verify the complete cross-process startup path together; metadata itself
has no authN dependency.

**Task boundaries and numbering.** Keep all existing IDs so issue links and recorded evidence
remain valid. P19 adds one active P0 task after this decision; T22 remains a transfer note.
Phase 6 is now T22a → T22b → T22c → T22d (P19/P20/P21), and T23 opens Phase 7. T23 keeps the new trait/models and a helper accepting explicit desired documents:
batch-read → compare → submit changes → poll, with bounded dependency retry. It neither
collects inventory nor deletes records omitted from the desired set. T28/T29 migrate existing
registration and read callers; they add no inventory registration call merely because a gear
has GTS declarations. T26 still deletes ready mode and the in-memory repository.

**P0 bootstrap.** T26 collects all linked Type Schema and Instance inventory, including other
gears, plus operator `cfg.entities`, starts the outbox worker, submits that combined set through
it and awaits every item before publishing the client (P3's order; the earlier "inline, before
starting the outbox" wording is corrected by P22, which also narrows the set at the end of T29). Keep one bounded seed batch: the combined set must fit
`limits.batch_candidates` and all other admission limits. Fail startup explicitly if it does
not; do not truncate or silently split dependency-related candidates. Admission already orders
the candidate graph. Verify real deployment inventories, cross-crate dependencies, the
combined-set limit, and unchanged repeat startup. This costs startup work proportional to
linked declarations, not a whole-table warm-up, and preserves C1/C4's closure.

**C3 remains open.** P0 persists `owning_gear = "types-registry"` as a documented compatibility
placeholder for admissions; it does not claim to identify their declaring gear. The field
and global NOT NULL constraint stay. Automatic inventory registration from another process
is unsupported until P1. This limitation does not widen C8's internal-only mutation surface.

**P1 acceptance boundary.** Add inventory metadata/filtering (former T22); compose it with the
platform client/security context and P0's reconciliation helper; migrate declaring gears to
push their own inventory, with dependency retry and readiness tests in both process layouts.
Reduce registry bootstrap to its own/base declarations plus `cfg.entities`. Correct existing
P0 attribution through the supported revision/provenance path even when authored content is
unchanged; a content-only `UpToDate` shortcut must not retain the placeholder. Preserve
operator/bootstrap attribution for `cfg.entities` and never infer owners from GTS namespaces.
Expose `owning_gear` on reads together with the ownership view; P0
persists it for this upgrade but returns it on no read and defines no ownership group, and
`provenance` stays `gts_spec_version`, `gts_impl_version` and `compat_forced`. Only then
close C3. Metadata acceptance and verification are tracked in #4827; integration
and migration remain epic obligations in #4628 for the P1 task breakdown.

### P19. Add field projection before the SDK and validator contracts

**Scope revision (2026-09-23).** T22b follows the completed T22a and precedes T23. It
implements `$select` on exact read, `:batchGet` and discovery. An absent selection on all
three returns a document-free P0 metadata set, following DESIGN §3.3's default; selected
documents are flat and individually addressable. P0 exposes only the managed `origin`
variant, and has no tenant availability or external origin to invent; the allowlist
contains only fields the P0 read path can actually answer.
The current 100-key batch ceiling and discovery page limits remain until a separately
specified response-byte budget justifies changing them.

This supersedes P10's deferral of arbitrary `$select`, SPEC §2's corresponding out-of-scope
row, and the fixed-projection part of ceiling C7. It also supersedes T22a's *end-state*
statements that exact/batch reads always return full documents and `$select` is refused;
T22a's completed implementation record remains intact. SPEC now fixes the P0 field
allowlist, default, DTO/SDK contract, cursor binding and validator input before coding.

**Order and boundaries.** Normalize one field set for all three reads. Exact/batch share
one projected lookup; discovery keeps one bounded keyset page and binds the normalized
selection into its cursor. Document-free reads avoid fetching and parsing documents in
storage; applying `toolkit::api::select::apply_select` after loading full documents would
change only response bytes. The result envelope, `kind` and tombstone lifecycle remain
mandatory outside selection. T23's reconciliation and hydration helpers explicitly request the
documents they consume; T22d digests the normalized selection rather than a fixed marker;
T30 keys cached representations by that same selection. T22b does not add tenant fields,
federation, or `expand_type_filter`.

**Implementation slices.** First land the SPEC/field-set contract and pure tests; next
project exact and batch reads with bounded, snapshot-consistent storage tests; last project
discovery, bind its cursor and verify OpenAPI/quickstart/router behavior. Each slice leaves
the gear building and passing its focused tests. Checkpoint 6 reviews the combined contract
before T26 begins the consumer cutover.

### P20. Add chain-depth and kind filters to P0 discovery

**Scope revision (2026-09-23).** T22c follows T22b and precedes T23. It adds only the
DESIGN §3.3 `GET /entities` filters `depth` and `kind` to the P0 read surface. `depth`
is an inclusive maximum length of parsed GTS identifier segments (`GtsId::segments()`;
one segment has depth 1), so `pattern` plus `depth` can bound a derivation or version
family without treating a greedy GTS wildcard as an exact chain level. `kind` is the
existing `type_schema`/`instance` enum. Both work without `pattern` and intersect with
it when supplied; discovery stays active-only by default. P0 still omits `origin`,
`availability`, `scope`, `tenant_id`, legacy segment filters and generic `$filter`.

**Boundary and order.** `gts-id` parses identifiers and patterns; admission stores
`chain_depth` and the parsed segments, and the repository compiles the parsed pattern into
exact per-segment joins (SPEC D14). Every filter, including `kind` and `lifecycle_status`,
is SQL before `LIMIT limit + 1` and `$select`. Extend the versioned
cursor with canonical optional `depth` and `kind`, rejecting continuation under a
changed filter; an absent field is distinct from an explicit value. This requires T22b's
cursor contract first and fixes `ListEntitiesRequest` before T23 publishes the SDK. T22d's
per-entity validator does not gain filter inputs: a validator describes one selected
entity, while a discovery page has none.

**Implementation slices.** First add `kind` through the query, repository, REST and
router tests. Then add `depth`, cursor binding and mixed-filter traversal tests on all
three backends. Last, migration 000005 materializes `chain_depth` and
`entity_gts_segment` (no backfill; it refuses a non-empty `entity`), the pattern compiles
to SQL, and a differential corpus pins it to `GtsId::matches_pattern` per backend. A
page with a cursor is full; no page is empty unless nothing matches. Checkpoint 6 reviews the
combined filter and projection contract; T27 promotes it with the other v2 routes.

**Amendment (2026-09-23).** Discovery adds `lifecycle_status=active|deleted|all`
(default `active`), an SQL predicate applied before the page limit and bound by the
cursor (absent equals `active`). All three reads always return `gts_id` and `gts_uuid`
beside `kind` and `lifecycle_status`, all required in OpenAPI and part of every normalized
selection. Exact
reads and `batchGet` are unchanged; SDK expansion requests `active` explicitly.

### P21. Conditional reads close Phase 6; the SDK contract opens Phase 7

**Scope revision (2026-09-27).** T22d moves from Phase 7 into Phase 6, directly after T22c;
it was T29 (retired) and is renamed so its ID matches its position.
T23 moves from the end of Phase 6 to the head of Phase 7, before T26. Phase 6 is now
T22a → T22b → T22c → T22d; Phase 7 is T23 → T26 → T27 → T31, with T28 → T29 alongside;
T30 followed T29 and could run alongside the e2e task (P22 later makes the e2e task, now T31, depend on T30). No task is added or split. T22d is the one rename P18's keep-every-ID rule allows:
T29 (retired) had no recorded evidence yet, and the epic and phase issues were updated with it. T29 (retired) is
retired rather than reused.

**Why.** Checkpoint 6 then closes the complete P0 REST contract on `/v2/` — all seven routes,
projection, discovery filters and conditional reads — rather than a contract whose validators
arrive after the cutover. The server work left after it is the cutover (T26) and the path
promotion (T27). Neither can join Phase 6: T26 opens the e2e red window P12 confines to
T26–T31, and Checkpoint 6 requires `make e2e-local` green with no e2e file edited.

**What T22d needed from T23, and why it no longer does.** Only the SDK model field. The server
half — the per-request digest in the domain service, `ETag` / `If-None-Match` → `304` on exact
reads, per-key `if_none_match` / `etag` / `unchanged` on `batchGet` — touches no SDK type, and
T22a already accepts and length-checks each item's `if_none_match` pending T22d. So T22d leaves
`types-registry-sdk` untouched, and T23 maps the validator T22d computes instead of declaring a
field no read fills yet.

**P9's constraint holds unchanged.** It requires the validator in the SDK models before T28/T29
move ~50 call sites onto them. T23 still precedes T26, T28 and T29, so the field still lands
first — now populated, which lets T23's mock-consumer tests exercise `unchanged` end to end.

**Cost.** Phase 7 gains T23 (M) and was already the largest phase. The REST validator shape is
fixed before the SDK exists, so a disagreement with SPEC §10.1 surfaces at T23 while the routes
are still behind `/v2/` with no consumer — the exposure T20a/T22a already accepted (P17).
T27 promotes conditional reads with the other routes; they are additive and need no changelog
break entry.

### P22. Out-of-process SDK and per-gear publication

**Accepted 2026-09-30.** Production uses a standalone registry (Profile 3).
P22 supersedes P18's push deferral, P4's `owning_gear` filter and P8's stale
codegen premise. SPEC D15–D17 and §8.4 define the contract.

- `PlatformTypesRegistryApi` is a toolkit contract with platform context on every
  method, serde-free models and separate wire DTOs. Helpers stay outside contract
  IR; default bodies would denote optional methods.
- The REST client is hand-written: codegen loses `Idempotency-Key`, `Location`,
  `Retry-After`, replay status and `ETag`/`304`. It uses runtime helpers,
  `DirectoryResolvingClient` and live-route contract tests. Moving idempotency into
  the body would hide it from POST retries and permanently change v1.
- Each declaring crate exposes plain-data `gts_declarations()` through
  `declare_gts_inventory!()`. The owning gear lists crates and publisher in
  `gts(crates = […], publisher = …)`; the macro records ownership and publishes
  after wiring. Linking a crate publishes nothing; independent collectors avoid
  version splits. Toolkit defines the publisher interface without naming the SDK.
- A collector hosted only in the SDK would invert gear → SDK dependencies.
  A registrar gear adds no benefit; manual type lists risk omissions; build-time
  bundles couple registry and gear releases.
- Publication runs in the background and contributes readiness. The cache becomes
  an SDK decorator over any local/REST implementation.

**Auth proposal, superseded by P24/P25.** P22 proposed either-plane listener auth
and bearer-only gateway auth. P24 enforced platform auth on every host; P25 removed
this either-plane axis before use. SPEC D17 now requires one plane per route.

**Migration.** Dual per-crate/global collection preserves coverage until every gear
publishes; identical submissions yield `unchanged`. Only then can the global pull
end and the seed set narrow to registry types, base types and configured entities.
P23 assigns the final task sequence below.

**Original phase split.** Phase 7 added SDK/REST/toolkit foundations and a review
checkpoint before consumers moved; Phase 8 performed cutover, publication, cache
and e2e migration. Database consumers stayed on the legacy path until cutover.
Open tasks were renumbered; completed task IDs remain evidence references:

| Before P22 | After P22 |
|---|---|
| T23 SDK trait and reconciliation | T23, contract-shaped |
| — | T24 REST path for gears in other processes (new) |
| — | T25 toolkit collectors, post-wiring, readiness (new) |
| T24 cutover | T26 |
| T24a (retired) retire v1, promote v2 | T27 |
| T25 system gears | T28 |
| T26 domain gears, delete the old trait | T29, plus the end of the pull |
| T30 client cache | T30, as an SDK decorator |
| T28 e2e | T31, plus the out-of-process run |

P22 reuses retired T27/T29 numbers; historical references mark them “(retired)”.
T22 remains a transfer note. Attribution on the wire, principal recording,
authorization and a separate platform listener remained deferred here; P23 revises
publisher metadata and release ordering.

### P23. Publication ordering, read-back operations and startup

**Accepted 2026-10-01; naming/requirement revised 2026-10-02.** Review found four
gaps in P22. SPEC D18–D22, C11–C13 and the amended D11/D17 record the decisions.

1. **Release ordering (D18).** CAS cannot stop an old release or queued operation
   from restoring older content. A per-entity publisher stamp is checked under
   `entity_write_order` before preconditions and any final refusal, including
   compatibility refusal. Lower versions are superseded; equal/higher versions
   use ordinary admission; higher identical content confirms metadata only.
2. **Read-back operations (D19).** Receipts lack items, including terminal replay.
   Both adapters submit then read `get_operation`; read failures name the accepted
   operation for same-key replay.
3. **Startup (D21).** Audit every startup phase, not just `init` (T28). Bootstrap
   and plugin dependencies become supervised work; publication gates `Required`
   readiness. T34 prevents caching while a locally registered same-contract/vendor
   plugin remains invisible. oagw root-tenant resolution and account-management
   bootstrap are known consumers of the old barrier.
4. **Earlier OoP validation.** T35 introduces a two-process fixture/cold-start pilot
   before consumer migration; its registry binary cannot pull fixture inventory.
   T49 repeats it with mixed versions and the guard enabled.

**Ordering tradeoffs.** Per-entity stamps let partial admission converge; an
owner-wide watermark would let A@2 block still-pending B@2 over B@1. An older
release may still create an identifier a newer one never published (C12). Equal
versions may change configuration-built Instances without rebuilding, at the cost
of overwrites between differently configured replicas of one release (C11).
No version override: rollback/hotfix content must ship above the last published
version. Overrides risk permanent accidental supersession.

Rejected: per-gear migration journals (not atomic with registry writes), mandatory
external upgrade runners (ordinary replicas can publish safely), and reader-floor
epochs (cannot stop already-running readers). Per-crate collectors remain behind
`declare_gts_inventory!()` / `gts_declarations()`; entries must be crate-local
newtypes, leaving `linkme` a possible local implementation change.

**Readiness.** Superseded-live is ready with warning/metric, matching an older pod
that never restarted; superseded-deleted holds readiness. Older-reader support is
the N−1 release obligation (D22). `ReportOnly`, runtime overrides, optional
consumption and stricter supersession (O6) remain P1.

**Historical sequencing (variant C′), superseded by P26.** The original queue was:
- **Phase 7, T23–T31:** SDK, REST client and cache precede T29's database cutover
  and mechanical migration of every consumer. Delete the old trait without a
  shim or uncached window. Local registration stays synchronous in `init`; T28
  audits the change from staged to immediate validation before cutover. T31
  narrows the e2e red window to T29–T31.
- **Phase 8, T32–T39:** per-gear publication leaves `init` in T28's dependency order;
  supervised prerequisites gate readiness. T38 ends the pull and moves dependent
  `cfg.entities` after wiring, avoiding an empty-database bootstrap cycle.
- **Phase 9, T40–T49:** rename, metadata, required wire field and guard land together.
  C11 applies from independent publication until the guard is complete; P0 ships
  only after Checkpoint 9.

T29 is large (~30 consumer crates) but mechanical, split into gear-group commits
and gated by compilation/tests. The publisher signature comes first; the former
combined toolkit task splits into T23, T25, T32 and T33. Retired T43–T45 work moved
to T33, T36/T44 and T37/T38.

**Renumbering.** Completed evidence keeps its IDs; open tasks are sequential again:

| Before the renumbering | Now | | Before the renumbering | Now |
|---|---|---|---|---|
| T25a | T23 | | T25b | T32 |
| T23 | T24 | | T25c | T33 |
| T25d | T25 | | T33b | T34 |
| T24 | T26 | | T32 | T35 |
| T30 | T27 | | T34a | T36 |
| T33a | T28 | | T28 | T37 |
| T26 | T29 | | T29 | T38 |
| T27 | T30 | | T31a | T39 |
| T31 | T31 | | T35–T42 | T40–T47 |
| T46 | T49 | | T34b, T47 | retired on 2026-10-02 |
| | | | T43, T44, T45 | retired, no successor ID |

**2026-10-02 revision.** `owning_gear` becomes `publisher_name`: the registry has
no gear semantics, and “owner” already denotes tenant ownership. Every global
platform mutation requires request-level `publisher { name, version }`, present
in SDK models from T24 and enforced in T48. The bodyless single-key DELETE leaves
P0. No unversioned branch, activation switch, writer inventory or adoption manifest
remains; first successful publication claims pre-migration rows. Registry seeds
and configured entities use `types-registry` name/version. Columns remain nullable
for history; the API enforces the requirement.

P1 binds names through a workload-identity allow-list (several gears may share one
identity), with SPIFFE version attestation. Phase 9's former adoption/activation
T40/T50 are retired; T41–T48 become T40–T47 and new T48 requires publisher.
O5 (bootstrap publishers) remains assigned to T36; O6 is P1.

### P24. Platform-only is enforced on every host, the gateway included

**Decision.** T25's `.platform_only()` was first built as P22/P23 specified: enforced on the
gear's own listener only, and treated by api-gateway as a plain required bearer. That made the
route's name promise more than Profile 1 delivered — any tenant bearer could mutate through the
gateway (C8) until T48 refused it in the handler. T25 was reworked so the variant binds every
host:

- api-gateway resolves a platform-only route to its own requirement: a presented bearer is
  validated but not required, and a gate shared with `oop_serve` (`caller_plane_middleware`)
  requires a validated internal token from the gateway's inbound authenticator. With none
  configured the route is refused. `auth_disabled` synthesizes no tenant context on it, and
  tenant scope rules do not apply to it.
- Its OpenAPI operation declares an `internalToken` apiKey scheme, and gateway discovery never
  publishes an operation that cannot be satisfied without it — the edge strips the token, so a
  proxied platform-only route could only degrade to bearer-only.
- The either-plane axis was unchanged here, and P25 then removed it: a route serves one
  plane, and `.platform_only()` became `.platform_authenticated()`.

**Consequences.** D20 and C6/C8 now read "a tenant bearer cannot mutate on any host". Profile 1
e2e mutates through the gateway with a platform token, so `config/e2e-local.yaml` configures
the gateway's `internal_auth: shared_secret` and the tests send `X-ToolKit-Internal-Token` —
that lands with T26, which moves the registry's mutation routes onto the variant. The
proxy still routes by path, not method: a platform-only method sharing a path with a published
read is forwarded bearer-only and refused by the registry's listener; method-aware proxy routing
is a separate follow-up.

### P25. One plane per route: a platform route set and a tenant read set

**Decision.** types-registry offers two APIs, one per plane, and each REST route serves exactly
one of them. T25's either-plane axis (`.authenticated_or_platform()`, D17 as first written) is
removed before any route used it.

- **Platform API** — `PlatformTypesRegistryApi`, `PlatformSecurityContext`, the full operation
  set. Its routes live under `/types-registry/platform/v1/...` and use
  `.platform_authenticated()` (T25's `.platform_only()`, renamed): a validated
  `X-ToolKit-Internal-Token` is required on every host, a bearer alone is `401`, the OpenAPI
  operation declares `internalToken`, and gateway discovery never publishes it. The hand-written
  REST client (T26) is its only HTTP consumer besides operators and e2e.
- **Tenant API** — `TypesRegistryApi`, `SecurityContext`, reads only. Other gears call it while
  serving a tenant's HTTP request; out of process its client forwards the caller's bearer, which
  every hop re-validates (ADR-0008). Its routes are `.authenticated()` under
  `/types-registry/v1/...` and can be exposed through the gateway like any tenant route. P0
  authenticates only; authorization arrives with the PDP (C6).

**Why.** ADR-0008 gives a request exactly one plane, and toolkit's contract codegen already
picks the plane per method from the context type. A route that admitted either plane needed a
second extractor (`ValidatedCaller`), a gateway special case and a handler that did not know
whose call it served. Two route sets cost duplicated read routes and a second SDK trait, and buy
an unambiguous plane per handler, a registry listener whose platform half can later move to the
separate platform listener ADR-0008 asks for (C8) without a path change, and tenant reads that
work through the Profile 3 edge, which strips the internal token.

**What T25 changed.** `.platform_authenticated()` replaces `.platform_only()`;
`OperationSpec::auth_plane: AuthPlane { Tenant, Platform }` replaces `platform_plane`;
`RouteAuth::Platform` and `platform_route_middleware` replace the either-plane variants, the
gate and `ValidatedCaller`, and handlers read `Extension<PlatformSecurityContext>` or
`Extension<SecurityContext>` directly. `#[toolkit::rest_contract]` now registers every
`PlatformSecurityContext` method with `.platform_authenticated()` instead of `.anonymous()`,
and refuses `#[anonymous]` on one; `authz-resolver`'s `evaluate` is the only such route today
and moves from an unauthenticated route to a platform one.

**Paths through the cutover.** The legacy v1 routes hold `GET /types-registry/v1/entities` and
`GET /types-registry/v1/entities/{gts_id}` until T30, so the tenant reads cannot take `/v1/`
before then:

| Step | Platform routes | Tenant read routes | Legacy v1 |
|---|---|---|---|
| T26 | all seven move from `/v2/` to `/types-registry/platform/v1/` | added on `/types-registry/v2/` | unchanged |
| T30 | unchanged; the REST client already targets them | move from `/v2/` to `/v1/` | deleted |

T26's REST client targets `/types-registry/platform/v1/` from the start, so T30 no longer moves
it. The tenant reads are the three entity reads — exact, `batchGet` and discovery;
`get_operation` stays platform-only, because a tenant cannot submit.

**Consequences.**
- D17 now reads "one plane per route"; D20's mutations exist only on platform routes; C8's
  missing piece is the separate listener, no longer a route marker.
- e2e: mutations and operation polling move to the platform paths with `platform_headers`;
  reads stay on a bearer, on `/v2/` until T30 (T26, T31).
- `oop_serve` installs the tenant plane only when a bearer authenticator is configured, so a
  tenant route on a registry without one must still answer a canonical `401` — T26 adds that
  gate rather than relying on the handler's extractor.

### P26. Both clients early, Account Management as the first real gear, a consolidated queue

**Accepted 2026-10-07; consolidated the same day.** The first P26 draft split the remaining
work into 39 micro-tasks (T26–T64) across Phases 7A, 7B, 8 and 9 with five internal
checkpoints. It was too granular. This revision keeps its decisions and every acceptance
criterion but regroups them into **17 feature-sized tasks, T26–T42, in three phases**,
with Phases 1–6 as the granularity reference: a task is one verifiable feature outcome, and
its technical steps are the commits it lists, not separate tasks. P26 supersedes P23's queue,
not its correctness, authentication, cache, cutover or publisher-ordering guarantees.

**Required P0 outcome.** Every gear uses the persistent registry; types-registry runs out of
process; both the platform and the tenant API have usable local and REST clients early;
Account Management is the first real gear proved on the new path — publication, root
bootstrap and tenant create/read through the new client — with its IdP, Resource Group and
authentication prerequisites; mixed-version rollout support stays in P0 and comes last.

**Decision — the queue.**

- **Phase 7 (T26–T32).** T26 platform API over REST; T27 tenant API and resolving clients
  (closing the REST contract); T28 the platform cache; T29 the generic post-wiring lifecycle;
  T30 the isolated pilot and cold-start handoff → **Checkpoint 7A**. Then T31 the startup audit
  and atomic cutover, T32 one REST version and migrated e2e → **Checkpoint 7**.
- **Phase 8 (T33–T38).** T33 collectors and the `gts(…)` attribute; T34 publication ownership,
  dependent configured entities after wiring and late-safe plugin selection; T35 Account
  Management and its IdP publish and bootstrap after wiring; T36 AM's host prerequisites and
  the real-gear proof, local and remote → **Checkpoint 8A**. Then T37 every remaining gear and
  the end of the pull, T38 the out-of-process run, HA and chart → **Checkpoint 8**.
- **Phase 9 (T39–T42).** T39 publisher state, T40 commit-time ordering, T41 every writer sends
  and `publisher` is required, T42 the mixed-version proof → **Checkpoint 9**.

**Why Account Management comes after the cutover, not before it.** AM's prerequisites —
resource-group, authz-resolver, tenant-resolver, its IdP plugin — read the registry in the same
host. Proving AM on the new client before T31 would mean either moving those gears one by one,
with old and new traits serving one host (the shim P23 rejected), or AM writing the database
while its peers read the in-memory catalogue. Neither preserves one data source. So T31 moves
every gear, AM included, in one atomic step — AM's first use of the new client, exercised by
its e2e suite at T32 — and AM is then the first gear to leave the startup barrier (T35) and to
run against a registry in another process (T36), before the remaining fleet (T37). The
pre-cutover client handoff runs on an isolated pilot instead (T30).

**Isolation of the pilot.** The pilot registry owns a separate database and seeds only its
control-plane and base types through the real outbox admission path; it does not link or pull
consumer declarations. Both clients read the same database-backed service; the remote consumer
has no local fallback. The existing embedded host stays on its legacy catalogue until T31. No
dual-write, legacy shim, admission fork or production feature switch is introduced. It is a
reusable development/integration composition, not a second registry implementation.

**Pilot build and CI.** A `publish = false` package
`cf-gears-types-registry-pilot` (`testing/fixtures/types-registry-pilot/`) holds the feature-gated
`pilot_registry` / `pilot_consumer` bins and the pilot test, all with
`required-features = ["pilot-fixtures"]`; `CARGO_BIN_EXE_*` gives cargo/nextest reproducible
paths. Bin imports are normal optional dependencies — bin targets cannot see dev-dependencies.
The harness depends on gear crates (AM from T36); no gear crate depends on the harness and the
registry never depends on AM, so no Cargo cycle forms and no fixture dependency enters a gear.
`make test-types-registry-pilot` joins `make ci` and the CI workflow, so every checkpoint gate
runs the pilot rather than silently skipping a feature-gated test.

**Topology and authentication.** The pilot has two application processes
plus the master host's `DirectoryService`, an explicit prerequisite process. Until T36 its
tenant plane uses an independent signed-token development authenticator, recorded as a
development-only limitation. T27 decides the production linked/remote authn-resolver topology
and records it in SPEC §8.4. T36 must run `AuthNResolverBearerAuthenticator` over the real
authn-resolver, with its plugin published after wiring, for both the AM host and the remote
registry's tenant plane; the pilot keeps authn-resolver out of the registry process so the
registry still links no consumer. If production uses the linked topology, T38 verifies it
once T37 has ended the pull. The development authenticator never satisfies Checkpoint 8A.

**Child tenant types.** Tenant create/read needs a non-root tenant type.
AM owns its configured root type; a deployment's child tenant types stay registry-owned
`cfg.entities`. A configured entity whose dependency is not in the registry's inline seed set
cannot be seeded inline — the pilot's case now and every registry's after T37 — so T34 adds
the D11 post-wiring path before AM's proof: types-registry publishes such an item after wiring
with its own context, retrying until the dependency (AM's base type) is admitted; the registry's
readiness does not wait for it, only its consumers do. T37's end of the pull reuses the path.

**Root binding versus root-type refusal.** The root binding — the stored
root's `tenant_type_uuid` against the configured root type, read from AM's own database — stays
checked in `init` and lifecycle-fatal regardless of `bootstrap.strict`. A registry refusal of
the configured root type happens after wiring, so it follows D21: AM stays not ready with a
terminal status naming the identifier and reason, bootstrap does not run, and the process does
not exit. A delayed registry only holds readiness and is never a saga failure. Business,
database or IdP failures of the saga itself keep today's `bootstrap.strict` policy
(`handle_bootstrap_failure`): fatal when strict, logged and skipped otherwise. Resource Group's `ResourceGroupTypeBootstrap`
stays init-only, sealed and without REST: its type registration uses RG's own database, so it
remains in AM's `init`.

**e2e windows.** T26 and T27 move routes and their e2e callers in the same commit, so the
async-surface suite stays green through Checkpoint 7A — the draft's T27–T32 route/auth red
window is gone. The legacy v1 window is T31–T32. No pilot work enters it.

**Boundaries.** Tenant REST uses the interim `/v2/` path until T32; callers see the semantic
API. The SDK's `PublisherContext` is required from T24, but adapters forward it only from T39
and the guard is required in T41. The early handoff promises no mixed-version mutation safety;
final P0 deployment requires Checkpoint 9.

**Not scheduled — optional later work, not approved here.** A generic multi-gear harness
beyond the types-registry pilot; co-hosting the directory service inside the pilot consumer
(allowed only after its lifecycle ordering is verified); method-aware gateway proxy routing
(P24 follow-up); a toolkit-contract codegen extension that would replace the hand-written REST
clients; a tenant-API client cache (P0 specifies none). The separate platform listener,
authorization and workload-bound publisher identity remain P1.

**Renumbering.** Only unfinished tasks change IDs; completed evidence is preserved. A
reference to the pre-P26 REST task (T26) means T26+T27; to the pre-P26 pilot (T35) means T30.

| Now | Work | P26 draft | Pre-P26 (P23/P25) |
|---|---|---|---|
| T26 | Platform API over REST, platform routes and their e2e callers | T26, T27, T28, platform half of T32 | platform half of T26 |
| T27 | Tenant API, resolving clients, REST contract closed | T29, T30, T31, tenant half of T32, T33 | tenant half of T26 |
| T28 | SDK client cache | T34 | T27 |
| T29 | Toolkit post-wiring lifecycle: hook, supervision, `Required` readiness | T35, T36, T37 | T33 (hook, readiness, supervision) |
| T30 | Isolated pilot harness and cold-start handoff | T38, T39 | T35 |
| T31 | Startup audit and atomic cutover | T40, T41 | T28, T29 |
| T32 | One REST version and e2e on the `202` contract | T42, T43 | T30, T31 |
| T33 | Per-crate collectors and the `gts(…)` attribute | T44, T45 | T32, T33 (attribute) |
| T34 | Ownership, dependent `cfg.entities` after wiring, plugin selection | T46, T47 (+ dependent-entity path from T53) | T34, T36 (+ from T38) |
| T35 | AM and static IdP publish and bootstrap after wiring | T48, T50, AM half of T49 | AM part of T37 |
| T36 | AM host prerequisites and real-gear proof, local and remote | prerequisite half of T49, T51 | prerequisite part of T37 |
| T37 | Every remaining gear after wiring; end of the pull | T52, T53 | T37, T38 |
| T38 | Out-of-process e2e, HA, chart | T54 | T39 |
| T39 | Publisher state: stamp, migration, durable context, wire field | T55, T56, T57, T60 | T40, T41, T42, T45 |
| T40 | Commit-time ordering for registration and deletion | T58, T59, T61 | T43, T44, T46 |
| T41 | Every writer sends its publisher; `publisher` required | T62, T63 | T47, T48 |
| T42 | Mixed-version rollout proof | T64 | T49 |

The draft's internal Checkpoints 7A.1–7A.5 are retired; Checkpoints 7A, 7, 8A, 8 and 9 remain,
each with the full gate.

**Planning horizon.** Later tasks keep full acceptance criteria now. Change the queue only for
a demonstrated correctness, security or compatibility blocker, recording the invariant and the
smallest affected task. Convenience refactors and new features do not expand a gate.
*Amended by P27:* the queue may also be regrouped for review packaging when no criterion is
dropped and no checkpoint gate moves, and an additive feature may enter a task's acceptance
criteria only through a numbered plan decision that names it and leaves every checkpoint gate
unchanged.

### P27. Every trait and local client in one pull request; T25 lands on its own

**Accepted 2026-10-08.** A regrouping for review, not a correctness change: no acceptance
criterion of P26 is dropped, and no checkpoint gate moves. It amends P26's planning horizon,
which allowed queue changes only for a correctness, security or compatibility blocker and
barred new features: P27 is neither, so the horizon now also admits review regrouping, and
an additive feature named by a plan decision. This decision names exactly one —
`TypesRegistryApiExt` in T24a. It extends T24a's acceptance criteria, not Checkpoint 7A or
any later gate.

- **T25 is its own toolkit pull request.** The platform-authenticated axis touches only
  toolkit and api-gateway, and nothing before T26's route move uses it, so it is reviewed by
  its owners on branch `toolkit-platform-route-auth`. Its criteria stay unchecked in the task
  list until it merges into `main`. T26's DTO and client commits need only T24; its route move
  and TCP contract test need T25 in `main`.
- **T24a — the tenant contract, its extension helpers and its local client** move out of T27
  (its former commit 1), so T24 + T24a deliver every P0 trait and local client in one pull
  request. T24a adds `TypesRegistryApiExt`, the platform's read conveniences over the tenant
  API, sharing one implementation of the helper logic; it is additive and changes no platform
  shape.
- **T27a — resolving clients and the contract's closure** split out of T27: both
  `DirectoryResolvingClient` wrappers, `rest-server` and `#[provides]`, the full auth matrix on
  both hosts, and the QUICKSTART tenant flows. T27 keeps the tenant REST client, the tenant
  routes and the standalone authenticator with its SPEC §8.4 topology decision.
- **The REST contract becomes a normative reference.** Its items lose their checkboxes and
  name their owning task (T24a, T26, T27 or T27a), so no item is tracked twice.

T24a reuses a number retired before P22 (*"T24a retire v1, promote v2"*, now T32); that
historical reference is marked "(retired)". The sequence becomes
T24a → T25 → T26 → T27 → T27a → T28 → … ; Checkpoint 7A and every later task are unchanged
except that T28 and T30 depend on T27a instead of T27.

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
                              T9a v1 restored; async surface on /v2/ (P12)
                                                                   │
                              T10 instances + chain-derived closure; family kind (P13)
                                                                   │
                              ─── Checkpoint 1 ───
                                                                   │
                                                        T11 revisions + CAS
                                                                   │
                                          T12 family shape + contiguity
                                             │
                              T13 dependency edges (3 kinds; only $ref is content-derived)
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
                                        T20a REST deletion + dry run
                                                        │
                                                 T21 outbox
                                                        │
                                             ─── Checkpoint 5 ───
                                                        │
                   ┌────────────────────────────────────┘
                   │
                   ▼
        T22a REST batchGet + discovery
        (needs T4, T9a, T20a)
                   │
                   ▼
        T22b field projection on all three reads
        (needs T22a; default is document-free)
                   │
                   ▼
        T22c discovery depth + kind filters
        (needs T22b; binds filters in cursor)
                   │
                   ▼
        T22d validators + conditional reads
        (needs T22a, T22b; server-only, P21)
                   │
        ─── Checkpoint 6: all seven v2 routes + conditional reads ───
                   │
        ─── Phase 7: both clients, isolated handoff, persistent registry (P26) ───
                   │
        T23 publisher status/version/supervision (complete)
                   ▼
        T24 platform contract + local + reconciliation/publication (complete)
                   ▼
        T24a tenant contract + TypesRegistryApiExt + tenant local client (P27)
                   ▼
        T25 platform-authenticated axis (own toolkit PR; merged before T26's route move)
                   ▼
        T26 platform API over REST + platform routes + their e2e callers
                   ▼
        T27 tenant API over REST + standalone tenant authenticator
                   ▼
        T27a resolving clients + provides for both APIs; REST contract closed
                   ▼
        T28 platform SDK cache ──► T29 post_wiring + supervision + Required readiness
                   ▼
        T30 isolated pilot harness + cold-start handoff (make test-types-registry-pilot)
                   │
        ─── Checkpoint 7A: both clients usable for integration ───
                   │
        T31 startup audit, then atomic SDK/database cutover; legacy trait/store deleted
                   ▼
        T32 tenant v2 → v1 + migrated e2e (red window T31–T32 only)
                   │
        ─── Checkpoint 7: every gear on the persistent registry ───
                   │
        ─── Phase 8: publication after wiring, out-of-process operation ───
                   │
        T33 per-crate collectors + gts(…) attribute (reruns the T30 pilot)
                   ▼
        T34 publication ownership + dependent cfg.entities after wiring + plugin selector
                   ▼
        T35 Account Management + static IdP publish and bootstrap after wiring (embedded)
                   ▼
        T36 AM host prerequisites + production authn; AM proof local and remote
                   │
        ─── Checkpoint 8A: Account Management, the first real gear ───
                   │
        T37 every remaining gear after wiring (T31's audit order); end of the pull
                   ▼
        T38 out-of-process e2e + two replicas + chart
                   │
        ─── Checkpoint 8: out-of-process operation; C11 applies ───
                   │
        ─── Phase 9: mixed-version rollout, last ───
                   │
        T39 publisher state: stamp, migration, durable context, wire field
                   ▼
        T40 commit-time ordering: check/claim, metadata-only confirmation, versioned deletion
                   ▼
        T41 every writer sends publisher; publisher required
                   ▼
        T42 mixed-version proof on the pilot and out of process

        ─── Checkpoint 9: ready for final review/deployment ───
```

Foundation order (T2→T5) is unavoidably layered: nothing can be registered before a table
exists. From T7 onward the graph is vertical.

## Task index

### Phase 0 — Upgrade (fail fast)
- T1: Upgrade to `gts-rust` 0.12.0, re-validate all declared identifiers

**Checkpoint 0**

### Phase 1 — One global entity of each kind, persisted, async, end to end (fixtures only)
- T2: Migration for the 9 tables
- T3: SeaORM entities for the core six
- T4: Repositories on `DBRunner`
- T5: Transient `gts-rust` store built from database rows
- T6: Typed configuration
- T7: Acceptance path and operation records
- T8: Admission worker — one dependency-free candidate
- T9: REST — `POST /entities`, `GET /operations/{id}`, `GET /entities/{entity_key}`
- T9a: Restore the v1 contract; the async surface moves to `/types-registry/v2/` (P12)
- T10: Registered Instances — **moved here from Phase 2** (P13)

**Checkpoint 1** ← proves the architecture

### Phase 2 — Revisions and concurrency
- T11: Content revisions and compare-and-swap
- T12: Version-family kind, shape and contiguity rules

**Checkpoint 2**

### Phase 3 — Dependencies and materialization
- T13: Dependency edge extraction and writes
- T14: Reverse-impact worklist and artifact refresh
- T15: Revision-vector guard and bounded retry (P15)
- T16: Observability for the admission path

**Checkpoint 3**

### Phase 4 — Compatibility
- T17: Compatibility against one baseline — verdicts counted, `Unknown` and `force` visible (P16)
- T18: Derivation chain and major-0 quarantine — each refusal its own counted reason (P16)

**Checkpoint 4**

### Phase 5 — Batching, deletion, dry run, and dispatch
- T19: Dependency-aware partial admission
- T20: Deletion and Dry Run — plus the `dry_run` / `kind` label sweep (P16)
- T20a: REST deletion and dry run — mutation OpenAPI and quickstart (P17)
- T21: Outbox dispatch wiring — **moved here from Phase 6** (P17)

**Checkpoint 5**

### Phase 6 — Read API and conditional reads
- **Deferred to P1:** T22 — inventory `owning_gear` metadata/filtering (#4628, P18)
- T22a: REST batchGet and discovery — complete OpenAPI and quickstart (P17)
- T22b: Field projection on all three read routes — document-free default (P19)
- T22c: Discovery `depth`, `kind` and `lifecycle_status` filters — cursor-bound and composed with `pattern` (P20)
- T22d: Freshness validators and conditional reads (`ETag` / `304`, batch validators) — **moved here from Phase 7, formerly T29 (retired)** (P21)

**Checkpoint 6**

### Phase 7 — Both clients, an isolated handoff, and every gear on the persistent registry
- T23: Toolkit — publisher signature and publication status (complete)
- T24: `PlatformTypesRegistryApi` contract, models, local client, reconciliation and publication (complete)
- T24a: `TypesRegistryApi` tenant contract, its extension helpers and local client
- T25: Toolkit — a platform-authenticated auth axis; one plane per route (own pull request; open until merged)
- T26: Platform API over REST
- T27: Tenant API over REST; the standalone tenant authenticator
- T27a: Both clients resolvable; the REST contract closed
- T28: SDK client cache — freshness window, byte bound, `fresh` bypass
- T29: Toolkit post-wiring lifecycle — hook, supervision and `Required` readiness
- T30: Isolated pilot harness and cold-start client handoff

**Checkpoint 7A** — client handoff

- T31: Startup audit and the atomic cutover — every gear on the persistent registry
- T32: One REST version and e2e on the `202` contract

**Checkpoint 7**

### Phase 8 — Publication after wiring: Account Management first, then every gear out of process
- T33: Per-crate GTS collectors and the gear's `gts(…)` publication attribute
- T34: Publication ownership, dependent configured entities after wiring, and late-safe plugin selection
- T35: Account Management and its IdP publish and bootstrap after wiring
- T36: Account Management's host prerequisites after wiring and the real-gear proof, local and remote

**Checkpoint 8A** — Account Management, the first real gear

- T37: Every remaining gear publishes after wiring; end of the pull
- T38: Out-of-process e2e run, HA and the deployment chart

**Checkpoint 8**

### Phase 9 — Mixed-version rollout: the publisher-version guard
- T39: Publisher state — stamp, migration, durable context and the wire field
- T40: Commit-time publisher ordering for registration and deletion
- T41: Every writer sends its publisher; `publisher` required
- T42: Mixed-version rollout proof

**Checkpoint 9**

## Checkpoints

Each phase/handoff checkpoint is a human review gate. Do not proceed past a failing
one. Checkpoints 0–9, 7A and 8A retain the full applicable phase gate, including
workspace `make dylint` (P13) and, from Checkpoint 7A on, `make test-types-registry-pilot`
inside `make ci`; Checkpoint 0 keeps T1's documented exception.

**Checkpoint 0** — `make ci` green; every declared GTS identifier still admits under
0.12.0; every difference in generated schema documents accounted for. This gate protects
other gears, so it is reviewed before any registry code is written.

**Checkpoint 1** — a fixture Type Schema registers over REST, the operation reaches
`completed`, the entity and its resolved artifacts are readable, and both survive a process
restart. **The new surface is additive (T9a, P12): v1 is intact, `make e2e-local` is green and no
e2e file was edited.** An Instance registers against a Type Schema committed by an earlier
operation, and a derived Type Schema admits against a committed base with the `dependency` table
empty (T10, P13). Consumers are untouched: the old trait is still served from its existing in-memory
repository, while the new path reads from the database and holds no store between admissions
(P6). The plain gear tests are green on SQLite,
`make test-types-registry-db` is green on PostgreSQL and MySQL, and `make dylint` is re-run
after T9a and T10 — the recorded run covers T1–T9 only (P13). This checkpoint proves the
architecture.

**Checkpoint 2** — equal content reports `unchanged` without a revision;
a stale `expected_resource_version` fails `precondition_failed`; family shape and contiguity
refusals hold under concurrency.

**Checkpoint 3** — a revision of a base type refreshes every dependent's artifacts in one
transaction; an identical recomputation moves no `resource_version`; the activation bound
refuses rather than partially committing; admission emits spans and metrics.

**Checkpoint 4** — the compatibility matrix passes, including `Unknown` rejected with its
own reason; provenance is persisted on every revision. **Every verdict is counted and `Unknown`
and a forced waiver are each distinguishable in the metrics, and admission reasons live in one
compile-enforced vocabulary** (P16) — quarantine and dialect refusals included, none of them
collapsed into `invalid_schema`.

**Checkpoint 5** — a batch with a failing dependency commits independent branches and
blocks everything downstream of it; a circular `$ref` is refused; deletion safety holds.
**No series blends a dry run with a commit or a deletion with a registration, and blocked
candidates are counted per reason** (P16). Registration and both deletion routes support
dry run on `/v2/`, with mutation OpenAPI and quickstart examples (T20a). All three routes,
in committed and dry-run mode, reach terminal outcomes through the outbox without a direct
worker call (T21): operation/outcome records persist, while a dry run changes no entity state,
revision or resource version. `make e2e-local` stays green with no e2e file edited.

**Checkpoint 6** — the P0 REST contract is complete on `/v2/` (P21). **All seven v2 routes are complete** (T20a, T22a, T22b, T22c, P17/P19/P20):
`batchGet` returns explicit per-key results; discovery is bounded and content-free by default, filters in SQL before the page limit, and its cursor
traverses an unchanged matching set exactly once under one `pattern`/`depth`/`kind` filter and
normalized `$select`, and all three reads
project the requested fields. **Conditional reads work** (T22d): an exact read carries a
per-request validator and honours `If-None-Match` with a `304` that carries its `ETag`, and
`batchGet` reports `unchanged` per key with its `etag`. OpenAPI covers every route and
`QUICKSTART.md` covers reads and mutations. Gear tests, `make lychee` and unchanged
`make e2e-local` pass; nothing has been cut over yet, and the new SDK trait is not written yet.

**Checkpoint 7A** — both APIs work through local and resolving REST adapters; the isolated
real-runtime pilot passes consumer-first and registry-first cold starts, late dependency
publication, terminal refusal, `Required` readiness, conditional and cached reads, restart and
graceful shutdown. Its registry seeds base and control-plane types through real admission and
cannot pull the consumer's declarations. Handoff commands are reproducible and
`make test-types-registry-pilot` runs inside `make ci`. Platform cache requirements are T28's;
no tenant-cache semantics are introduced. `make ci`, gear tests on three backends, SDK feature
builds and `make e2e-local` pass, followed by human review. The fleet cutover and mixed-version
safety remain later gates.

**Checkpoint 7** — every gear is on the new SDK and the database (P23, C′, P26).
`PlatformTypesRegistryApi` and its helpers work through the real local client, carrying T22d's
validators; both adapters return operations read through `get_operation`, never a synthesized
receipt (T24, D19). The platform REST client passes its contract test on
`/types-registry/platform/v1/` and the tenant client on `/types-registry/v1/`; platform routes
serve a validated internal token only, tenant reads a validated bearer only (T26, T27, T32,
P25). Linked inventory and `cfg.entities` seed through the outbox, a second start reports
`unchanged`, and no registration T31's audit listed is refused by immediate validation.
`TypesRegistryClient`, ready mode, the in-memory repository and the old v1 routes are gone;
reads are cached with the late-fill guard from the first migrated read (T31, T28). One REST
version remains, and the e2e suites pass on the `202` contract (T32). Gear tests on three
backends, `make ci`, `make e2e-local`, `make e2e-docker`, `make dylint`.

**Checkpoint 8A** — real Account Management and static-idp-plugin publish after wiring,
bootstrap an Active root and pass authenticated child-tenant create/read, in the embedded host
and with the registry in another process, using the production authenticator (T35, T36). Both
start orders, delayed prerequisites, configuration changes and restarts converge. Root-binding
drift is fatal; a refused root type or a type-invalid request is refused without failing boot;
readiness covers publication plus bootstrap; RG's init-only, no-REST boundary stays intact.
Embedded AM e2e is green before the remaining fleet migrates in T37.

**Checkpoint 8** — out-of-process operation (P23). Per-crate collectors, the `gts(…)`
attribute, `post_wiring`, `Required` readiness and supervised publication are in toolkit; the
plugin selector caches no incomplete selection (T29, T33, T34). Every declaring gear publishes
its own crates after wiring and gates its readiness on them; no registry call remains in any
`init()`, and no startup phase fails on an unpublished or unreachable registry (T35–T37). No
process-global GTS inventory remains; the registry seeds inline only its own types, the base
types and the `cfg.entities` whose dependencies lie within that set, and the rest publish after
wiring; the coverage test is green (T34, T37). With types-registry in its own process and two
replicas, a gear in another process publishes, becomes ready and reads through the same trait
(T38). Ceiling C11 still applies.

**Checkpoint 9** — the publisher-version guard is active (P23). Lower versions are
`superseded` before the precondition and before any pre-commit refusal, delayed old operations
do not overwrite newer commits, partial admission converges per entity, a higher version with
identical content moves no `resource_version`, validator or `updated_at`, and another
publisher name is `publisher_mismatch` (T40, SPEC §13). Every writer submits with the actual
publisher's context and a mutation without one is refused (T41); rows written before the
migration were claimed by their first publication; 0.1 → 0.2 → restart 0.1 keeps the newer
content and stamp and 0.1 is ready with a warning (T42). **One REST version: no `/v2/` path
survives (T32, P12).** All 20 success criteria of SPEC §16; `make ci`,
`make test-types-registry-db`, `make e2e-local`, the out-of-process run and `make dylint` green.

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| 0.12.0 semantics reject a currently-admitted schema in another gear | **High** — breaks unrelated gears | T1 is first and is its own commit; full re-validation sweep before any registry code |
| Database bootstrap regresses platform boot or exceeds admission limits | **High** — all linked declarations seed before consumers initialize | T31 tests the combined inventory + configuration set, dependency ordering, repeat startup and limit refusal; keep one bounded batch and verify quickstart/e2e configurations (P18) |
| Removing the old trait breaks ~50 call sites in 20+ gears | **High** | Split by gear group; new trait exists and is tested (T24) before the first consumer moves; `cargo test --workspace` gates each migration task |
| An existing explicit registrant fails during startup | Medium | Registration leaves `init()`: T24's `publish_gts` runs after wiring with bounded retries per cycle and backoff across them, and the gear reports not ready — naming the identifier and reason — instead of failing boot (P22) |
| A consumer calls the registry from `init()` — registration **or** a read | **High** — works in Profile 1, fails out of process because the remote client is wired after every `init` | T31 audits every init-time call site, not only the ~13 `register` sites; T35–T37 move each to the post-wiring hook or a lazy first use. T36 and T38 run with types-registry in another process which is what makes a missed site fail |
| A startup phase after `init` depends on the old barrier — oagw's `post_init` → tenant-resolver plugin selection, account-management's bootstrap | **High** — boot race in Profile 1 once configuration-built plugin instances publish after wiring; boot failure out of process | T31's audit lists every registry-dependent step in `post_init`, `start`, sagas and first plugin selection; T35–T37 move each to a supervised task that waits for its prerequisite; T34 stops `GtsPluginSelector` caching while an instance of the same contract and vendor is invisible (D21) |
| An older release rewrites what a newer one published — rollback, restart or a delayed outbox operation | **High** once every gear publishes at startup | Per-entity publisher stamp checked at commit before the precondition (D18, T40); tests cover delayed operations and partial admission (T42) |
| A pre-Phase-9 registry binary writes to a migrated database | Low while P0 is not in production — the check binds only writers that run it | The migration and the binary ship together, P0 is not deployed before Checkpoint 9, and rolling the registry back below Phase 9 is unsupported (§8.4) |
| The wrong publisher claims an unclaimed row first | Low — every writer names itself from T41, and T37's coverage check assigns each crate to one gear | Cooperative attribution (C3); P1 binds `publisher_name` to the validated identity |
| `cfg.entities` depend on a schema a remote gear publishes through the registry | **High** on an empty database — a bootstrap cycle | An item whose dependency is outside the registry's inline seed set publishes after wiring as types-registry, gating only its consumers (D11 as amended, T34); while the pull lasts an embedded host keeps such items inline, and T37's end of the pull reuses the same path |
| A receipt is returned as an operation | Medium — a terminal replay reads as vacuous success | Both adapters read the operation back (D19, T24) |
| The out-of-process design is first exercised at the end | **High** — late discovery after twenty gears moved | T30 runs the isolated pilot at Checkpoint 7A before existing consumers migrate, inside `make ci`; T33 reruns it with automatic declarations, T36 with real Account Management, and T42 for the guard in Phase 9 |
| A feature-gated pilot test is silently skipped by the checkpoint gate | Medium — the out-of-process handoff looks proven while nothing ran | T30 puts the bins and test in a `publish = false` harness package with `required-features`, and adds `make test-types-registry-pilot` to `make ci` and the CI workflow |
| The development authenticator hides a production authentication gap | **High** — the remote tenant plane could pass without the real authn-resolver | T30 records the development authenticator as development-only; T36 requires `AuthNResolverBearerAuthenticator` over the real authn-resolver for Checkpoint 8A, and T38 covers a linked topology after the pull ends |
| The hand-written REST client drifts from the hand-written handlers | **High** — silent wire mismatch across processes | T26/T27 use the real routes over TCP and T27a closes both API contracts. Platform paths are final from the start; tenant paths promote in T32 and the contract test reruns |
| A declaring crate is owned by no gear, or its owner is enabled but does not publish it | Medium — its types never reach the database, found only on first read | T37's ownership coverage: an independent expected set (cargo metadata plus a `declare_gts_inventory!()` scan) is checked against the gears' `gts(crates = …)` metadata, and per binary each enabled gear's publication is registered. Linking a crate whose owner runs elsewhere is not an omission (P22) |
| Per-crate collectors lose entries under LTO or in a crate reached only through `gts_declarations()` | Medium | T33 carries a release+LTO fixture binary that asserts per-crate counts (P22) |
| T25's toolkit pull request waits on other owners' review | Medium — schedule: T26's route move and contract test are blocked | P27 lands it on its own branch; T24a and T26's DTO and client commits proceed on `main` meanwhile |
| Toolkit changes (`post_wiring`, readiness, macro collectors) need other owners' review | Medium — schedule, not correctness | T29 lands the generic lifecycle before the pilot and cutover; T33 adds collectors and the attribute later. Each lands as separate focused toolkit commits, reviewable alone. T26/T27 use hand-written clients without changing toolkit-contract codegen |
| Two pods of one release with different configuration publish different content | Low — last writer wins until the rollout ends | Accepted (C11); only a newer release orders them |
| An older pod serves against a newer contract after a restart or rollback | Medium — the same exposure every rolling update already has | Superseded is ready with a warning and a metric (D21); the release's N−1 obligation and mixed-version tests (T42) carry compatibility; a stricter opt-in is deferred to P1 (O6) |
| Dual path (in-memory + DB) live through phases 1–6 | Medium | The existing embedded DB path has no production consumer until T31; the T30 pilot uses an isolated composition/database; no dual-write, no reconciliation between them. P6 keeps them from converging by accident: the new path holds no persistent store, so there is no second copy of entity state that could drift from the old repository. **This was breached by T9 and repaired by T9a (P12):** repointing the v1 routes made the DB path consumer-visible ~19 tasks early, and `oagw` / `account-management` were then registering into the database while resolving from memory. The mitigation is now structural — v1 and v2 are separate routes over separate stores, and the criterion "no route straddles the two stores" is grep-checkable |
| DB revisions land before reverse-impact refresh and compatibility | Medium | T11 documents the staging window; minor-bearing Type Schema revisions and effective `force` are refused, and existing embedded consumers stay on legacy until T31 (the isolated pilot is P26). Checkpoints 3 and 4 must close T14/T17 before cutover |
| Read latency regresses at T31, when reads move from memory to the database | Medium | Correctness first, then the cache: D3 already materializes what a read returns, so a read is one keyed `SELECT`, and T28 restores caching with DESIGN's contract (P7). T28 lands before T31, so no migrated consumer reads uncached |
| A cached entry can be stale inside its freshness window | Low | DESIGN §3.3's sanctioned trade, and now bounded further: T22d's validators let T28 revalidate rather than guess, `fresh` gives an authoritative read, `0s` disables the window, and invalidation is immediate on an observed terminal outcome |
| The validator field reaches the SDK models after consumers have migrated | **High** — a second migration across 20+ gears | T22d computes the validator in Phase 6 and T24 carries it in the models from the start, before T31 moves any consumer (P9, P21) |
| A narrow projection reuses a validator or cache entry for a wider representation | **High** — an incomplete answer can be accepted as current | T22b defines one normalized field set; T22d digests it into the validator and T28 keys representations by it (P19) |
| A sparse `depth`/`kind` discovery page skips a later match or resumes under changed filters | **High** — incomplete traversal looks successful | T22c decides every filter in SQL before `LIMIT limit + 1`, binds the filters into the cursor, and tests sparse and mixed-depth/mixed-kind traversal: pages with a cursor are full (P20) |
| The SQL pattern compiler drifts from `gts-id` matching | **High** — discovery silently omits or adds entities | A differential corpus on all three backends compares every pattern shape with `GtsId::matches_pattern`; exhaustive segment matches break the build on a new `gts-id` variant; a `gts-rust` upgrade reruns it (SPEC D14) |
| A filter selective on no index reads a wide identifier range in one statement | Medium — slower pages without a scan budget | The first segment bounds a `gts_id` range; `idx_tr_entity_gts_segment_lookup`, `idx_tr_entity_depth`, `idx_tr_entity_kind_lifecycle` and `idx_tr_entity_lifecycle` serve selective segments, `depth=1` and one lifecycle status with or without `kind`; `EXPLAIN` on 18k rows confirms them on all three backends, every page under 2.2 ms; DESIGN names the residue, including `kind` with `lifecycle_status=all` |
| A materialized `effective_*` value differs from the deleted client-side computation | Medium — reads as a regression, invites a "fix" back to the old wrong answer | 12 call sites in `account-management`, `resource-group`, `credstore` consume those methods today. The old ones resolved only the parent `$ref` and approximated trait defaults (`TODO(#1723)`), so `gts-rust` is authoritative; T31 and T37 carry an explicit criterion to accept the new value, and SPEC §13 pins the outside-the-chain `$ref` case as a test |
| Document-free discovery default changes list reads at ~87 call sites | Medium | The SDK helpers select documents internally, on the page or via `batchGet`, so call shapes survive (P10); T24 fixes the helper shape before T31 touches a consumer |
| Read-shape change reaches e2e alongside the `POST` break | Medium | T32 handles paged discovery and explicit document selection on exact/batch reads through its shared helpers; route stability is `unstable`. Under P12 both breaks arrive at once: T31 deletes old v1 and T32 promotes the async surface |
| Concurrency protocol wrong under the least-tested backend (MySQL) | Medium | Plain gear tests on SQLite plus `make test-types-registry-db` on PostgreSQL/MySQL at every checkpoint |
| The `POST /entities` 202 break reaches other gears' e2e suites | Medium | Confirmed surface: 6 types-registry e2e files (~95 references to `/entities`) plus `account_management/conftest.py` and — **missed until P12** — `oagw/helpers.py`, which registers a batch of schemas *and* instances and reads them back through the list route. T32 owns the migration behind one shared polling helper, not open-coded loops. The break itself no longer arrives at T9: T9a keeps v1 intact, so the suite goes red at T31 and green at T32 rather than being red for ~19 tasks |
| T20a/T22a/T22d's v2 DTOs are authored before T24 fixes the SDK trait shape | Low | The contract is SPEC §10.1/§10.2, not any one task: `items`, `key`, `ListEntitiesResponse`, the validator wire form. All are written against that section, and a disagreement surfaces at T24 while the routes are still behind `/v2/` with no consumer (P17, P21) |
| Interim tenant paths and legacy writers drift during handoff | Low | T26/T27 update async-surface e2e credentials/paths in the same commit as the routes while legacy v1 writers remain intact; T32 promotes tenant paths and migrates legacy writers. Completed P12/P17 evidence remains historical; both handoff and cutover have explicit e2e gates |
| A later refusal or outcome ships without a metric, silently emptying a panel | Medium | P16 makes it a compile error rather than a review item: `ItemFailure::new` takes a `Reason` newtype whose only constructors are the vocabulary's consts, `dry_run` and `kind` become required port parameters at T20, and each of T17–T20 carries T16's evidence bar — contract test, emission test, mutation check |
| Activation write set exceeds the measured 27 in a future deployment | Low | Configured bound 512, refuses rather than partially commits (T14) |

## Sequence

**Accepted queue (P26): sequential, in numeric order.** Completed phases and IDs stay
as recorded; the next implementation task is **T24a** (P27).

1. Phase 7: T24a → T25 → T26 → T27 → T27a → T28 → T29 → T30 (Checkpoint 7A) → T31 → T32
   (Checkpoint 7). T24 and T24a ship as one pull request; T25 as its own toolkit pull request.
2. Phase 8: T33 → T34 → T35 → T36 (Checkpoint 8A) → T37 → T38 (Checkpoint 8).
3. Phase 9: T39 → T40 → T41 → T42 (Checkpoint 9).

Each task lands as the commits it lists; every commit clears the standing bar, and each
checkpoint runs the full gate including the pilot. T26 and T27 move routes and e2e callers
together, so e2e stays green through Checkpoint 7A. T31's audit commit precedes its cutover
commits, and the cutover merges as one change: the legacy trait is deleted in it. The legacy v1
red window is T31–T32 and nothing else enters it. T35–T37 follow T31's audited prerequisite
order; T37's final commit ends the pull. T33, T36 and T42 extend the T30 pilot rather than
creating new cold-start harnesses.
