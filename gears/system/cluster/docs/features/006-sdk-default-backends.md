# Feature: SDK Default Backends

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-sdk-default-backends-implemented`

- [x] `p2` - `cpt-cf-clst-feature-sdk-default-backends`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Cache-Only Plugin Gets All Four Primitives](#cache-only-plugin-gets-all-four-primitives)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [CAS-Based Leader Election](#cas-based-leader-election)
  - [CAS-Based Distributed Lock](#cas-based-distributed-lock)
  - [Cache-Based Service Discovery](#cache-based-service-discovery)
  - [Constructor Safety Guard](#constructor-safety-guard)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [CAS-Based Leader-Election Default Backend](#cas-based-leader-election-default-backend)
  - [CAS-Based Distributed-Lock Default Backend](#cas-based-distributed-lock-default-backend)
  - [Cache-Based Service-Discovery Default Backend](#cache-based-service-discovery-default-backend)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Delivers the "implement cache only, get all four primitives" guarantee. Provides compare-and-swap-based default implementations of leader election and distributed lock and a cache-based default implementation of service discovery, all built on the cache backend, with a safety constructor pair that rejects eventually-consistent caches by default for the consistency-sensitive primitives.

### 1.2 Purpose

Lowering the barrier to integrating a new backend means a plugin author should only have to implement the cache. This feature builds the other three primitives on cache operations so a minimal plugin works end-to-end, while preventing silent split-brain by gating weak-consistency use behind an explicit opt-in.

**Requirements**: `cpt-cf-clst-fr-routing-cache-only-plugin`, `cpt-cf-clst-nfr-leader-guarantee`

**Principles**: `cpt-cf-clst-principle-cas-universal`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-plugin-author` | Implements only the cache backend and gets all four primitives |
| `cpt-cf-clst-actor-platform-gear` | Consumes the default-backed primitives transparently |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.5
- **Design**: [DESIGN.md](../DESIGN.md) §3.11 (SDK default backends, constructor pair), §4.1 (per-backend strategy)
- **ADRs**: [ADR-001](../ADR/001-provider-compatibility-and-performance.md), [ADR-009](../ADR/009-leader-election-backend-safety.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-cache-primitive`
  - [x] `p2` - `cpt-cf-clst-feature-leader-election`
  - [x] `p2` - `cpt-cf-clst-feature-distributed-lock`
  - [x] `p2` - `cpt-cf-clst-feature-service-discovery`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — backend-determined: the default backends are built on cache operations, so throughput depends on the underlying cache backend (ADR-001).
- Reliability — addressed: the constructor safety guard that rejects an eventually-consistent cache by default, TTL reaping, and conditional release (§3) prevent split-brain and stale holders.

## 2. Actor Flows (CDSL)

### Cache-Only Plugin Gets All Four Primitives

- [x] `p1` - **ID**: `cpt-cf-clst-flow-sdk-default-backends-cache-only`

**Actor**: `cpt-cf-clst-actor-plugin-author`

**Success Scenarios**:
- A plugin implementing only the cache backend obtains working leader election, lock, and service discovery via SDK defaults.

**Error Scenarios**:
- The cache is eventually consistent and a default for a consistency-sensitive primitive is constructed without opt-in — construction is rejected.

**Steps**:
1. [x] - `p1` - Plugin author implements only the cache backend - `inst-co-cache`
2. [x] - `p1` - SDK wraps the cache backend in the default leader-election, lock, and service-discovery backends - `inst-co-wrap`
3. [x] - `p1` - **IF** a consistency-sensitive default is constructed over an eventually-consistent cache without opt-in - `inst-co-guard`
   1. [x] - `p1` - **RETURN** an invalid-config error - `inst-co-reject`
4. [x] - `p1` - **RETURN** four working primitives backed by the single cache implementation - `inst-co-return`

## 3. Processes / Business Logic (CDSL)

### CAS-Based Leader Election

- [x] `p1` - **ID**: `cpt-cf-clst-algo-sdk-default-backends-cas-election`

**Input**: A cache backend and an election name

**Output**: Single-leader behavior over cache operations

**Steps**:
1. [x] - `p1` - Use insert-if-absent on the election key to claim candidacy - `inst-ce-claim`
2. [x] - `p1` - Watch the election key for status changes - `inst-ce-watch`
3. [x] - `p1` - Renew the claim on the derived renewal interval - `inst-ce-renew`
4. [x] - `p1` - **IF** the key expires or is lost - `inst-ce-lost`
   1. [x] - `p1` - Emit leadership-lost and auto-reenroll - `inst-ce-reenroll`

### CAS-Based Distributed Lock

- [x] `p1` - **ID**: `cpt-cf-clst-algo-sdk-default-backends-cas-lock`

**Input**: A cache backend and a lock name

**Output**: TTL-bounded mutual exclusion over cache operations

**Steps**:
1. [x] - `p1` - Use insert-if-absent on the lock key to acquire - `inst-cl-acquire`
2. [x] - `p1` - Watch the lock key to notify blocked waiters on release - `inst-cl-watch`
3. [x] - `p1` - Release conditionally via compare-and-swap so a foreign holder cannot release - `inst-cl-release`
4. [x] - `p1` - Rely on the cache TTL to reap a crashed holder's entry - `inst-cl-reap`

### Cache-Based Service Discovery

- [x] `p1` - **ID**: `cpt-cf-clst-algo-sdk-default-backends-cache-sd`

**Input**: A cache backend and a service name

**Output**: Registration, discovery, and topology over cache operations

**Steps**:
1. [x] - `p1` - Store each instance under a per-instance key with a heartbeat TTL - `inst-cs-put`
2. [x] - `p1` - Watch the service prefix for topology change events - `inst-cs-watch`
3. [x] - `p1` - Apply metadata filtering client-side - `inst-cs-filter`

### Constructor Safety Guard

- [x] `p1` - **ID**: `cpt-cf-clst-algo-sdk-default-backends-constructor-guard`

**Input**: A cache backend whose consistency is known

**Output**: A safe-by-default construction or an explicit weak-consistency opt-in

**Steps**:
1. [x] - `p1` - **IF** the default-safe constructor is used and the cache is eventually consistent - `inst-cg-default`
   1. [x] - `p1` - **RETURN** an invalid-config error - `inst-cg-reject`
2. [x] - `p1` - **IF** the weak-consistency constructor is used - `inst-cg-weak`
   1. [x] - `p1` - Construct successfully and emit a warning acknowledging the split-brain risk - `inst-cg-warn`

## 4. States (CDSL)

Not applicable — the default backends reuse the leadership and serving-intent state machines defined by the leader-election and service-discovery features; they introduce no new entity lifecycle.

## 5. Definitions of Done

### CAS-Based Leader-Election Default Backend

- [x] `p1` - **ID**: `cpt-cf-clst-dod-sdk-default-backends-leader`

The system **MUST** provide a CAS-based default leader-election backend over the cache, with a default-safe constructor that rejects an eventually-consistent cache and an explicit weak-consistency constructor that always succeeds and warns. Under a linearizable cache it **MUST** preserve at-most-one-leader.

**Implements**:
- `cpt-cf-clst-flow-sdk-default-backends-cache-only`
- `cpt-cf-clst-algo-sdk-default-backends-cas-election`
- `cpt-cf-clst-algo-sdk-default-backends-constructor-guard`

**Touches**:
- Entities: CasBasedLeaderElectionBackend

### CAS-Based Distributed-Lock Default Backend

- [x] `p1` - **ID**: `cpt-cf-clst-dod-sdk-default-backends-lock`

The system **MUST** provide a CAS-based default lock backend over the cache, with the same constructor pair (default-safe rejecting eventually-consistent, plus explicit weak-consistency opt-in), conditional release, and TTL-based reaping.

**Implements**:
- `cpt-cf-clst-flow-sdk-default-backends-cache-only`
- `cpt-cf-clst-algo-sdk-default-backends-cas-lock`
- `cpt-cf-clst-algo-sdk-default-backends-constructor-guard`

**Touches**:
- Entities: CasBasedDistributedLockBackend

### Cache-Based Service-Discovery Default Backend

- [x] `p1` - **ID**: `cpt-cf-clst-dod-sdk-default-backends-sd`

The system **MUST** provide a cache-based default service-discovery backend over the cache with a single constructor (transient staleness is acceptable for set-membership semantics), per-instance keys with heartbeat TTL, prefix-watch topology, and client-side metadata filtering.

**Implements**:
- `cpt-cf-clst-flow-sdk-default-backends-cache-only`
- `cpt-cf-clst-algo-sdk-default-backends-cache-sd`

**Touches**:
- Entities: CacheBasedServiceDiscoveryBackend

## 6. Acceptance Criteria

- [x] A plugin implementing only the cache backend yields working leader election, lock, and service discovery via SDK defaults.
- [x] The default-safe constructor rejects an eventually-consistent cache for leader election and lock; the weak-consistency constructor succeeds and warns.
- [x] Under a linearizable cache, the default leader-election backend preserves at-most-one-leader.
- [x] The default lock backend releases conditionally and recovers crashed holders via TTL.
- [x] The default service-discovery backend supports registration, prefix-watch topology, and client-side metadata filtering.
