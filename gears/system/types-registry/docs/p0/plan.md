# Implementation Plan: Types Registry P0

Spec: [`SPEC.md`](./SPEC.md)
Task list: [`todo.md`](./todo.md)

> **Location note.** These planning artifacts live with the gear they describe, in `gears/system/types-registry/docs/p0/`, not in a repository-root `tasks/`. This is deliberate: the monorepo holds many gears, and a shared root `tasks/` would collide across concurrent work. Downstream commands that default to `tasks/todo.md` — including `/agent-skills:build` — must be pointed at `gears/system/types-registry/docs/p0/todo.md`.

## Overview

Make Types Registry durable: entities move from a process-local `gts-rust` store into the platform database, admission becomes an asynchronous operation-based protocol, effective artifacts are materialized, and a new SDK trait replaces the old one outright. Types Registry runs in its own process in production, so the new trait is a toolkit contract served in process and over a hand-written REST client, and each gear publishes its own GTS declarations after wiring. Registry-side inventory pull ends once every gear does (P22, superseding P18's deferral).

Global entities only — no tenant ownership, no PDP, no federation. `PlatformSecurityContext` is in the contract and every route authenticates its caller; authorization and a platform listener remain P1 (P22).

**Coordination state, stated once for the whole plan.** `types_registry__coordination_state` exists — it is created and seeded by this P0's second migration. Only its `entity_write_order` row is seeded and used in P0: the serialization point every commit that writes entity state advances as its first statement (historical decision P15). `source_claim` is not created in P0. The `routing` state row — the future routing generation — arrives with federation, seeded by that phase's own migration alongside `source_claim`. A standalone `routing_config` table is never created, in P0 or any later phase.

`limits.resolved_document` and `limits.resolution_closure` are enforced during admission
and dependent refresh. Both must be positive. Closure accounting is per document over the
candidate overlay; the resolved-size budget applies to the canonical bytes of each effective
artifact. Exceeding either refuses the candidate without committing partial state.

The executable list uses task IDs **T1–T45**, plus the inserted **T35a** SDK payload-validation and **T35b** mock Type Schema admission tasks. T1–T30 are complete; **T31–T45 plus T35a/T35b are the 17 remaining tasks**. T35a and T35b follow T35 without renumbering existing work. Task status, acceptance criteria and recorded evidence are preserved; task references are synchronized with this numbering. The next task is T31 (database seeding and local ClientHub handoff), followed by T32 (full local Account Management/IdP/TR migration).

## Decisions and task numbering

References to P1–P31 in this plan, the spec and the task list name those historical decisions; their task numbers are local to the journal. The graph, index, checkpoints, risks and execution queue below use only current task IDs. There are no separate merged or removed tasks in the executable list. Checkpoint IDs (0–9, 7A and 8A) stay unchanged.

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
                              T10 v1 restored; async surface on /v2/ (P12)
                                                                   │
                              T11 instances + chain-derived closure; family kind (P13)
                                                                   │
                              ─── Checkpoint 1 ───
                                                                   │
                                                        T12 revisions + CAS
                                                                   │
                                          T13 family shape + contiguity
                                             │
                              T14 dependency edges (3 kinds; only $ref is content-derived)
                                             │
                     ┌───────────────────────┼───────────────────────┐
                     ▼                       ▼                       ▼
        T15 reverse impact      T16 revision-vector guard    T17 observability
                     │                       │
                     └───────────┬───────────┘
                                 ▼
                     T18 compatibility ──► T19 derivation + quarantine
                                 │
                     T20 partial admission ──► T21 delete + dry run
                                                        │
                                        T22 REST deletion + dry run
                                                        │
                                                 T23 outbox
                                                        │
                                             ─── Checkpoint 5 ───
                                                        │
                   ┌────────────────────────────────────┘
                   │
                   ▼
        T24 REST batchGet + discovery
        (needs T4, T10, T22)
                   │
                   ▼
        T25 field projection on all three reads
        (needs T24; default is document-free)
                   │
                   ▼
        T26 discovery depth + kind filters
        (needs T25; binds filters in cursor)
                   │
                   ▼
        T27 validators + conditional reads
        (needs T24, T25; server-only, P21)
                   │
        ─── Checkpoint 6: all seven v2 routes + conditional reads ───
                   │
        ─── Phase 7: both clients, real local AM, persistent registry (P31) ───
                   │
        T28 publisher version (complete; status/supervision moved to T37)
                   ▼
        T29 platform contract + local + reconciliation (complete; publish_gts moved to T37)
                   ▼
        T30 tenant contract + TypesRegistryApiExt + tenant local client (P27)
                   ▼
        T31 database seed barrier + local ClientHub APIs + new-gear init proof (P28)
                   ▼
        T32 complete AM + IdP/TR local group; existing AM REST e2e (P29)
                   ▼
        T33 platform-authenticated axis (own toolkit PR; merged before T34's route move)
                   ▼
        T34 platform API over REST + platform routes + their e2e callers
                   ▼
        T35 tenant REST + both resolving/provides clients + complete auth closure
                   ▼
        T35a SDK resolved-payload validation (needs a compatible released GTS helper)
                   ▼
        T35b valid consumer fixtures, then GTS Type Schema admission in the mock
                   ▼
        T36 platform SDK cache ──► T37 post_wiring + supervision + Required readiness
                   │
        ─── Checkpoint 7A: both clients usable for integration ───
                   │
        T38 remaining-fleet/legacy cutover + tenant v1 promotion + green e2e
                   │
        ─── Checkpoint 7: every gear on the persistent registry ───
                   │
        ─── Phase 8: publication after wiring, out-of-process operation ───
                   │
        T39 per-crate collectors + gts(…) attribute + independent compiler fixtures
                   ▼
        T40 publication ownership + dependent cfg.entities after wiring + plugin selector
                   ▼
        T41 AM/IdP after wiring + host closure + production auth + remote proof
                   │
        ─── Checkpoint 8A: Account Management after wiring and remotely ───
                   │
        T42 every remaining gear after wiring (T38's audit order); end of the pull
                   ▼
        T43 out-of-process e2e + two replicas + chart
                   │
        ─── Checkpoint 8: out-of-process operation; C11 applies ───
                   │
        ─── Phase 9: mixed-version rollout, last ───
                   │
        T44 publisher state + durable context + serialized commit/deletion guard
                   ▼
        T45 writer/SDK activation + required publisher + mixed-version proof

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
- T10: Restore the v1 contract; the async surface moves to `/types-registry/v2/` (P12)
- T11: Registered Instances — **moved here from Phase 2** (P13)

**Checkpoint 1** ← proves the architecture

### Phase 2 — Revisions and concurrency
- T12: Content revisions and compare-and-swap
- T13: Version-family kind, shape and contiguity rules

**Checkpoint 2**

### Phase 3 — Dependencies and materialization
- T14: Dependency edge extraction and writes
- T15: Reverse-impact worklist and artifact refresh
- T16: Revision-vector guard and bounded retry (P15)
- T17: Observability for the admission path

**Checkpoint 3**

### Phase 4 — Compatibility
- T18: Compatibility against one baseline — verdicts counted, `Unknown` and `skip_compatibility_check` visible (P16)
- T19: Derivation chain and major-0 quarantine — each refusal its own counted reason (P16)

**Checkpoint 4**

### Phase 5 — Batching, deletion, dry run, and dispatch
- T20: Dependency-aware partial admission
- T21: Deletion and Dry Run — plus the `dry_run` / `kind` label sweep (P16)
- T22: REST deletion and dry run — mutation OpenAPI and quickstart (P17)
- T23: Outbox dispatch wiring — **moved here from Phase 6** (P17)

**Checkpoint 5**

### Phase 6 — Read API and conditional reads
- T24: REST batchGet and discovery — complete OpenAPI and quickstart (P17)
- T25: Field projection on all three read routes — document-free default (P19)
- T26: Discovery `depth`, `kind` and `lifecycle_status` filters — cursor-bound and composed with `pattern` (P20)
- T27: Freshness validators and conditional reads (`ETag` / `304`, batch validators) — moved here from Phase 7 (P21)

**Checkpoint 6**

### Phase 7 — Both clients, real local AM, and every gear on the persistent registry
- T28: Toolkit — publisher signature and publication status (complete; the unused status and supervision were removed, T37 writes them)
- T29: `PlatformTypesRegistryApi` contract, models, local client, reconciliation and publication (complete; the unused `publish_gts` was removed, T37 writes it)
- T30: `TypesRegistryApi` tenant contract, its extension helpers and local client
- T31: Database seeding and local ClientHub handoff — a new gear publishes in `init()`
- T32: Full Account Management local migration — one registry client and real REST proof
- T33: Toolkit — a platform-authenticated auth axis; one plane per route (own pull request; open until merged)
- T34: Platform API over REST
- T35: Tenant REST and both resolving clients — complete SDK transport handoff
- T35a: SDK payload validation over a resolved Type Schema; depends on a compatible released GTS helper
- T35b: Mock Type Schema admission uses GTS validation; valid fixtures first, then shared real-client conformance
- T36: SDK client cache — freshness window, byte bound, `fresh` bypass
- T37: Toolkit post-wiring lifecycle — hook, supervision and `Required` readiness

**Checkpoint 7A** — local application and SDK contract handoff

- T38: Remaining-fleet cutover and one REST version — delete legacy, keep e2e green

**Checkpoint 7**

### Phase 8 — Publication after wiring: Account Management first, then every gear out of process
- T39: Per-crate GTS collectors and the gear's `gts(…)` publication attribute
- T40: Publication ownership, dependent configured entities after wiring, and late-safe plugin selection
- T41: AM after wiring and remotely — host dependencies, bootstrap and readiness

**Checkpoint 8A** — Account Management after wiring and remotely

- T42: Every remaining gear publishes after wiring; end of the pull
- T43: Out-of-process e2e run, HA and the deployment chart

**Checkpoint 8**

### Phase 9 — Mixed-version rollout: the publisher-version guard
- T44: Publisher guard — durable state, wire context and commit-time ordering
- T45: Publisher activation and mixed-version rollout proof

**Checkpoint 9**

## Checkpoints

Each phase/handoff checkpoint is a human review gate. Do not proceed past a failing one. Checkpoints 0–9, 7A and 8A retain the full applicable phase gate, including workspace `make dylint` (P13) and, from T41/Checkpoint 8A on, `make e2e-am-remote` inside `make ci`; Checkpoint 0 keeps T1's documented exception.

**Checkpoint 0** — `make ci` green; every declared GTS identifier still admits under
0.12.0; every difference in generated schema documents accounted for. This gate protects
other gears, so it is reviewed before any registry code is written.

**Checkpoint 1** — a fixture Type Schema registers over REST, the operation reaches `completed`, the entity and its resolved artifacts are readable, and both survive a process restart. **The new surface is additive (T10, P12): v1 is intact, `make e2e-local` is green and no e2e file was edited.** An Instance registers against a Type Schema committed by an earlier operation, and a derived Type Schema admits against a committed base with the `dependency` table empty (T11, P13). Consumers are untouched: the old trait is still served from its existing in-memory repository, while the new path reads from the database and holds no store between admissions (P6). The plain gear tests are green on SQLite, `make test-integration LANE=types-registry-db` is green on PostgreSQL and MySQL, and `make dylint` is re-run after T10 and T11 — the recorded run covers T1–T9 only (P13). This checkpoint proves the architecture.

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

**Checkpoint 5** — a batch with a failing dependency commits independent branches and blocks everything downstream of it; a circular `$ref` is refused; deletion safety holds. **No series blends a dry run with a commit or a deletion with a registration, and blocked candidates are counted per reason** (P16). Registration and both deletion routes support dry run on `/v2/`, with mutation OpenAPI and quickstart examples (T22). All three routes, in committed and dry-run mode, reach terminal outcomes through the outbox without a direct worker call (T23): operation/outcome records persist, while a dry run changes no entity state, revision or resource version. `make e2e-local` stays green with no e2e file edited.

**Checkpoint 6** — the P0 REST contract is complete on `/v2/` (P21). **All seven v2 routes are complete** (T22, T24, T25, T26, P17/P19/P20): `batchGet` returns explicit per-key results; discovery is bounded and content-free by default, filters in SQL before the page limit, and its cursor traverses an unchanged matching set exactly once under one `pattern`/`depth`/`kind` filter and normalized `$select`, and all three reads project the requested fields. **Conditional reads work** (T27): an exact read carries a per-request validator and honours `If-None-Match` with a `304` that carries its `ETag`, and `batchGet` reports `unchanged` per key with its `etag`. OpenAPI covers every route and `QUICKSTART.md` covers reads and mutations. Gear tests, `make lychee` and unchanged `make e2e-local` pass; nothing has been cut over yet, and the new SDK trait is not written yet.

**Checkpoint 7A** — T31's seed/local ClientHub path and T32's complete real AM application pass, and both SDKs pass real TCP/resolving/auth contracts. T35a's payload-validation wrapper matches the released GTS helper and rejects incomplete projections; T35b's mock Type Schema admission passes shared real-client conformance. T36's cache and T37's generic lifecycle/supervision/readiness contracts hold. The handoff promises local AM and those SDK contracts, not complete process-level cold-start/recovery behavior. That application proof belongs to T41/Checkpoint 8A. Full CI/backend/local e2e gates and human review remain.

**Checkpoint 7** — every gear is on the new SDK and the database (P23, C′, P26). `PlatformTypesRegistryApi` and its helpers work through the real local client, carrying T27's validators; both adapters return operations read through `get_operation`, never a synthesized receipt (T29, D19). The platform REST client passes its contract test on `/types-registry/platform/v1/` and the tenant client on `/types-registry/v1/`; platform routes serve a validated internal token only, tenant reads a validated bearer only (T34, T35, T38, P25). Linked inventory and `cfg.entities` seed through the outbox, a second start reports `unchanged`, and no registration T38's audit listed is refused by immediate validation. `TypesRegistryClient`, ready mode, the in-memory repository and the old v1 routes are gone; reads are cached with the late-fill guard by T36 and throughout T38; T32's earlier local handoff is explicitly uncached. One REST version remains, and the e2e suites pass on the `202` contract (T38). Gear tests on three backends, `make ci`, `make e2e-local`, `make e2e-docker`, `make dylint`.

**Checkpoint 8A** — real Account Management and static-idp-plugin publish after wiring, bootstrap an Active root and pass authenticated child-tenant create/read, in the embedded host and with the registry in another process, using the production authenticator (T41). Both start orders, delayed prerequisites, configuration changes and restarts converge. Root-binding drift is fatal; a refused root type or a type-invalid request is refused without failing boot; readiness covers publication plus bootstrap; RG's init-only, no-REST boundary stays intact. Embedded AM e2e is green before the remaining fleet migrates in T42.

**Checkpoint 8** — out-of-process operation (P23). Per-crate collectors, the `gts(…)` attribute, `post_wiring`, `Required` readiness and supervised publication are in toolkit; the plugin selector caches no incomplete selection (T37, T39, T40). Every declaring gear publishes its own crates after wiring and gates its readiness on them; no registry call remains in any `init()`, and no startup phase fails on an unpublished or unreachable registry (T41/T42). No process-global GTS inventory remains; the registry seeds inline only its own types, the base types and the `cfg.entities` whose dependencies lie within that set, and the rest publish after wiring; the coverage test is green (T40, T42). With types-registry in its own process and two replicas, a gear in another process publishes, becomes ready and reads through the same trait (T43). Ceiling C11 still applies.

**Checkpoint 9** — the publisher-version guard is active (P23). Lower versions are `superseded` before the precondition and before any pre-commit refusal, delayed old operations do not overwrite newer commits, partial admission converges per entity, a higher version with identical content moves no `resource_version`, validator or `updated_at`, and another publisher name is `publisher_mismatch` (T44, SPEC §13). Every writer submits with the actual publisher's context and a mutation without one is refused (T45); rows written before the migration were claimed by their first publication; 0.1 → 0.2 → restart 0.1 keeps the newer content and stamp and 0.1 is ready with a warning (T45). **One REST version: no `/v2/` path survives (T38, P12).** All 20 success criteria of SPEC §16; `make ci`, `make test-integration LANE=types-registry-db`, `make e2e-local`, the out-of-process run and `make dylint` green.

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| 0.12.0 semantics reject a currently-admitted schema in another gear | **High** — breaks unrelated gears | T1 is first and is its own commit; full re-validation sweep before any registry code |
| Database bootstrap regresses platform boot or exceeds admission limits | **High** — all linked declarations seed before consumers initialize | T31 tests the combined inventory + configuration set, dependency ordering, repeat startup and limit refusal; T38 rechecks it after legacy deletion. Keep one bounded batch and verify deployment configurations (P18/P28) |
| Removing the old trait breaks ~50 call sites in 20+ gears | **High** | Split by gear group; new trait exists and is tested (T29) before the first consumer moves; `cargo test --workspace` gates each migration task |
| An existing explicit registrant fails during startup | Medium | Registration leaves `init()`: T37's `publish_gts` runs after wiring with bounded retries per cycle and backoff across them, and the gear reports not ready — naming the identifier and reason — instead of failing boot (P22) |
| A consumer calls the registry from `init()` — registration **or** a read | **High** — works in Profile 1, fails out of process because the remote client is wired after every `init` | T38 audits every init-time call site, not only the ~13 `register` sites; T41/T42 move each to the post-wiring hook or a lazy first use. T41 and T43 run with types-registry in another process which is what makes a missed site fail |
| A startup phase after `init` depends on the old barrier — oagw's `post_init` → tenant-resolver plugin selection, account-management's bootstrap | **High** — boot race in Profile 1 once configuration-built plugin instances publish after wiring; boot failure out of process | T38's audit lists every registry-dependent step in `post_init`, `start`, sagas and first plugin selection; T41/T42 move each to a supervised task that waits for its prerequisite; T40 stops `GtsPluginSelector` caching while an instance of the same contract and vendor is invisible (D21) |
| An older release rewrites what a newer one published — rollback, restart or a delayed outbox operation | **High** once every gear publishes at startup | Per-entity publisher stamp checked at commit before the precondition (D18, T44); tests cover delayed operations and partial admission (T45) |
| A pre-Phase-9 registry binary writes to a migrated database | Low while P0 is not in production — the check binds only writers that run it | The migration and the binary ship together, P0 is not deployed before Checkpoint 9, and rolling the registry back below Phase 9 is unsupported (§8.4) |
| The wrong publisher claims an unclaimed row first | Low — every writer names itself from T45, and T42's coverage check assigns each crate to one gear | Cooperative attribution (C3); P1 binds `publisher_name` to the validated identity |
| `cfg.entities` depend on a schema a remote gear publishes through the registry | **High** on an empty database — a bootstrap cycle | An item whose dependency is outside the registry's inline seed set publishes after wiring as types-registry, gating only its consumers (D11 as amended, T40); while the pull lasts an embedded host keeps such items inline, and T42's end of the pull reuses the same path |
| A receipt is returned as an operation | Medium — a terminal replay reads as vacuous success | Both adapters read the operation back (D19, T29) |
| Remote behavior is assumed from local/TCP success | **High** | P31 explicitly limits Checkpoint 7A to local AM and SDK contracts. T41 first proves complete production AM process startup/recovery with an isolated registry; T43 reruns it after end-of-pull/HA, T45 with mixed versions |
| Real remote AM tests are skipped by CI | Medium | T41 creates `make e2e-am-remote` and wires it into CI; Checkpoints 8A/8/9 require it. T39 compiler/LTO/isolation checks are wired into the existing macro-test CI venue independently |
| Test authentication hides a production gap | **High** | T35 provides the production registry host with linked AuthN; T41 verifies both real host authenticators and authn readiness, T43 after end-of-pull |
| The hand-written REST client drifts from the hand-written handlers | **High** — silent wire mismatch across processes | T34/T35 use the real routes over TCP and T35 closes both API contracts. Platform paths are final from the start; tenant paths promote in T38 and the contract test reruns |
| A declaring crate is owned by no gear, or its owner is enabled but does not publish it | Medium — its types never reach the database, found only on first read | T42's ownership coverage: an independent expected set (cargo metadata plus a `declare_gts_inventory!()` scan) is checked against the gears' `gts(crates = …)` metadata, and per binary each enabled gear's publication is registered. Linking a crate whose owner runs elsewhere is not an omission (P22) |
| Per-crate collectors lose entries under LTO or in a crate reached only through `gts_declarations()` | Medium | T39 carries a release+LTO fixture binary that asserts per-crate counts (P22) |
| T33's toolkit pull request waits on other owners' review | Medium — schedule: T34's route move and contract test are blocked | P27 lands it on its own branch; T30 and T34's DTO and client commits proceed on `main` meanwhile |
| Toolkit changes (`post_wiring`, readiness, macro collectors) need other owners' review | Medium — schedule, not correctness | T37 lands generic lifecycle before the local cutover; T41 supplies real remote AM; T39 adds collectors and the attribute later. Each lands as separate focused toolkit commits, reviewable alone. T34/T35 use hand-written clients without changing toolkit-contract codegen |
| Two pods of one release with different configuration publish different content | Low — last writer wins until the rollout ends | Accepted (C11); only a newer release orders them |
| An older pod serves against a newer contract after a restart or rollback | Medium — the same exposure every rolling update already has | Superseded is ready with a warning and a metric (D21); the release's N−1 obligation and mixed-version tests (T45) carry compatibility; a stricter opt-in is deferred to P1 (O6) |
| Independent old/new catalogues during migration | **High** — a new writer can disappear from an old reader | P28 gates local publication on database seeding. P29 migrates the complete AM/IdP/TR group in one merge and audits external readers/writers; no fallback or mirrored writes. Unrelated workflows remain legacy until T38, and v1/v2 routes never straddle stores |
| DB revisions land before reverse-impact refresh and compatibility | Medium | T12 documents the staging window; minor-bearing Type Schema revisions and effective `skip_compatibility_check` are refused, and remaining embedded consumers stay legacy until T38; T32 already proves the real AM group. Checkpoints 3 and 4 must close T15/T18 before cutover |
| Read latency increases as consumers move from memory to the database | Medium | T32 accepts uncached reads for the early local handoff and adds no private memo. D3 materializes artifacts; T36 supplies the shared SDK cache before the remaining T38 fleet cutover |
| A cached entry can be stale inside its freshness window | Low | DESIGN §3.3's sanctioned trade, and now bounded further: T27's validators let T36 revalidate rather than guess, `fresh` gives an authoritative read, `0s` disables the window, and invalidation is immediate on an observed terminal outcome |
| The validator field reaches the SDK models after consumers have migrated | **High** — a second migration across 20+ gears | T27 computes the validator in Phase 6 and T29 carries it in the models from the start, before T38 moves any consumer (P9, P21) |
| A narrow projection reuses a validator or cache entry for a wider representation | **High** — an incomplete answer can be accepted as current | T25 defines one normalized field set; T27 digests it into the validator and T36 keys representations by it (P19) |
| A sparse `depth`/`kind` discovery page skips a later match or resumes under changed filters | **High** — incomplete traversal looks successful | T26 decides every filter in SQL before `LIMIT limit + 1`, binds the filters into the cursor, and tests sparse and mixed-depth/mixed-kind traversal: pages with a cursor are full (P20) |
| The SQL pattern compiler drifts from `gts-id` matching | **High** — discovery silently omits or adds entities | A differential corpus on all three backends compares every pattern shape with `GtsId::matches_pattern`; exhaustive segment matches break the build on a new `gts-id` variant; a `gts-rust` upgrade reruns it (SPEC D14) |
| A filter selective on no index reads a wide identifier range in one statement | Medium — slower pages without a scan budget | The first segment bounds a `gts_id` range; `idx_tr_entity_gts_segment_lookup`, `idx_tr_entity_depth`, `idx_tr_entity_kind_lifecycle` and `idx_tr_entity_lifecycle` serve selective segments, `depth=1` and one lifecycle status with or without `kind`; `EXPLAIN` on 18k rows confirms them on all three backends, every page under 2.2 ms; DESIGN names the residue, including `kind` with `lifecycle_status=all` |
| A materialized `effective_*` value differs from the deleted client-side computation | Medium — reads as a regression, invites a "fix" back to the old wrong answer | 12 call sites in `account-management`, `resource-group`, `credstore` consume those methods today. The old ones resolved only the parent `$ref` and approximated trait defaults (`TODO(#1723)`), so `gts-rust` is authoritative; T38 and T42 carry an explicit criterion to accept the new value, and SPEC §13 pins the outside-the-chain `$ref` case as a test |
| Document-free discovery default changes list reads at ~87 call sites | Medium | The SDK helpers select documents internally, on the page or via `batchGet`, so call shapes survive (P10); T29 fixes the helper shape before T38 touches a consumer |
| Read-shape change reaches e2e alongside the `POST` break | Medium | T38 handles paged discovery and explicit document selection on exact/batch reads through its shared helpers; route stability is `unstable`. Under P12 both breaks arrive at once: T38 deletes old v1 and T38 promotes the async surface |
| Concurrency protocol wrong under the least-tested backend (MySQL) | Medium | Plain gear tests on SQLite plus `make test-integration LANE=types-registry-db` on PostgreSQL/MySQL at every checkpoint |
| The `POST /entities` 202 break reaches other gears' e2e suites | Medium | Confirmed surface: 6 types-registry e2e files (~95 references to `/entities`) plus `account_management/conftest.py` and — **missed until P12** — `oagw/helpers.py`, which registers a batch of schemas *and* instances and reads them back through the list route. T38 owns the migration behind one shared polling helper, not open-coded loops. The break itself no longer arrives at T9: T10 keeps v1 intact, so P30 keeps the suite green by merging deletion, path promotion and caller migration in T38 |
| T22/T24/T27's v2 DTOs are authored before T29 fixes the SDK trait shape | Low | The contract is SPEC §10.1/§10.2, not any one task: `items`, `key`, `ListEntitiesResponse`, the validator wire form. All are written against that section, and a disagreement surfaces at T29 while the routes are still behind `/v2/` with no consumer (P17, P21) |
| Interim tenant paths and legacy writers drift during handoff | Low | T34/T35 update async-surface e2e credentials/paths in the same commit as the routes while legacy v1 writers remain intact; T38 promotes tenant paths and migrates legacy writers. Completed P12/P17 evidence remains historical; both handoff and cutover have explicit e2e gates |
| A later refusal or outcome ships without a metric, silently emptying a panel | Medium | P16 makes it a compile error rather than a review item: `ItemFailure::new` takes a `Reason` newtype whose only constructors are the vocabulary's consts, `dry_run` and `kind` become required port parameters at T21, and each of T18–T21 carries T17's evidence bar — contract test, emission test, mutation check |
| Activation write set exceeds the measured 27 in a future deployment | Low | Configured bound 512, refuses rather than partially commits (T15) |

## Sequence

**Accepted queue (P32): sequential, in numeric order.** Completed phases retain their status and evidence; the next implementation task is **T31**.

1. Phase 7: T31 → T32 → T33 → T34 → T35 → T35a → T35b → T36 → T37 (Checkpoint 7A) → T38 (Checkpoint 7). T29/T30 are complete; T33 remains its own toolkit pull request. T35a delegates to the released GTS snapshot helper; any required GTS/spec migration is a separate prerequisite. T35b prepares valid fixtures before enabling mock Type Schema validation and needs no new upstream helper.
2. Phase 8: T39 → T40 → T41 (Checkpoint 8A) → T42 → T43 (Checkpoint 8).
3. Phase 9: T44 → T45 (Checkpoint 9).

Each task lands as the commits it lists; every commit clears the standing bar, and each checkpoint runs the full gate including the real AM remote target from T41 onward. T34 and T35 move routes and e2e callers together, so e2e stays green through Checkpoint 7A. T38's audit commit precedes its cutover commits, and the cutover merges as one change: the legacy trait is deleted in it. Legacy removal, tenant path promotion and caller migration now merge together in T38; no red window is accepted. T41/T42 follow T38's audited prerequisite order; T42's final commit ends the pull. T41, T43 and T45 reuse the real AM process venue.
