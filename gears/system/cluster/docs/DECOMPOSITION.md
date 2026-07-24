# DECOMPOSITION — Cluster

- [x] `p1` - **ID**: `cpt-cf-clst-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 SDK Foundation & Shared Contract ✅ HIGH](#21-sdk-foundation--shared-contract--high)
  - [2.2 Distributed Cache Primitive ✅ HIGH](#22-distributed-cache-primitive--high)
  - [2.3 Leader Election Primitive ✅ MEDIUM](#23-leader-election-primitive--medium)
  - [2.4 Distributed Lock Primitive ✅ MEDIUM](#24-distributed-lock-primitive--medium)
  - [2.5 Service Discovery Primitive ✅ MEDIUM](#25-service-discovery-primitive--medium)
  - [2.6 SDK Default Backends ✅ LOW](#26-sdk-default-backends--low)
  - [2.7 Per-primitive Scoping & Prefix-Watch Polyfill ✅ LOW](#27-per-primitive-scoping--prefix-watch-polyfill--low)
  - [2.8 Watch Auto-Restart Combinator ✅ LOW](#28-watch-auto-restart-combinator--low)
  - [2.9 Registration Helpers, GTS Spec & Observability Contract ✅ MEDIUM](#29-registration-helpers-gts-spec--observability-contract--medium)
  - [2.10 Lock-Misuse Lint (no-remote-in-critical-section) ✅ LOW](#210-lock-misuse-lint-no-remote-in-critical-section--low)
  - [2.11 Smoke Tests (in-process stub backends) ✅ MEDIUM](#211-smoke-tests-in-process-stub-backends--medium)
  - [2.12 Showcase Examples & Traceability Audit ✅ LOW](#212-showcase-examples--traceability-audit--low)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

This decomposition breaks the **cluster SDK contract** (`cf-gears-cluster-sdk`, lib `cluster_sdk`, at `gears/system/cluster/cluster-sdk/`) into independently implementable FEATURE work packages. It realizes the in-scope design component `cpt-cf-clst-component-sdk` and the design elements that component implements. The decomposition follows the dependency order established in DESIGN: a shared contract foundation enables the cache primitive (the universal CAS building block), which in turn enables the three remaining primitives, the SDK default backends, scoping, the watch auto-restart combinator, registration/observability helpers, the lock-misuse lint, smoke tests, and showcase examples.

**Decomposition Strategy**:

- Features are grouped by functional cohesion — one feature per coordination primitive, plus foundation, cross-cutting, and verification features.
- Dependencies are minimized: the three primitive features (leader election, distributed lock, service discovery) depend only on the foundation and cache features and are mutually independent (parallelizable).
- Each design element (requirement, principle, constraint, component, sequence) in scope is assigned to exactly one feature; the cache primitive carries the shared cross-primitive contracts (watch-union shape, capability validation) because it is the first concrete facade-plus-backend pair and the foundation the others build on.
- 100% coverage of in-scope design elements is verified; out-of-scope elements are enumerated below with rationale.

**In scope (this change)**: the SDK contract only — facade structs (`*V1`), backend traits (`*Backend`), resolver builders, profile marker, capability/features types, shared types, SDK default backends, scoping wrappers, the prefix-watch polyfill, the watch auto-restart combinator, registration/deregistration helpers, the GTS plugin spec, the observability naming contract, the workspace lock-misuse lint, smoke tests, and showcase examples.

**Scope note.** This change delivers the SDK contract only (per the in-scope list above). The lifecycle wiring and a production backend remain **follow-ups**: the collapsed one-crate model (`cf-gears-cluster` gear owning `ClusterHandle`, with the standalone in-process plugin) is the agreed design but is **not implemented in this change** — see the amendment in `DESIGN.md` §3.7 for the intended crate boundary.

- **Component `cpt-cf-clst-component-wiring`** — the lifecycle wiring (`ClusterWiring::builder(...).build_and_start() -> ClusterHandle`) is **pending**, to land in the `cf-gears-cluster` gear crate rather than a separate wiring crate (host collapsed in).
- **Component `cpt-cf-clst-component-plugins`** — the in-process **standalone** plugin is **pending**, alongside the per-backend plugin crates (Postgres, K8s, Redis, NATS, etcd) as separate follow-up changes.
- **FRs realized by the wiring / operator config / lifecycle owner**: `cpt-cf-clst-fr-routing-omit-default` (wiring auto-wrap), `cpt-cf-clst-fr-lifecycle-owner` (gear crate owns `ClusterHandle`), `cpt-cf-clst-fr-shutdown-revoke` (leader revocation on `stop`) and `cpt-cf-clst-fr-shutdown-ttl-cleanup` (no remote cleanup on `stop`) are **follow-ups** that build against the contract frozen here. `cpt-cf-clst-fr-routing-per-primitive` likewise remains a follow-up: binding a native non-cache backend per primitive is rejected at config time until those providers ship.
- **Sequences `cpt-cf-clst-seq-lifecycle-startup` and `cpt-cf-clst-seq-shutdown`** — pending, to be realized by the gear crate's `ClusterHandle` start/stop (leader revocation included); `cpt-cf-clst-seq-per-primitive-resolution` is realized by the SDK resolvers in this change.

The remaining out-of-scope elements (lifecycle wiring, standalone and external per-backend plugins, per-primitive native routing) will be decomposed in the follow-up changes that build against the contract frozen here.

## 2. Entries

### 2.1 [SDK Foundation & Shared Contract](features/001-sdk-foundation.md) ✅ HIGH

- [x] `p1` - **ID**: `cpt-cf-clst-feature-sdk-foundation`

- **Purpose**: Establish the `cf-gears-cluster-sdk` crate, wire it into the workspace, and build the shared contract foundation every primitive depends on: the unified error model, programmatic retryability classification, the typed profile marker, profile-scope and name-validation helpers, and the dyn-compatibility assertion harness. This is the bedrock that lets the four primitives evolve on a stable, serde-free, dyn-safe contract.

- **Depends On**: None

- **Scope**:
  - Create `gears/system/cluster/cluster-sdk/` (package `cf-gears-cluster-sdk`, lib `cluster_sdk`); add to root workspace members.
  - Dependencies limited to `tokio`, `tokio_util`, `async-trait`, `toolkit`, `gts`, `gts-macros`, `toolkit-gts`, `types-registry-sdk` — no serde.
  - `ClusterError` (full variant set incl. `CapabilityNotMet`, `ProfileNotBound`, `ProfileNotSpecified`, `Shutdown`, `Provider`; no `NotStarted`) and `ProviderErrorKind` retryability classification.
  - `ClusterProfile` marker trait; `profile_scope(name)` helper (`cluster:{profile}` scope); name validation (`[a-zA-Z0-9_/-]+`).
  - Compile-time dyn-compatibility assertion harness pattern, applied per backend trait thereafter.

- **Out of scope**:
  - Any primitive facade or backend trait (delivered in the per-primitive features).
  - The wiring crate and ClientHub registration orchestration (follow-up).

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-clst-fr-validation-typed-profile`
  - [x] `p1` - `cpt-cf-clst-nfr-error-retryability`
  - [x] `p1` - `cpt-cf-clst-nfr-plugin-stability`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-clst-constraint-no-serde`
  - [x] `p1` - `cpt-cf-clst-constraint-dyn-compat`

- **Domain Model Entities**:
  - ClusterError
  - ProviderErrorKind
  - ClusterProfile

- **Design Components**:

  - [x] `p1` - `cpt-cf-clst-component-sdk`

- **API**:
  - Rust: `ClusterProfile` trait (`const NAME`), `profile_scope(name) -> ClientScope`, `ClusterError`, `ProviderErrorKind`.

- **Sequences**: None (foundation types; no interaction sequence).

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.2 [Distributed Cache Primitive](features/002-cache-primitive.md) ✅ HIGH

- [x] `p1` - **ID**: `cpt-cf-clst-feature-cache-primitive`

- **Purpose**: Deliver the cache primitive — the universal CAS building block on which leader election, locks, and service discovery are built. Defines versioned key-value storage, atomic conditional operations, TTL, and reactive key/prefix notifications, plus the canonical watch-union event shape and the per-primitive fluent resolver with capability validation that every other primitive reuses.

- **Depends On**: `cpt-cf-clst-feature-sdk-foundation`

- **Scope**:
  - `ClusterCacheBackend` plugin trait (get/put/delete/contains/put_if_absent/compare_and_swap/compare_and_delete/watch/watch_prefix) and `ClusterCacheV1` facade (same async surface minus the backend-only `compare_and_delete`; sync `consistency()`/`features()`/`resolver()`/`scoped()`). `compare_and_delete` is a value-guarded delete used by SDK-default coordination backends, not part of the public facade.
  - `CacheEntry` (version ≥ 1, 0 reserved), `CacheConsistency`, `CacheEvent` (key-only Changed/Deleted/Expired), `CacheWatchEvent` union (Event/Lagged/Reset/Closed), `CacheWatch`, `CacheFeatures`, `CacheCapability`.
  - `CacheResolverBuilder` (profile/require/resolve) and `validate_cache_capabilities`; the canonical resolution + startup capability-validation pattern.
  - Per-trait dyn-compat assertion.

- **Out of scope**:
  - SDK default backends built on cache (separate feature).
  - The prefix-watch polyfill and scoping wrappers (separate features).

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-clst-fr-cache-storage`
  - [x] `p1` - `cpt-cf-clst-fr-cache-atomic`
  - [x] `p1` - `cpt-cf-clst-fr-cache-ttl`
  - [x] `p1` - `cpt-cf-clst-fr-cache-watch`
  - [x] `p1` - `cpt-cf-clst-fr-validation-capability-declarations`
  - [x] `p1` - `cpt-cf-clst-fr-validation-honest-declaration`
  - [x] `p1` - `cpt-cf-clst-fr-validation-startup-fail`
  - [x] `p1` - `cpt-cf-clst-fr-watch-lifecycle-signals`
  - [x] `p1` - `cpt-cf-clst-nfr-capability-validation`
  - [x] `p1` - `cpt-cf-clst-nfr-watch-delivery`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-clst-principle-cas-universal`
  - [x] `p1` - `cpt-cf-clst-principle-facade-plus-backend-trait`
  - [x] `p1` - `cpt-cf-clst-principle-lightweight-notifications`
  - [x] `p1` - `cpt-cf-clst-principle-version-based-cas`
  - [x] `p1` - `cpt-cf-clst-principle-watch-union-shape`

- **Domain Model Entities**:
  - ClusterCacheV1
  - ClusterCacheBackend
  - CacheEntry
  - CacheConsistency
  - CacheEvent
  - CacheWatchEvent
  - CacheWatch
  - CacheFeatures
  - CacheCapability

- **Design Components**:

  - [x] `p1` - `cpt-cf-clst-component-sdk`

- **API**:
  - Rust: `ClusterCacheV1::resolver(hub).profile(P).require(CacheCapability::..).resolve()`; `get/put/delete/contains/put_if_absent/compare_and_swap/watch/watch_prefix`.

- **Sequences**:

  - [x] `p1` - `cpt-cf-clst-seq-per-primitive-resolution`

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.3 [Leader Election Primitive](features/003-leader-election.md) ✅ MEDIUM

- [x] `p2` - **ID**: `cpt-cf-clst-feature-leader-election`

- **Purpose**: Provide named single-leader election with automatic renewal, configurable failover timing, dual observability (event-driven `changed()` and gate-driven `status()`/`is_leader()`), graceful step-down, and explicitly advisory semantics. Reuses the watch-union shape from the cache primitive.

- **Depends On**: `cpt-cf-clst-feature-sdk-foundation`, `cpt-cf-clst-feature-cache-primitive`

- **Scope**:
  - `LeaderElectionBackend` trait and `LeaderElectionV1` facade (elect/elect_with_config; resolver/scoped).
  - `LeaderStatus` (Leader/Follower/Lost; Lost is transient), `LeaderWatch` (changed/status/is_leader/resign; no-op Drop), `LeaderWatchEvent` union, `ElectionConfig` (validates ttl & max_missed_renewals > 0; derives renewal interval), `LeaderElectionCapability`, `LeaderElectionFeatures`.
  - `LeaderElectionResolverBuilder` and `validate_leader_election_capabilities`; per-trait dyn-compat assertion.

- **Out of scope**:
  - The CAS-based default leader-election backend (separate SDK-default-backends feature).
  - Shutdown revocation orchestration (wiring; follow-up).

- **Requirements Covered**:

  - [x] `p2` - `cpt-cf-clst-fr-leader-elect`
  - [x] `p2` - `cpt-cf-clst-fr-leader-config`
  - [x] `p2` - `cpt-cf-clst-fr-leader-observability`
  - [x] `p2` - `cpt-cf-clst-fr-leader-resign`
  - [x] `p2` - `cpt-cf-clst-fr-leader-advisory`

- **Domain Model Entities**:
  - LeaderElectionV1
  - LeaderElectionBackend
  - LeaderStatus
  - LeaderWatch
  - LeaderWatchEvent
  - ElectionConfig
  - LeaderElectionCapability
  - LeaderElectionFeatures

- **API**:
  - Rust: `LeaderElectionV1::resolver(hub)...resolve()`; `elect/elect_with_config`; `LeaderWatch::{changed,status,is_leader,resign}`.

- **Sequences**: None net-new (reuses `cpt-cf-clst-seq-per-primitive-resolution`).

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.4 [Distributed Lock Primitive](features/004-distributed-lock.md) ✅ MEDIUM

- [x] `p2` - **ID**: `cpt-cf-clst-feature-distributed-lock`

- **Purpose**: Provide TTL-bounded distributed locks with non-blocking and blocking-with-timeout acquisition, explicit async release, and TTL extension. No fencing tokens and a no-op `Drop` — the no-remote-in-critical-section rule (enforced by a separate lint feature) eliminates the stale-writer scenario.

- **Depends On**: `cpt-cf-clst-feature-sdk-foundation`, `cpt-cf-clst-feature-cache-primitive`

- **Scope**:
  - `DistributedLockBackend` trait and `DistributedLockV1` facade (try_lock/lock; resolver/scoped).
  - `LockGuard` (renew/release; no-op Drop), lock error variants (`LockContended`/`LockTimeout`/`LockExpired`), `LockCapability`, `LockFeatures`.
  - `LockResolverBuilder` and `validate_lock_capabilities`; per-trait dyn-compat assertion.

- **Out of scope**:
  - The CAS-based default lock backend (separate SDK-default-backends feature).
  - The compile-time no-remote-in-critical-section lint (separate lint feature).

- **Requirements Covered**:

  - [x] `p2` - `cpt-cf-clst-fr-lock-acquire`
  - [x] `p2` - `cpt-cf-clst-fr-lock-release`

- **Domain Model Entities**:
  - DistributedLockV1
  - DistributedLockBackend
  - LockGuard
  - LockCapability
  - LockFeatures

- **API**:
  - Rust: `DistributedLockV1::resolver(hub)...resolve()`; `try_lock/lock`; `LockGuard::{renew,release}`.

- **Sequences**: None net-new (reuses `cpt-cf-clst-seq-per-primitive-resolution`).

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.5 [Service Discovery Primitive](features/005-service-discovery.md) ✅ MEDIUM

- [x] `p2` - **ID**: `cpt-cf-clst-feature-service-discovery`

- **Purpose**: Provide instance registration with metadata, a single extensible discovery filter (state + AND-conjoined metadata predicates, enabled-only default), an unfiltered topology watch with lifecycle signals, and a module-declared serving-intent signal (`InstanceState`) that is explicitly intent, not health observation.

- **Depends On**: `cpt-cf-clst-feature-sdk-foundation`, `cpt-cf-clst-feature-cache-primitive`

- **Scope**:
  - `ServiceDiscoveryBackend` trait and `ServiceDiscoveryV1` facade (register/discover/watch; resolver/scoped).
  - `ServiceRegistration`, `ServiceInstance`, `InstanceState` (Enabled/Disabled — intent, not health), `MetaMatch`, `DiscoveryFilter` (default enabled-only), `StateFilter`, `TopologyChange`, `ServiceWatch`, `ServiceWatchEvent` union, `ServiceHandle` (deregister/update_metadata/set_state; no-op Drop), `ServiceDiscoveryCapability`, `ServiceDiscoveryFeatures`.
  - `ServiceDiscoveryResolverBuilder` and `validate_service_discovery_capabilities`; per-trait dyn-compat assertion.

- **Out of scope**:
  - The cache-based default service-discovery backend (separate SDK-default-backends feature).
  - Metadata-key namespacing exclusion enforcement in scoping wrappers (handled in the scoping feature).
  - External health probing / liveness observation (out of scope per PRD §4.2).

- **Requirements Covered**:

  - [x] `p2` - `cpt-cf-clst-fr-sd-register`
  - [x] `p2` - `cpt-cf-clst-fr-sd-discover`
  - [x] `p2` - `cpt-cf-clst-fr-sd-watch`
  - [x] `p2` - `cpt-cf-clst-fr-sd-state`

- **Domain Model Entities**:
  - ServiceDiscoveryV1
  - ServiceDiscoveryBackend
  - ServiceRegistration
  - ServiceInstance
  - InstanceState
  - MetaMatch
  - DiscoveryFilter
  - StateFilter
  - TopologyChange
  - ServiceWatch
  - ServiceWatchEvent
  - ServiceHandle
  - ServiceDiscoveryCapability
  - ServiceDiscoveryFeatures

- **API**:
  - Rust: `ServiceDiscoveryV1::resolver(hub)...resolve()`; `register/discover/watch`; `ServiceHandle::{deregister,update_metadata,set_state}`.

- **Sequences**: None net-new (reuses `cpt-cf-clst-seq-per-primitive-resolution`).

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.6 [SDK Default Backends](features/006-sdk-default-backends.md) ✅ LOW

- [x] `p3` - **ID**: `cpt-cf-clst-feature-sdk-default-backends`

- **Purpose**: Ship the "implement cache only → get all four primitives" guarantee. Provides CAS-based default implementations of leader election, distributed lock, and service discovery built entirely on `Arc<dyn ClusterCacheBackend>`, with the safety constructor pair that rejects eventually-consistent caches by default for the consistency-sensitive primitives.

- **Depends On**: `cpt-cf-clst-feature-cache-primitive`, `cpt-cf-clst-feature-leader-election`, `cpt-cf-clst-feature-distributed-lock`, `cpt-cf-clst-feature-service-discovery`

- **Scope**:
  - `CasBasedLeaderElectionBackend`, `CasBasedDistributedLockBackend`, `CacheBasedServiceDiscoveryBackend`.
  - Constructor pair on the two consistency-sensitive backends: `new()` rejects `EventuallyConsistent` → `InvalidConfig`; `new_allow_weak_consistency()` always succeeds + warn-log. Service discovery uses a single `new()`.

- **Out of scope**:
  - The wiring-crate omit-primitive auto-wrap selection logic (follow-up).
  - Native per-backend overrides (per-plugin follow-ups).

- **Requirements Covered**:

  - [x] `p3` - `cpt-cf-clst-fr-routing-cache-only-plugin`
  - [x] `p3` - `cpt-cf-clst-nfr-leader-guarantee`

- **Domain Model Entities**:
  - CasBasedLeaderElectionBackend
  - CasBasedDistributedLockBackend
  - CacheBasedServiceDiscoveryBackend

- **API**:
  - Rust: `CasBasedLeaderElectionBackend::{new,new_allow_weak_consistency}`; `CasBasedDistributedLockBackend::{new,new_allow_weak_consistency}`; `CacheBasedServiceDiscoveryBackend::new`.

- **Sequences**: None net-new.

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.7 [Per-primitive Scoping & Prefix-Watch Polyfill](features/007-scoping-polyfill.md) ✅ LOW

- [x] `p3` - **ID**: `cpt-cf-clst-feature-scoping-polyfill`

- **Purpose**: Let consumers carve composable sub-namespaces inside any primitive without manual prefixing, and synthesize prefix-watch semantics on backends that lack native support. Scoping applies to coordination names only; service-discovery metadata keys/values pass through unchanged.

- **Depends On**: `cpt-cf-clst-feature-cache-primitive`, `cpt-cf-clst-feature-leader-election`, `cpt-cf-clst-feature-distributed-lock`, `cpt-cf-clst-feature-service-discovery`

- **Scope**:
  - `ScopedCacheBackend`, `ScopedLeaderElectionBackend`, `ScopedDistributedLockBackend`, `ScopedServiceDiscoveryBackend` delegating wrappers (prefix on write, strip on read; composable).
  - Service-discovery scoping applies to `name` only — metadata keys/values are never renamed.
  - `PollingPrefixWatch::spawn(...)` synthesizing `watch_prefix` on backends declaring `prefix_watch == false`.

- **Out of scope**:
  - Native prefix-watch backend implementations (per-plugin follow-ups).

- **Requirements Covered**:

  - [x] `p3` - `cpt-cf-clst-fr-namespacing-scoped`
  - [x] `p3` - `cpt-cf-clst-fr-namespacing-sd-metadata-unscoped`

- **Domain Model Entities**:
  - ScopedCacheBackend
  - ScopedLeaderElectionBackend
  - ScopedDistributedLockBackend
  - ScopedServiceDiscoveryBackend
  - PollingPrefixWatch

- **API**:
  - Rust: `*V1::scoped(prefix)`; `PollingPrefixWatch::spawn(cache, prefix, interval) -> CacheWatch`.

- **Sequences**: None net-new.

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.8 [Watch Auto-Restart Combinator](features/008-watch-auto-restart.md) ✅ LOW

- [x] `p3` - **ID**: `cpt-cf-clst-feature-watch-auto-restart`

- **Purpose**: Ship one canonical, opt-in watch-restart combinator for all three watch types so consumers do not each reinvent reconnect loops with inconsistent backoff and retryability classification. Turns retryable terminal closes into transparent reconnection with backoff, synthesizes `Reset` on resubscribe, and propagates non-retryable closes unchanged.

- **Depends On**: `cpt-cf-clst-feature-cache-primitive`, `cpt-cf-clst-feature-leader-election`, `cpt-cf-clst-feature-service-discovery`

- **Scope**:
  - `RetryPolicy` (initial/max backoff, jitter, optional retry cap; `default()` = 1s→30s, full jitter, no cap).
  - `RestartingWatch<W>` for `CacheWatch`/`LeaderWatch`/`ServiceWatch` via `*Watch::auto_restart(policy)`; retryability read from `ProviderErrorKind` per the DESIGN §3.9 table.

- **Out of scope**:
  - The raw watch event types themselves (defined in the per-primitive features).

- **Requirements Covered**:

  - [x] `p3` - `cpt-cf-clst-fr-watch-auto-restart`

- **Domain Model Entities**:
  - RetryPolicy
  - RestartingWatch

- **API**:
  - Rust: `*Watch::auto_restart(policy: RetryPolicy) -> RestartingWatch<_>`.

- **Sequences**: None net-new.

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.9 [Registration Helpers, GTS Spec & Observability Contract](features/009-registration-observability.md) ✅ MEDIUM

- [x] `p2` - **ID**: `cpt-cf-clst-feature-registration-observability`

- **Purpose**: Provide the ClientHub registration/deregistration helpers (per profile per primitive) that the cluster gear's wiring (`ClusterWiring`, brought into this change per the scope note above) composes, the GTS plugin-spec scaffolding that lets follow-up plugins register and be discovered, and the versioned observability naming contract (span/metric/log names plus the cardinality rule) that every follow-up plugin must emit against.

- **Depends On**: `cpt-cf-clst-feature-cache-primitive`, `cpt-cf-clst-feature-leader-election`, `cpt-cf-clst-feature-distributed-lock`, `cpt-cf-clst-feature-service-discovery`

- **Scope**:
  - `register_*_backend` / `deregister_*_backend` ClientHub helpers (scoped via `profile_scope`).
  - GTS plugin-spec scaffolding (`gts_type_schema`) for the cluster plugin contract.
  - Observability naming module (stable span/metric/log-event name constants) + the versioned observability reference doc, including the cardinality rule (operation keys / lock names / election names never appear as metric labels).

- **Out of scope**:
  - Runtime signal emission by concrete backends (per-plugin follow-ups). (The wiring orchestration that calls these helpers is no longer a follow-up — it ships in this change as `ClusterWiring`; see the scope note above.)

- **Requirements Covered**:

  - [x] `p2` - `cpt-cf-clst-nfr-observability`

- **Design Principles Covered**:

  - [x] `p2` - `cpt-cf-clst-principle-per-primitive-routing`

- **Domain Model Entities**:
  - register/deregister backend helpers (per primitive)
  - Cluster plugin GTS spec
  - Observability naming constants

- **Design Components**:

  - [x] `p2` - `cpt-cf-clst-component-sdk`

- **API**:
  - Rust: `register_cache_backend(hub, profile, backend)` and siblings; `deregister_*_backend(...)`; observability name constants module.

- **Sequences**: None net-new.

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.10 [Lock-Misuse Lint (no-remote-in-critical-section)](features/010-lock-lint.md) ✅ LOW

- [x] `p3` - **ID**: `cpt-cf-clst-feature-lock-lint`

- **Purpose**: Make the no-remote-I/O-in-critical-section rule enforceable rather than aspirational, via a workspace architecture lint rule (via `cargo gears lint`) that flags cross-instance remote calls inside a cluster lock's critical section at compile time. Sequenced after the lock primitive so the lint has real `try_lock`/`release` scopes to target.

- **Depends On**: `cpt-cf-clst-feature-distributed-lock`

- **Scope**:
  - New architecture lint rule in the `cargo-gears` CLI (e.g. `de14_cluster/de14XX_no_remote_in_critical_section/`); modeled on the existing `de0707_drop_zeroize` lint.
  - Lint scope restricted to the four cluster backend traits within `try_lock`/`release` scopes (DB-tx enforcement is a follow-up rule extension).

- **Out of scope**:
  - DB-transaction critical-section enforcement (follow-up rule extension).

- **Requirements Covered**:

  - [x] `p3` - `cpt-cf-clst-fr-lock-no-remote`
  - [x] `p3` - `cpt-cf-clst-nfr-bounded-critical-section`

- **Design Constraints Covered**:

  - [x] `p3` - `cpt-cf-clst-constraint-no-remote-in-critical-section`

- **Domain Model Entities**:
  - None (architecture lint rule; no domain entities).

- **API**:
  - Lint: `DE14XX_NO_REMOTE_IN_CRITICAL_SECTION` (Deny).

- **Sequences**: None net-new.

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.11 [Smoke Tests (in-process stub backends)](features/011-smoke-tests.md) ✅ MEDIUM

- [x] `p2` - **ID**: `cpt-cf-clst-feature-smoke-tests`

- **Purpose**: Verify the SDK contract end-to-end against minimal in-process stub backends with no external infrastructure — exercising resolution, capability-mismatch failure, every watch lifecycle variant, CAS conflict, single-leader-under-contention, lock release-on-timeout, scoping, and the polyfill. Establishes the cross-backend behavioral baseline.

- **Depends On**: `cpt-cf-clst-feature-cache-primitive`, `cpt-cf-clst-feature-leader-election`, `cpt-cf-clst-feature-distributed-lock`, `cpt-cf-clst-feature-service-discovery`, `cpt-cf-clst-feature-sdk-default-backends`, `cpt-cf-clst-feature-scoping-polyfill`, `cpt-cf-clst-feature-watch-auto-restart`

- **Scope**:
  - Minimal in-process `MemCacheBackend` and sibling stubs (explicitly NOT production backends).
  - Tests: per-primitive resolution; capability-mismatch startup failure; watch Lagged/Reset/Closed variants; CAS conflict; single-leader under contention; lock release-on-timeout; scoping; prefix-watch polyfill.

- **Out of scope**:
  - Distributed-correctness verification under partition/clock-skew (per-plugin integration tests; follow-up).
  - The production in-process plugin with TTL reapers and broadcast watches — these stub-based smoke tests deliberately do not exercise it. (The standalone plugin itself ships in this change under its own feature, per the scope note above; it is just not the subject of these tests.)

- **Requirements Covered**:

  - [x] `p2` - `cpt-cf-clst-nfr-cross-backend-stability`

- **Domain Model Entities**:
  - MemCacheBackend (test stub)

- **API**:
  - None (test-only crate/module).

- **Sequences**: None net-new.

- **Data**: None — no persistent schema (see DESIGN §3.14).

### 2.12 [Showcase Examples & Traceability Audit](features/012-showcase-audit.md) ✅ LOW

- [x] `p4` - **ID**: `cpt-cf-clst-feature-showcase-audit`

- **Purpose**: Demonstrate the canonical consumer patterns (single-primitive, multi-primitive, multi-profile) and the plugin-author builder/handle shape, and close the change with the pre-archive documentation/traceability audit that verifies every FR/NFR maps to a DESIGN/ADR and that code `cpt-*` markers are wired.

- **Depends On**: `cpt-cf-clst-feature-sdk-default-backends`, `cpt-cf-clst-feature-scoping-polyfill`, `cpt-cf-clst-feature-watch-auto-restart`, `cpt-cf-clst-feature-registration-observability`, `cpt-cf-clst-feature-smoke-tests`

- **Scope**:
  - Showcase example crates: single-primitive, multi-primitive, multi-profile, plugin-author (builder/handle) shape.
  - Resolve the two PRD/DESIGN open questions (ADR-003 generalization note).
  - Pre-archive traceability audit: every FR/NFR → DESIGN/ADR mapping checked; `cpt-*` code markers wired.

- **Out of scope**:
  - Net-new contract surface (this feature only demonstrates and audits existing features).

- **Requirements Covered**:
  - None net-new — this feature demonstrates and audits the requirements delivered by features 2.1–2.11 (notably capability declaration and startup-fail behavior) and verifies end-to-end traceability against the PRD Acceptance Criteria.

- **Domain Model Entities**:
  - None (example crates + audit; no new contract types).

- **API**:
  - None (example crates).

- **Sequences**: None net-new.

- **Data**: None — no persistent schema (see DESIGN §3.14).

## 3. Feature Dependencies

```text
cpt-cf-clst-feature-sdk-foundation
    ↓
