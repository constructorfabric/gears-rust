# PRD — Cluster


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Deployment Shapes](#31-deployment-shapes)
  - [3.2 Gear-Specific Environment Constraints](#32-gear-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 P1 — Distributed Cache](#51-p1--distributed-cache)
  - [5.2 P1 — Leader Election](#52-p1--leader-election)
  - [5.3 P1 — Distributed Locks](#53-p1--distributed-locks)
  - [5.4 P1 — Service Discovery](#54-p1--service-discovery)
  - [5.5 P1 — Per-Backend Routing](#55-p1--per-backend-routing)
  - [5.6 P1 — Consumer Requirements and Startup Validation](#56-p1--consumer-requirements-and-startup-validation)
  - [5.7 P1 — Lifecycle and Shutdown](#57-p1--lifecycle-and-shutdown)
  - [5.8 P1 — Operational Namespacing](#58-p1--operational-namespacing)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

<!--
=============================================================================
PRODUCT REQUIREMENTS DOCUMENT (PRD)
=============================================================================
PURPOSE: WHAT the system must do and WHY — business requirements,
functional capabilities, and quality attributes. NOT how it's built
(see DESIGN.md for architecture, ADRs for decisions).

REQUIREMENT LANGUAGE:
  - "MUST" / "SHALL" for mandatory requirements
  - Express requirements as observable behaviors, not API signatures
  - No specific type names, no method signatures, no code patterns
=============================================================================
-->

## 1. Overview

### 1.1 Purpose

Cluster is the platform-level coordination service that gives every Gear a uniform way to share state and coordinate across instances. It offers four kinds of coordination — a distributed cache, leader election, distributed locks, and service discovery — and lets each be served by whichever backend (in-process, Postgres, Redis, K8s, NATS, etcd) the operator chooses for a given deployment. Consumers ask for the coordination they need; the platform validates that the operator's deployment can actually deliver it; if not, startup fails with a clear message rather than silently misbehaving in production.

The product exists because, today, every gear that needs cross-instance coordination either reinvents it from scratch or simply ships a single-instance version. Cluster removes that gap by making coordination a first-class platform capability with consistent semantics across gears and a stable contract that survives backend changes.

### 1.2 Background / Problem Statement

Gears increasingly need cross-instance coordination — the event broker has to assign topic shards to workers without two instances claiming the same shard, OAGW has to enforce per-tenant rate limits across replicas, mini-chat has to elect a single leader per chat room, and a future out-of-process (OOP) deployment will need to route requests to the specific delivery instance currently serving a given topic. Today these needs are met inconsistently: one team built K8s-Lease leader election inside mini-chat, another built file-based advisory locks inside `toolkit-db`, the nodes-registry exists as an in-memory bag of IDs. None of them is reusable across gears, none of them composes with the others, and none of them works the same way in dev (where there's no K8s) and production.

A unified cluster gear gives every Gear the same four coordination primitives — distributed cache, leader election, distributed lock, and service discovery — with the same observable behavior, regardless of whether the deployment runs locally on a developer laptop, on a multi-instance VM cluster with Postgres, or in production on Kubernetes with Redis. The same code that an event broker writes against the cluster gear works in every deployment shape — the operator picks the backend, the consumer doesn't change. This eliminates an entire class of "works in dev, breaks in prod" bugs and lets the platform's plugin model own coordination instead of having every gear reinvent it.

### 1.3 Goals (Business Outcomes)

- **Eliminate duplicated coordination code** — a single platform gear replaces the per-gear fragments (mini-chat's K8s leader election, toolkit-db's file locks, the nodes-registry). Target: zero per-gear reimplementations of cache / leader election / lock / service discovery in Gears within two release cycles of cluster GA.
- **Enable reliable multi-instance deployments** — gears that today run in single-instance mode (because they cannot coordinate state across replicas) can be deployed at any replica count once they adopt cluster. Target: at least two gears (event broker, OAGW) running multi-instance in production within one release cycle of cluster GA.
- **Support zero-infrastructure dev/test** — every cluster-aware behavior can be exercised on a developer laptop without spinning up Postgres, Redis, K8s, or any other backend. Target: full gear test suites pass against the in-process backend with no external dependencies.
- **Allow per-deployment backend selection without consumer code changes** — the same gear binary works against different backends in different environments by changing operator configuration only. Target: zero recompilations required to switch a deployment from Postgres to Redis-plus-K8s.
- **Catch deployment misconfigurations at startup, not in production** — if a consumer requires a coordination guarantee (e.g., linearizable leader election) and the operator-chosen backend cannot provide it, startup fails with a specific, actionable error message naming the consumer requirement and the bound backend. Target: zero production incidents caused by silent capability mismatches.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Coordination primitive | One of the four capabilities the cluster gear exposes: distributed cache, leader election, distributed lock, service discovery. |
| Backend | An external (or in-process) system that implements one or more coordination primitives. Examples: Postgres, Redis, Kubernetes API, NATS, etcd, in-process. |
| Plugin | A piece of code that adapts a backend to the cluster's primitive contracts. One plugin per backend. Plugins ship as separate releases on independent schedules; cluster's primitive contracts are stable across plugin versions. |
| Profile | A named configuration that maps each coordination primitive to a backend. Operators define profiles in deployment YAML; consumer gears reference profiles by name. The same gear can use different profiles in different deployments without code changes. |
| Capability requirement | A specific guarantee a consumer gear declares it needs from a primitive — for example, "the cache I'm using for leader-election state must be linearizable" or "the service-discovery I'm using must support server-side metadata filtering." Requirements are matched against backend characteristics at startup. |
| Capability mismatch | A startup error raised when a consumer's declared capability requirements are not met by the operator-chosen backend bound for the consumer's profile. The error names the consumer requirement and the bound backend, and prevents startup from completing. |
| Serving intent (instance state) | A binary signal a registered service instance can flip to indicate "send me work" or "drain me, don't send new work." Distinct from health: a stuck instance cannot flip its own intent — health is observed externally (via probes, circuit breakers, etc.). |
| Watch lifecycle signal | A non-data event a watch subscriber may receive: lag (events were dropped because the subscriber fell behind), reset (the underlying subscription was re-established from scratch), or close (the watch ended terminally). All three watches in cluster — cache, leader election, service discovery — surface these signals uniformly so consumers can recover the same way regardless of primitive. |
| TTL safety net | The backend-side time-to-live on every cluster-managed resource (lock entries, leader election keys, service registrations). If a consumer crashes or forgets to release explicitly, TTL bounds the leak window. Cluster never relies on Rust `Drop` to make remote calls. |
| In-scope vs out-of-scope (this change) | The cluster gear ships in multiple coordinated changes. THIS change ships the platform contract — the SDK that defines the four primitives, the consumer-side resolution and validation, the per-primitive backend interface, and the SDK-default backends built on the cache primitive — **plus the lifecycle wiring (the `ClusterHandle` builder/stop sequence) and one production-shaped in-process backend (the standalone plugin)** so the contract is exercised end-to-end. The cluster gear and its lifecycle owner are collapsed into a single gear crate (there is no separate host gear). External per-backend plugins (Postgres, K8s, Redis, NATS, etcd) remain explicit follow-up changes. |

## 2. Actors

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-clst-actor-operator`

<!-- cpt-cf-id-content -->
**Role**: Picks which backend serves which primitive in a given deployment. Defines profiles in deployment YAML — for example, "in our K8s + Redis production, the `event-broker` profile uses Redis for cache and K8s Lease for leader election." Manages backend rollout (provisioning Postgres tables, K8s RBAC, Redis credentials, etc.) outside the cluster gear's scope.
**Needs**: Per-primitive backend selection within one profile (mix-and-match Redis + K8s under the same profile). Convenient default for "use one backend for everything" without writing four config blocks. Clear startup errors when the chosen backend doesn't meet a consumer gear's requirements. No magic strings to copy between consumer code and YAML — profile names live in YAML and in one declaration per consumer gear, never re-typed at call sites.
<!-- cpt-cf-id-content -->

#### Plugin Author

**ID**: `cpt-cf-clst-actor-plugin-author`

<!-- cpt-cf-id-content -->
**Role**: Builds and maintains a backend plugin that adapts a specific external system (Postgres, Redis, K8s, NATS, etcd, etc.) to the cluster's primitive contracts. May implement only one primitive (cache) or several. Ships their plugin as a separate release on its own schedule — the cluster's primitive contracts are stable across plugin versions.
**Needs**: A clear, narrow contract for what each primitive must deliver. Honest characteristic declaration — a way to say "my backend is eventually consistent" or "my backend supports server-side metadata filtering" without lying. A plugin that implements only the cache primitive should "just work" for the other three primitives via SDK defaults — no obligation to implement all four.
<!-- cpt-cf-id-content -->

### 2.2 System Actors

#### Event Broker

**ID**: `cpt-cf-clst-actor-event-broker`

<!-- cpt-cf-id-content -->
**Role**: Uses leader election to pick a single instance to manage worker pool coordination, uses cache to publish shard-assignment state, and uses service discovery to route messages to the specific delivery instance currently serving a given topic shard. Topics and shards are dynamic — added at runtime, redistributed when delivery instances scale up or down.
**Needs**: Linearizable leader election (no split-brain when shards move). Reactive notifications when shard assignments change (no polling). Service discovery that filters by metadata (find the delivery instance serving topic-shard "t-42" in one call) and supports fan-out routing (find all instances serving any of these shards). Drain semantics — when an instance shuts down, the broker must stop sending it new work before it disappears entirely.
<!-- cpt-cf-id-content -->

#### Outbound API Gateway (OAGW)

**ID**: `cpt-cf-clst-actor-oagw`

<!-- cpt-cf-id-content -->
**Role**: Uses distributed locks for cross-instance rate limiting and uses cache CAS for shared counters (per-tenant request budgets). Holds locks for sub-second windows to atomically read-and-update counters; never holds locks across remote calls.
**Needs**: High-throughput cache operations (10k+ counter updates per second). Bounded lock holding so a crashed OAGW instance never permanently blocks others. No fencing tokens — the rate-limiting flow is local-only inside the lock, so the classic fencing scenarios don't apply.
<!-- cpt-cf-id-content -->

#### Platform Gear (consumer)

**ID**: `cpt-cf-clst-actor-platform-gear`

<!-- cpt-cf-id-content -->
**Role**: Any gear that needs one or more cluster primitives. Declares which primitives it needs and what guarantees it requires; the platform validates this against the operator's deployment at startup.
**Needs**: A resolution model that doesn't force consumers to reason about backends — the consumer asks for "the cache for the event-broker profile" and gets back something usable. Startup-time failure when the deployment can't deliver what the consumer requires (loud misconfiguration), not silent runtime degradation. Backwards-compatible evolution — when a future cluster version changes a primitive's contract, existing consumers continue working under their original contract until they migrate.
<!-- cpt-cf-id-content -->

#### Gear Host (parent gear)

**ID**: `cpt-cf-clst-actor-host`

<!-- cpt-cf-id-content -->
**Role**: The single gear in a deployment that owns the cluster lifecycle — brings cluster up before any consumer gear can resolve a primitive, and brings cluster down cleanly during graceful shutdown. The gear host is where operator YAML for cluster lives and where the orchestration sequence runs (start each plugin, register each backend, hand consumers a working environment).
**Needs**: A clean lifecycle entry point that doesn't require inventing a new dependency-ordering mechanism. A graceful shutdown that revokes leader claims before consumer code can run again on stale assumptions. A bounded shutdown deadline — when the framework's overall shutdown timer fires, cluster surrenders and lets the framework cancel.
<!-- cpt-cf-id-content -->

## 3. Operational Concept & Environment

> **Note**: Project-wide runtime, OS, architecture, lifecycle policy, and integration patterns defined in root PRD. Document only gear-specific deviations here.

> **Open: backend authentication and credential wiring.** How cluster plugins acquire credentials for their backend connections is **not yet established** and is intentionally out of scope. The shape will be settled as part of the broader OOP (out-of-process) deployment design, where cluster meets the rest of the platform's credential and transport story.

### 3.1 Deployment Shapes

The cluster gear is designed to work across the full range of Gears deployments:

- **Developer laptop / unit test** — one process, no external dependencies, no network. Cluster runs entirely in-process. Every cluster-aware gear behavior is exercisable without infrastructure.
- **Single-host multi-process** — a few processes on one machine, coordinating through a shared backend (typically Postgres). No K8s, no Redis. Cluster delivers full functionality with one optional database dependency.
- **Multi-instance, no K8s** — multiple machines or containers without K8s orchestration. Cluster coordinates through a backend that's reachable from all instances (typically Postgres or Redis).
- **K8s, low-throughput** — K8s deployment where coordination volume is modest. Cluster uses K8s-native resources (Lease for leader election and locks, custom resources for cache).
- **K8s + high-throughput cache** — production-shaped K8s deployment. Cluster mixes backends: Redis for the high-volume cache and lock paths, K8s Lease for leader election (where consistency matters more than throughput), K8s Lease-per-instance for service discovery (so the dispatcher can filter delivery instances by topic shard).

A consumer gear's code does not change between these shapes. The operator picks the backend per primitive in deployment YAML.

### 3.2 Gear-Specific Environment Constraints

- The in-process backend has no external dependencies and is the default for development.
- Each backend other than in-process has its own infrastructure prerequisites (Postgres requires a database; K8s requires API-server access with appropriate permissions; Redis requires network reachability). These belong to the per-backend plugin and the operator's deployment plan, not to the cluster gear itself.
- Within one profile, each primitive can be served by a different backend. There is no requirement to use a single backend for all four primitives.
- The gear host is responsible for bringing cluster up before any consumer gear can resolve a primitive. Gears' existing gear-dependency mechanism enforces this at the gear level — the cluster gear host is registered as a dependency of every consumer gear.

## 4. Scope

### 4.1 In Scope

This change ships the **platform contract** — the part of the cluster gear that consumer gears and plugin authors depend on — together with the lifecycle wiring and one production-shaped in-process backend that exercise the contract end-to-end. External per-backend plugins (Postgres, K8s, Redis, NATS, etcd) are separate, follow-up changes that build against the contract established here.

In scope for this change:

- The four coordination primitives — distributed cache, leader election, distributed lock, service discovery — defined as a stable contract.
- The cluster lifecycle wiring: the `ClusterHandle` builder that registers each profile's backends (auto-filling unbound primitives with the SDK defaults over the cache) and the single `stop()` shutdown sequence that revokes active leadership, deregisters backends, and runs plugin stop hooks. The lifecycle owner is the cluster gear crate itself (host collapsed in).
- One production-shaped in-process backend (the standalone plugin): a `ClusterCacheBackend` with monotonic versioning, a TTL sweeper, native exact + prefix watches with non-blocking fan-out, declaring linearizable consistency — enough for the SDK defaults to derive all four primitives over it.
- Consumer-side primitive resolution: a consumer gear declares the profile and capability requirements it needs and gets back a working primitive (or a clear error).
- Capability validation at startup: when the operator-chosen backend cannot deliver what a consumer requires, startup fails with a specific, actionable error.
- Plugin contract: the narrow interface a plugin author implements to adapt a backend to a primitive, plus the characteristic-declaration mechanism by which plugins honestly state what they support.
- SDK-default primitive implementations built on the cache primitive: a plugin that only implements the cache primitive automatically gets working leader election, locks, and service discovery via cluster-provided defaults built on cache CAS operations.
- Operational namespacing: a consumer can carve out a sub-namespace within a primitive (per-gear key prefixes, per-shard subdivisions) without affecting other consumers.
- Watch lifecycle signaling: a uniform way for watch subscribers across all three watches (cache, leader election, service discovery) to recover from lag, reset, or terminal close.
- A workspace-wide static-analysis rule that catches cluster-lock misuse — specifically, cross-instance remote calls inside a lock's critical section, which would create stale-writer scenarios.
- Smoke tests against in-process test backends to verify the contract works end-to-end.
- Showcase example gears demonstrating typical consumer patterns.

### 4.2 Out of Scope

The following are out of scope for this change and ship as follow-ups:

- External per-backend plugins (Postgres, K8s, Redis, NATS, etcd, Hazelcast). Each plugin is a separate change that builds against the contract this change establishes. The in-process standalone plugin shipped here is the reference backend, not a substitute for these.
- Operator-YAML-driven instantiation of **non-cache** native backends. The wiring reads config and instantiates the cache provider, then auto-fills leader election, lock, and service discovery with the SDK defaults over that cache. Binding a native non-cache backend in YAML is **rejected at config time** (loud `InvalidConfig`) until the per-primitive routing follow-up lands — see `cpt-cf-clst-fr-routing-per-primitive`.
- Active **remote release** of held lock and service-discovery state on shutdown. In-flight lock waiters and active service-discovery / cache watches now receive a terminal `Shutdown` (`cpt-cf-clst-fr-shutdown-revoke`), but **held** locks and live service registrations are not remotely deleted — they lapse via their backend TTL (`cpt-cf-clst-fr-shutdown-ttl-cleanup`).
- Migration of existing per-gear coordination code (mini-chat's K8s leader election, toolkit-db's file locks, the nodes-registry). Each migration is a separate per-gear change.
- Reliable pub/sub messaging with delivery guarantees, consumer groups, offsets, replay. The event broker gear owns reliable messaging; cluster's reactive cache notifications serve a different role (data-change observation, not message delivery).
- Cross-cluster or geo-distributed coordination. Cluster is single-cluster.
- Universal linearizability. Each primitive's consistency is a function of its bound backend; consumers requiring linearizable behavior declare it as a capability requirement.
- External health probing, service-mesh integration, sidecar models. Cluster's serving-intent signal is gear-declared, not externally observed.
- Fencing tokens for distributed locks. The "no remote I/O inside the critical section" architectural rule eliminates the failure scenario fencing tokens would protect against.
- Backend authentication and credential management. Deferred to the broader OOP deployment design.

## 5. Functional Requirements

### 5.1 P1 — Distributed Cache

#### Versioned Key-Value Storage

- [x] `p1` - **ID**: `cpt-cf-clst-fr-cache-storage`

<!-- cpt-cf-id-content -->
The system **MUST** provide a distributed cache that stores opaque byte values under named keys, with optional time-to-live, and where every value carries a monotonically increasing version that consumers can use for optimistic concurrency. The key naming convention **MUST** be uniform across all cluster primitives so that consumers can use the same naming patterns for cache keys, lock names, election names, and service names.

**Rationale**: Versioned values are the foundation for optimistic concurrency, which in turn is the foundation for all cluster coordination patterns — counters, shard assignments, distributed locks, leader election. A single uniform naming convention removes the per-primitive cognitive load of remembering which primitive accepts which characters.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-oagw`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

#### Atomic Conditional Operations

- [x] `p1` - **ID**: `cpt-cf-clst-fr-cache-atomic`

<!-- cpt-cf-id-content -->
The system **MUST** provide atomic conditional storage operations — insert-if-absent and version-based compare-and-swap — so consumers can implement leader election, locks, counters, and idempotent initialization without races. The compare-and-swap operation **MUST** signal mismatched-version conflicts in a way that lets the consumer recover (typically by re-reading and retrying).

**Rationale**: Optimistic concurrency is the primary tool for cross-instance coordination in cluster. Without atomic conditional operations, every consumer would need pessimistic locking even for benign updates.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-oagw`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

#### TTL-Bounded Storage

- [x] `p1` - **ID**: `cpt-cf-clst-fr-cache-ttl`

<!-- cpt-cf-id-content -->
The system **MUST** allow consumers to attach a time-to-live to any stored value, after which the value is automatically removed and any subscribers watching the key receive a removal notification. Values stored without a TTL persist until explicitly deleted; backends that don't support indefinite persistence (the in-process backend, for example) **MUST** document that constraint.

**Rationale**: TTL is the safety net for every cluster resource — locks, leader claims, service registrations all rely on TTL-bounded entries to recover from forgotten cleanup or crashes. Cache TTL serves the same purpose for application-level entries (rate-limit windows, token caches, ephemeral state).
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-oagw`
<!-- cpt-cf-id-content -->

#### Reactive Notifications by Key and Prefix

- [x] `p1` - **ID**: `cpt-cf-clst-fr-cache-watch`

<!-- cpt-cf-id-content -->
The system **MUST** allow consumers to subscribe to change notifications for an exact key and for a key prefix. Notifications carry only enough information to identify the affected key — consumers retrieve the current value if needed via a follow-up read. Per-key notification ordering **MUST** be preserved so that consumers see updates in the order they happened. Backends that cannot support prefix subscriptions natively **MUST** declare that limitation; consumers can either honor the limitation or use a polling fallback.

**Rationale**: Reactive notifications eliminate polling for shard assignments, configuration changes, and leader status. Lightweight notifications (key only, no value) sidestep the stale-value-in-event problem and map cleanly to all backends, including those with payload-size limits.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

### 5.2 P1 — Leader Election

#### Single-Leader Election

- [x] `p1` - **ID**: `cpt-cf-clst-fr-leader-elect`

<!-- cpt-cf-id-content -->
The system **MUST** allow gears to participate in a named leader election where, at any time, at most one participant observes itself as the leader. The system **MUST** automatically renew the leader's claim so consumers don't write renewal loops. When the leader fails or is partitioned, remaining participants **MUST** detect the loss within a bounded time and promote a new leader.

**Rationale**: Singleton patterns — worker pool coordination, migration gating, scheduler election — require the platform to guarantee at-most-one-leader. Without automatic renewal, every consumer reimplements the same heartbeat loop with the same bugs.
**Actors**: `cpt-cf-clst-actor-event-broker`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Configurable Election Timing

- [x] `p1` - **ID**: `cpt-cf-clst-fr-leader-config`

<!-- cpt-cf-id-content -->
The system **MUST** let consumers tune the trade-off between failover speed (short TTL, frequent renewals) and tolerance to transient backend errors (long TTL, more renewal attempts before losing leadership). A reasonable default **MUST** be provided so consumers without strong opinions don't have to choose. Misconfigured timing values **MUST** be rejected at construction with a clear error.

**Rationale**: A worker-pool leader needs fast failover; a migration-gating leader prefers conservative timing to avoid handing off mid-migration. One-size-fits-all timing forces every consumer to either tolerate over-aggressive failover or under-aggressive failover.
**Actors**: `cpt-cf-clst-actor-event-broker`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Leader Status Observability

- [x] `p1` - **ID**: `cpt-cf-clst-fr-leader-observability`

<!-- cpt-cf-id-content -->
The system **MUST** allow consumers to react to leadership transitions in two ways: by awaiting the next transition (event-driven), and by checking the current status synchronously (gate-driven, suitable for use inside event-loop selection arms or timer-driven loops). Transient backend errors during renewal **MUST NOT** surface as transitions — the system retries internally. Loss of leadership is a transient observable event: after the system reports loss, the consumer continues participating in the election and will be reported as either re-elected or as a follower without writing any re-enrollment code.

**Rationale**: Different consumer patterns need different observation styles. Forcing every consumer through a single event-driven model produces awkward code for timer-driven workers. Treating leader-loss as transient, not terminal, removes the re-enrollment boilerplate every consumer would otherwise have to write.
**Actors**: `cpt-cf-clst-actor-event-broker`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Graceful Step-Down

- [x] `p1` - **ID**: `cpt-cf-clst-fr-leader-resign`

<!-- cpt-cf-id-content -->
The system **MUST** allow a current leader to step down explicitly during planned shutdown or maintenance, releasing the claim immediately so a successor can be elected within a backend round-trip rather than waiting for TTL expiry.

**Rationale**: Without explicit step-down, every planned restart introduces a TTL-bounded leadership gap that can be longer than the deployment can tolerate.
**Actors**: `cpt-cf-clst-actor-event-broker`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Advisory Semantics, Not Mutual Exclusion

- [x] `p1` - **ID**: `cpt-cf-clst-fr-leader-advisory`

<!-- cpt-cf-id-content -->
Leader election **MUST** be documented as advisory coordination — it tells *which node should run* a workload, not *prevents two nodes from writing simultaneously*. Consumers that need correctness-critical mutual exclusion **MUST** be directed to the distributed lock primitive combined with application-level optimistic concurrency, not to leader-status checks.

**Rationale**: Every TTL-based leader election has a small window where a stale leader can write before observing it has lost leadership. Pretending otherwise produces silent data corruption. Documenting the advisory boundary explicitly prevents misuse.
**Actors**: `cpt-cf-clst-actor-event-broker`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.3 P1 — Distributed Locks

#### Acquire-Or-Fail and Acquire-With-Wait

- [x] `p1` - **ID**: `cpt-cf-clst-fr-lock-acquire`

<!-- cpt-cf-id-content -->
The system **MUST** provide both non-blocking lock acquisition (returns a contention error if the lock is held) and blocking acquisition with a timeout (returns a timeout error if not acquired within the timeout). Lock acquisitions **MUST** carry a TTL so that a crashed holder cannot block others indefinitely.

**Rationale**: Rate limiting and resource guarding need non-blocking acquisition (fail fast and shed load). Serialized critical sections need waiting acquisition. Both patterns need TTL-bounded recovery from crashed holders.
**Actors**: `cpt-cf-clst-actor-oagw`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Explicit Release with TTL Safety Net

- [x] `p1` - **ID**: `cpt-cf-clst-fr-lock-release`

<!-- cpt-cf-id-content -->
Consumers **MUST** release locks explicitly. If a consumer panics, crashes, or forgets to release, the backend's TTL bounds the leak — the lock is automatically released after the TTL elapses. Consumers **MUST** be able to extend an active lock's TTL for long-running operations; attempting to extend an already-expired lock **MUST** return a specific error so the consumer knows it lost the lock and needs to abort whatever it was doing.

**Rationale**: Cluster operations are remote and Gears is fully async — automatic release on Rust `Drop` cannot reliably perform network I/O without creating subtle correctness bugs. Explicit release forces consumers to think about cleanup; TTL bounds the worst case when they don't.
**Actors**: `cpt-cf-clst-actor-oagw`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### No Remote I/O Inside the Critical Section

- [x] `p1` - **ID**: `cpt-cf-clst-fr-lock-no-remote`

<!-- cpt-cf-id-content -->
Consumers **MUST NOT** make remote calls inside the critical section protected by a cluster lock. All remote effects must happen before lock acquisition or after lock release. The system **MUST** include a static-analysis rule that flags violations at compile time so this rule is enforceable rather than aspirational.

**Rationale**: Combined with async timeouts on every operation, this rule eliminates the unbounded-pause scenario that Kleppmann-style fencing tokens exist to protect against. The architectural constraint is strictly stronger than the fencing-token mitigation: there's no stale writer to fence because the critical section did no remote work.
**Actors**: `cpt-cf-clst-actor-oagw`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.4 P1 — Service Discovery

#### Instance Registration with Metadata

- [x] `p1` - **ID**: `cpt-cf-clst-fr-sd-register`

<!-- cpt-cf-id-content -->
The system **MUST** allow gears to register themselves as instances of a named service, with an endpoint address and arbitrary metadata key-value pairs (string keys, string values). The system **MUST** assign an instance identifier if the registering gear doesn't provide one. Registrations are TTL-bounded — if a registered instance stops heartbeating, it disappears from discovery automatically. Consumers **MUST** be able to deregister explicitly during graceful shutdown.

**Rationale**: Out-of-process deployments need to know which instances are alive and where they are. Metadata enables routing decisions beyond just "is this instance up" (e.g., "which instance currently owns topic-shard t-42").
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

#### Discovery with State and Metadata Filtering

- [x] `p1` - **ID**: `cpt-cf-clst-fr-sd-discover`

<!-- cpt-cf-id-content -->
The system **MUST** provide a single discovery operation that returns the registered instances of a named service matching a filter. The filter **MUST** support both serving-state predicates (only enabled instances, only disabled instances, all) and metadata predicates (key equals a value, key is in a set of values). Multiple metadata predicates combine with AND semantics. The default filter **MUST** return only enabled instances with no metadata constraint, so consumers using the default cannot accidentally route traffic to drained instances. Callers needing all instances regardless of state **MUST** opt in explicitly. The filter contract **MUST** be designed so that future additions (geographic region, version selectors, result limits) extend the filter without breaking existing consumers. The order of the returned instance set is **unspecified** and may differ across backends and across calls; consumers requiring deterministic selection sort client-side.

**Rationale**: A single extensible filter is forward-compatible; multiple disjoint discovery methods would force every new filter dimension to be a new method. Defaulting to "enabled only" eliminates a class of misconfigurations where an admin-tool query path leaks into production routing.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

#### Topology Watch with Lifecycle Signals

- [x] `p1` - **ID**: `cpt-cf-clst-fr-sd-watch`

<!-- cpt-cf-id-content -->
The system **MUST** provide a watch operation that yields topology change events (instance joined, left, or updated). Watches are unfiltered — consumers apply filtering client-side to each change event. After a lag or reset signal (see §5.7 lifecycle signals), consumers **MUST** be able to recover by re-reading current membership.

**Rationale**: Reactive topology awareness avoids polling and enables efficient connection-pool management. Unfiltered watches sidestep the ambiguity of "did this enabled-to-disabled transition produce a Joined or a Left event" — consumers apply consistent filtering on their side.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

#### Gear-Declared Serving Intent (Not Health)

- [x] `p1` - **ID**: `cpt-cf-clst-fr-sd-state`

<!-- cpt-cf-id-content -->
Each registered service instance **MUST** carry a binary serving intent (enabled or disabled) that the registering gear can flip at any time. New registrations default to enabled. Gears that need to start in a non-serving state (warm-up, dependency wait, drain on shutdown) **MUST** be able to register as disabled or flip to disabled before exposing themselves to traffic.

The serving-intent signal **MUST NOT** be presented as health observation. A stuck or deadlocked instance cannot flip its own intent; it disappears from discovery only when its TTL-bounded heartbeat stops. External health observation (Kubernetes readiness probes, service-mesh outlier detection, client-side circuit breakers) is the mechanism for detecting unexpected failure and is explicitly out of scope for this primitive.

**Rationale**: A gear-owned drain signal is genuinely useful for graceful shutdown, warm-up, and maintenance — but the operator must understand it as intent rather than observation. Conflating the two has caused outages in other systems where gears silently kept claiming to be healthy while their request handlers were broken.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

### 5.5 P1 — Per-Backend Routing

#### Per-Primitive Backend Selection

- [ ] `p1` - **ID**: `cpt-cf-clst-fr-routing-per-primitive`

<!-- cpt-cf-id-content -->
The system **MUST** allow operators to bind each coordination primitive to a different backend within one profile. For example: cache served by Redis, leader election served by K8s Lease, distributed lock served by Redis, service discovery served by K8s Lease-per-instance — all under the same profile. Consumer gears referencing this profile see four working primitives without knowing or caring that they're served by different backends.

**Rationale**: Different backends excel at different things. Forcing one backend to serve all four primitives produces either suboptimal performance (Redis for leader election with weaker consistency than K8s Lease) or impossible-to-deploy combinations (K8s for cache when application throughput is too high).
**Status (this change)**: Deferred. The wiring exposes `with_leader_election` / `with_lock` / `with_service_discovery` programmatically, but the YAML path **rejects** an explicit non-cache native binding at config time (loud `InvalidConfig` naming the primitive) rather than silently ignoring it. Operators get per-primitive backends once native non-cache providers ship; until then, omit the non-cache bindings to use the SDK defaults over the cache.
**Actors**: `cpt-cf-clst-actor-operator`
<!-- cpt-cf-id-content -->

#### Convenient Single-Backend Default

- [x] `p1` - **ID**: `cpt-cf-clst-fr-routing-omit-default`

<!-- cpt-cf-id-content -->
The system **MUST** provide a convenient configuration shorthand for "use one backend for everything." When an operator binds a backend to the cache primitive but does not bind anything to leader election, lock, or service discovery, the system **MUST** automatically provide working implementations of the unbound primitives via cluster-provided defaults built on the cache backend. Explicit per-primitive bindings **MUST** always override the default.

**Rationale**: The "single-backend Postgres-only" deployment is the most common starting point. Forcing operators to write four config blocks for it (or to introduce a magic-string sentinel for "use the default") is friction without benefit. Omission as an opt-in is unambiguous and minimal.
**Actors**: `cpt-cf-clst-actor-operator`
<!-- cpt-cf-id-content -->

#### Plugin Implements Cache Only Is Sufficient

- [x] `p1` - **ID**: `cpt-cf-clst-fr-routing-cache-only-plugin`

<!-- cpt-cf-id-content -->
A plugin author **MUST** be able to ship a working integration by implementing only the cache primitive. The cluster gear **MUST** ship default implementations of leader election, distributed lock, and service discovery built on the cache primitive's atomic conditional operations and reactive notifications. When a backend natively supports a primitive better than the default (Kubernetes Lease for leader election, Redis SET-NX-EX for locks), the plugin author **MAY** override the default with a native implementation; otherwise the default is used.

**Rationale**: Lowers the barrier to integrating new backends. A plugin author wanting to add NATS support shouldn't need to also figure out how to model leader election in NATS — they get it for free if they implement cache.
**Actors**: `cpt-cf-clst-actor-plugin-author`
<!-- cpt-cf-id-content -->

### 5.6 P1 — Consumer Requirements and Startup Validation

#### Consumer Declares Profile by Typed Reference

- [x] `p1` - **ID**: `cpt-cf-clst-fr-validation-typed-profile`

<!-- cpt-cf-id-content -->
Consumer gears **MUST** reference profiles by a typed identifier defined once in their crate, not by passing the profile name as a string at every call site. The profile name string **MUST** appear in exactly two places per consumer gear: the crate's typed declaration and the operator's deployment YAML. There **MUST NOT** be a third place where the string is re-typed.

**Rationale**: Gears forbid magic strings in code paths. Typo-prone string profile names are a class of bug the platform should rule out by construction. Typed identifiers fail the build on typo; bare strings fail at startup or worse.
**Actors**: `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Consumer Declares Capability Requirements

- [x] `p1` - **ID**: `cpt-cf-clst-fr-validation-capability-declarations`

<!-- cpt-cf-id-content -->
When resolving a primitive, a consumer **MUST** be able to declare specific capability requirements — for example, "the cache I'm using must be linearizable" or "the cache I'm using must support native prefix subscriptions" or "the service-discovery I'm using must support server-side metadata pushdown." Each declared requirement **MUST** map to a concrete characteristic of the bound backend that the system can check directly. Multiple requirements combine with AND semantics.

**Rationale**: Different consumer gears need different guarantees from the same primitive. Without per-consumer requirements, the platform either has to lock every consumer to the strongest guarantees (forcing all deployments to use linearizable backends even when some consumers don't need them) or accept that some consumers will silently misbehave on weaker backends.
**Actors**: `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Plugin Declares Backend Characteristics Honestly

- [x] `p1` - **ID**: `cpt-cf-clst-fr-validation-honest-declaration`

<!-- cpt-cf-id-content -->
Each plugin **MUST** honestly declare the characteristics of its backend — for example, whether the backend is linearizable, whether it supports native prefix subscriptions, whether it supports server-side metadata pushdown. The declaration mechanism **MUST** be designed so that adding a new characteristic in a future version of cluster does not break existing plugin implementations.

**Rationale**: The startup-validation guarantee depends on plugins telling the truth about their backends. The platform cannot validate consumer requirements against unstated characteristics.
**Actors**: `cpt-cf-clst-actor-plugin-author`
<!-- cpt-cf-id-content -->

#### Capability Mismatch Fails Startup, Not Production

- [x] `p1` - **ID**: `cpt-cf-clst-fr-validation-startup-fail`

<!-- cpt-cf-id-content -->
When a consumer's declared capability requirements cannot be met by the operator-bound backend, the system **MUST** fail startup with a specific, actionable error naming the consumer gear's requirement, the primitive, and the bound backend. Startup **MUST NOT** complete with a silently-degraded primitive. The error message **MUST** be specific enough that an operator can either change the YAML binding or contact the consumer gear's owner without first having to read source code to figure out what failed.

**Rationale**: Silent capability degradation produces production incidents that look like consumer bugs but are actually deployment configuration issues. Loud startup failure puts the error where the cause is — in deployment configuration — at the time the deployment is rolling out, not at 3am during traffic.
**Actors**: `cpt-cf-clst-actor-operator`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.7 P1 — Lifecycle and Shutdown

#### Single Lifecycle Owner

- [x] `p1` - **ID**: `cpt-cf-clst-fr-lifecycle-owner`

<!-- cpt-cf-id-content -->
The cluster gear **MUST** be brought up and down by a single owning gear in the deployment, not by every consumer or by the framework directly. The owning gear is responsible for orchestrating plugin start, ensuring all backends are registered before consumers can resolve them, and ensuring all backends are deregistered before plugins shut down. Consumer gears **MUST** depend on the owning gear via the existing gear-dependency mechanism so that ordering between cluster-up and consumer-resolves is enforced at the gear level.

**Rationale**: Without a single owner, plugin start and backend registration become a distributed coordination problem at startup — exactly the problem cluster exists to solve, ironically. A single owner gives code-flow ordering: parent starts cluster, then consumers can resolve, period.
**Status (this change)**: Implemented. The cluster gear crate is itself the owner (host collapsed in): its `start` builds the `ClusterHandle` (registering all backends), and its `stop` runs the single shutdown sequence. The gear-dependency ordering between cluster-up and consumer-resolve is enforced via the framework's gear dependency mechanism.
**Actors**: `cpt-cf-clst-actor-host`
<!-- cpt-cf-id-content -->

#### Watch Lifecycle Signals (Lag, Reset, Close)

- [x] `p1` - **ID**: `cpt-cf-clst-fr-watch-lifecycle-signals`

<!-- cpt-cf-id-content -->
All three watch types — cache, leader election, service discovery — **MUST** surface three lifecycle signals beyond ordinary value events: lag (the watcher fell behind, events were dropped, some count or "unknown" is reported), reset (the underlying subscription was re-established from scratch, all prior assumptions are invalid), and close (the watch ended terminally, no further events will arrive). Consumers **MUST** be able to handle these signals uniformly across the three watch types — same recovery patterns regardless of which primitive's watch they're observing.

**Rationale**: Watches are unreliable across network and process boundaries. Pretending otherwise causes silent state divergence between consumer and backend. Surfacing lag, reset, and close as first-class events lets consumers recover correctly. Uniform shape across the three watches lets consumer code handle them once instead of three different ways.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

#### SDK-shipped Watch Auto-Restart Combinator

- [x] `p1` - **ID**: `cpt-cf-clst-fr-watch-auto-restart`

<!-- cpt-cf-id-content -->
The cluster SDK **MUST** ship an opt-in watch-restart combinator that wraps the consumer-facing `*Watch` types and turns terminal close events into transparent reconnection with operator-configurable backoff. The combinator distinguishes retryable terminal causes (connection lost, timeout, resource exhausted, transient backend outage) from non-retryable terminal causes (auth failure, capability mismatch, explicit shutdown), retries the former according to a `RetryPolicy` (initial backoff, maximum backoff, jitter, and an optional retry-attempt cap), surfaces a `Reset` event to the consumer on each successful resubscribe so the consumer re-reads state, and propagates non-retryable closes to the consumer unchanged. The combinator **MUST** be available for all three watch types (cache, leader-election, service-discovery) using a single uniform policy type. Consumers that want a custom restart loop **MUST** still be able to consume the raw `*WatchEvent` stream without going through the combinator.

**Rationale**: Without an SDK-shipped combinator, every consumer gear reinvents the same restart loop independently, with inconsistent backoff and inconsistent retryability classification. That diverges across gears and produces thundering-herd reconnect storms against a recovering backend. A single combinator at the SDK layer, parameterized by `RetryPolicy`, eliminates this class of regression and gives consumer code one canonical pattern to follow.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`, `cpt-cf-clst-actor-oagw`
<!-- cpt-cf-id-content -->

#### Graceful Shutdown Revokes Leader Confidence

- [x] `p1` - **ID**: `cpt-cf-clst-fr-shutdown-revoke`

<!-- cpt-cf-id-content -->
On graceful shutdown, the system **MUST** revoke any active leader's claim before shutdown completes. A current leader **MUST** observe loss-of-leadership before any consumer code runs again on the assumption that the leader is still in charge. After loss has been observed, the watch then ends terminally (close signal). In-flight blocking operations (lock acquisitions, leader claims, discovery requests) **MUST** be cancelled with a specific shutdown error so consumers know the difference between "I lost my lock" and "the cluster is going down."

**Rationale**: Without explicit revocation, a graceful shutdown can leave a leader-process believing it still leads while the cluster handle is gone — a stale-writer setup. Revoking confidence before shutdown completes prevents this.
**Status (this change)**: Fully implemented (leader + in-flight lock + service-discovery watch + cache watch). `ClusterHandle::stop` revokes each wiring-created default backend before it returns: the leader-election backend latches `Status(Lost)` then emits `Closed(ClusterError::Shutdown)` to every active leader and awaits those tasks; an in-flight blocking `lock()` waiter returns `Err(ClusterError::Shutdown)` (not `LockTimeout`); an active service-discovery watch observes `Closed(ClusterError::Shutdown)`. Active **cache** watches are closed with `Closed(ClusterError::Shutdown)` via the standalone plugin's stop hook (`StandaloneCache::shutdown`), one phase after the leader/lock/SD revocation but still within `stop()`. No remote release is performed — held claims, locks, and registrations lapse via TTL per `cpt-cf-clst-fr-shutdown-ttl-cleanup`.
**Actors**: `cpt-cf-clst-actor-host`, `cpt-cf-clst-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### TTL Handles Remote Cleanup, Not Shutdown

- [x] `p1` - **ID**: `cpt-cf-clst-fr-shutdown-ttl-cleanup`

<!-- cpt-cf-id-content -->
On shutdown, the system **MUST NOT** make best-effort remote cleanup calls (deleting leader keys, releasing locks, deregistering services). Remote cleanup is bounded by the backend's TTL on each resource. Best-effort remote calls during shutdown are unreliable — the network may be partially gone, the backend may be evicting connections — and produce log noise without correctness benefit.

**Rationale**: TTL is the safety net by design; pretending shutdown can clean up perfectly is a fiction that produces flaky shutdowns. Better to commit to the TTL bound and document it.
**Status (this change)**: Implemented. `ClusterHandle::stop` performs no remote cleanup, and the leader revocation path returns without a remote release — leader keys, locks, and service registrations all lapse via their backend TTL.
**Actors**: `cpt-cf-clst-actor-host`
<!-- cpt-cf-id-content -->

### 5.8 P1 — Operational Namespacing

#### Per-Primitive Sub-Namespacing for Consumers

- [x] `p1` - **ID**: `cpt-cf-clst-fr-namespacing-scoped`

<!-- cpt-cf-id-content -->
A consumer gear **MUST** be able to carve out a sub-namespace within any primitive — typically per-gear (so two gears using the same profile don't collide on cache keys) and optionally per-shard (so a sharded gear can subdivide its own namespace). Sub-namespacing **MUST** compose so a sharded gear's per-shard namespace nests cleanly inside its per-gear namespace. Consumers see name-relative names; the sub-namespacing is invisible inside the consumer's own code.

**Rationale**: Without per-gear namespacing, two unrelated gears using the same profile would collide on cache keys, lock names, election names, and service names. Forcing every consumer to manually prefix every name is bug-prone (one missed prefix and you have a collision). The platform should handle namespacing for them.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

#### Service Discovery Metadata Is Not Namespaced

- [x] `p1` - **ID**: `cpt-cf-clst-fr-namespacing-sd-metadata-unscoped`

<!-- cpt-cf-id-content -->
For service discovery specifically, sub-namespacing **MUST** apply only to service names, not to metadata keys or values. Two unrelated services in different namespaces **MUST** be able to use the same metadata key (for example, both using "region" or "topic-shard") without collision and without silent renaming.

**Rationale**: Metadata keys are an attribute namespace per instance, not a coordination namespace. Scoping "region" to "gear-name/region" would either silently rename the key (breaking interop with platform tools that read raw metadata) or rename it inconsistently across consumers. The platform's coordination namespace lives on the service name; metadata is per-instance attribute data.
**Actors**: `cpt-cf-clst-actor-platform-gear`, `cpt-cf-clst-actor-event-broker`
<!-- cpt-cf-id-content -->

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### Capability Validation at Startup

- [x] `p1` - **ID**: `cpt-cf-clst-nfr-capability-validation`

<!-- cpt-cf-id-content -->
When a consumer gear declares capability requirements that the operator-bound backend cannot meet, the system MUST detect the mismatch and fail startup with an actionable error within bounded time of the consumer's resolution attempt. The error MUST identify the consumer's primitive, the consumer's unmet requirement, and the bound backend so the operator can either change the YAML binding or contact the consumer's owner.

**Threshold**: Startup validation rejects mismatched capability declarations within 1 second of the consumer's resolution attempt, with an error message naming the primitive, the unmet capability, and the bound backend.
**Rationale**: Silent runtime degradation produces production incidents that look like application bugs but are actually deployment misconfigurations. Loud startup failure surfaces the root cause where it is — in operator config — at deployment time.
**Architecture Allocation**: See DESIGN.md.
<!-- cpt-cf-id-content -->

#### Stable Cross-Backend Behavior

- [x] `p1` - **ID**: `cpt-cf-clst-nfr-cross-backend-stability`

<!-- cpt-cf-id-content -->
A consumer gear's behavior MUST NOT change when an operator switches the backend bound to a primitive (for example, swapping Postgres for Redis under the cache primitive), provided the new backend meets the consumer's declared capability requirements. The same gear binary running against different backends MUST produce the same observable behavior at the cluster API level.

**Threshold**: Gear integration tests pass identically against any backend that satisfies the gear's declared capability requirements.
**Rationale**: Backend substitutability is the central value proposition. Without it, "deploy to a different backend" becomes a per-gear rewrite.
**Architecture Allocation**: See DESIGN.md.
<!-- cpt-cf-id-content -->

#### At-Most-One Leader Under Linearizable Backends

- [x] `p1` - **ID**: `cpt-cf-clst-nfr-leader-guarantee`

<!-- cpt-cf-id-content -->
For any election whose backend is declared linearizable, the system MUST guarantee that at most one participant observes leadership at any time. Under non-linearizable backends, the consumer must explicitly acknowledge the weaker guarantee; the system MUST NOT silently downgrade. Leader loss MUST be detectable within the configured TTL.

**Threshold**: Under contention testing with 10+ concurrent candidates across 3+ nodes against a linearizable backend, zero observed split-brain occurrences.
**Rationale**: Split-brain leadership corrupts shard assignment and worker pool coordination. The capability-validation rule prevents this from being a silent footgun.
**Architecture Allocation**: See DESIGN.md §3.11 and ADR-009.
<!-- cpt-cf-id-content -->

#### Bounded Lock Holding Enforced at Compile Time

- [x] `p1` - **ID**: `cpt-cf-clst-nfr-bounded-critical-section`

<!-- cpt-cf-id-content -->
Consumer code holding a cluster lock MUST NOT make remote calls inside the critical section. The system MUST include a workspace-level static-analysis rule that flags violations at compile time. Combined with async timeouts on every operation, this constraint architecturally eliminates the unbounded-pause stale-writer scenario.

**Threshold**: Zero static-analysis violations in workspace consumer code; integration tests verify that locks held under normal operation are released within bounded time (single-digit seconds).
**Rationale**: Compile-time enforcement prevents the rule from rotting into "aspirational documentation" that nobody reads. The architectural rule is strictly stronger than the fencing-token mitigation it replaces.
**Architecture Allocation**: See DESIGN.md and ADR-002.
<!-- cpt-cf-id-content -->

#### Watch Delivery: At-Most-Once with Per-Key Ordering

- [x] `p1` - **ID**: `cpt-cf-clst-nfr-watch-delivery`

<!-- cpt-cf-id-content -->
Watch events MUST be delivered at most once per subscriber. The system makes no exactly-once delivery guarantee — events may be missed during partitions or subscriber backpressure, in which case the system MUST surface lag or reset signals so consumers can recover. Per-key event ordering MUST be preserved.

**Threshold**: Zero duplicate events per subscriber per key in normal operation; per-key ordering verified under concurrent writes; lag and reset signals observable in induced-failure smoke tests.
**Rationale**: At-most-once with per-key ordering maps to every backend cleanly. Reliable messaging with stronger delivery guarantees belongs in the event broker, not here.
**Architecture Allocation**: See DESIGN.md and ADR-003.
<!-- cpt-cf-id-content -->

#### Observability Contract

- [x] `p1` - **ID**: `cpt-cf-clst-nfr-observability`

<!-- cpt-cf-id-content -->
All cluster plugins MUST emit OpenTelemetry spans, Prometheus metrics, and structured log events using a stable naming convention defined in the observability reference. Span names, metric names, log event names, their attributes, and the rules for which fields can appear as high-cardinality metric labels are part of the cluster gear's contract — renames are breaking changes. Operation keys, lock names, and election names MUST NOT appear as metric labels (cardinality control); they MAY appear in trace attributes and log fields.

**Threshold**: Every signal defined in the observability reference is emitted by every plugin; no high-cardinality labels appear in metrics; consumer-built dashboards remain stable across plugin minor versions.
**Rationale**: Cluster is foundational infrastructure that every gear depends on. Inconsistent observability across plugins forces consumers to retrofit their own per-plugin instrumentation, producing uneven coverage and gaps during incidents.
**Architecture Allocation**: See DESIGN.md and ADR-004.
<!-- cpt-cf-id-content -->

#### Programmatic Error Retryability

- [x] `p1` - **ID**: `cpt-cf-clst-nfr-error-retryability`

<!-- cpt-cf-id-content -->
Backend-specific errors MUST be wrapped in a structured form that lets consumers make programmatic retryability decisions without parsing backend-specific error strings. The structured form classifies errors as: connection lost (retryable after reconnect), timeout (retryable), authentication failure (not retryable), resource exhaustion (retryable with backoff), or other.

**Threshold**: Every plugin maps its native error taxonomy to the structured classification; consumer retry logic is identical across backends.
**Rationale**: Different infrastructure errors require different responses (retry, fail-fast, circuit-break). Without a structured classification, every consumer reimplements per-backend error parsing.
**Architecture Allocation**: See DESIGN.md.
<!-- cpt-cf-id-content -->

#### Plugin Contract Stability Across Versions

- [x] `p1` - **ID**: `cpt-cf-clst-nfr-plugin-stability`

<!-- cpt-cf-id-content -->
The plugin contract — the interface a plugin author implements to adapt a backend to a primitive — MUST remain stable within a major version. Plugins built against version N MUST continue to work against version N.x for any value of x. Breaking changes to the plugin contract MUST be expressed as a new major version that coexists with the prior version, allowing plugin crates to migrate on independent schedules from cluster.

**Threshold**: A plugin compiled against the initial released version of the cluster contract MUST work against every minor and patch release of the same major version without modification.
**Rationale**: Plugin authors are typically not the same teams as cluster maintainers (think: NATS plugin maintained by an external team). Forcing plugin authors to recompile and re-release on every cluster minor version creates an ecosystem coordination problem.
**Architecture Allocation**: See DESIGN.md.
<!-- cpt-cf-id-content -->

### 6.2 NFR Exclusions

The following non-functional concerns are deliberately NOT in scope for this change cycle:

- **Standalone-plugin latency NFR**: deferred to the standalone plugin follow-up change. The smoke-test in-process stubs in this change exercise contract shape, not production latency.
- **Per-backend performance numbers** (throughput ceilings, p50/p99 latency under load, connection-pool sizing): each plugin's own integration tests own these. ADR-001 documents qualitative envelopes; quantitative SLOs are per-plugin.
- **Cluster-wide compliance NFRs** (FedRAMP, FIPS-140, PCI, etc.): platform-tier concern handled by the platform compliance baseline, not by individual gears.
- **Backend authentication and credential management NFRs**: deferred to the broader OOP deployment design as captured in §13 Open Questions.
- **Cross-cluster / geo-distributed coordination performance**: cluster is single-cluster scope per §4.2.
- **Universal linearizability across all primitives regardless of backend**: cluster does NOT promise linearizability as a flat NFR — each primitive's consistency depends on its bound backend, surfaced through capability validation per §5.6.

## 7. Public Library Interfaces

### 7.1 Public API Surface

The cluster gear exposes a public API consumed by Gears and a separate plugin-facing interface implemented by per-backend plugins. The two surfaces evolve on independent versioning schedules per the plugin-contract-stability NFR — a plugin built against the initial plugin-facing interface continues working against future minor versions of the consumer-facing API.

#### Consumer API

- [ ] `p1` - **ID**: `cpt-cf-clst-interface-consumer`

<!-- cpt-cf-id-content -->
**Type**: Rust public-API per-primitive surface
**Stability**: stable (V1)
**Description**: Consumer gears see four primitive-specific entry points (cache, leader election, distributed lock, service discovery). For each, they declare a profile and any required capabilities, and receive a working primitive or a startup error. The consumer surface is intentionally narrow — consumers do not hold or name plugin-side types; the platform mediates entirely.
**Breaking Change Policy**: Major version bump per primitive, ships independently of the others; the platform supports one previous major version concurrently to give consumers a migration window.
<!-- cpt-cf-id-content -->

#### Plugin Interface

- [ ] `p1` - **ID**: `cpt-cf-clst-interface-plugin`

<!-- cpt-cf-id-content -->
**Type**: Rust plugin-facing surface
**Stability**: stable (V1)
**Description**: Plugin authors implement one or more primitive contracts and declare their backend's characteristics so the platform can validate consumer capability requirements. Plugins ship as separate crates on independent release schedules. A plugin implementing only the cache primitive automatically gets working leader election, lock, and service discovery via cluster-provided defaults built on cache.
**Breaking Change Policy**: Major version bump per primitive contract, ships independently; the prior major version remains supported during a migration window.
<!-- cpt-cf-id-content -->

### 7.2 External Integration Contracts

Per-backend wire formats and external resource layouts are deferred to per-plugin follow-up changes per §4.2 Out of Scope. This includes:

- **Postgres plugin**: schema (cache table, lock table, advisory-lock key allocation, NOTIFY channel naming), `synchronous_commit` configuration requirements (per ADR-009).
- **K8s plugin**: API resources used (`coordination.k8s.io/v1.Lease` per leader election and per service-discovery instance per ADR-008; CRD shape for cache; RBAC requirements).
- **Redis plugin**: key naming patterns, Lua script catalog for atomic operations, keyspace-notification configuration.
- **NATS plugin**: KV bucket naming, JetStream replication factor requirements (per ADR-009 — R≥3 for leader/lock).
- **etcd plugin**: KV key layout, lease/watch usage patterns.
- **Standalone plugin**: in-process types (no external wire format).

This PRD does not enumerate these contracts. Each plugin's own PRD/DESIGN documents its external integration surface.

## 8. Use Cases

#### UC-001: Event Broker Elects Worker Pool Leader

- [ ] `p1` - **ID**: `cpt-cf-clst-usecase-worker-leader`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-clst-actor-event-broker`

**Preconditions**:
- Cluster is up; the deployment's profile binds leader election to a linearizable backend
- Multiple event broker instances are running

**Main Flow**:
1. Each event broker instance joins the worker-pool election on startup
2. Exactly one instance is reported as leader; all others are reported as followers
3. The leader writes shard-assignment state to cache; followers subscribe to the shard-assignment cache prefix and react to changes
4. If the leader fails or is partitioned, remaining participants observe leadership loss within the election TTL and one of them becomes the new leader

**Postconditions**:
- Exactly one event broker instance coordinates shard assignments at any time
- Shard assignment changes propagate to all instances reactively

**Alternative Flows**:
- **Leader steps down gracefully during planned shutdown**: The leader observes loss before shutdown completes; remaining candidates promote a new leader within a backend round-trip rather than waiting for TTL.
- **Leader observes transient loss**: The system reports leadership loss; the consumer continues participating without writing re-enrollment code; the next leader event is either re-acquired or follower.
- **Watcher falls behind**: The cache-watch surfaces a lag signal; the follower reads the current shard-assignment state to recover and continues.
<!-- cpt-cf-id-content -->

#### UC-002: OAGW Acquires Rate Limit Lock

- [ ] `p1` - **ID**: `cpt-cf-clst-usecase-rate-limit`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-clst-actor-oagw`

**Preconditions**:
- Cluster is up; the OAGW profile binds the lock primitive to a linearizable backend
- OAGW is processing an API request that requires per-tenant rate limiting

**Main Flow**:
1. OAGW attempts non-blocking lock acquisition for the per-tenant key
2. Lock is available; OAGW reads and increments the rate counter via local cache CAS — no remote calls inside the critical section
3. OAGW releases the lock explicitly
4. Other OAGW instances can now acquire the lock for the same tenant

**Postconditions**:
- Rate counter is atomically incremented; no double-counting across instances
- Lock released within a backend round-trip of step 3

**Alternative Flows**:
- **Lock is held by another instance**: Acquisition returns a contention error immediately; OAGW retries, sheds load, or rejects the request based on rate-limiting policy.
- **OAGW crashes mid-critical-section**: The lock's TTL releases the backend entry within a bounded window; the next acquirer proceeds. Because the critical section did no remote work, no stale writes are possible.
- **Consumer forgets to release explicitly**: TTL releases the lock; bounded leak window.
<!-- cpt-cf-id-content -->

#### UC-003: Gear Resolves Primitive with Capability Validation

- [ ] `p1` - **ID**: `cpt-cf-clst-usecase-profile-resolve`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-clst-actor-platform-gear`

**Preconditions**:
- The gear's crate has declared a typed profile reference once
- The deployment's YAML binds a backend to the cache primitive for that profile

**Main Flow**:
1. The gear resolves the cache primitive for its profile, declaring "I require linearizable cache and native prefix subscriptions."
2. The platform looks up the bound backend, checks its declared characteristics against the gear's requirements
3. Both requirements are satisfied; the platform returns a working cache primitive
4. The gear uses the cache for its application-level work

**Postconditions**:
- The gear has a validated cache primitive matching its declared requirements

**Alternative Flows**:
- **Capability mismatch**: The bound backend does not declare native prefix subscription support; the platform fails startup with an error naming the gear, the primitive, the unmet requirement, and the bound backend. The operator either binds a different backend or contacts the gear owner to relax the requirement.
- **Profile not bound at all**: The platform fails startup with an error naming the missing profile.
- **Gear forgets to specify the profile in code**: The build fails on a typed-reference error long before deployment.
<!-- cpt-cf-id-content -->

#### UC-004: Operator Routes Primitives to Different Backends per Profile

- [ ] `p1` - **ID**: `cpt-cf-clst-usecase-mixed-routing`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-clst-actor-operator`

**Preconditions**:
- K8s deployment with Redis provisioned
- Operator wants Redis for cache (high throughput) and K8s Lease for leader election (consistency)

**Main Flow**:
1. Operator writes a profile with per-primitive bindings: cache → Redis, leader election → K8s Lease, lock → Redis, service discovery → K8s Lease (per instance)
2. The cluster gear starts each plugin once and registers each backend under the profile
3. Consumer gears referencing this profile resolve cache and lock through Redis, leader election and service discovery through K8s

**Postconditions**:
- Each primitive routes to the operator's chosen backend; consumer gears are unaware of the mix

**Alternative Flows**:
- **Single-backend convenience**: Operator binds only the cache primitive to Postgres for a profile; the system automatically provides leader election, lock, and service discovery via cluster-provided defaults built on the Postgres cache. Operator writes one config block instead of four.
- **Capability mismatch at startup**: Operator binds an eventually-consistent cache and a consumer requires linearizable; startup fails with a specific error before traffic ever reaches the consumer gear.
<!-- cpt-cf-id-content -->

#### UC-005: Dispatcher Routes by Topic Shard via Service Discovery

- [ ] `p1` - **ID**: `cpt-cf-clst-usecase-service-discovery`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-clst-actor-event-broker`

**Preconditions**:
- Multiple event-broker delivery instances are running, each owning one or more topic shards
- The deployment binds service discovery to a backend supporting metadata-based discovery
- Topics and their shards are dynamic — added at runtime, redistributed when delivery instances scale

**Main Flow**:
1. Each delivery instance registers itself under the "delivery" service name with metadata indicating which topic shard it currently owns
2. The dispatcher discovers the delivery instance(s) currently serving a specific topic shard by passing a metadata filter to the discovery operation; for fan-out routing across multiple shards, the dispatcher passes a set-membership filter so all matching instances come back in one call
3. The dispatcher subscribes to the delivery service's topology watch and updates its routing table when instances join, leave, or change ownership

**Postconditions**:
- The dispatcher has an up-to-date view of enabled delivery instances filtered by topic shard
- Sharded routing decisions are made in one discovery call, not N+1 lookups for fan-out

**Alternative Flows**:
- **No instances currently own the requested shard**: Discovery returns an empty list; the dispatcher applies its no-owner policy (queue, retry, or fail).
- **Delivery instance shuts down gracefully**: The instance flips its serving intent to disabled before deregistering; the dispatcher excludes it from new routing as soon as the topology watch notifies the change.
- **Delivery instance crashes**: TTL-bounded heartbeat stops; the instance disappears from discovery within the TTL window. Detection of the crash is via heartbeat expiry, not via the serving-intent signal — serving intent is gear-declared, not externally observed.
- **Dispatcher's topology watch falls behind**: The watch surfaces a lag or reset signal; the dispatcher re-reads current membership via discovery to recover.
<!-- cpt-cf-id-content -->

## 9. Acceptance Criteria

- [ ] All four coordination primitives are exposed through a uniform consumer-facing surface where consumers declare a profile and capability requirements and receive either a working primitive or a startup error
- [ ] All four primitives have a corresponding plugin interface that backend authors implement, with honest characteristic declaration so the platform can validate consumer requirements
- [ ] The system ships default leader-election, lock, and service-discovery implementations built on the cache primitive, so a plugin author implementing only cache produces a working four-primitive integration
- [ ] An operator binding only the cache primitive in a profile results in working leader-election, lock, and service-discovery automatically
- [ ] An operator binding different backends to different primitives within one profile results in each primitive routing to its bound backend
- [ ] A consumer gear declaring a capability requirement that the bound backend cannot meet produces a startup error within bounded time, naming the gear, primitive, unmet requirement, and bound backend
- [ ] All three watch types — cache, leader election, service discovery — surface lag, reset, and close lifecycle signals that consumers handle uniformly
- [ ] Graceful shutdown revokes leadership confidence before consumer code can run again on stale assumptions; the watch then ends terminally
- [ ] Consumer code holding a cluster lock cannot make remote calls inside the critical section without triggering a workspace-wide static-analysis error at compile time
- [ ] Consumer gears declare profile references once per crate as typed identifiers, never as bare strings at call sites
- [ ] Per-gear sub-namespacing inside a primitive is composable and applies to coordination names (cache keys, lock names, election names, service names) but does NOT apply to service-discovery metadata keys or values
- [ ] Backend-specific errors are wrapped in a structured form supporting programmatic retryability decisions
- [ ] Smoke tests cover all of the above against minimal in-process test backends without external infrastructure
- [ ] Showcase example gears demonstrate single-primitive, multi-primitive, multi-profile, and plugin-author usage patterns

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| ToolKit framework | Gear lifecycle, dependency ordering, plugin registration | `p1` |
| Async runtime | All cluster operations are async; the platform's standard async runtime is required | `p1` |

## 11. Assumptions

- The platform's existing gear-dependency mechanism is sufficient to enforce that the cluster's owning gear starts before any consumer gear attempts primitive resolution. No new framework-level ordering primitives are needed.
- Plugin authors will honestly declare their backend's characteristics. Capability validation depends on this — a plugin that lies about being linearizable defeats the validation.
- The five deployment shapes documented in §3.1 cover the operationally relevant cases. Variations (multi-region, geo-distributed) are explicitly out of scope.
- The platform's broader OOP credentials work will land before the first per-backend plugin requiring credentials (Postgres, K8s, Redis) ships to production. The cluster contract established by this change does not depend on the credential model.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|-----------|
| Backend characteristics are subtle and plugins may declare them dishonestly | Capability validation produces false negatives or false positives, degrading the startup-validation guarantee | Per-backend integration tests in each plugin's own change verify declared characteristics against actual behavior. Reference documentation captures known per-backend gotchas (Redis Sentinel async replication, Postgres `synchronous_commit=off`, NATS replication factor). |
| Per-primitive routing config is too flexible for operators to use safely | Operators produce confusing combinations that work in one environment but not another | Documented recommended deployment combinations in DESIGN.md. Capability validation at startup catches incompatible combinations before traffic. Single-backend omit-default convenience covers the common case in one config block. |
| Smoke tests verify API shape, not distributed correctness | Smoke tests pass against in-process stubs, but real-backend failures (partition, clock skew, split-brain) are not exercised in this change | Each per-backend plugin ships its own integration tests against the real backend in CI. Distributed correctness is verified per-plugin, not by the SDK. |
| The change introduces a contract that follow-up plugins must conform to; if the contract is wrong, every plugin pays | Plugin authors must rework against contract changes; the platform may have to ship breaking versions | Three rounds of architect review before contract freeze. Smoke-test the contract against minimal in-process stubs before any external plugin is built. The plugin contract supports independent versioning so contract evolution doesn't force all plugins to migrate simultaneously. |

## 13. Open Questions

| Question | Owner | Target Resolution |
|----------|-------|-------------------|
| Backend authentication and credential management | Platform OOP deployment design | Resolved as part of the broader OOP design; not blocking this change |
| Whether the watch-lifecycle-signals architectural pattern (originally documented for cache) gets a single broad ADR or one ADR per primitive | Cluster gear owner | Resolved during ADR audit task in this change's implementation |

## 14. Traceability

The functional and non-functional requirements above are realized by architectural decisions documented in `DESIGN.md` and (for cross-cutting choices) in the `ADR/` series. The following table maps requirement groups back to the relevant DESIGN sections and ADRs; the inverse mapping (decision → FR coverage) is verified during the pre-archive documentation audit task.

| Requirement group | Realized by (DESIGN section) | Target ADRs |
|-------------------|------------------------------|-------------|
| Cache (5.1) | §3.3 cache contract; §3.11 SDK default backends | ADR-001 (backend compatibility); ADR-003 (watch lifecycle) |
| Leader election (5.2) | §3.3 leader-election contract; §3.11 SDK default backends | ADR-001; ADR-003 (watch lifecycle); ADR-009 (per-backend safety under failure) |
| Distributed lock (5.3) | §3.3 lock contract; §2 no-I/O-in-Drop and no-remote-in-critical-section principles | ADR-002; ADR-009 (per-backend safety under failure) |
| Service discovery (5.4) | §3.3 service-discovery contract; serving-intent vs health note | ADR-001; ADR-003 (watch lifecycle); ADR-008 (state is intent, not health) |
| Per-backend routing (5.5) | §3.2 component model; §3.11 SDK defaults; §3.12 polyfill | ADR-001; ADR-006 (builder/handle pattern) |
| Consumer requirements and validation (5.6) | §3.6 resolution pattern; §3.10 capability validation | ADR-007 (capability typing and profile resolution) |
| Lifecycle and shutdown (5.7) | §3.7 builder/handle pattern; §3.13 shutdown sequence | ADR-002; ADR-006 (builder/handle pattern) |
| Operational namespacing (5.8) | §3.8 per-primitive scoping | (covered in DESIGN.md §3) |
| Capability validation NFR | §3.6; §3.10 | ADR-007 |
| Cross-backend stability NFR | §3.2; §3.3; §3.6 | ADR-005 (facade + backend trait pattern) |
| Leader guarantee NFR | §3.3 leader-election contract; §3.11 default leader-election backend | ADR-001; ADR-009 (per-backend safety; constructor pair) |
| Bounded critical section NFR | §3.3 lock contract; §2 principles | ADR-002 |
| Watch delivery NFR | §3.9 watch event shape | ADR-003 |
| Watch auto-restart (5.x) | §3.1 `RetryPolicy` / `RestartingWatch<W>`; §3.3 `*Watch::auto_restart`; §3.9 retryability table | ADR-003 §"SDK auto-restart combinator" |
| Observability contract NFR | §3.3 telemetry expectations | ADR-004 |
| Plugin contract stability NFR | §3.2 component model; per-primitive versioning policy | ADR-005 |
