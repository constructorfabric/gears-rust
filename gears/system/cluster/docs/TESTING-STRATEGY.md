# Testing Strategy — Cluster

> **Status: DRAFT — for iteration.** This document captures the intended testing
> strategy for the cluster gear as it grows from an SDK-only change into a wiring
> crate plus a family of real backend plugins (standalone, Postgres, K8s, Redis,
> NATS, etcd). It is a living strategy, not a feature spec — the per-backend
> integration suites it describes are delivered by the follow-up plugin changes.
>
> **Companion document:** the concrete, ordered list of behaviors to test — with
> stable IDs, layer, status, and traceability — lives in the
> [scenario catalog](./scenarios/README.md). This document defines the *how* (layers,
> tooling, crate layout); the catalog enumerates the *what*.

<!-- toc -->

- [1. Context & Goals](#1-context--goals)
  - [1.1 Where we are today](#11-where-we-are-today)
  - [1.2 The governing requirement](#12-the-governing-requirement)
  - [1.3 What in-process stubs cannot prove](#13-what-in-process-stubs-cannot-prove)
- [2. The Test Pyramid](#2-the-test-pyramid)
- [3. Layer 1 — Unit & Smoke (in-process)](#3-layer-1--unit--smoke-in-process)
- [4. Layer 2 — Backend Conformance Suite (the keystone)](#4-layer-2--backend-conformance-suite-the-keystone)
- [5. Layer 3 — Per-Backend Integration (testcontainers)](#5-layer-3--per-backend-integration-testcontainers)
- [6. Layer 4 — Fault Injection & Deterministic Simulation](#6-layer-4--fault-injection--deterministic-simulation)
- [7. Cross-Cutting Tooling](#7-cross-cutting-tooling)
- [8. Lifecycle & Contract Gaps to Close First](#8-lifecycle--contract-gaps-to-close-first)
- [9. CI Cadence](#9-ci-cadence)
- [10. Open Questions](#10-open-questions)

<!-- /toc -->

## 1. Context & Goals

### 1.1 Where we are today

`cluster-sdk` is the only crate. Test coverage is:

- **Unit tests** co-located with source (`src/**/*_tests.rs`) — resolver builders,
  scoping wrappers, polyfill diffing, default backends, observability instrumentation.
- **Smoke / integration tests** (`tests/coordination.rs`, `tests/resolution.rs`,
  `tests/watch_lifecycle.rs`) driving an in-process stub cache
  (`src/defaults/test_cache.rs`) through the public contract.

This satisfies `cpt-cf-clst-feature-smoke-tests` and is explicitly scoped (per the
smoke-test feature and DESIGN §6) to **API shape**, not distributed correctness. The
wiring crate (`cf-gears-cluster`) and every real backend are follow-up changes — and
that is precisely where the deeper testing described here applies.

### 1.2 The governing requirement

The central NFR drives the entire strategy:

> `cpt-cf-clst-nfr-cross-backend-stability` — *a consumer gear's behavior MUST NOT
> change when an operator switches the backend bound to a primitive, provided the new
> backend meets the consumer's declared capability requirements.* Threshold: *gear
> integration tests pass identically against any backend that satisfies the declared
> capability requirements.*

"Pass identically against any backend" is only verifiable with **one shared test
body run against every backend**. That observation produces the keystone of this
strategy (§4).

A second NFR sets the bar for the strong-guarantee backends:

> `cpt-cf-clst-nfr-leader-guarantee` — under contention testing with 10+ concurrent
> candidates across 3+ nodes against a linearizable backend, **zero observed
> split-brain occurrences**.

### 1.3 What in-process stubs cannot prove

The stub backend has, by design (smoke-test feature §3), **one state map, one clock,
one FIFO event channel**. It therefore cannot reproduce: network partition, clock
skew, split-brain, cross-subscriber message reordering, connection loss / reconnect,
backpressure-induced lag from a real broker, or backend-specific failure semantics
(Redis AOF loss, Postgres `synchronous_commit` windows, NATS JetStream sequence gaps,
K8s API-server throttling). DESIGN §6 calls these out as the deliberate boundary of
smoke testing and assigns them to per-plugin verification. The layers below fill that
gap.

## 2. The Test Pyramid

Ordered cheap+fast → expensive+slow. Higher layers run more often; lower layers gate
releases.

```
 ┌──────────────────────────────────────────────────────────┐
 │ L4  Fault injection (Toxiproxy) + DST (turmoil)           │  nightly + per-PR (DST is fast)
 ├──────────────────────────────────────────────────────────┤
 │ L3  Per-backend integration (testcontainers, envtest/kind)│  per-PR in plugin crate, nightly full
 ├──────────────────────────────────────────────────────────┤
 │ L2  Backend conformance suite (shared, parametrized)      │  per-PR (stub) + per-backend (L3 infra)
 ├──────────────────────────────────────────────────────────┤
 │ L1  Unit + smoke (in-process stub)                        │  every PR, milliseconds
 └──────────────────────────────────────────────────────────┘
```

Every scenario in the [scenario catalog](./scenarios/README.md) is tagged with the
layer it belongs to (`L2`/`L4`), so the catalog is the per-layer work-list for
this pyramid.

## 3. Layer 1 — Unit & Smoke (in-process)

**Status: implemented.** Keep as-is. Per-PR, no external dependencies, sub-second.
Covers public API shape, resolver/capability-mismatch error paths, scoping
round-trips, polyfill diffing, and the watch-union variants the stub can emit
(`Lagged`, `Reset`, `Closed(Shutdown)`, `CasConflict`, `CapabilityNotMet`).

This layer remains the contract's first gate. Everything below verifies that real
backends *reproduce* what this layer specifies.

## 4. Layer 2 — Backend Conformance Suite (the keystone)

**Status: to build. Highest leverage.**

**Decision: a standalone `cluster-conformance` crate** (not a `conformance` feature on
`cluster-sdk`). Standalone keeps plugin crates depending on it as a normal
dev-dependency without a feature-flag cycle through the SDK, and lets the suite carry
its own test-only dependencies (`proptest`, fault-injection mocks) that the SDK
contract crate must not pull in.

The crate exposes parametrized suites that accept a backend factory and assert the
contract — independent of *which* backend:

```rust
pub fn cache_conformance_suite(factory: impl Fn() -> Arc<dyn ClusterCacheBackend>);
pub fn leader_conformance_suite(factory: impl Fn() -> Arc<dyn LeaderElectionBackend>);
pub fn lock_conformance_suite(factory: impl Fn() -> Arc<dyn DistributedLockBackend>);
pub fn sd_conformance_suite(factory: impl Fn() -> Arc<dyn ServiceDiscoveryBackend>);
```

Every plugin's integration tests then reduce to ~10 lines: build the backend, hand it
to the suite. This is the *only* mechanism that operationalizes
`cpt-cf-clst-nfr-cross-backend-stability` — "identical observable behavior" is a claim
you can only test by running identical assertions everywhere.

**Capability-gated assertions.** Each run declares the backend's `features()` /
`consistency()`. The suite asserts strict guarantees *only* where the backend claims
them — e.g. single-leader-under-contention is asserted only when
`LeaderElectionFeatures::linearizable == true`. This simultaneously tests the
honest-declaration requirement (`cpt-cf-clst-fr-validation-honest-declaration`): a
backend that lies about a feature fails the suite.

**Contract items the suite must cover** (drawn from DESIGN §3.1/§3.3 and the NFRs;
each maps to an `SC-*` row in the [scenario catalog](./scenarios/README.md)):

- Version monotonicity; version 0 reserved as sentinel; each `put` increments.
- CAS conflict surfaces `current` entry; `compare_and_swap` succeeds iff
  `expected_version == current`.
- `compare_and_delete` survives a key's version resetting to 1 on delete+recreate
  (the value/owner-token guard, DESIGN §3.3 backend-trait note) — a named regression
  test, since a version guard would alias a successor's fresh claim.
- TTL expiry removes the entry and emits an `Expired` notification.
- Per-key watch ordering preserved; delivery at-most-once
  (`cpt-cf-clst-nfr-watch-delivery`).
- Default `DiscoveryFilter` returns enabled-only; metadata predicates AND-combine;
  result ordering is unspecified (suite must NOT assume order).
- Scoping round-trip: `scoped(p)` write/read strips back to the bare key/name.

**Model-based / property variants.** Where practical, pair example-based tests with
`proptest`: generate random op sequences and compare the backend against a trivial
`HashMap` reference model, asserting the version/CAS invariants hold under any
interleaving the single-threaded model permits.

### 4.1 Proposed crate layout

A sketch, not committed code — to be scaffolded when we build the suite. One module
per primitive, plus a reusable in-memory fixture and a `proptest` reference model.

```
gears/system/cluster/cluster-conformance/
  Cargo.toml                # pkg cf-gears-cluster-conformance, lib cluster_conformance
                            #   deps: cluster-sdk, async-trait, tokio
                            #   dev-deps: tokio (test-util, macros, rt), proptest
  src/
    lib.rs                  # crate docs + re-exports of the run_*_conformance entry points
    fixture.rs              # MemCache: a compact in-process ClusterCacheBackend
                            #   (Linearizable, real versioning/TTL/watch) the suite
                            #   self-tests against; doubles as a reference fixture
    cache.rs                # SC-CACHE-* scenarios + run_cache_conformance(factory)
    leader.rs               # SC-LEAD-*   scenarios + run_leader_conformance(factory)
    lock.rs                 # SC-LOCK-*   scenarios + run_lock_conformance(factory)
    discovery.rs            # SC-DISC-*   scenarios + run_discovery_conformance(factory)
    model.rs                # (later) proptest HashMap reference model for CAS/version
```

Entry-point shape (factory-per-scenario so state never leaks; capability-gated
assertions read `features()`/`consistency()`):

```rust
pub async fn run_cache_conformance<F>(make: F)
where F: Fn() -> Arc<dyn ClusterCacheBackend>;

// Each scenario is its own `pub async fn scenario_<id>(backend)` so a plugin can
// run the whole suite or cherry-pick one case. The runner builds a fresh backend
// per scenario via `make()`.
```

Cache-only plugins feed the SDK defaults into the other three suites. `CasBased*` /
`CacheBased*` live in the `cluster` gear crate (`cluster::defaults::*`), not in
`cluster-sdk` or `cluster-conformance` — `cluster-conformance` deliberately has no
dependency on `cluster` (every plugin depends on `cluster-conformance`, so its real
dependency graph stays limited to `cluster-sdk`), so this wiring is the plugin's own
code, not something `cluster-conformance` provides:

```rust
run_leader_conformance(|| {
    let cache: Arc<dyn ClusterCacheBackend> = make_cache();
    Arc::new(cluster::defaults::CasBasedLeaderElectionBackend::new(cache).unwrap()) as _
}).await;
```

A plugin's integration test then reduces to: build the real backend in a container,
hand it to `run_*_conformance`. Open question on the async-runtime entry shape (plain
`async fn` vs. macro-generated `#[tokio::test]` cases) is tracked in §10.

The `cluster` gear itself is the one exception that proves this composition works at
all (`SC-ROUTE-001`/`002`): since it can't depend on `cluster-conformance` without
creating a plugin-facing dependency problem, it proves the "cache-only in, all four
primitives out" guarantee with its own bespoke tests
(`cluster/src/defaults/{leader,lock,discovery}_tests.rs`) rather than by calling
`run_*_conformance` — see [scenarios/routing.md](./scenarios/routing.md).

## 5. Layer 3 — Per-Backend Integration (testcontainers)

**Status: per plugin follow-up.** Two artifacts are required per backend, and both are
mandatory — they are complementary, not alternatives:

1. **Conformance-suite usage** — the plugin's integration tests instantiate the §4
   `cluster-conformance` suites against the real backend in a container. This proves
   the cross-backend baseline (`cpt-cf-clst-nfr-cross-backend-stability`) without
   restating it.
2. **A per-plugin testing design** — a `TESTING.md` (or a testing section in the
   plugin's own DESIGN) that documents the backend-specific surface: the container
   topology, which native paths are tested, the backend's `*Features` /
   `consistency()` declaration and *why*, the `ProviderErrorKind` mapping, and the
   fault-injection / DST scenarios that apply. Per-plugin designs **reference** this
   strategy for the shared layers rather than duplicating them; this document owns the
   cross-cutting strategy, each plugin owns its specifics.

Beyond the conformance baseline, each plugin tests native-path behavior the suite
can't express generically:

| Backend | Container | Native paths beyond the conformance suite |
|---|---|---|
| **Standalone** | none (in-process) | Conformance suite + `loom` concurrency (§7) |
| **Postgres** | `testcontainers` postgres image | `LISTEN/NOTIFY` watch + 8KB payload limit (why events are key-only); `pg_advisory_lock`; the `synchronous_commit=off` path that makes the cache `EventuallyConsistent` and MUST reject the default leader-election constructor (ADR-009) |
| **Redis** | redis image + **Sentinel** topology | Lua CAS scripts; `SET NX EX` locks; keyspace notifications; **Sentinel failover** is where the `EventuallyConsistent` declaration must hold |
| **K8s** | `envtest` (apiserver+etcd) for fast; `kind` / `k3s` (k3s testcontainers module) for e2e | Lease API; CRD + `resourceVersion`; watch streams; RBAC; Lease-per-instance for SD (ADR-008) |
| **NATS** | `async-nats`-compatible image | JetStream KV bucket + revision-based CAS; watch subscriptions |
| **etcd** | `etcd` image | Native `mod_revision` CAS; native lease/lock/election APIs |

The conformance suite is the *baseline*; these tables list only the
backend-specific additions.

## 6. Layer 4 — Fault Injection & Deterministic Simulation

**Status: to introduce.** This is the layer currently missing between in-process
stubs and full distributed testing — it tests the watch lifecycle and coordination
*recovery* the contract promises but stubs only fake.

- **Toxiproxy** (`toxiproxy-rust`, or via testcontainers) sits between the plugin and
  a real backend container and injects latency, bandwidth caps, connection drops, and
  partial partitions. This is the fastest path to exercising:
  - `CacheWatchEvent` / `LeaderWatchEvent` / `ServiceWatchEvent` `Lagged` / `Reset` /
    `Closed` signals against a real connection (`cpt-cf-clst-fr-watch-lifecycle-signals`).
  - The `RestartingWatch` auto-restart combinator's backoff + retryability
    classification (`cpt-cf-clst-fr-watch-auto-restart`) — verify retryable kinds
    (`ConnectionLost`, `Timeout`, `ResourceExhausted`) reconnect and emit `Reset`,
    while non-retryable kinds (`AuthFailure`, `Shutdown`, `CapabilityNotMet`)
    propagate unchanged.
  - `ProviderErrorKind` mapping per backend (DESIGN §4.1 table).

- **Deterministic simulation testing — `turmoil` (recommended).** A simulated network
  and controllable clock with seeded, replayable multi-node scenarios. This is the
  clean way to test:
  - Leader election under partition and the `TTL + observation_lag` dual-leadership
    window formalized in DESIGN §3.3, without flaky wall-clock timing.
  - Split-brain avoidance under controlled message reorder / drop, modelling 3+ nodes
    that each hold their own SDK instance over a shared simulated backend.

  **Why `turmoil` over `madsim`:** `turmoil` is tokio-native and adopted
  incrementally — no global runtime swap, no `--cfg madsim` build, and no requirement
  that every dependency have a madsim-compatible shim. The cluster plugins depend on
  mainstream client crates (`sqlx`, `fred`, `kube`, `async-nats`) that have no madsim
  forks, so madsim's whole-system determinism would force us to either fork those or
  exclude them from simulation — a poor trade. `madsim` is the more powerful tool when
  you need fully deterministic task scheduling across an entire system; if a class of
  heisenbug surfaces that `turmoil` + `tokio::time` control cannot reproduce
  deterministically, revisit madsim for that specific backend. Until then, `turmoil`
  for multi-node scenarios plus a programmable fault-injecting mock cache backend (with
  `tokio::time` pause/advance) for the SDK default backends' renewal/TTL timing.

DST gives reproducible failure scenarios cheap enough to run per-PR; Toxiproxy tests
typically run nightly alongside the L3 containers.

## 7. Cross-Cutting Tooling

| Tool | Buys | Target |
|---|---|---|
| **`loom`** | Exhaustive interleaving check of concurrent code | SDK default backends + standalone plugin: `AtomicU64` version source, watch fan-out, TTL reaper vs. renewal races, the `shutdown_observed` `AtomicBool` (DESIGN §3.7) |
| **`proptest`** | Property / model-based testing | CAS & version invariants, scoping round-trips, `DiscoveryFilter` AND-semantics — folded into the §4 suite |
| **`cargo-mutants`** | Mutation testing — proves tests fail when logic breaks | SDK default backends, capability-validation logic (coverage % lies; mutants don't) |
| **`ui_test` (dylint)** | Lint fires on violations, no false positives | The no-remote-I/O-in-critical-section rule (`cpt-cf-clst-nfr-bounded-critical-section`) — needs positive/negative fixtures or it silently rots |
| **`miri`** | UB / leak detection | SDK unit tests; cheap insurance for any `unsafe`/FFI in plugins |

## 8. Lifecycle & Contract Gaps to Close First

Three contractual behaviors are currently only stub-tested (or not testable yet) and
should get dedicated coverage as the relevant code lands:

1. **Shutdown sequence** (DESIGN §3.13, `cpt-cf-clst-fr-shutdown-revoke`): once the
   wiring crate lands, a lifecycle integration test must assert the ordering —
   `LeaderWatchEvent::Status(Lost)` *then* `Closed(Shutdown)`; an in-flight blocking
   `lock()` waiter returns `Err(Shutdown)` (distinct from `LockTimeout`); active
   cache/SD watches receive `Closed(Shutdown)`; and **no remote release** is performed
   (resources lapse via TTL, `cpt-cf-clst-fr-shutdown-ttl-cleanup`).

2. **Dylint rule** (`cpt-cf-clst-constraint-no-remote-in-critical-section`): the
   compile-time guarantee needs `ui_test` fixtures *now* — nothing currently proves
   the lint actually triggers, so it could regress to aspirational documentation.

3. **Watch auto-restart reconnect path** (`SC-REST-001/003/004`,
   `cpt-cf-clst-fr-watch-auto-restart`): the retryable-close → backoff → resubscribe →
   `Reset` path cannot be exercised at L2 because it needs a facade-installed
   resubscribe seam that only exists once a `ClientHub` is wired up. `cluster-conformance`
   currently proves only the pass-through and non-retryable-close paths (`SC-REST-002`).
   Once the wiring crate lands, these three scenarios move from L2-blocked to L3
   integration tests.

## 9. CI Cadence

| Trigger | Runs |
|---|---|
| Every PR | L1 (unit + smoke); L2 conformance against stub/standalone; L4 DST (turmoil, fast); dylint `ui_test`; `miri` on SDK |
| Nightly | L3 testcontainers (Postgres, Redis+Sentinel, NATS, etcd); L3 K8s via envtest/kind; L4 Toxiproxy fault injection; `loom`; `cargo-mutants` |

## 10. Open Questions

**Resolved:**

| Decision | Resolution |
|---|---|
| Conformance suite crate boundary | **Standalone `cluster-conformance` crate** (§4) — avoids a feature-flag cycle through the SDK and isolates test-only dependencies. |
| DST tool | **`turmoil`** (§6) — tokio-native, incremental adoption, no madsim shim requirement for mainstream client crates. Revisit `madsim` only for a backend where `turmoil` can't reproduce a heisenbug deterministically. |
| Conformance vs. per-plugin design | **Both, mandatory** (§5) — every plugin ships conformance-suite usage *and* a per-plugin testing design that references this strategy for shared layers. |

**Still open:**

| Question | Notes |
|---|---|
| `cluster-conformance` async-runtime entry shape | Whether the suites are plain `async fn`s the caller drives, or macro-generated `#[tokio::test]` cases. Plain async fns compose better with `turmoil`/`madsim` runtimes; decide when the first suite is scaffolded. |
| K8s integration: `envtest` vs. `kind` as the per-PR default | `envtest` is faster (apiserver+etcd only) but not a full cluster; `kind`/`k3s` is fuller but slower. Likely `envtest` per-PR, `kind` nightly — confirm once the K8s plugin starts. |
| Traceability IDs for test artifacts | Whether the conformance suite and per-plugin designs get `cpt-cf-clst-*` IDs in the traceability audit, or remain referenced informally. |