cpt-cf-clst-feature-cache-primitive
    ├─→ cpt-cf-clst-feature-leader-election
    ├─→ cpt-cf-clst-feature-distributed-lock
    ├─→ cpt-cf-clst-feature-service-discovery
    └─→ cpt-cf-clst-feature-scoping-polyfill
            │
   (leader + lock + service-discovery)
            ├─→ cpt-cf-clst-feature-sdk-default-backends
            ├─→ cpt-cf-clst-feature-registration-observability
            └─→ cpt-cf-clst-feature-watch-auto-restart   (cache + leader + service-discovery)

cpt-cf-clst-feature-distributed-lock
    └─→ cpt-cf-clst-feature-lock-lint

(default-backends + scoping-polyfill + watch-auto-restart + the four primitives)
    └─→ cpt-cf-clst-feature-smoke-tests
            └─→ cpt-cf-clst-feature-showcase-audit
                    (also depends on registration-observability)
```

**Dependency Rationale**:

- `cpt-cf-clst-feature-cache-primitive` requires `cpt-cf-clst-feature-sdk-foundation`: the cache facade, resolver, and watch types are built on the shared error model, profile marker, and dyn-compat harness.
- `cpt-cf-clst-feature-leader-election`, `cpt-cf-clst-feature-distributed-lock`, and `cpt-cf-clst-feature-service-discovery` each require the foundation and the cache primitive (they reuse the watch-union shape, resolver pattern, and capability-validation mechanism), and are mutually independent — they can be developed in parallel.
- `cpt-cf-clst-feature-scoping-polyfill` requires the cache primitive (and the other primitive facades it wraps): scoping wrappers and the prefix-watch polyfill delegate to the primitive backends.
- `cpt-cf-clst-feature-sdk-default-backends` requires the cache primitive plus all three other primitive traits: the CAS/cache-based defaults implement those traits over `Arc<dyn ClusterCacheBackend>`.
- `cpt-cf-clst-feature-watch-auto-restart` requires the cache, leader-election, and service-discovery features: the combinator wraps their watch types.
- `cpt-cf-clst-feature-registration-observability` requires all four primitive traits: the register/deregister helpers and observability names are keyed per primitive.
- `cpt-cf-clst-feature-lock-lint` requires `cpt-cf-clst-feature-distributed-lock`: the lint targets `try_lock`/`release` scopes that only exist once the lock primitive lands.
- `cpt-cf-clst-feature-smoke-tests` requires the primitives plus default backends, scoping/polyfill, and the auto-restart combinator: the stubs exercise the full contract surface.
- `cpt-cf-clst-feature-showcase-audit` depends on the verified contract (smoke tests) plus default backends, scoping, auto-restart, and registration/observability: examples consume the complete public surface and the audit verifies end-to-end traceability.
