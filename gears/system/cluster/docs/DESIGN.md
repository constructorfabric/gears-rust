# Technical Design — Cluster


<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Resolution Pattern](#36-resolution-pattern)
  - [3.7 Lifecycle Pattern (Builder/Handle)](#37-lifecycle-pattern-builderhandle)
  - [3.8 Per-primitive Scoping](#38-per-primitive-scoping)
  - [3.9 Watch Event Shape](#39-watch-event-shape)
  - [3.10 Capability Validation](#310-capability-validation)
  - [3.11 SDK Default Backends](#311-sdk-default-backends)
  - [3.12 Polyfill](#312-polyfill)
  - [3.13 Interactions & Sequences](#313-interactions--sequences)
  - [3.14 Database schemas & tables](#314-database-schemas--tables)
  - [3.15 Deployment Topology](#315-deployment-topology)
- [4. Additional Context](#4-additional-context)
  - [4.1 Backend Feature Compatibility](#41-backend-feature-compatibility)
  - [4.2 Recommended Deployment Combinations](#42-recommended-deployment-combinations)
  - [4.3 Existing Code Migration](#43-existing-code-migration)
- [5. Traceability](#5-traceability)
- [6. Risks / Trade-offs](#6-risks--trade-offs)
- [7. Open Questions](#7-open-questions)

<!-- /toc -->

## 1. Architecture Overview

> **Open: backend authentication and credential wiring.** How cluster plugins (Redis, Postgres, K8s, NATS, etcd) acquire credentials for their backend connections is **not yet established** and is intentionally out of scope for this design. The shape (`secret_ref` on each backend config struct, resolved via the credstore plugin at start; K8s falling back to `kube-rs`'s in-cluster service-account / kubeconfig chain) is sketched but the concrete wiring, startup ordering, and per-backend mTLS/SASL/IAM specifics are deferred to the broader **OOP (out-of-process) deployment design**, where cluster meets the rest of the platform's credential and transport story (TLS termination, identity propagation, secret rotation). Treat any credential references below as placeholder shape, not committed contract.

### 1.1 Architectural Vision

Cluster is a platform-level system gear that provides cluster coordination and shared-state primitives to all Gears. It exposes three independent primitives — distributed cache (KV with TTL, version-based CAS, watch notifications), leader election, and distributed locks with TTL-bounded mutual exclusion — each as a versioned public-API facade struct (`ClusterCacheV1`, `LeaderElectionV1`, `DistributedLockV1`) wrapping a plugin-implemented backend trait (`ClusterCacheBackend`, `LeaderElectionBackend`, `DistributedLockBackend`). Plugins register their backend implementations in ClientHub per profile per primitive; consumers resolve via per-primitive fluent resolvers.

The architecture follows the ToolKit Gateway + Plugins pattern (same as authn-resolver, authz-resolver, credstore, tenant-resolver). An SDK crate (`cf-cluster-sdk`) defines the facade structs, backend traits, and resolver builders. The wiring — delivered in the `cf-gears-cluster` gear crate (§3.7 amendment: collapsed rather than a separate `cf-cluster`) — handles ClientHub registration, per-primitive provider dispatch, and plugin orchestration via the outbox-style builder/handle pattern. Backend-specific implementations ship as plugin crates under `plugins/`; `standalone-cluster-plugin` and `postgres-cluster-plugin` are shipped, with K8s, Redis, NATS, and etcd as follow-up changes.

The key architectural differentiator is **per-primitive backend routing as operator config**. Each profile in platform YAML maps each primitive to a specific plugin's backend impl independently. Operators can run Redis for cache and K8s Lease for leader election — all in the same profile, registered side-by-side in ClientHub under that profile's scope. There is no runtime compositor object; the wiring crate iterates the config and registers each backend independently.

The SDK also ships **default backend implementations** of leader election and distributed lock built entirely on `ClusterCacheBackend` CAS operations. This means a minimal plugin only needs to implement the cache backend trait — the SDK builds the other two on top. Native plugin backends override the defaults when a backend excels (e.g., K8s Lease for elections). Operators opt into SDK defaults by **omitting** the primitive in YAML; explicit binding always wins.

Lifecycle is owned by a parent host gear via the **outbox-style builder/handle pattern**. The wiring crate is NOT registered as its own `RunnableCapability` — it's a library exposing `ClusterWiring::builder(...).build_and_start() -> ClusterHandle`. The parent host gear's `RunnableCapability::start` calls `build_and_start()`; its `RunnableCapability::stop` calls `handle.stop()`. Plugins are nested builder/handle pairs owned by the cluster handle, NOT separate `RunnableCapability` implementors. Code-flow ordering inside the parent gear's `start` removes the need for a framework-level dependency mechanism between wiring and plugin lifecycles.

Explicit pub/sub messaging is excluded. The event broker gear provides reliable pub/sub with delivery guarantees, consumer groups, offsets, and replay. The cluster provides reactive cache notifications (watch by key or prefix) for data-change observation — "this data changed" vs "deliver this message reliably".

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| Cluster-wide shared state for gears | `ClusterCacheV1` with version-based CAS, TTL, and watch notifications |
| Worker pool coordination (event broker, schedulers) | `LeaderElectionV1` with watch-based status model and automatic renewal |
| Distributed rate limiting (OAGW) | `DistributedLockV1` with TTL and explicit async release |
| Multiple infrastructure backends per profile | Per-primitive backend routing as operator config; per-primitive ClientHub registration; no runtime compositor |
| Zero-infrastructure dev/test | SDK ships with in-process stub backends for smoke tests; production standalone plugin is a follow-up change |

#### Architecture Decision Records

| ADR | Summary |
|-----|---------|
| `cpt-cf-clst-adr-provider-compat-perf` (ADR-001) | Provider compatibility and performance analysis — per-primitive routing as operator config, per-backend characteristics, prefix-based routing, subscriber leases as cache not locks |
| `cpt-cf-clst-adr-async-boundary-no-remote-critical` (ADR-002) | Async boundary and no remote I/O in critical sections — no-op `Drop` with explicit async release, fencing tokens removed from public API, `cargo gears lint` enforcement (cluster-trait-scoped) |
| `cpt-cf-clst-adr-watch-event-lifecycle-contract` (ADR-003) | Watch event lifecycle contract for both watches — union-type `*WatchEvent { value-variant, Lagged, Reset, Closed }` instead of `Result`-based signaling, applied to cache and leader watches; lightweight key-only cache events as the contract twin of `Lagged`/`Reset` |
| `cpt-cf-clst-adr-observability-contract` (ADR-004) | Observability as a versioned naming contract — spans, metrics, log events are part of the SDK contract; cardinality rule forbids keys/names as metric labels |
| `cpt-cf-clst-adr-facade-backend-pattern` (ADR-005) | Per-primitive facade-plus-backend-trait pattern, per-primitive `*V1` versioning, no root `Cluster` trait |
| `cpt-cf-clst-adr-builder-handle-lifecycle` (ADR-006) | Outbox-style builder/handle lifecycle owned by parent host gear, no two-tier `RunnableCapability` ordering |
| `cpt-cf-clst-adr-capability-typing-and-profile-resolution` (ADR-007) | Per-primitive capability typing — `*Capability` enums replace bundled `CapabilityClass`; consequences: `ClusterProfile` typed marker, fluent resolver, capability-mismatch fails startup |
| `cpt-cf-clst-adr-leader-election-backend-safety` (ADR-009) | Per-backend correctness analysis for SDK-default leader election (and lock) under failure; constructor pair `new` (rejects `EventuallyConsistent`) + `new_allow_weak_consistency` (opt-in with warning); promotes the r2 deep-dive to decision-of-record |
| `cpt-cf-clst-adr-cache-scan-prefix-for-polyfill` (ADR-010) | Cache `scan_prefix` enumeration added to the frozen cache contract so the SDK `PollingPrefixWatch` polyfill can enumerate keys under a prefix without a native prefix-watch backend |
| `cpt-cf-clst-adr-remote-backend-seam` (ADR-011) | The process boundary is the three backend traits, with exactly one `dyn ClusterClient` per process as their factory (local winning over remote); the profile is a request parameter resolved server-side; facades bind lazily and capability validation reads the profile descriptor |
| `cpt-cf-clst-adr-store-owned-leases` (ADR-012) | Leases are fenced records in the backing store rather than session state, so no process's death ends another's lease and any replica serves any lease operation; the Postgres liveness beacon is removed and sub-TTL reclaim is traded for one lease mechanism across every profile |

#### NFR Allocation

| NFR Summary | Allocated To | Design Response | Verification Approach |
|-------------|--------------|-----------------|----------------------|
| At most one leader per election name (when bound to `Linearizable` cache) | All backends + SDK defaults | Trait contract enforces single-leader guarantee; capability validation rejects `EventuallyConsistent` cache without explicit opt-in | Multi-task contention smoke tests against `MemCacheBackend`; per-backend integration tests in plugin follow-ups |
| Bounded lock holding (no stale writers) | Consumers + architecture lint rule | Async + timeouts bound critical section; `cargo gears lint` forbids remote I/O inside `try_lock`/`release` scopes (lint scope is initially restricted to the three cluster backend traits; DB-tx enforcement is a follow-up rule extension) | Architecture lint rule check; smoke tests for lock release-on-timeout |
| No serde in contract types | SDK crate | `cargo gears lint` layer rules enforce no serde in trait definitions | `make check` (architecture lints) |
| Watch event delivery — at-most-once with per-key ordering and lifecycle signals | All backends | Union-type events (`*WatchEvent`) carry `Lagged{dropped}`, `Reset`, `Closed(err)` so consumers recover from missed events explicitly | Smoke tests across all three watches verifying each variant is observable |
| Backend trait dyn-compatibility | SDK crate | Compile-time assertions (`fn _assert_dyn_compat(_: Arc<dyn _Backend>) {}`) per trait | Build fails if dyn-compat is broken |

#### Functional Requirements Coverage

Each functional requirement from the PRD maps to the SDK surface and design section that satisfies it.

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-clst-fr-cache-storage` | `ClusterCacheV1` facade over `ClusterCacheBackend`; versioned key-value entries (§3.2, §3.3) |
| `cpt-cf-clst-fr-cache-atomic` | Version-based compare-and-set on `ClusterCacheBackend` (§2.1 CAS-as-universal, §3.3) |
| `cpt-cf-clst-fr-cache-ttl` | TTL-bounded entries with backend-side expiry (§3.3 `ClusterCacheV1`) |
| `cpt-cf-clst-fr-cache-watch` | Key- and prefix-scoped `CacheWatchEvent` stream (§3.9 Watch Event Shape) |
| `cpt-cf-clst-fr-leader-elect` | `LeaderElectionV1` with single-leader guarantee bound to `Linearizable` cache (§3.3, §3.10) |
| `cpt-cf-clst-fr-leader-config` | Configurable lease/renew timing on the leader resolver (§3.3, §3.7) |
| `cpt-cf-clst-fr-leader-observability` | Watch-based `LeaderWatchEvent` status model (§3.9) |
| `cpt-cf-clst-fr-leader-resign` | Graceful step-down on handle drop / shutdown sequence (§3.13 Shutdown Sequence) |
| `cpt-cf-clst-fr-leader-advisory` | Advisory semantics documented on the facade contract (§3.3, §4.1) |
| `cpt-cf-clst-fr-lock-acquire` | `DistributedLockV1` acquire-or-fail and acquire-with-wait (§3.3) |
| `cpt-cf-clst-fr-lock-release` | Explicit async release with TTL safety net; no-op `Drop` (§2.2 no-remote-in-critical-section, §3.3) |
| `cpt-cf-clst-fr-lock-no-remote` | Architecture lint rule forbidding remote I/O inside lock critical sections (§2.2, §3.10) |
| `cpt-cf-clst-fr-routing-cache-only-plugin` | SDK default backends derive all three primitives from `ClusterCacheBackend` (§2.1, §3.11) |
| `cpt-cf-clst-fr-validation-typed-profile` | `ClusterProfile` typed marker resolved via the fluent resolver (§3.6 Resolution Pattern, ADR-007) |
| `cpt-cf-clst-fr-validation-capability-declarations` | Per-primitive `*Capability` requirement enums on the resolver (§3.10 Capability Validation) |
| `cpt-cf-clst-fr-validation-honest-declaration` | Plugin-declared `*Features` characteristic structs (§3.10) |
| `cpt-cf-clst-fr-validation-startup-fail` | Capability mismatch fails resolution at startup, not production (§3.10) |
| `cpt-cf-clst-fr-watch-lifecycle-signals` | Union `*WatchEvent` carrying `Lagged`/`Reset`/`Closed` (§3.9, ADR-003) |
| `cpt-cf-clst-fr-watch-auto-restart` | SDK auto-restart combinator (§3.9 Watch Event Shape) / `PollingPrefixWatch` (§3.12 Polyfill) |
| `cpt-cf-clst-fr-namespacing-scoped` | Per-primitive `scoped()` sub-namespacing helpers (§3.8 Per-primitive Scoping) |
| `cpt-cf-clst-fr-routing-omit-default` | `ClusterHandle` wiring auto-fills unbound primitives with SDK defaults over the cache (§3.7 Lifecycle, §3.11) |
| `cpt-cf-clst-fr-lifecycle-owner` | Single owner: the cluster gear crate's `ClusterHandle` start/stop sequence (§3.7, §3.13) |
| `cpt-cf-clst-fr-shutdown-revoke` | `ClusterHandle::stop` revokes leadership (`Status(Lost)` then `Closed(Shutdown)`) before completing (§3.13 Shutdown Sequence) |
| `cpt-cf-clst-fr-shutdown-ttl-cleanup` | `ClusterHandle::stop` performs no remote cleanup; resources lapse via backend TTL (§3.13 Shutdown Sequence) |

#### Non-Functional Requirements Coverage

Each non-functional requirement from the PRD maps to its design response and verification approach (see §1.2 NFR Allocation for the cross-cutting allocation view).

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-clst-nfr-leader-guarantee` | Single-leader contract bound to `Linearizable` cache; weak-consistency requires explicit opt-in (§3.10, ADR-009) |
| `cpt-cf-clst-nfr-bounded-critical-section` | Async + timeouts plus architecture lint no-remote-I/O rule bound the critical section (§2.2, §3.10) |
| `cpt-cf-clst-nfr-watch-delivery` | At-most-once, per-key-ordered delivery with explicit `Lagged`/`Reset`/`Closed` recovery (§3.9, ADR-003) |
| `cpt-cf-clst-nfr-observability` | Versioned spans/metrics/log-event naming contract; cardinality rule (§3.10, ADR-004) |
| `cpt-cf-clst-nfr-capability-validation` | Capability requirements validated at resolution/startup (§3.10) |
| `cpt-cf-clst-nfr-cross-backend-stability` | Backend trait contract gives stable cross-backend behavior; per-backend smoke/integration tests (§3.2, §4.1) |
| `cpt-cf-clst-nfr-error-retryability` | Programmatic error classification exposes retryability on the facade error types (§3.3) |
| `cpt-cf-clst-nfr-plugin-stability` | Per-primitive `*V1` versioning isolates plugin contract changes (§2.1 facade-plus-backend-trait, ADR-005) |

### 1.3 Architecture Layers

```
┌─────────────────────────────────────────────────────────────────┐
│            Consumers (Event Broker, OAGW, gears)                │
│  Hold ClusterCacheV1 / LeaderElectionV1 / DistributedLockV1 /   │
│  facades. Define ClusterProfile markers.                        │
├─────────────────────────────────────────────────────────────────┤
│  Parent host gear (this change: out of scope; future)           │
│  Owns ClusterHandle from RunnableCapability::start/stop.        │
├─────────────────────────────────────────────────────────────────┤
│  cf-cluster-sdk (THIS CHANGE)                                   │
│  Facade structs, backend traits, resolver builders, profile     │
│  marker, *Capability and *Features enums/structs, SDK default   │
│  backends, scoping helpers, polyfill, shared types.             │
├─────────────────────────────────────────────────────────────────┤
│  cf-gears-cluster wiring (delivered)                            │
│  ClusterWiring::builder().build_and_start() -> ClusterHandle.   │
│  Reads operator YAML; instantiates plugins; registers each      │
│  Arc<dyn _Backend> per profile per primitive in ClientHub.      │
├─────────────────────────────────────────────────────────────────┤
│  Plugin crates (standalone + postgres shipped)                  │
│  ┌────────────────┐ ┌──────────────┐ ┌────────────────┐         │
│  │ standalone     │ │ postgres     │ │ k8s            │  ...    │
│  │ (in-process)   │ │ (CRD+L/N)    │ │ (Lease+CRD)    │         │
│  └────────────────┘ └──────────────┘ └────────────────┘         │
│  Each plugin: builder/handle pair (outbox pattern).             │
├─────────────────────────────────────────────────────────────────┤
│  External (out of all change scopes)                            │
│  PostgreSQL, K8s API, Redis, NATS, etcd                         │
└─────────────────────────────────────────────────────────────────┘
```

**The `grpc-client` feature layer, which the diagram above does not show.** The stack above is Profile 1: every
layer is linked into one process and the arrows are function calls. Profile 3 cuts it between the SDK and the
wiring, and the cut is a **Cargo feature on `cf-gears-cluster-sdk`**, not a new layer:

```
  Consumers                        (unchanged source, both profiles)
      |
  cf-gears-cluster-sdk             facades + backend traits + resolvers
      |                            ClusterClient: unfeatured, three sync factory
      |                            methods + async descriptor()
      +-- (feature off) ---------> LocalClusterClient  -> the real backend Arc
      |                            registered by the cluster gear's start
      |
      +-- (feature "grpc-client") -> RemoteClusterClient -> tonic stubs -> cluster pod
                                     Remote{Cache,Lock,Leader}Backend
```

Three properties of that seam matter more than the picture. The **boundary is the three backend traits**, so a
consumer names a facade and never a `Remote*Backend` (invariant I4) — the remote types are `pub` inside a private
module and reachable only as `Arc<dyn _Backend>`. **Exactly one `Arc<dyn ClusterClient>` is registered per
process**, local winning over remote at *registration* time, so the decision is made once by what the binary
linked rather than per call. And **Profile 1 links no cluster transport**: with the feature off, the SDK has no
direct `tonic` edge at all, which is why `ClusterClient` itself is unfeatured — a feature-gated trait would have
made the seam visible to consumers. The gear crate is the exception and enables `grpc-client` unconditionally,
because it *serves* the contract; see §3.15.1 for what that costs an embedding process.

| Layer | Responsibility | Technology |
|-------|---------------|------------|
| SDK | Public-API facade structs (`*V1`), backend traits (`*Backend`), per-primitive resolver builders, `ClusterProfile` marker trait, `*Capability` requirement enums, `*Features` characteristic structs, shared types, per-primitive `scoped()` helpers, `PollingPrefixWatch` polyfill, `register_*_backend` / `deregister_*_backend` helpers | Rust crate (`cf-cluster-sdk`) |
| Cluster gear | SDK default backend implementations (`CasBasedLeaderElectionBackend`, `CasBasedDistributedLockBackend`), `ShutdownRevoke` seam, wiring lifecycle (`ClusterHandle`) | Rust crate (`cf-gears-cluster`) |
| Wiring | Operator YAML parsing, per-primitive provider dispatch (`ProviderRegistry` → `ClusterWiring::from_config`), plugin orchestration, per-primitive ClientHub registration, builder/handle exposed as library API. Each of `leader_election` / `lock` resolves against its own provider registry independently of the `cache` binding, so one profile can mix backends (`cpt-cf-clst-fr-routing-per-primitive`); an omitted primitive falls back to the SDK default over that profile's cache (`cpt-cf-clst-fr-routing-omit-default`) | Rust crate (`cf-gears-cluster`) — collapsed into the gear crate, see the §3.7 amendment |
| Plugins | Backend-specific primitive implementations exposed as builder/handle pairs, plus the `Cluster*Provider` impls the wiring dispatches on. A plugin may ship a cache provider only, a native non-cache provider only, or both (the Postgres plugin ships a cache provider and a standalone lock provider) | Rust crates per backend (`standalone-cluster-plugin`, `postgres-cluster-plugin` today; K8s, Redis, NATS, etcd follow-up) |
| External | Persistence, coordination, cluster state | PostgreSQL, K8s API server, Redis, NATS, etcd |

## 2. Principles & Constraints

### 2.1 Design Principles

#### Cache CAS as Universal Building Block

- [x] `p1` - **ID**: `cpt-cf-clst-principle-cas-universal`

`ClusterCacheBackend` with version-based CAS is the foundational primitive. Leader election and distributed locks can both be built on top of cache CAS + watch. The SDK ships default backend implementations of both using only cache operations. This means a minimal plugin needs to implement only `ClusterCacheBackend` to get all three primitives (the wiring crate auto-wraps the cache backend in the SDK defaults when a primitive is omitted in operator config). Native overrides improve performance but are never required.

#### Per-primitive Routing as Operator Config

- [x] `p1` - **ID**: `cpt-cf-clst-principle-per-primitive-routing`

Each primitive routes independently to the best backend for the job. The wiring crate's `ClusterWiring::builder(...).build_and_start()` reads each profile's per-primitive config and registers the corresponding `Arc<dyn _Backend>` in ClientHub under the profile scope. Mixed backends within one profile (Redis cache + K8s Lease for leader election) are the common case, supported directly by the per-primitive registration model. There is no runtime compositor object — registration is per-primitive and the wiring crate is a thin iterator over operator config.

#### Facade-plus-Backend-Trait Pattern

- [x] `p1` - **ID**: `cpt-cf-clst-principle-facade-plus-backend-trait`

There is no root `Cluster` trait. Each primitive is split into a public-API facade struct (`ClusterCacheV1`) and a plugin-facing backend trait (`ClusterCacheBackend`). Consumers hold the facade — a cheap-clone Arc-backed struct with inherent async methods. Plugins implement the backend trait. This keeps consumers off the `dyn` surface, lets the public API evolve independently of the plugin contract, and gives consumers a clean fluent resolver entry point: `ClusterCacheV1::resolver(hub).profile(P).require(...).resolve()`. Per-primitive versioning (`*V1`, `*V2`) allows incompatible primitive changes to coexist via separate `TypeKey`/ClientHub registration.

#### Lightweight Notifications, Not Messaging

- [x] `p1` - **ID**: `cpt-cf-clst-principle-lightweight-notifications`

Cache watch events carry only the key and event type (`Changed`, `Deleted`, `Expired`) — no value payload. Consumers call `cache.get(key)` for the current value. This avoids stale-value issues, maps cleanly to all backends (Redis keyspace notifications carry no value, Postgres NOTIFY has 8KB limit), and keeps events fixed-size. Reliable messaging belongs in the event broker.

#### Version-Based Optimistic Concurrency

- [x] `p1` - **ID**: `cpt-cf-clst-principle-version-based-cas`

`compare_and_swap` takes an `expected_version: u64` obtained from a prior `get()`, not an expected byte value. `get()` returns `CacheEntry { value, version }`. This maps natively to all backends: `resourceVersion` (K8s), `revision` (NATS), `mod_revision` (etcd), `BIGSERIAL` (Postgres), Lua counter (Redis), `AtomicU64` (in-process). Value-based CAS would require racy get-compare-put loops on revision-based backends.

#### Watch Union Shape Across All Three Watches

- [x] `p1` - **ID**: `cpt-cf-clst-principle-watch-union-shape`

Both watch event types (`CacheWatchEvent`, `LeaderWatchEvent`) follow the same union shape: `{value-variant, Lagged{dropped}, Reset, Closed(err)}`. Infallible at the type level — there is no `Result`-returning `changed()` method on any watch. Terminal errors arrive via `Closed(err)`. Transient backend errors (`ConnectionLost`, `Timeout`, `ResourceExhausted`) are retried internally by the watch's background task and do not surface as events. ADR-003 captures the rationale and applies to both watches.

### 2.2 Constraints

#### No Serde in Contract Types

- [x] `p1` - **ID**: `cpt-cf-clst-constraint-no-serde`

The `cf-cluster-sdk` crate MUST NOT depend on serde. Serialization concerns belong in plugin implementations. Enforced by architecture lints in the workspace.

#### No Remote I/O in Cluster Critical Sections

- [x] `p1` - **ID**: `cpt-cf-clst-constraint-no-remote-in-critical-section`

Code protected by a `LockGuard` MUST NOT make additional remote calls. Remote effects MUST occur before `try_lock` or after `release`, never between them. Together with async + timeouts, this eliminates the Kleppmann fencing scenario at the architectural level. Enforced by a workspace architecture lint rule scoped to the three cluster backend traits within `try_lock`/`release` scopes; DB-tx enforcement is a follow-up rule extension once the wiring crate and consumer migrations land. See ADR-002.

#### Backend Trait Dyn-Compatibility

- [x] `p1` - **ID**: `cpt-cf-clst-constraint-dyn-compat`

All three backend traits MUST be dyn-compatible. The SDK includes compile-time assertions per trait so any future change that breaks dyn-compatibility fails the build. No `Self: Sized` bounds on async trait methods; no GATs.

## 3. Technical Architecture

### 3.1 Domain Model

| Entity | Description |
|--------|-------------|
| `ClusterCacheV1` | Public-API facade struct; cheap-clone (Arc-backed) wrapper over `Arc<dyn ClusterCacheBackend>`. Inherent async methods: `get`, `put`, `delete`, `contains`, `put_if_absent`, `compare_and_swap`, `watch`, `watch_prefix`. Inherent sync: `consistency()`, `features()`, `resolver(hub)`, `scoped(prefix)`. |
| `LeaderElectionV1` | Public-API facade struct over `Arc<dyn LeaderElectionBackend>`. Inherent async: `elect`, `elect_with_config`. Inherent sync: `resolver(hub)`, `scoped(prefix)`. |
| `DistributedLockV1` | Public-API facade struct over `Arc<dyn DistributedLockBackend>`. Inherent async: `try_lock`, `lock`. Inherent sync: `resolver(hub)`, `scoped(prefix)`. |
| `ClusterCacheBackend` | Plugin-facing async trait. Methods: `consistency()`, `features()`, `get`, `put`, `delete`, `contains`, `put_if_absent`, `compare_and_swap`, `compare_and_delete`, `watch`, `watch_prefix`. `compare_and_delete` is backend-only — not surfaced on `ClusterCacheV1`. |
| `LeaderElectionBackend` | Plugin-facing async trait. Methods: `features() -> LeaderElectionFeatures`, `elect`, `elect_with_config`. |
| `DistributedLockBackend` | Plugin-facing async trait. Methods: `features() -> LockFeatures`, `try_lock`, `lock`. |
| `ClusterProfile` | Marker trait: `pub trait ClusterProfile: 'static + Send + Sync + Copy { const NAME: &'static str; }`. Consumer crates impl this on a ZST struct once per profile; the `NAME` is the only place the profile string lives on the consumer side. |
| `CacheCapability` | `#[non_exhaustive] enum { Linearizable, PrefixWatch }`. Per-primitive requirement enum used at resolver call sites. |
| `LeaderElectionCapability` | `#[non_exhaustive] enum { Linearizable }`. |
| `LockCapability` | `#[non_exhaustive] enum { Linearizable }`. |
| `CacheFeatures` | `#[non_exhaustive] struct { prefix_watch: bool, ... }`. Backend declares native capability availability. |
| `LeaderElectionFeatures` | `#[non_exhaustive] struct { linearizable: bool, ... }`. |
| `LockFeatures` | `#[non_exhaustive] struct { linearizable: bool, ... }`. |
| `*ResolverBuilder<'a>` | Per-primitive fluent builder: `.profile<P: ClusterProfile>(_: P)`, `.require(cap: *Capability)`, `.resolve() -> Result<*V1, ClusterError>`. |
| `CacheConsistency` | `enum { Linearizable, EventuallyConsistent }`. Cache-only — leader election and lock backends use `*Features { linearizable: bool }` instead. |
| `CacheEntry` | Versioned key-value pair: `{ value: Vec<u8>, version: u64 }`. Version is opaque, monotonically increasing per key, starting at 1. Version 0 is reserved as sentinel. **The monotonicity holds only while the key exists**: a `delete`, or a TTL reap, removes the counter with the key, and the next write of that key starts again at 1 (measured: `standalone-cluster-plugin/src/cache.rs`). It is therefore safe as a CAS predicate — which compares versions of a key it just read — and **unsafe as a durable fence**, because "version 1" does not identify one incarnation of a key. Anything needing a value that outlives its key must carry its own counter in the value: that is exactly what the store-owned lease record does with `fence`, and why `fence_retention` keeps the record alive past the lease (DESIGN-DEPLOYABLE-GEAR §5.8.1). |
| `CacheEvent` | Lightweight notification: `Changed { key }`, `Deleted { key }`, `Expired { key }`. No payload — consumer calls `get(key)` for current value. |
| `CacheWatchEvent` | Watch union: `Event(CacheEvent)`, `Lagged { dropped: u64 }`, `Reset`, `Closed(ClusterError)`. Per ADR-003. |
| `CacheWatch` | Async receiver yielding `CacheWatchEvent` items. Dropping unsubscribes. Per-key ordering guaranteed; no cross-key ordering. |
| `LeaderStatus` | `enum { Leader, Follower, Lost }`. `Lost` is a transient observable transition — the watch auto-reenrolls and the next `Status` event resolves to `Leader` or `Follower`. Not terminal. |
| `LeaderWatchEvent` | Watch union: `Status(LeaderStatus)`, `Lagged { dropped: u64 }`, `Reset`, `Closed(ClusterError)`. |
| `LeaderWatch` | Handle into an ongoing election. `async fn changed() -> LeaderWatchEvent`; `fn status() -> LeaderStatus`; `fn is_leader() -> bool`; `async fn resign(self) -> Result<()>`. `Drop` is a no-op (no I/O in `Drop`). |
| `ElectionConfig` | `{ ttl: Duration (default 30s), max_missed_renewals: u8 (default 2) }`. Constructor `new(ttl, max_missed_renewals)` validates both > 0. Derived: `renewal_interval() = ttl / (max_missed_renewals + 1)`. |
| `LockGuard` | Lock handle. `async fn renew(new_ttl)`, `async fn release(self)`. `Drop` is a no-op (TTL is the safety net; no I/O in `Drop`). |
| `RetryPolicy` | Combinator config: `initial_backoff: Duration`, `max_backoff: Duration`, `jitter_factor: f32` (0.0–1.0), `max_retries: Option<u32>` (None = retry forever). Constructor `default()` returns exponential backoff `1s → 30s`, full jitter (`jitter_factor: 1.0`), no retry cap. |
| `RestartingWatch<W>` | SDK combinator wrapping a base `*Watch`. Implemented for `W: CacheWatch | LeaderWatch`. Consumes `Closed(retryable)` internally per the bound `RetryPolicy`, synthesizes `Reset` to the consumer on each successful resubscribe, propagates `Closed(non-retryable)` and `Closed(Shutdown)` to the consumer unchanged. Constructed via `*Watch::auto_restart(policy)`. Retryability is read from `ProviderErrorKind`: `ConnectionLost`, `Timeout`, `ResourceExhausted` are retryable; `AuthFailure`, `Other` are not. `ClusterError::Shutdown`, `CapabilityNotMet`, and the lock/leader-specific terminal variants are also not retryable. |
| `ClusterError` | Unified error enum. Variants: `InvalidName { name, reason }`, `InvalidConfig { reason }`, `LockContended { name }`, `LockTimeout { name, waited }`, `LockExpired { name }`, `CasConflict { key, current: Option<CacheEntry> }`, `Unsupported { feature: &'static str }`, `ProfileNotSpecified`, `ProfileNotBound { profile: &'static str }`, `CapabilityNotMet { primitive: &'static str, capability: &'static str, provider: &'static str }`, `Shutdown`, `Provider { kind: ProviderErrorKind, message: String }`. `ClusterError` derives `Clone` so it can ride the watch-union `Closed(_)` signal to multiple watchers; the provider error chain is therefore flattened into `message` rather than carried as a non-`Clone` boxed `source`. **No `NotStarted` variant** — pre-resolution access surfaces as `ProfileNotBound` (the resolver enforces presence at consumer construction time, so resolved facades cannot observe a "not started" state). |
| `ProviderErrorKind` | `enum { ConnectionLost, Timeout, AuthFailure, ResourceExhausted, Other }`. Programmatic retryability classification. |
| `ScopedCacheBackend` (and three siblings) | Internal SDK wrapper struct implementing the corresponding `*Backend` trait by delegating to an inner `Arc<dyn _Backend>` with prefix translation. Returned by `*V1::scoped(prefix)`. |
| `PollingPrefixWatch` | SDK polyfill: synthesizes `watch_prefix` behavior on backends declaring `features().prefix_watch == false` by periodically listing the prefix and emitting `CacheWatchEvent::Event` diffs (Changed/Deleted). Explicit opt-in; doc comments describe the cost (N gets per interval). |
| `ClusterWiring` (follow-up) | Wiring crate's builder entry point. `ClusterWiring::builder(config, hub).build_and_start() -> ClusterHandle`. |
| `ClusterHandle` (follow-up) | Wiring crate's lifecycle handle. `handle.stop() -> ()` deregisters all backends and stops nested plugin handles. Owned by the parent host gear. |

**Relationships**:
- A `CacheEntry` belongs to exactly one key. Each `put` increments the version.
- A `LeaderWatch` belongs to one election name. At most one `LeaderWatch` across all nodes observes `Leader` (advisory — see staleness bound in §3.3).
- A `LockGuard` belongs to one lock name. Mutual exclusion is bounded by TTL; explicit `release().await` is the idiomatic release path. Consumers MUST NOT make remote I/O calls inside the critical section (see §2 Constraints).
- A `ClusterCacheV1` is `Arc<dyn ClusterCacheBackend>`-backed; cloning the facade is a single atomic increment.

### 3.2 Component Model

```
┌────────────────────────────────────────────────────────────────────┐
│                          cf-cluster-sdk                            │
│  ┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐    │
│  │ ClusterCacheV1   │ │LeaderElectionV1  │ │ DistributedLockV1│    │
│  │ + CacheBackend   │ │ + LEBackend      │ │ + LockBackend    │    │
│  └──────────────────┘ └──────────────────┘ └──────────────────┘    │
│  ┌──────────────────┐ ┌─────────────────────────────────────────┐  │
│  │                  │ │ Resolver builders (one per primitive)   │  │
│  │ + SDBackend      │ │ ClusterProfile marker, *Capability,     │  │
│  └──────────────────┘ │ *Features, ClusterError, shared types   │  │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │ Per-primitive Scoped*Backend wrappers                       │   │
│  │ PollingPrefixWatch polyfill                                 │   │
│  │ register_*_backend / deregister_*_backend helpers           │   │
│  └─────────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────┘
                                   ▲
                                   │ Arc<dyn _Backend> registered per primitive per profile
                                   │
┌────────────────────────────────────────────────────────────────────┐
│                       cf-cluster (follow-up change)                │
│  ClusterWiring::builder(config, hub).build_and_start() →           │
│       ClusterHandle (owns nested plugin handles)                   │
│  Reads operator YAML; iterates profile×primitive matrix;           │
│  starts each plugin's builder; registers each backend in ClientHub │
└────────────────────────────────────────────────────────────────────┘
                                   ▲
                                   │ owned by parent host gear's RunnableCapability::start
                                   │
┌────────────────────────────────────────────────────────────────────┐
│             Plugin crates (each follow-up change)                  │
│  cf-standalone-cluster-plugin / cf-postgres-cluster-plugin /       │
│  cf-k8s-cluster-plugin / cf-cluster-redis / cf-cluster-nats / ...  │
│  Each: builder/handle pair (outbox pattern)                        │
└────────────────────────────────────────────────────────────────────┘
```

**Two components the diagram predates.** `cf-gears-cluster` is no longer only a wiring library owned by a parent
host gear: it is the cluster **gear** (`name = "cluster"`, capabilities `stateful, system, grpc, rest`), it serves
the four coordination services over gRPC from `src/api/grpc/`, and it ships a `cluster-oop` binary. The
builder/handle library described below still exists and is still embeddable — that half is unchanged — but the
same crate now also owns the profile registry, the composite readiness check, the local client and the deployable
entry point. And beside the SDK sits its `grpc-client` half (§1.3): `RemoteClusterClient` plus the three
`Remote*Backend` handles, compiled only when the feature is on, which is what lets a Profile 3 consumer link the
SDK and no plugins at all.

#### cf-cluster-sdk (this change)

- [x] `p1` - **ID**: `cpt-cf-clst-component-sdk`

Per-primitive public-API facade structs, plugin-facing backend traits, resolver builders, profile marker, capability and features types, shared types, scoping wrappers, polyfill, registration/deregistration helpers, name validation utilities. Zero external dependencies beyond `tokio`, `tokio_util`, `async-trait`, and platform crates (`toolkit`, `gts`, `types-registry-sdk`). Default backend implementations (`CasBasedLeaderElectionBackend`, `CasBasedDistributedLockBackend`) live in the cluster gear crate, not here.

#### cf-cluster wiring (follow-up change)

- [ ] `p1` - **ID**: `cpt-cf-clst-component-wiring`

Wiring library. Implements no `RunnableCapability` itself. Exposes `ClusterWiring::builder(config, hub).build_and_start() -> ClusterHandle`. The handle's `stop()` is the single shutdown entry point. A parent host gear owns the handle from inside its own `RunnableCapability::start`/`stop`.

#### Plugin crates (follow-up changes)

- [ ] `p1` - **ID**: `cpt-cf-clst-component-plugins`

Each plugin (Postgres, K8s, Redis, NATS, etcd, standalone) exposes a builder/handle pair (`MyCachePlugin::builder(...).build_and_start() -> MyCacheHandle`), with the handle's `stop()` cancelling internal `CancellationToken`s and joining background tasks (TTL reapers, renewal loops, watch fan-out). The wiring crate composes these into the cluster handle.

### 3.3 API Contracts

#### ClusterCacheV1 — Cache primitive

| Method | Signature | Contract |
|--------|-----------|----------|
| `resolver` | `fn resolver(hub: &ClientHub) -> CacheResolverBuilder<'_>` | Static entry point. Returns a fluent builder. |
| `consistency` | `fn consistency(&self) -> CacheConsistency` | Surfaces backend's declared consistency class. |
| `features` | `fn features(&self) -> CacheFeatures` | Surfaces backend's native capability flags. |
| `scoped` | `fn scoped(&self, prefix: &str) -> ClusterCacheV1` | Returns a scoped wrapper that prepends `prefix + "/"` on the write path and strips it on the read path. Validates prefix per the cluster name rule. |
| `get` | `async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError>` | Returns versioned entry or `None`. Never errors for missing keys. |
| `put` | `async fn put(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), ClusterError>` | Stores value, increments version. Emits `Changed`. Overwrites if exists. |
| `delete` | `async fn delete(&self, key: &str) -> Result<bool, ClusterError>` | Removes entry. Emits `Deleted` if existed. Return MAY be `true` unconditionally if backend cannot determine prior existence. |
| `contains` | `async fn contains(&self, key: &str) -> Result<bool, ClusterError>` | Existence check. MAY be `get(key).is_some()`. |
| `put_if_absent` | `async fn put_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<Option<CacheEntry>, ClusterError>` | Atomic. `Some(entry)` if created, `None` if key existed. Emits `Changed` on creation only. |
| `compare_and_swap` | `async fn compare_and_swap(&self, key: &str, expected_version: u64, new_value: &[u8], ttl: Option<Duration>) -> Result<CacheEntry, ClusterError>` | Atomic version-based CAS. Emits `Changed` on success. `CasConflict { key, current }` on mismatch — `current` SHOULD contain the entry if cheaply obtainable. |
| `watch` | `async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError>` | Yields `CacheWatchEvent` for exact key. Drop unsubscribes. |
| `watch_prefix` | `async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError>` | Yields `CacheWatchEvent` for matching keys. Backends declaring `features().prefix_watch == false` return `Err(Unsupported { feature: "prefix_watch" })`. Callers may polyfill via `PollingPrefixWatch`. |
| `CacheWatch::auto_restart` | `fn auto_restart(self, policy: RetryPolicy) -> RestartingWatch<CacheWatch>` | Wraps the watch with the SDK auto-restart combinator. See §3.9 for retryability classification and `RetryPolicy` defaults. `LeaderWatch::auto_restart` follows the same shape. |

> **Backend-trait-only — `compare_and_delete`.** `ClusterCacheBackend` additionally declares `async fn compare_and_delete(&self, key: &str, expected_value: &[u8]) -> Result<bool, ClusterError>`: an atomic value-guarded delete that removes `key` only if its current value equals `expected_value`. A value mismatch or an absent key returns `Ok(false)`, never an error. It is deliberately **not** exposed on `ClusterCacheV1` — the public CAS contract is version-based (`compare_and_swap`), while this is the value/owner-token-guarded counterpart used internally by SDK-default coordination backends (e.g. the leader elector's guarded release, which must survive a key's version resetting to 1 on delete+recreate, where a version guard would alias a successor's fresh claim). The trait's default impl is a best-effort, non-atomic `get`-then-`delete`; backends with an atomic store override it for a genuine compare-and-delete.

#### LeaderElectionV1 — Leader election primitive

| Method | Signature | Contract |
|--------|-----------|----------|
| `resolver` | `fn resolver(hub: &ClientHub) -> LeaderElectionResolverBuilder<'_>` | Static entry point. |
| `scoped` | `fn scoped(&self, prefix: &str) -> LeaderElectionV1` | Scopes election names. |
| `elect` | `async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError>` | Join election. Auto-renews. `LeaderWatch` auto-reenrolls on `Status(Lost)`. |
| `elect_with_config` | `async fn elect_with_config(&self, name: &str, config: ElectionConfig) -> Result<LeaderWatch, ClusterError>` | Same with custom timing. |
| `LeaderWatch::changed` | `async fn changed(&mut self) -> LeaderWatchEvent` | Next watch event (`Status` / `Lagged` / `Reset` / `Closed`). Infallible at type level per ADR-003. Transient backend errors retried internally. Terminal errors arrive via `Closed(err)`. |
| `LeaderWatch::status` | `fn status(&self) -> LeaderStatus` | Cached snapshot from background task. Synchronous, no I/O. **Advisory** — see staleness bound. |
| `LeaderWatch::is_leader` | `fn is_leader(&self) -> bool` | `matches!(status(), Leader)`. Advisory — do NOT use for correctness-critical mutual exclusion. |
| `LeaderWatch::resign` | `async fn resign(self) -> Result<(), ClusterError>` | Explicit step-down. Triggers immediate re-election. |

**Staleness bound**: `is_leader() == true` at time T does NOT guarantee this node holds leadership at time T on the backend. The background task's state lags by up to one renewal interval plus a provider round-trip in steady state, and up to a full TTL under partition.

**Worst-case window with default config** (`ttl=30s`, `max_missed_renewals=2`, derived `renewal_interval=10s`): under network partition, renewal attempts fail at T+10s, T+20s, and T+30s; the third consecutive failure triggers `LeaderWatchEvent::Status(Lost)` emission. The backend revokes the lease at T+30s, after which a successor's `put_if_absent` may succeed. The consumer-perceived dual-leadership window is `TTL + observation_lag`, where `observation_lag` is the time between renewal-failure emission and the consumer's code reaching a watch-polling await point. A consumer with a 1s iteration cycle observes the transition ~30s after partition begins; one with a 60s synchronous compute block ~90s. Operators tune `ttl` and `max_missed_renewals` against this trade-off: shorter TTL shortens the window at the cost of more renewal traffic and lower tolerance for transient network jitter. Pattern C below (lock + CAS) eliminates the dual-write effect at the resource level regardless of window size.

**Profile 3 widens the bound by one transport hop, and does not change its shape.** With cluster deployed as its
own pod, `status()` is still a synchronous read of a cached snapshot — the remote handle keeps a local cache fed
by the event stream, exactly as the in-process watch keeps one fed by its channel — so the *cost* of the call is
unchanged. What changes is how the snapshot got there: leadership transitions are derived client-side from
`renew` results and a re-`join` on the renewal cadence (the server announces no leadership; see
DESIGN-DEPLOYABLE-GEAR.md §6.6), and each of those is now an RPC. So `observation_lag` gains one round trip in
steady state, and under partition the client's renew fails against an unreachable *cluster pod* rather than an
unreachable *backend* — which produces the same `Status(Lost)` after the same `max_missed_renewals`, because
renewal remains client-driven precisely so that it stays the liveness proxy (invariant I8). The worst-case window
is therefore `TTL + observation_lag + one_rpc`, and the three consumer patterns below apply unchanged: a
consumer that needs mutual exclusion still gets it from `try_lock` or a CAS failing, not from a timing argument.

Three consumer patterns are available, ordered by tolerance for transient dual-leadership:

- **Tolerant work — `is_leader()` gate, short jobs.** For workloads where brief dual-execution is acceptable or recoverable (idempotent rebalancing, periodic cleanup, log compaction, leader-coordinated metrics emission): gate each iteration on the cached `is_leader()` snapshot and bound the iteration's duration to a small fraction of the TTL. Optional: app-level guard (e.g., a row lock in the consumer's own database) on the actual write.
- **Reactive work — `changed()` + cancellation token.** For workloads where dual-execution should end as soon as leadership transitions: subscribe to `LeaderWatch::changed().await`, hold a `CancellationToken` per leader-only task, fire the token on `Status(Lost)`, and structure the task to observe cancellation at every await point. This pattern reduces the dual-leader window relative to the tolerant pattern (reactive vs. cached) but does not eliminate it: the window between backend lease revocation and the consumer's cancel-observation is bounded by `renewal_lag + consumer_poll_lag + cancellation_propagation`, never zero.
- **Mutually exclusive work — `DistributedLockV1` + cache CAS.** For workloads where two simultaneous writers would corrupt state: combine the reactive pattern with either (a) `DistributedLockV1::try_lock` around the write, or (b) `ClusterCacheV1::compare_and_swap` with `expected_version` drawn from a prior `get` on the protected key. A `LockContended`/`LockExpired` from (a) or a `CasConflict` from (b) is the authoritative "you are no longer the writer" signal — closes the residual window from the reactive pattern by failing the actual write rather than relying on cancellation timing.

#### DistributedLockV1 — Distributed lock primitive

| Method | Signature | Contract |
|--------|-----------|----------|
| `resolver` | `fn resolver(hub: &ClientHub) -> LockResolverBuilder<'_>` | Static entry point. |
| `scoped` | `fn scoped(&self, prefix: &str) -> DistributedLockV1` | Scopes lock names. |
| `try_lock` | `async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError>` | Non-blocking. `LockContended { name }` if held. |
| `lock` | `async fn lock(&self, name: &str, ttl: Duration, timeout: Duration) -> Result<LockGuard, ClusterError>` | Blocking up to `timeout`. `LockTimeout { name, waited }` if not acquired. |
| `LockGuard::renew` | `async fn renew(&self, new_ttl: Duration) -> Result<(), ClusterError>` | Renews the lease (resets the TTL to `new_ttl` from now; does not add to the time left). `LockExpired { name }` if TTL elapsed. |
| `LockGuard::release` | `async fn release(self) -> Result<(), ClusterError>` | Explicit release. Consumers MUST call this. `Drop` is a no-op (no I/O in `Drop`). |

**Critical-section rule** (see §2 Constraints, ADR-002): Consumers MUST NOT make remote I/O calls inside the critical section between `try_lock` / `lock` and `release`. No fencing tokens — the no-remote-in-critical-section rule eliminates the stale-writer scenario fencing tokens protect against.

### 3.4 Internal Dependencies

| Dependency | Direction | Purpose |
|-----------|-----------|---------|
| `toolkit` | SDK → toolkit | GTS registration, ClientHub wiring |
| `gts` / `gts-macros` | Wiring → gts | Plugin schema definitions (used by follow-up wiring crate) |
| `tokio` | SDK | Async runtime (watch channels, broadcast, TTL timers in stub backends) |
| `tokio_util` | SDK | `CancellationToken` for `PollingPrefixWatch` and (follow-up) plugin lifecycles |
| `async-trait` | SDK | `#[async_trait]` on the three backend traits |
| `types-registry-sdk` | Wiring → registry | GTS plugin-spec registration (used by follow-up wiring crate) |

### 3.5 External Dependencies

The cluster SDK has **no external dependencies** of its own. External backend libraries (`sqlx`, `kube`, `redis`, `async-nats`, `etcd-client`, `hazelcast`) belong to the follow-up plugin crates (`cf-postgres-cluster-plugin`, `cf-k8s-cluster-plugin`, `cf-cluster-redis`, `cf-cluster-nats`, `cf-cluster-etcd`, `cf-cluster-hazelcast`) and are NOT SDK dependencies.

| Plugin (follow-up) | External library | Purpose |
|---|---|---|
| Postgres plugin | `sqlx` | Connection pool, prepared statements, LISTEN/NOTIFY |
| K8s plugin | `kube` | API client, watch streams, Lease/CRD types |
| Redis plugin | `fred` (or `redis`) | Connection management, Lua script execution, keyspace notifications |
| NATS plugin | `async-nats` | JetStream KV access, watch subscriptions |
| etcd plugin | `etcd-client` | KV access, native lease/lock/election APIs |
| Hazelcast plugin | `hazelcast-rust` (TBD) | CP Subsystem access |

### 3.6 Resolution Pattern

There is no root trait. Each primitive has its own public-API facade struct with a static `resolver(hub)` entry point returning a fluent builder.

**Consumer-side definition (one place per consumer crate)**:

```rust
#[derive(Clone, Copy)]
pub struct EventBrokerProfile;
impl ClusterProfile for EventBrokerProfile {
    const NAME: &'static str = "event-broker";
}
```

**Call site**:

```rust
let cache = ClusterCacheV1::resolver(&hub)
    .profile(EventBrokerProfile)
    .require(CacheCapability::Linearizable)
    .require(CacheCapability::PrefixWatch)
    .resolve()?;

let leader = LeaderElectionV1::resolver(&hub)
    .profile(EventBrokerProfile)
    .require(LeaderElectionCapability::Linearizable)
    .resolve()?;
```

**Resolver builder body** (cache; the other three are identical in shape):

```rust
impl<'a> CacheResolverBuilder<'a> {
    pub(crate) fn new(hub: &'a ClientHub) -> Self {
        Self { hub, profile_name: None, requirements: Vec::new() }
    }
    pub fn profile<P: ClusterProfile>(mut self, _: P) -> Self {
        self.profile_name = Some(P::NAME);
        self
    }
    pub fn require(mut self, cap: CacheCapability) -> Self {
        self.requirements.push(cap);
        self
    }
    pub fn resolve(self) -> Result<ClusterCacheV1, ClusterError> {
        let profile = self.profile_name
            .ok_or(ClusterError::ProfileNotSpecified)?;
        // Map ClientHub's ScopedNotFound to our domain-level ProfileNotBound
        // so consumers see one error model.
        let inner: Arc<dyn ClusterCacheBackend> = self.hub
            .get_scoped(profile_scope(profile))
            .map_err(|_| ClusterError::ProfileNotBound { profile })?;
        validate_cache_capabilities(&*inner, &self.requirements)?;
        Ok(ClusterCacheV1 { inner })
    }
}
```

**Resolution flow**:
1. Consumer crate defines a `ClusterProfile` marker once. The `NAME` const is the only place the profile string appears on the consumer side.
2. Gear calls `*V1::resolver(hub).profile(P).require(Cap...).resolve()`.
3. The wiring crate's `ClusterWiring::builder(...).build_and_start()` had previously registered the corresponding `Arc<dyn _Backend>` in ClientHub under `profile_scope(P::NAME)`.
4. The resolver looks up the registered backend, validates declared `*Capability` requirements against the backend's actual `features()` (and `consistency()` for cache), and returns the wrapped facade. Mismatch → `CapabilityNotMet { primitive, capability, provider }` at startup.

Multiple resolutions of the same primitive on the same profile are cheap (`Arc`-clone-equivalent) and idempotent.

`profile_scope(name)` is an SDK helper that maps a profile name to a `ClientScope`. Convention: scope name `cluster:{profile}`. Validation: profile name MUST conform to `[a-zA-Z0-9_-]+`; reject invalid names at registration time.

### 3.7 Lifecycle Pattern (Builder/Handle)

> **Amendment (2026-06-16): collapsed to one gear crate.** As designed (this is follow-up work, not delivered in the SDK-only change that freezes this contract), the wiring library and the host gear are **the same crate** (`cf-gears-cluster`, gear name `cluster`), matching the platform's universal one-gear-per-domain layout (`<gear>-sdk` + `<gear>` + plugins). The crate will both (a) register the `cluster` gear — a `RunnableCapability` whose `start` builds the wiring from operator config and whose `stop` owns teardown — and (b) exports the builder/handle (`ClusterWiring`, `ClusterHandle`, `ClusterWiring::from_config`, `ProviderRegistry`) as `pub` library API, so a consumer gear may still embed the wiring directly without depending on the `cluster` gear. The separate non-gear wiring crate + separate host gear described below was rejected because it introduced a third core crate no other gear has; the genuinely reusable surface is `cluster-sdk` (already its own crate). The substance below holds — a `RunnableCapability` owns the handle, plugins remain builder/handle libraries composed by `ClusterHandle::stop()`, backends register under `cluster:{profile}` — only the crate boundary changed. The `ClusterCacheProvider` trait (a plugin implements it to build its cache backend from config options) lives in `cluster-sdk`, so plugins depend on the SDK only.

The `cluster` gear (`cf-gears-cluster`) is the single `RunnableCapability` that owns the cluster handle across its lifecycle; the same crate also exposes the wiring as a builder/handle pair (the outbox-style library API) for a consumer gear that prefers to embed it directly. Either way one `RunnableCapability` owns the `ClusterHandle` inside its own `start`/`stop`:

```rust
// In the cluster gear's RunnableCapability impl (or a consumer gear embedding the wiring):
impl RunnableCapability for ClusterGear {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let cluster_handle = ClusterWiring::builder(&self.config.cluster, &self.hub)
            .build_and_start()
            .await?;
        self.cluster_handle.set(cluster_handle).ok();
        Ok(())
    }

    async fn stop(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        if let Some(handle) = self.cluster_handle.take() {
            tokio::select! {
                () = handle.stop() => {} // graceful: deregister, cancel tokens, join
                () = cancel.cancelled() => {} // framework deadline
            }
        }
        Ok(())
    }
}
```

`ClusterHandle::stop().await` is the single entry point that:
1. Deregisters every registered backend from ClientHub via `deregister_*_backend` helpers (subsequent `*V1::resolver(...).resolve()` calls return `ProfileNotBound`).
2. Calls each plugin's internal stop sequence — cancels the plugin's `CancellationToken`, joins its background tasks (renewal loops, watch fan-out, TTL reapers).
3. Delivers `LeaderWatchEvent::Status(Lost)` then `LeaderWatchEvent::Closed(Shutdown)` to active leaders (two distinct events — `Status(Lost)` revokes confidence before the consumer can observe shutdown; `Closed(Shutdown)` ends the watch), and `CacheWatchEvent::Closed(Shutdown)` to active cache watches before returning.

**Why this shape**:
- Outbox is the codebase's production-mature long-running background-task pattern (`cluster/libs/toolkit-db/src/outbox/manager.rs:455–596`). Mini-chat owns its outbox via `Outbox::builder(...).start()` from inside its own `RunnableCapability::start`.
- Ordering is by code flow inside the parent gear's `start`, NOT framework declarations. The parent gear is registered as a `RunnableCapability` dependency of consumer gears (via existing ToolKit gear-dependency mechanism), so consumers can't try to resolve before cluster is up.
- Plugins are NOT separate `RunnableCapability` implementors. They expose builder/handle types like outbox does. The cluster wiring's builder calls each plugin's builder; the cluster handle owns each plugin's handle and stops them in reverse-start order.

**Post-shutdown behavior (narrowed best-effort `Ok`)**:
- `LockGuard::release(self)` / `LeaderWatch::resign(self)` MAY return `Ok(())` on a best-effort basis ONLY after the backend has observed `RunnableCapability::stop` (e.g., via an internal `AtomicBool::shutdown_observed`). Outside the shutdown window, real errors (`LockExpired`, foreign-holder release attempts, connection-lost mid-release) MUST propagate normally — silently masking them under the "best-effort" rule would hide real consumer bugs.

### 3.8 Per-primitive Scoping

Each public-API facade exposes `pub fn scoped(&self, prefix: &str) -> Self` returning a wrapped instance auto-prepending `prefix + "/"` on the write path and stripping it on the read path. Scoping composes: `cache.scoped("event-broker").scoped("shard-0")` produces effective prefix `"event-broker/shard-0/"`.

**Per-primitive scoping rules**:

| Primitive | Scoped argument(s) | Read-path strip | NOT scoped |
|---|---|---|---|
| `ClusterCacheV1` | `key` on `get`/`put`/`delete`/`contains`/`put_if_absent`/`compare_and_swap`/`watch`; `prefix` on `watch_prefix` | `CacheEvent::{Changed,Deleted,Expired}{key}` — strip prefix on the way back to the consumer | (none — cache has only keys) |
| `LeaderElectionV1` | `name` on `elect`/`elect_with_config` | n/a — `LeaderWatch` events don't carry names; the consumer already holds the watch handle | (none — election has only a name) |
| `DistributedLockV1` | `name` on `try_lock`/`lock` | n/a — `LockGuard` is opaque, consumer doesn't see backend names | (none — lock has only a name) |

**Examples**:

```rust
// Cache: keys
let cache = ClusterCacheV1::resolver(...).resolve()?.scoped("event-broker");
cache.put("shard-assignments", ...);          // backend sees "event-broker/shard-assignments"
cache.watch_prefix("");                        // backend sees "event-broker/"

// Leader election: election names
let leader = LeaderElectionV1::resolver(...).resolve()?.scoped("event-broker");
let watch = leader.elect("shard-leader").await?;  // backend sees "event-broker/shard-leader"

```

**Wrapper implementation**: each public-API struct's `scoped()` returns a new instance whose `inner: Arc<dyn _Backend>` is a `Scoped*Backend` wrapper that prepends/strips the prefix. The wrapper is internal to the SDK — consumers see only `ClusterCacheV1`, etc.

**Scope validation**: the `prefix` argument MUST conform to `[a-zA-Z0-9_/-]+`. Invalid prefixes fail at scope construction with `ClusterError::InvalidName { name, reason }`.

### 3.9 Watch Event Shape

All three watches yield events via union enums of the same shape (per ADR-003).

```rust
enum CacheWatchEvent {
    Event(CacheEvent),                // a cache mutation; consumer calls cache.get(key) for value
    Lagged { dropped: u64 },          // watcher fell behind; treat watched keys as stale, re-read
    Reset,                            // subscription re-established (reconnect, compaction); re-read
    Closed(ClusterError),             // terminal — watch is no longer usable
}

enum LeaderWatchEvent {
    Status(LeaderStatus),             // leadership transition; Lost is transient (auto-reenroll)
    Lagged { dropped: u64 },
    Reset,
    Closed(ClusterError),
}

```

Both are `#[non_exhaustive]` and infallible at the type level — there is no `Result<_, _>`-returning `changed()` method on any watch. **Terminal errors arrive via `Closed(err)`. Transient backend errors (`ConnectionLost`, `Timeout`, `ResourceExhausted`) are retried internally by the watch's background task and do not surface as events.**

**Consumer obligations**:
- On `Lagged { dropped }` or `Reset`: treat current state as potentially stale and recover. Cache: re-read affected keys via `get`. Leader watch: wait for the next `Status` event before resuming leader-only work.
- After `Closed(err)`: the watch is no longer usable; no further events follow. Consumer MAY restart at the application level (call `elect()` / `watch()` again) once cluster is up.

**Shutdown sequence** for `LeaderWatch`: the wiring crate's `ClusterHandle::stop()` delivers `LeaderWatchEvent::Status(Lost)` synchronously to every active `LeaderWatch` currently in `Leader` state, immediately followed by `LeaderWatchEvent::Closed(ClusterError::Shutdown)` as the terminal event. Two distinct events at the type level — `Status(Lost)` revokes the leader's confidence before the consumer can observe shutdown; `Closed(Shutdown)` ends the watch.

**Auto-restart combinator** (`*Watch::auto_restart(policy: RetryPolicy)`): the SDK provides an opt-in wrapper that turns retryable terminal closes into transparent reconnection with backoff. Retryability classification:

| `Closed(err)` payload | Classification | Combinator action |
|---|---|---|
| `Provider { kind: ConnectionLost, .. }` | retryable | reconnect after backoff; emit `Reset` on success |
| `Provider { kind: Timeout, .. }` | retryable | same |
| `Provider { kind: ResourceExhausted, .. }` | retryable | same; backoff respects backend's signal where available |
| `Provider { kind: AuthFailure, .. }` | non-retryable | propagate `Closed(err)` to consumer |
| `Provider { kind: Other, .. }` | non-retryable | propagate |
| `Shutdown` | non-retryable | propagate; consumer ends loop |
| `CapabilityNotMet { .. }` | non-retryable | propagate (capability validation rejects re-resolution anyway) |
| `LockExpired`, `LockContended`, `LockTimeout` | non-retryable on `LeaderWatch`/`CacheWatch` | propagate (these are state-loss signals on the renewal-task path; see §"Watch task and renewal task: independent signal paths" in ADR-003) |

`RetryPolicy::default()` uses exponential backoff `1s → 30s` with full jitter (`jitter_factor: 1.0`) and no retry cap. Operators can override via `RetryPolicy { initial_backoff, max_backoff, jitter_factor, max_retries }` constructor. When `max_retries` is exhausted, the combinator propagates the most recent `Closed(err)` to the consumer unchanged.

ADR-003 captures the rationale for the union shape over `Result`/`?`-based signaling, applies to all three watches for consistency, and is the source of the auto-restart combinator's semantics.

### 3.10 Capability Validation

Each primitive declares its own `*Capability` enum carrying the requirements a consumer can demand at resolution time. Each variant maps to a concrete backend characteristic check:

| Capability | Backend method | Check |
|---|---|---|
| `CacheCapability::Linearizable` | `ClusterCacheBackend::consistency()` | `== CacheConsistency::Linearizable` |
| `CacheCapability::PrefixWatch` | `ClusterCacheBackend::features()` | `.prefix_watch == true` |
| `LeaderElectionCapability::Linearizable` | `LeaderElectionBackend::features()` | `.linearizable == true` |
| `LockCapability::Linearizable` | `DistributedLockBackend::features()` | `.linearizable == true` |

**Validation helpers** (one per primitive):

```rust
fn validate_cache_capabilities(
    backend: &dyn ClusterCacheBackend,
    reqs: &[CacheCapability],
) -> Result<(), ClusterError> {
    for cap in reqs {
        match cap {
            CacheCapability::Linearizable
                if backend.consistency() != CacheConsistency::Linearizable =>
            {
                return Err(ClusterError::CapabilityNotMet {
                    primitive: "ClusterCacheV1",
                    capability: "Linearizable",
                    provider: backend.provider_name(),
                });
            }
            CacheCapability::PrefixWatch if !backend.features().prefix_watch => {
                return Err(ClusterError::CapabilityNotMet {
                    primitive: "ClusterCacheV1",
                    capability: "PrefixWatch",
                    provider: backend.provider_name(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}
```

Same shape for `validate_leader_election_capabilities` and `validate_lock_capabilities`. The `provider` field uses the backend's `provider_name()` method — a provided trait method that resolves `std::any::type_name::<Self>()` through the vtable — to give the operator a concrete diagnostic name for the bound backend. (`std::any::type_name_of_val` applied to a `&dyn ClusterCacheBackend` would yield only the trait-object name, never the concrete backend, because it is monomorphized on the static type.)

**Why per-primitive (not bundled `CapabilityClass`)**: the prior bundled `CapabilityClass { Standalone, Durable, InMemory, Coordination }` collapsed three orthogonal axes (topology, persistence, consistency) into one fuzzy ordering. Per-primitive `*Capability` enums are type-safe (a cache resolver cannot accept `MetadataFiltering`) and grounded in concrete backend characteristic checks rather than coarse tier claims.

#### 3.10.1 `resolve()` is `async`, and where validation lands

`resolve()` on all three resolvers is `async fn`. **It is the only SDK signature the remote-backend model changes** — the facades, the typed-profile resolver, `scoped()`, the watch-event unions and `auto_restart` all keep their shapes.

The reason is validation, not the resolution itself: checking a declared capability needs the bound backend's `consistency()`/`features()`, and for a *remote* binding those come from a `ProfileDescriptor` that has to be fetched. A synchronous signature cannot await one. In-process there is nothing to await — the bound object **is** the real backend, so its characteristics are known immediately and validation is inline, exactly as the tables above describe.

Two rules for consumers follow, and both are cheap:

- **Resolve facades in `start`, never in `init`.** Both are already `async fn` on the gear traits, so the `.await` costs a consumer nothing structurally. `init` is the wrong phase regardless: backends are registered by the cluster gear's own `start`.
- **A consumer that branches on `CapabilityNotMet` is relying on the inline path.** That is always the in-process path. Against a remote cluster the same check can instead land on readiness — see the specification of the bounded descriptor await and the inline-vs-deferred split in [DESIGN-DEPLOYABLE-GEAR.md](./DESIGN-DEPLOYABLE-GEAR.md) §4.7.1. The *guarantee* is identical either way (no consumer serves traffic against an unmet requirement) and so is the error text; only the delivery point moves.

**What `resolve()` actually does**, now that the seam is in place (ADR-011):

1. Takes the process's one `Arc<dyn ClusterClient>` from the `ClientHub`.
2. Asks it for this profile's backend. Synchronous and pure in both deployment profiles — the real backend locally, a remote handle remotely. A client that does not bind the profile is `Err(ProfileNotBound)` here, immediately.
3. **Awaits the profile's `ProfileDescriptor`, bounded** by an SDK constant (2 s). This is the only `await` on the path, and it waits on the descriptor — never on cluster becoming reachable.
4. Validates the declared requirements against that descriptor, or defers to readiness when it did not arrive in time.

Two consequences worth stating rather than leaving to be found:

- **Validation reads the descriptor, not the backend.** The tables above describe the check; its *input* is now what the profile's binding declares, which is what a remote consumer can obtain at all. In-process the descriptor is computed from the real backends, so the answer is identical — and the error text is byte-identical across deployment profiles, which is the property the equivalence gate asserts. One thing does change under operator config: `CapabilityNotMet { provider }` names the provider **the operator wrote** (`postgres`) rather than the Rust type behind it.
- **A process with no cluster client wired at all is not a resolve failure.** `resolve()` returns `Ok` and the facade reports `ProfileNotBound` on its first *call*, naming the profile; the distinguishing phrase (*no cluster client registered in this process*) is logged at `warn`, because `ClusterError` is frozen and cannot carry a second message. That tolerance is what lets a Profile 3 cold start proceed, and the readiness contributor is what stops it hiding a Profile 1 build mistake.

### 3.11 SDK Default Backends

> **Implementation location:** The three default backend implementations live in the **cluster gear** (`cf-gears-cluster`), not in the SDK. Consumer gears never import them directly; only the cluster gear's wiring layer instantiates them. The SDK retains only the backend *traits* and facades that consumers depend on.

The cluster gear ships three default backend implementations built on `Arc<dyn ClusterCacheBackend>`:

- `CasBasedLeaderElectionBackend` — `put_if_absent(election_key, node_id, ttl)` for candidacy, `watch(election_key)` for status changes, background renewal task at `ttl / (max_missed_renewals + 1)`, TTL expiry → `Status(Lost)` followed by auto-reenroll. `features()` returns `LeaderElectionFeatures { linearizable: cache.consistency() == Linearizable }` — derives from the underlying cache's consistency.
- `CasBasedDistributedLockBackend` — `put_if_absent(lock_key, holder_id, ttl)` for `try_lock`, `watch(lock_key)` to notify blocked waiters on release, background TTL reaper. Release via delete-if-still-holder using CAS (a foreign holder cannot release another's lock). No fencing tokens (the no-remote-in-critical-section rule eliminates the stale-writer scenario). `features()` returns `LockFeatures { linearizable: cache.consistency() == Linearizable }`.

**Constructor pair per default backend**:
- `new(cache: Arc<dyn ClusterCacheBackend>) -> Result<Self, ClusterError>` — returns `Err(ClusterError::InvalidConfig)` if `cache.consistency() == EventuallyConsistent`. Default-safe.
- `new_allow_weak_consistency(cache: Arc<dyn ClusterCacheBackend>) -> Self` — always succeeds. Caller acknowledges the safety implications. Construction emits a warning log at instantiation. Required by spec for use cases where the underlying cache is intentionally `EventuallyConsistent` (Redis Sentinel, NATS R=1, Postgres `synchronous_commit=off`) and the consumer accepts the split-brain risk.

**SDK-default selection at the wiring layer (omit-primitive auto-wrap)**: operator YAML uses **omission** to opt into SDK defaults. If a profile binds a `cache` provider but does not bind `leader_election` / `lock`, the wiring crate auto-wraps the bound cache backend in the corresponding SDK default backend and registers each under the same profile scope. Explicit binding always wins. If both `cache` and another primitive are omitted (no anchor to wrap), the wiring crate fails startup with `ClusterError::InvalidConfig`.

```yaml
cluster:
  profiles:
    # Single-backend profile via omission
    default:
      cache: { provider: postgres }
      # leader_election omitted → CasBasedLeaderElectionBackend over postgres cache
      # lock              omitted → CasBasedDistributedLockBackend  over postgres cache

    # Mixed: native LE + auto-wrapped lock
    in-memory:
      cache: { provider: redis }
      leader_election: { provider: k8s-lease }
      # lock omitted → CasBasedDistributedLockBackend over redis cache
```

### 3.12 Polyfill

`PollingPrefixWatch` synthesizes `watch_prefix` semantics on backends that declare `features().prefix_watch == false`:

```rust
PollingPrefixWatch::spawn(
    cache: Arc<dyn ClusterCacheBackend>,
    prefix: &str,
    interval: Duration,
) -> CacheWatch
```

Periodically lists keys under the prefix, diffs against the previous list, and emits `CacheWatchEvent::Event(CacheEvent::Changed | Deleted)` for observed changes. Cost: N `get` calls per interval, no millisecond-level precision. Doc comments explicitly warn about the cost and recommend routing to a backend with native prefix watch at scale. Drop on the watch stops the polling task.

Enumeration is provided by `ClusterCacheBackend::scan_prefix(prefix) -> Vec<String>`, a defaulted (returns `Unsupported`) additive extension to the cache contract so existing backends keep compiling and opt in by override (see ADR-010). The polyfill lists keys via `scan_prefix`, then issues one `get` per key to read its version for change detection (the `N + 1` round-trips above); a `scan_prefix` error closes the synthesized watch with a terminal `Closed`. Because the polyfill emits full backend keys like a native `watch_prefix`, `ScopedCacheBackend` strips the scope prefix from them on the read path, so scoping composes with the polyfill.

### 3.13 Interactions & Sequences

#### Per-primitive Resolution

- [x] `p1` - **ID**: `cpt-cf-clst-seq-per-primitive-resolution`

```
  Consumer Gear                    SDK                         ClientHub
       │                              │                              │
       │  ClusterCacheV1::resolver(&hub)                              │
       │   .profile(EventBrokerProfile)                              │
       │   .require(CacheCapability::Linearizable)                   │
       │   .resolve()                 │                              │
       │ ────────────────────────────>│                              │
       │                              │  hub.get_scoped::<dyn        │
       │                              │     ClusterCacheBackend>(    │
       │                              │     profile_scope("event-broker"))│
       │                              │ ────────────────────────────>│
       │                              │  Arc<dyn ClusterCacheBackend>│
       │                              │ <────────────────────────────│
       │                              │  validate_cache_capabilities │
       │                              │     (consistency() check)    │
       │                              │  wrap in ClusterCacheV1      │
       │  ClusterCacheV1              │                              │
       │ <────────────────────────────│                              │
```

#### Lifecycle: Parent host gear → Cluster wiring → Plugins

- [ ] `p1` - **ID**: `cpt-cf-clst-seq-lifecycle-startup`

```
  Gear Host         Parent Gear               Cluster Wiring          Plugins
       │                   │                          │                      │
       │ start(cancel)     │                          │                      │
       │ ─────────────────>│                          │                      │
       │                   │ ClusterWiring::builder() │                      │
       │                   │  .build_and_start()      │                      │
       │                   │ ────────────────────────>│                      │
       │                   │                          │ read profile config  │
       │                   │                          │ (cache: redis,       │
       │                   │                          │  leader: k8s-lease)  │
       │                   │                          │                      │
       │                   │                          │ Plugin::builder()    │
       │                   │                          │  .build_and_start()  │
       │                   │                          │ ────────────────────>│
       │                   │                          │                      │  spawn
       │                   │                          │                      │  CancellationToken
       │                   │                          │                      │  + JoinHandles
       │                   │                          │                      │
       │                   │                          │ register_*_backend   │
       │                   │                          │  (per profile per    │
       │                   │                          │   primitive in       │
       │                   │                          │   ClientHub)         │
       │                   │                          │                      │
       │                   │ ClusterHandle            │                      │
       │                   │ <────────────────────────│                      │
       │                   │ store handle             │                      │
       │ Ok                │                          │                      │
       │ <─────────────────│                          │                      │

  Consumer gears now resolve via *V1::resolver(...).profile(P).resolve()
```

#### Shutdown Sequence

- [ ] `p1` - **ID**: `cpt-cf-clst-seq-shutdown`

```
  Gear Host       Parent Gear        Cluster Handle         Active Watches
       │                 │                    │                        │
       │ stop(cancel)    │                    │                        │
       │ ───────────────>│                    │                        │
       │                 │ handle.stop()      │                        │
       │                 │ ──────────────────>│                        │
       │                 │                    │ revoke: deliver        │
       │                 │                    │  Status(Lost) to leaders│
       │                 │                    │ ──────────────────────>│ Status(Lost)
       │                 │                    │ revoke: Closed(Shutdown)│
       │                 │                    │  to leader/lock/SD      │
       │                 │                    │ ──────────────────────>│ Closed(Shutdown)
       │                 │                    │                        │
       │                 │                    │ deregister all backends│
       │                 │                    │  from ClientHub         │
       │                 │                    │                        │
       │                 │                    │ stop hooks: plugin      │
       │                 │                    │  cache.shutdown() →     │
       │                 │                    │ ──────────────────────>│ Closed(Shutdown)
       │                 │                    │  cancel sweeper, drop   │
       │                 │                    │                        │
       │                 │ Ok                 │                        │
       │                 │ <──────────────────│                        │
       │ Ok              │                    │                        │
       │ <───────────────│                    │                        │
```

**Implementation status (this change).** The lifecycle owner is the cluster gear crate itself (host collapsed in); `ClusterHandle::stop()` lives there, not in a separate wiring crate. The implementation now matches the sequence diagram above. It revokes in-flight coordination **first** for every wiring-created default backend: the leader-election backend latches `Status(Lost)` then `Closed(ClusterError::Shutdown)` to active leaders (awaiting those tasks); an in-flight blocking `lock()` waiter returns `Err(ClusterError::Shutdown)` (distinct from `LockTimeout`); and it then deregisters backends from the `ClientHub` and runs the plugin stop hooks in reverse-start order. Active **cache** watches now receive an explicit `Closed(ClusterError::Shutdown)` too — delivered via the standalone plugin's stop hook (`StandaloneCache::shutdown`), which closes every watcher before the sweeper stops and the cache is dropped. That cache-watch close lands one phase after the leader/lock revocation but still within `stop()` (the chosen simplest path). No remote release is performed; held claims and locks lapse via TTL (`cpt-cf-clst-fr-shutdown-ttl-cleanup`).

### 3.14 Database schemas & tables

N/A — the cluster SDK has no persistent database schemas. Cluster is an in-process library that delegates all storage to plugin-owned backends (Redis, Postgres, K8s API, NATS, etcd), each of which manages its own schema or storage layout independently. The SDK's only durable types are the wire-stable contract surfaces (facade methods, backend traits, error variants) documented in §3.3 and §3.1; those are Rust types, not database tables.

Per-backend storage layout (e.g., the Postgres plugin's `cluster_cache` and `cluster_cache_subscriber_lease` tables, the K8s plugin's CRDs) is documented in each follow-up plugin's own DESIGN, not here.

### 3.15 Deployment Topology

**Cluster has a deployment topology of its own, and it is mapped to the platform's deployment profiles.** This
section previously said the opposite — "an in-process Rust library SDK; it has no deployment topology of its
own" — which was true while the only shape was a library linked into a consumer's process. It is no longer:
`cf-gears-cluster` ships a `[[bin]] cluster-oop` and can be deployed as its own pod, with consumers reaching it
over gRPC. The consumer API is unchanged in both shapes (DESIGN-DEPLOYABLE-GEAR.md Goal 2), so what varies is
the topology, not the code.

| Platform profile | Topology | What owns the backends | How a consumer gets a primitive |
|---|---|---|---|
| **Profile 1 — Embedded** | One process. `cluster` and its plugins are linked into the consumer's binary; the gear's `start` owns the `ClusterHandle`, or a consumer owns `ClusterWiring` directly (§3.7) | The consumer's own process | `resolve()` returns the real backend `Arc` through a `LocalClusterClient` — no wrapper on the request path, no network |
| **Profile 2 — Host + Workers** | **Not designed.** Out of scope for the first deployable version, and stated as a scope limit rather than a deferral: no endpoint-resolution mechanism exists for it, and its topology fork (one cluster process per *deployment* vs. per *host*) is unanswered — the second silently makes locks per-host rather than deployment-wide | — | — |
| **Profile 3 — K8s Native** | The `cluster-oop` binary in its own pod, serving the four coordination services on the gRPC port and the framework probes on the HTTP port. One replica by default, pending the cross-replica failover suite; store-owned leases (ADR-012) already make any replica able to serve any lease operation | The cluster pod | The framework's proxy-wiring phase registers a `RemoteClusterClient`; `resolve()` derives per-primitive remote handles from it and the profile rides on each request |

Within a profile, the shape that still matters operationally is the **profile × backend** matrix. §4.2
Recommended Deployment Combinations enumerates the supported shapes (single-instance dev/test, multi-instance
non-K8s, K8s-low-throughput, K8s + Redis production, Redis-only); each is realized by the deployment of whatever
process owns the wiring — a `cluster-oop` pod in Profile 3, the consumer's own pod, systemd unit or container in
Profile 1 — plus the backend bindings declared in operator YAML. The wiring instantiates each primitive's bound
provider independently and auto-fills only the primitives the operator omits with the SDK defaults over that
profile's cache, so the mixed-backend shapes in the matrix below are expressible in YAML today
(`cpt-cf-clst-fr-routing-per-primitive`) for whichever native providers the linked plugins ship.

**What `cluster-oop` does not contain** is worth stating, because the absence is the design: no directory
registration, no heartbeat, no backoff, no dependency retry and no drain logic. `/healthz`, `/readyz`, `/health`
and `/openapi.json` are bound and served **before** the gear's `start` runs, self-registration and the presence
loop run in the background, and the drain sequence and deregistration run on SIGTERM — all of it supplied by
`toolkit::bootstrap::oop::run_oop_with_options`, which the binary's `main` calls and otherwise does nothing
(ADR-0005). The binary is a `clap` CLI over that one call plus a `registered_gears.rs` naming the two gears the
process must link.

Cross-cluster / geo-distributed coordination is out of scope (§4.2 Out of Scope in PRD).

#### 3.15.1 Linking `cluster` requires linking `grpc-hub`

**Any process that links the `cluster` gear must also link `grpc-hub` and give it a `listen_addr`, or it fails at startup.** This is not confined to the deployable (out-of-process) shape — it applies to every in-process monolith too, and it is a hard failure rather than a degradation: the framework refuses to build a registry that has gRPC services and no hub, with `RegistryError::GrpcRequiresHub` (`libs/toolkit/src/runtime/host_runtime.rs:777-779`).

The cause is that the gear declares the `grpc` capability and exports the four coordination services (`cluster.{cache,lock,leader,profile}.v1`), which it does so that one profile-dispatch mechanism serves both an embedded and a remote consumer. Two consequences an operator has to plan for:

- **Once the hub is linked, cluster's four services are served on that process's hub port.** That is a network surface an embedded cluster never had, so an embedding process needs the same `NetworkPolicy` treatment as a dedicated cluster pod: the coordination port is platform-plane and must not be reachable from outside the platform namespaces.
- **The hub must bind a port the operator is willing to expose.** There is no "link the hub but serve nothing" mode today.

Gating the capability behind a `serve-grpc` feature *is* expressible — but only as two mutually exclusive `#[cfg_attr(..., toolkit::gear(...))]` attributes, since `#[toolkit::gear]` accepts a `#[cfg]` **inside** its `capabilities = [..]` list and then silently ignores it (measured against `toolkit-macros`; the capability is registered either way). It is not adopted, because the gear links `tonic` unconditionally regardless, so the feature would remove the hub requirement without removing the dependency — see DESIGN-DEPLOYABLE-GEAR.md Risk 8.

## 4. Additional Context

### 4.1 Backend Feature Compatibility

**Sub-capability implementation strategy per backend:**

| Backend | Cache | Leader Election | Distributed Lock |
|---------|-------|----------------|-----------------|
| **Standalone** (in-process, shipped) | Native (HashMap + AtomicU64) | Native (watch channel) | Native (Mutex + Notify) |
| **Postgres** (shipped) | Native (table + LISTEN/NOTIFY) | SDK default (on PG cache) | Native (`cluster_lock` row, owner + fence) |
| **K8s** (follow-up) | Native (CRD + `resourceVersion`) | Native (Lease API) | Native (Lease API) |
| **Redis** (follow-up) | Native (GET/SET/Lua) | SDK default (on Redis cache) | Native (SET NX EX + Lua) |
| **NATS KV** (follow-up) | Native (KV bucket + revision) | SDK default (on NATS cache) | SDK default (on NATS cache) |
| **etcd** (follow-up) | Native (KV + `mod_revision`) | Native (election API) | Native (lock API) |

**ProviderErrorKind mapping per backend:**

| ProviderErrorKind | Redis (fred) | Postgres (sqlx) | NATS (async-nats) | K8s (kube) | etcd (etcd-client) |
|---|---|---|---|---|---|
| `ConnectionLost` | `ErrorKind::IO` | `Error::Io` | `ConnectErrorKind::Io` | `HyperError` | `TransportError` |
| `Timeout` | `ErrorKind::Timeout` | `Error::PoolTimedOut` | `*ErrorKind::TimedOut` | hyper timeout | gRPC `DeadlineExceeded` |
| `AuthFailure` | `ErrorKind::Auth` | SQLSTATE `28xxx` | `Authentication` | HTTP `401`/`403` | gRPC `Unauthenticated` |
| `ResourceExhausted` | `ErrorKind::Backpressure` | — | — | HTTP `429` | gRPC `ResourceExhausted` |

### 4.2 Recommended Deployment Combinations

| Deployment | Config | Cache | LE | Lock | SD | Notes |
|-----------|--------|-------|----|----|----|----|
| Dev / single-instance | `provider: standalone` | Standalone | Standalone | Standalone | Standalone | Zero deps |
| Multi-instance, no K8s | `provider: postgres` | Postgres | SDK default | Postgres | SDK default | Zero new infra |
| K8s, low-throughput | `provider: k8s` | K8s CRD | K8s Lease | K8s Lease | K8s Lease (per instance) | Zero new infra |
| K8s + Redis (recommended) | hybrid | Redis | K8s Lease | Redis | K8s Lease (per instance) | Best of both |
| Redis-only | `provider: redis` | Redis | SDK default | Redis | SDK default | Single infra dep |
| NATS stack | `provider: nats` | NATS KV | SDK default | SDK default | SDK default | Single infra dep |
| etcd available | `provider: etcd` | etcd | etcd (native) | etcd (native) | SDK default | Best coordination guarantees |

### 4.3 Existing Code Migration

The following existing code overlaps with cluster capabilities and will be migrated in **separate follow-up changes**:

| Existing Code | Location | Overlap | Migration Plan |
|------|----------|---------|---|
| `LeaderElector` trait + `K8sLeaseElector` | `mini-chat/src/infra/leader/` | Leader election (production-quality K8s Lease impl) | Extract into `cf-k8s-cluster-plugin`; mini-chat consumes via `LeaderElectionV1::resolver(&hub).profile(MiniChatProfile).resolve()` |
| File-based advisory locks | `libs/toolkit-db/src/advisory_locks.rs` | Distributed lock (single-host only, no fencing) | Not reusable — cluster provides true distributed locks via `DistributedLockV1`. Gears migrate on adoption. |

## 5. Traceability

DESIGN realizes the requirements stated in [PRD.md](./PRD.md) §5 (Functional Requirements) and §6 (Non-Functional Requirements). The inverse mapping (FR/NFR → realizing DESIGN section + supporting ADR) is the source of truth at PRD §14 Traceability. This section captures the forward direction: which decisions in DESIGN annotate which ADRs.

**ADR coverage of DESIGN decisions** (each cluster ADR annotates one or more DESIGN sections with rationale):

- **ADR-001** — annotates §3.11 SDK Default Backends (cache-CAS-universal model), §3.2 Component Model (per-backend characteristics drive component shape), §4.1 Backend Feature Compatibility, §4.2 Recommended Deployment Combinations.
- **ADR-002** — annotates §2.2 Constraints (no-remote-in-critical-section), §3.3 lock contract (no I/O in `Drop`, explicit async release).
- **ADR-003** — annotates §2.1 watch-union-shape principle, §2.1 lightweight-notifications principle, §3.9 Watch Event Shape, §3.13 Shutdown Sequence.
- **ADR-004** — annotates §3.3 telemetry expectations across all three primitives.
- **ADR-005** — annotates §1.1 Architectural Vision (facade-plus-backend-trait), §2.1 facade-plus-backend-trait principle, §3.1 Domain Model (eight types), §3.2 Component Model.
- **ADR-006** — annotates §3.7 Lifecycle Pattern (Builder/Handle), §3.11 SDK Default Backends (omit-primitive auto-wrap as wiring-crate behavior), §3.13 lifecycle/shutdown sequences.
- **ADR-007** — annotates §3.6 Resolution Pattern, §3.10 Capability Validation.
- **ADR-009** — annotates §3.11 SDK Default Backends (constructor pair `new` + `new_allow_weak_consistency`), §4.1 per-backend safety classification.

**DESIGN component IDs** (from §3.2): `cpt-cf-clst-component-sdk`, `cpt-cf-clst-component-wiring`, `cpt-cf-clst-component-plugins`.

**DESIGN sequence IDs** (from §3.13): `cpt-cf-clst-seq-per-primitive-resolution`, `cpt-cf-clst-seq-lifecycle-startup`, `cpt-cf-clst-seq-shutdown`.

**DESIGN principle IDs** (from §2.1): `cpt-cf-clst-principle-cas-universal`, `cpt-cf-clst-principle-per-primitive-routing`, `cpt-cf-clst-principle-facade-plus-backend-trait`, `cpt-cf-clst-principle-lightweight-notifications`, `cpt-cf-clst-principle-version-based-cas`, `cpt-cf-clst-principle-watch-union-shape`.

**DESIGN constraint IDs** (from §2.2): `cpt-cf-clst-constraint-no-serde`, `cpt-cf-clst-constraint-no-remote-in-critical-section`, `cpt-cf-clst-constraint-dyn-compat`.

## 6. Risks / Trade-offs

**[Risk: Abstraction leakage]** Different backends have fundamentally different consistency guarantees (Redis RedLock is "probably correct", Postgres advisory locks are strictly serializable, Hazelcast IMap is CP or AP depending on config). Trait documentation must be explicit about minimum guarantees, and plugins must document their actual guarantees.
- Mitigation: Define minimum guarantees in trait docs (e.g., "at most one leader at any point per `LeaderElectionFeatures::linearizable == true` plus advisory staleness bound"). Plugin authors document their `*Features` declarations honestly. Capability requirements at the resolver site enforce honest characteristic claims at startup.

**[Risk: SDK contract verifies API shape, not distributed correctness]** Smoke tests against minimal in-process stubs verify that consumer code compiles against the SDK, handles the happy path, and exercises the error variants stubs emit (`Lagged`, `Closed(Shutdown)`, `CasConflict`, `CapabilityNotMet`). They do NOT verify behavior under network partition, clock skew, split-brain, message reordering across subscribers, or backend-specific failure semantics (Redis AOF loss, Postgres `synchronous_commit` windows, NATS JetStream sequence gaps, K8s API-server throttling). These failure modes cannot be faithfully simulated in-process — stubs have one state map, one clock, and one FIFO event channel.
- Mitigation: Each plugin follow-up change ships feature-gated integration tests against the real backend using CI infrastructure (Postgres containers for Phase 3, kind/minikube for Phase 4 K8s, future Redis/NATS/etcd containers). These tests are the authoritative source of distributed-correctness verification for each backend.
- Operator-facing partition behavior is concretely bounded: the consumer-perceived dual-leadership window under partition is `TTL + observation_lag`. See §3.3 staleness bound for the worst-case formula with default config and the operator-tuning trade-off.
- Future work (out of initial scope): Jepsen-style correctness harness exercising partition, clock skew, and process-kill scenarios against each plugin.

**[Trade-off: Per-primitive routing config complexity]** Per-primitive backend routing in operator YAML adds configuration surface. Operators could create confusing combinations (e.g., three different backends for three primitives).
- Mitigation: Documented recommended combinations in §4.2. Capability validation surfaces incompatible combinations at startup with clear error messages naming the bound backend. SDK-default omit-primitive auto-wrap simplifies single-backend profiles to a 1-line YAML config.

**[Trade-off: SDK-only this change ships without runnable cluster]** Until the wiring crate (`cf-cluster`) and at least one production plugin (`cf-standalone-cluster-plugin`) ship, the cluster is not deployable beyond SDK consumption — consumers can compile against the SDK but cannot run.
- Mitigation: Showcase example crates demonstrate consumer usage and plugin author shape (builder/handle pattern). Smoke tests prove the SDK contract works. Follow-up plugin changes can begin in parallel against the stable SDK contract.

## 7. Open Questions

| Question | Owner | Target Resolution |
|----------|-------|-------------------|
| Backend authentication and credential wiring | Platform OOP deployment design | Resolved as part of the broader OOP design |
| Whether ADR-003 (cache watch backpressure) broadens to cover all three watches, or a new ADR captures the generalization | Cluster gear owner | Resolved during ADR audit — recommendation: broaden ADR-003 with a "Generalization to all three watches" section |
