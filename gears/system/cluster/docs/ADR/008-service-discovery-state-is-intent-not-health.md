---
status: accepted
date: 2026-04-09
---

# ADR-008: Service Discovery — Gear-declared Serving Intent, Not Health

**ID**: `cpt-cf-clst-adr-sd-state-is-intent-not-health`

> Decision originally captured during the cluster design review (APPLIED 2026-04-09); promoted to versioned ADR-of-record on 2026-04-27.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [What `state` means and what it does not mean](#what-state-means-and-what-it-does-not-mean)
  - [Liveness via heartbeat/TTL, not state](#liveness-via-heartbeatttl-not-state)
  - [K8s mapping: Lease per instance, not EndpointSlice](#k8s-mapping-lease-per-instance-not-endpointslice)
  - [External probes: deferred, not rejected](#external-probes-deferred-not-rejected)
  - [Discovery filter defaults](#discovery-filter-defaults)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Self-reported HealthStatus enum](#option-1-self-reported-healthstatus-enum)
  - [Option 2: Cluster-driven liveness probing](#option-2-cluster-driven-liveness-probing)
  - [Option 3: Rename to `InstanceState { Enabled, Disabled }` with honest semantics (CHOSEN)](#option-3-rename-to-instancestate--enabled-disabled--with-honest-semantics-chosen)
  - [Option 4: Drop `ServiceDiscovery` entirely; use K8s `Service`/`EndpointSlice` directly](#option-4-drop-servicediscovery-entirely-use-k8s-serviceendpointslice-directly)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

A registered service instance has two questions a discoverer might want answered: "is this instance running?" (liveness) and "is this instance willing to serve traffic right now?" (serving intent). An earlier shape of the cluster's `ServiceDiscovery` API conflated them under a single `HealthStatus { Healthy, Unhealthy }` enum, exposed as `ServiceHandle::set_health(HealthStatus)`. A registered gear that wanted to drain traffic before shutdown called `set_health(Unhealthy)`; a discoverer using `discover_healthy(name)` would skip it.

This shape is wrong. Self-reported health is unreliable in exactly the failure modes it matters most:

- **A broken gear cannot report that it is broken.** Deadlocked, OOM, infinite-loop gears are by definition not running the code path that would call `set_health(Unhealthy)`. The registry continues to report them as healthy.
- **A partially-broken gear has false confidence.** A gear returning errors to every request but whose health-update loop is fine will report `Healthy` indefinitely.
- **Even correct self-reporting has a race.** A gear that calls `set_health(Unhealthy)` races against in-flight requests; routing updates lag.

Self-reported health is *not health*. It is a statement of intent by the observed party, not an observation by an external observer. Real systems (K8s, Consul, Eureka) all separate observation from declaration: the thing being judged does not get to judge itself. Probes (HTTP `/healthz`, TCP connect, exec) come from outside the observed instance.

Cluster's audience is platform gears running inside Gears, not arbitrary network services. Cluster also does not own a probe runner — building one is significant API surface (probe registration, per-backend probe semantics, health combination rules across multiple probe sources, observability). The right call is to be honest about what the API actually models — *serving intent* — and stop using the word "health".

This ADR captures the rename + semantic clarification + K8s remapping that result. The decision was originally recorded during the cluster design review as APPLIED on 2026-04-09; this ADR promotes it to a versioned ADR so it sits in the durable decision-of-record set alongside the other architectural choices.

## Decision Drivers

- **Cluster does not own probe infrastructure.** Building a probe runner inside the cluster gear is significant new API surface that no current consumer needs.
- **Self-reported health is structurally unreliable.** Every plausible failure mode either prevents the gear from updating its health or makes the update race-prone.
- **Gear-declared *serving intent* is genuinely useful.** A gear preparing to shut down legitimately wants to declare "stop sending me new traffic." That is *intent*, not *health*.
- **Liveness already has a mechanism.** Heartbeat/TTL renewal at the registration layer is how cluster knows an instance is alive. A stuck gear disappears from discovery when its TTL expires, regardless of any state field.
- **The cluster's word for "intent" must not be "health".** Reusing the word "health" for serving intent invites consumers to treat it as health and rebuild every failure mode this ADR aims to avoid.
- **K8s mapping must be honest.** `EndpointSlice` is a probe-driven concept (readiness probes flip endpoints in/out). If cluster doesn't own probes, mapping to `EndpointSlice` is a lie — endpoints would be in/out based on a self-declared bit that K8s users would reasonably assume is probe-derived.

## Considered Options

1. **Self-reported `HealthStatus { Healthy, Unhealthy }`** — keep the original API; document the unreliability.
2. **Cluster-driven liveness probing** — cluster owns a probe runner; consumers register probe configs.
3. **Rename to `InstanceState { Enabled, Disabled }` with honest semantics; cluster does not probe; liveness is via heartbeat/TTL** (CHOSEN).
4. **Drop `ServiceDiscovery` entirely**; consumers wanting service discovery use K8s `Service` / `EndpointSlice` directly.

## Decision Outcome

Chosen option: **Option 3** — rename the field, the enum, the filter, and the setter to use the word "state"; document the field as *gear-declared serving intent*; document liveness as a TTL/heartbeat property; map K8s service discovery to `coordination.k8s.io/v1.Lease` per instance, NOT to `EndpointSlice`.

The concrete renames:

| Old | New |
|---|---|
| `HealthStatus { Healthy, Unhealthy }` | `InstanceState { Enabled, Disabled }` |
| `HealthFilter { Healthy, Unhealthy, Any }` | `StateFilter { Enabled, Disabled, Any }` |
| `ServiceHandle::set_health(HealthStatus)` | `ServiceHandle::set_state(InstanceState)` |
| `DiscoveryFilter::health` | `DiscoveryFilter::state` |
| `ServiceInstance.health` | `ServiceInstance.state` |
| `ServiceDiscovery::discover_healthy(name)` | `ServiceDiscovery::discover(name, filter)` (with `filter.state = StateFilter::Enabled` as the default for primary routing) |

### What `state` means and what it does not mean

`state` is a single bit a registered gear can flip on its own registration:

- `Enabled` — the gear is willing to receive traffic for this instance under default routing rules.
- `Disabled` — the gear is asking discoverers using primary routing to skip this instance.

That is all. `Disabled` does not mean "the gear is broken". It does not mean "the gear's process is dead". It does not mean "you should not send any traffic". It means "under primary routing, skip me." A discoverer that wants every instance regardless of state passes `StateFilter::Any`; a discoverer that specifically wants disabled instances (e.g., for draining-traffic dashboards) passes `StateFilter::Disabled`.

API documentation MUST frame `state` as *intent*, not health. The word "health" does not appear in the public API surface (struct fields, enum variants, method names, doc comments).

### Liveness via heartbeat/TTL, not state

A registered instance survives only as long as the registration's TTL is renewed. The plugin's registration-layer background task heartbeats periodically (interval = TTL / `(max_missed_renewals + 1)`). A stuck gear fails to heartbeat; the registration's TTL elapses; the instance disappears from discovery. This happens regardless of the instance's `state` field.

Failure detection is therefore not the cluster's job. Cluster guarantees that a *crashed*, *deadlocked*, or *partitioned* instance disappears from discovery within roughly one TTL after it stops heartbeating. That is the platform's only liveness contract. If a consumer needs faster failure detection, they layer it on top: circuit breakers, outlier detection, request-time fallbacks. Cluster does not own that surface.

### K8s mapping: Lease per instance, not EndpointSlice

The K8s plugin (follow-up change) maps `ServiceDiscovery` to `coordination.k8s.io/v1.Lease` per instance, not to `Service` / `EndpointSlice`:

- `Lease` is a built-in resource (no CRD install required).
- `Lease.Spec.RenewTime` + `Lease.Spec.LeaseDurationSeconds` is the natural K8s representation of the heartbeat/TTL liveness contract.
- `Lease.Annotations` carries arbitrary metadata for filtering (no `EndpointSlice` topology constraints).
- The K8s plugin's leader-election backend already uses Lease; the renewal loop is reusable.

Why not `Service` / `EndpointSlice`:

- `EndpointSlice` is a probe-driven concept. K8s flips endpoints in/out of an `EndpointSlice` based on readiness probes. Cluster does not own probes; mapping a self-declared `Enabled`/`Disabled` bit to a probe-driven endpoint slot would be a lie to anyone reading the K8s manifests.
- `EndpointSlice` topology (per-zone, per-port) is rigid. Cluster's metadata filtering (`topic-shard`, `region`, `version`) does not project onto `EndpointSlice` fields.
- IP-keyed traffic routing (the actual purpose of `Service`/`EndpointSlice`) is a separate platform-tier concern from cluster's "discover instances by metadata" use case. Gears wanting K8s-native traffic routing use the K8s platform directly; they don't go through cluster's `ServiceDiscovery`.

### External probes: deferred, not rejected

A future extension can add probe-driven external observation as a *separate* signal alongside `state`. The shape would be roughly:

```rust
pub enum ServiceInstanceReachability {
    Reachable,           // probe succeeded
    Unreachable,         // probe failed
    Unknown,             // no probes configured
}
```

A discoverer's filter could combine `state` AND `reachability` (consumer specifies "enabled AND reachable" for primary routing; "any" for draining-traffic dashboards). The two surfaces would compose: `state` is consumer-controlled intent, `reachability` is observer-driven observation.

This extension is deferred — not because it is wrong, but because no current consumer needs it. Cluster ships the `state` surface today; if a consumer later needs probe-driven observation, the `reachability` extension lands as a non-breaking addition (`#[non_exhaustive]` enums and structs are designed for this).

### Discovery filter defaults

`DiscoveryFilter::default()` returns `state = StateFilter::Enabled` and no metadata predicate. This is the **primary routing** filter — discover instances that are willing to serve and don't impose any extra metadata constraint.

`DiscoveryFilter::any()` returns `state = StateFilter::Any` and no metadata predicate. This is for tools that want every registered instance (status dashboards, drain-traffic visualizations).

Other filters compose `StateFilter` with metadata predicates: `DiscoveryFilter::default().with_metadata("region", MetaMatch::Equals("us-east".into()))`.

The default-is-enabled choice keeps consumer code minimal at the common case (`sd.discover("delivery", DiscoveryFilter::default()).await?` does the right thing) and forces consumers wanting other behavior to be explicit.

### Consequences

- **The word "health" does not appear in the cluster's `ServiceDiscovery` public API.** Type names, field names, method names, doc comments — all use `state` / `enabled` / `disabled`.
- **Consumers wanting failure detection layer it on top of cluster.** Cluster guarantees TTL-bounded disappearance of stuck instances; everything finer-grained is on the consumer.
- **K8s plugin uses `Lease` per instance.** `EndpointSlice`-based service discovery is explicitly out of scope.
- **Probe-driven observation is a future extension, not part of this contract.** The `state` surface composes cleanly with a future `reachability` surface; nothing in the current API blocks the extension.
- **`DiscoveryFilter::default()` does the right thing for primary routing.** Consumers who want enabled-only routing don't have to think about it.
- **No `discover_healthy` convenience method.** The single `discover(name, filter)` method covers all cases. Filter construction is a one-line `DiscoveryFilter::default()` for the common case.
- **Trade-off**: existing K8s users who expected cluster's `ServiceDiscovery` to feed into K8s `Service` IP routing will be surprised. Documentation must call this out: cluster provides discovery-with-metadata, NOT IP-keyed traffic routing. Gears wanting both ask the platform for both, separately.

### Confirmation

- A consumer-side smoke test registers an instance with `state: Enabled`, calls `set_state(InstanceState::Disabled)`, and asserts that `discover` with default filter returns an empty list.
- A heartbeat-stop test stops a registered instance's heartbeat (without calling `deregister`); after `TTL + epsilon`, asserts the instance disappears from `discover` with `StateFilter::Any`.
- A docs lint (Phase 0d/0e) greps `ServiceDiscovery` API surface (struct fields, enum variants, method names, doc comments) and asserts no occurrence of "health", "healthy", "unhealthy".
- Future K8s plugin integration tests register instances as Lease objects and verify discovery returns them; explicitly do NOT register `EndpointSlice` resources.

## Pros and Cons of the Options

### Option 1: Self-reported HealthStatus enum

- Good, because it matches consumer expectation of "service discovery has a health concept."
- Bad, because self-reported health is structurally unreliable in every failure mode it matters most.
- Bad, because the word "health" implies an observation; this is a declaration. Naming mismatch invites consumer-side bugs.
- Bad, because consumers will reasonably assume probes drive health and will be surprised when a deadlocked gear never flips to `Unhealthy`.
- Bad, because production-grade real systems all separate observation from declaration; following the unreliable pattern would set cluster up to be replaced as soon as anyone noticed.

### Option 2: Cluster-driven liveness probing

- Good, because it would make the word "health" honest.
- Bad, because it's significant new API surface: probe registration shape, per-backend probe execution, probe failure threshold semantics, probe-result combination rules, probe observability.
- Bad, because probe execution is a backend-specific concern (HTTP probe, TCP probe, exec probe). Implementing it inside cluster would require either picking a single probe model (excludes use cases) or building a flexible probe registry (significant complexity).
- Bad, because no current consumer needs cluster-driven probes. Consumers that do need probes already have them (K8s readiness probes, OAGW upstream health checks).
- Bad, because probe ownership belongs in the platform's health/observability surface, not in the cluster gear. The cluster's job is coordination + discovery, not failure detection.

### Option 3: Rename to `InstanceState { Enabled, Disabled }` with honest semantics (CHOSEN)

- Good, because the renamed surface accurately models what the API actually does — gear-declared serving intent.
- Good, because it keeps the useful "drain-traffic" use case intact (a shutting-down gear flips `set_state(InstanceState::Disabled)` before it stops heartbeating).
- Good, because it's clear that liveness comes from heartbeat/TTL, not from `state`. No room for false confidence.
- Good, because external probes can be added later as a separate, composable signal without breaking the `state` contract.
- Good, because the K8s `Lease`-per-instance mapping is a clean, native K8s concept that doesn't lie about probe-driven semantics.
- Bad, because it's a rename of an established-feeling word ("health" → "state") that requires consumer migration if any code already used the old name.
- Bad, because some consumers may want probe-driven observation and have to layer it on top of cluster instead of getting it built-in. Mitigated by deferring (not rejecting) the future `reachability` extension.

### Option 4: Drop `ServiceDiscovery` entirely; use K8s `Service`/`EndpointSlice` directly

- Good, because zero new API surface in cluster — consumers use K8s primitives directly.
- Bad, because the event broker dispatcher needs to discover delivery instances by `topic-shard` metadata, dynamically rebalanced as topics are added at runtime. This use case does not map to K8s `Service` / `EndpointSlice` (no metadata filtering) and does not map to a static `StatefulSet` + DNS pattern (assignment is dynamic, not hash-based).
- Bad, because non-K8s deployments (Postgres-only, standalone) have no analog. Service discovery would become K8s-only and require a separate non-K8s discovery story.
- Bad, because cluster's `ServiceDiscovery` cleanly models *self-registration with metadata + topology watch*, which is exactly what the dispatcher use case requires. Removing it would force every consumer to reinvent it.

## More Information

**The dispatcher use case that anchored the decision.** The event broker dispatcher routes messages to delivery instances by `topic-shard`. Topics are added at runtime; shard-to-instance assignment is dynamic. The dispatcher needs `discover("delivery", filter.metadata("topic-shard", MetaMatch::OneOf(["t1-s0", "t1-s1", "t2-s0"])))` and the result is the set of instances responsible for those shards. Service discovery with metadata + watch is the right shape; K8s `Service`/`EndpointSlice` is not.

**Why `set_state(InstanceState)` and not `set_enabled(bool)`.** An earlier revision exposed the setter as `set_enabled(bool)`, reasoning that a bool reads unambiguously at the call site. Code review (PR #4098) reversed this: the domain already models the serving-intent state space as the `InstanceState` enum, so a bool forced every backend and the command/request path to re-map `true`/`false` back to `InstanceState` by hand — duplicated mapping that invites inconsistency — and `set_enabled(false)` is in fact *less* self-documenting than `set_state(InstanceState::Disabled)`. The setter therefore takes `InstanceState` directly, carrying the typed intent end-to-end through `ServiceCommand`/`ServiceRequest::SetState` with no lossy bool hop.

**Why state is NOT scoped by `scoped(prefix)`.** Per DESIGN.md §3.8, the per-primitive scoping rules namespace the service `name` only — `metadata` keys/values pass through unchanged, and so does `state`. State is a property of an instance, not a coordination namespace.

**Why `ProvisionalHealth` / `Reachable` / similar future variants compose cleanly.** Future probe-driven observation lands as a *separate field*, not as new variants of `InstanceState`. Combining "gear says Enabled" AND "external probe says Reachable" is the consumer's choice, expressed as a composed filter. Adding the second field is non-breaking because the structs are `#[non_exhaustive]`.

**References:**

- ADR-001 — backend compatibility. The cache-CAS-universal model includes `CacheBasedServiceDiscoveryBackend`, which uses `put(svc/{name}/{instance_id}, metadata, ttl)` and renews TTL — exactly the heartbeat/TTL liveness contract this ADR codifies.
- ADR-005 — facade + backend trait. `ServiceDiscoveryV1` (facade) wraps `Arc<dyn ServiceDiscoveryBackend>`; the rename is a backend-trait surface change.
- DESIGN.md §3.1 (`InstanceState` definition), §3.3 (service-discovery contract).
- PRD.md §5.4 (service discovery requirements: registration with metadata, discovery with state/metadata filtering, topology watch, gear-declared serving intent vs health).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-clst-fr-sd-state` — `state` is gear-declared serving intent (`Enabled`/`Disabled`).
- `cpt-cf-clst-fr-sd-state` — `DiscoveryFilter::default()` is enabled-only primary routing.
- `cpt-cf-clst-fr-sd-register` — Heartbeat/TTL renewal is the liveness signal, not state.
- `cpt-cf-clst-fr-sd-register` — Explicit deregister; no probe-driven endpoint flipping.
- DESIGN §3.1 `InstanceState` definition and `StateFilter` semantics.
- DESIGN §3.3 service-discovery contract — `register` / `discover` / `watch` / `set_state` method shapes.
- DESIGN §4.1 K8s plugin mapping — `Lease` per instance, NOT `EndpointSlice`.

**Sibling ADRs:**

- ADR-001 — `CacheBasedServiceDiscoveryBackend` uses heartbeat/TTL liveness consistent with this ADR.
- ADR-005 — `ServiceDiscoveryV1` facade hosts the renamed `set_state` / `state` surface.
