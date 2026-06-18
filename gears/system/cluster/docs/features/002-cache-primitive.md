# Feature: Distributed Cache Primitive

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-cache-primitive-implemented`

- [x] `p2` - `cpt-cf-clst-feature-cache-primitive`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Resolve Cache with Capability Validation](#resolve-cache-with-capability-validation)
  - [Versioned Compare-and-Swap Update](#versioned-compare-and-swap-update)
  - [Watch and Recover on Lifecycle Signals](#watch-and-recover-on-lifecycle-signals)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Validate Cache Capabilities](#validate-cache-capabilities)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Cache Backend Trait and Facade](#cache-backend-trait-and-facade)
  - [Cache Domain Types](#cache-domain-types)
  - [Resolver and Capability Validation](#resolver-and-capability-validation)
  - [Watch Union and Reactive Notifications](#watch-union-and-reactive-notifications)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Delivers the distributed cache primitive — the universal compare-and-swap building block on which leader election, locks, and service discovery are built. It provides versioned key-value storage, atomic conditional operations, TTL, and reactive key/prefix notifications, plus the canonical watch-union event shape and the per-primitive fluent resolver with startup capability validation that every other primitive reuses.

### 1.2 Purpose

Cross-instance coordination in cluster is built on optimistic concurrency over a versioned cache. This feature defines that foundation and the canonical resolution + capability-validation pattern, so consumers get a working, capability-checked cache and the other primitives can be layered on cache operations.

**Requirements**: `cpt-cf-clst-fr-cache-storage`, `cpt-cf-clst-fr-cache-atomic`, `cpt-cf-clst-fr-cache-ttl`, `cpt-cf-clst-fr-cache-watch`, `cpt-cf-clst-fr-validation-capability-declarations`, `cpt-cf-clst-fr-validation-honest-declaration`, `cpt-cf-clst-fr-validation-startup-fail`, `cpt-cf-clst-fr-watch-lifecycle-signals`, `cpt-cf-clst-nfr-capability-validation`, `cpt-cf-clst-nfr-watch-delivery`

**Principles**: `cpt-cf-clst-principle-cas-universal`, `cpt-cf-clst-principle-facade-plus-backend-trait`, `cpt-cf-clst-principle-lightweight-notifications`, `cpt-cf-clst-principle-version-based-cas`, `cpt-cf-clst-principle-watch-union-shape`

This feature realizes the per-primitive resolution sequence (DESIGN §3.13) and the in-scope `component-sdk`; those links are tracked in `DECOMPOSITION.md` per the kit reference rules.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-platform-gear` | Resolves the cache and uses it for application-level shared state |
| `cpt-cf-clst-actor-oagw` | Uses high-throughput CAS for shared counters |
| `cpt-cf-clst-actor-event-broker` | Publishes shard-assignment state and watches by prefix |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.1
- **Design**: [DESIGN.md](../DESIGN.md) §3.3 (cache contract), §3.9 (watch event shape), §3.6 (resolution), §3.10 (capability validation), §3.1 (entities)
- **ADRs**: [ADR-001](../ADR/001-provider-compatibility-and-performance.md), [ADR-003](../ADR/003-watch-event-lifecycle-contract.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-sdk-foundation`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — backend-determined: throughput and latency depend on the bound cache backend (ADR-001); this contract specifies versioned CAS and watch semantics, not performance targets.
- Reliability — addressed: watch recovery on lagged/reset/closed signals and CAS-conflict re-read/retry (§2) define how consumers stay correct under concurrent writes and subscription loss.

## 2. Actor Flows (CDSL)

### Resolve Cache with Capability Validation

- [x] `p1` - **ID**: `cpt-cf-clst-flow-cache-primitive-resolve`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- The consumer declares its required capabilities and receives a working cache facade.

**Error Scenarios**:
- The bound backend cannot meet a declared capability — startup fails with a specific, actionable error.
- No backend is bound for the profile — resolution fails with profile-not-bound.

**Steps**:
1. [x] - `p1` - Consumer starts the cache resolver and supplies a profile marker - `inst-res-profile`
2. [x] - `p1` - Consumer declares required capabilities (e.g. linearizable, prefix-watch) - `inst-res-require`
3. [x] - `p1` - SDK looks up the bound cache backend for the profile scope - `inst-res-lookup`
4. [x] - `p1` - **IF** no backend is bound - `inst-res-unbound`
   1. [x] - `p1` - **RETURN** a profile-not-bound error - `inst-res-unbound-return`
5. [x] - `p1` - Validate declared capabilities against the backend's consistency and features - `inst-res-validate`
6. [x] - `p1` - **IF** a declared capability is unmet - `inst-res-unmet`
   1. [x] - `p1` - **RETURN** a capability-not-met error naming primitive, capability, and provider - `inst-res-unmet-return`
7. [x] - `p1` - **RETURN** the wrapped cache facade - `inst-res-return`

### Versioned Compare-and-Swap Update

- [x] `p1` - **ID**: `cpt-cf-clst-flow-cache-primitive-cas-update`

**Actor**: `cpt-cf-clst-actor-oagw`

**Success Scenarios**:
- A read-modify-write completes atomically using the prior version.

**Error Scenarios**:
- A concurrent writer changed the value — the consumer re-reads and retries.

**Steps**:
1. [x] - `p1` - Consumer reads the current versioned entry for the key - `inst-cas-get`
2. [x] - `p1` - Consumer computes the new value from the current one - `inst-cas-compute`
3. [x] - `p1` - Consumer requests compare-and-swap using the prior version - `inst-cas-swap`
4. [x] - `p1` - **IF** the stored version no longer matches - `inst-cas-conflict`
   1. [x] - `p1` - Surface a CAS-conflict carrying the current entry when cheaply available - `inst-cas-conflict-return`
   2. [x] - `p1` - **RETURN** so the consumer re-reads and retries from step 2 - `inst-cas-retry`
5. [x] - `p1` - **RETURN** the new versioned entry; emit a changed notification - `inst-cas-return`

### Watch and Recover on Lifecycle Signals

- [x] `p1` - **ID**: `cpt-cf-clst-flow-cache-primitive-watch-recover`

**Actor**: `cpt-cf-clst-actor-event-broker`

**Success Scenarios**:
- The watcher reacts to key/prefix changes without polling and recovers cleanly after falling behind.

**Error Scenarios**:
- The subscription is re-established or the watcher lags — the consumer re-reads to recover.
- The watch ends terminally — the consumer stops.

**Steps**:
1. [x] - `p1` - Consumer subscribes to an exact key or a key prefix - `inst-w-subscribe`
2. [x] - `p1` - Consumer awaits the next watch event (key-only; no value payload) - `inst-w-next`
3. [x] - `p1` - **IF** the event is a change/delete/expiry - `inst-w-change`
   1. [x] - `p1` - Consumer reads the current value for the affected key if needed - `inst-w-reread`
4. [x] - `p1` - **IF** the event is lagged or reset - `inst-w-lag`
   1. [x] - `p1` - Consumer treats watched keys as stale and re-reads current state - `inst-w-recover`
5. [x] - `p1` - **IF** the event is a terminal close - `inst-w-closed`
   1. [x] - `p1` - **RETURN** and stop consuming the watch - `inst-w-stop`

## 3. Processes / Business Logic (CDSL)

### Validate Cache Capabilities

- [x] `p1` - **ID**: `cpt-cf-clst-algo-cache-primitive-validate-capabilities`

**Input**: The bound cache backend and a list of declared capability requirements

**Output**: Success, or a capability-not-met error

**Steps**:
1. [x] - `p1` - **FOR EACH** declared capability - `inst-vc-foreach`
   1. [x] - `p1` - **IF** the capability is linearizable and the backend's consistency is not linearizable - `inst-vc-lin`
      1. [x] - `p1` - **RETURN** capability-not-met (primitive, capability, provider) - `inst-vc-lin-return`
   2. [x] - `p1` - **IF** the capability is prefix-watch and the backend does not declare prefix-watch support - `inst-vc-pw`
      1. [x] - `p1` - **RETURN** capability-not-met (primitive, capability, provider) - `inst-vc-pw-return`
2. [x] - `p1` - **RETURN** success - `inst-vc-ok`

## 4. States (CDSL)

Not applicable — cache entries are versioned values without a lifecycle state machine; watch lifecycle signals are handled as flow steps, not entity states.

## 5. Definitions of Done

### Cache Backend Trait and Facade

- [x] `p1` - **ID**: `cpt-cf-clst-dod-cache-primitive-backend-facade`

The system **MUST** provide the cache backend trait and the cache facade with the full operation set (get, put, delete, contains, put-if-absent, compare-and-swap, watch, watch-prefix) plus synchronous accessors for consistency and features, and the resolver entry point. The backend trait **MUST** be dyn-compatible. Per-primitive scoping (`scoped()`) is delivered separately by the scoping & polyfill feature, which owns the `ScopedCacheBackend` wrapper.

**Implements**:
- `cpt-cf-clst-flow-cache-primitive-cas-update`

**Constraints**: `cpt-cf-clst-constraint-dyn-compat`

**Touches**:
- Entities: ClusterCacheBackend, ClusterCacheV1

### Cache Domain Types

- [x] `p1` - **ID**: `cpt-cf-clst-dod-cache-primitive-types`

The system **MUST** provide the cache value type with a monotonically increasing version (starting at 1; 0 reserved), the consistency class, the key-only event type (changed/deleted/expired), the features descriptor, and the capability requirement enum. TTL-bounded storage **MUST** auto-remove values and notify watchers; values without TTL persist until deleted, with backend-specific persistence limits documented.

**Implements**:
- `cpt-cf-clst-flow-cache-primitive-cas-update`

**Touches**:
- Entities: CacheEntry, CacheConsistency, CacheEvent, CacheFeatures, CacheCapability

### Resolver and Capability Validation

- [x] `p1` - **ID**: `cpt-cf-clst-dod-cache-primitive-resolver`

The system **MUST** provide the fluent cache resolver and the capability-validation helper such that a declared requirement unmet by the bound backend fails resolution with a specific capability-not-met error naming the primitive, the requirement, and the provider — at startup, not at runtime.

**Implements**:
- `cpt-cf-clst-flow-cache-primitive-resolve`
- `cpt-cf-clst-algo-cache-primitive-validate-capabilities`

**Touches**:
- Entities: CacheResolverBuilder

### Watch Union and Reactive Notifications

- [x] `p1` - **ID**: `cpt-cf-clst-dod-cache-primitive-watch`

The system **MUST** provide exact-key and prefix watches that yield the cache watch-union events (event, lagged, reset, closed), preserve per-key ordering, deliver at most once, and carry only the affected key. Backends lacking native prefix subscriptions **MUST** declare that limitation so callers can honor it or polyfill.

**Implements**:
- `cpt-cf-clst-flow-cache-primitive-watch-recover`

**Touches**:
- Entities: CacheWatch, CacheWatchEvent

## 6. Acceptance Criteria

- [x] A consumer resolves the cache for a profile, declaring capabilities, and receives a working facade or a specific capability-not-met error at startup.
- [x] Get/put/delete/contains/put-if-absent/compare-and-swap behave atomically; CAS conflicts surface the current entry for retry.
- [x] Values carry a monotonically increasing version starting at 1; TTL-bounded values are auto-removed and notify watchers.
- [x] Exact-key and prefix watches yield key-only events with preserved per-key ordering and surface lagged/reset/closed signals.
- [x] A backend without native prefix-watch support declares the limitation rather than silently misbehaving.
