# Feature: Service Discovery Primitive

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-service-discovery-implemented`

- [x] `p2` - `cpt-cf-clst-feature-service-discovery`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Register an Instance with Metadata](#register-an-instance-with-metadata)
  - [Discover Instances with a Filter](#discover-instances-with-a-filter)
  - [Watch Topology and Recover](#watch-topology-and-recover)
  - [Drain on Graceful Shutdown](#drain-on-graceful-shutdown)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Apply Discovery Filter](#apply-discovery-filter)
  - [Heartbeat Renewal](#heartbeat-renewal)
- [4. States (CDSL)](#4-states-cdsl)
  - [Instance Serving-Intent State Machine](#instance-serving-intent-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Service-Discovery Backend Trait and Facade](#service-discovery-backend-trait-and-facade)
  - [Discovery Types and Filter](#discovery-types-and-filter)
  - [Topology Watch](#topology-watch)
  - [Registration Handle and Serving Intent](#registration-handle-and-serving-intent)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Provides instance registration with metadata, a single extensible discovery filter (serving-state plus AND-conjoined metadata predicates, defaulting to enabled-only), an unfiltered topology watch with lifecycle signals, and a module-declared serving-intent signal that is explicitly intent — not a health observation.

### 1.2 Purpose

Out-of-process deployments need to know which instances are alive, where they are, and which can take work, with routing decisions driven by metadata. This feature delivers registration, filtered discovery, reactive topology, and a drain signal, while keeping serving intent distinct from externally observed health.

**Requirements**: `cpt-cf-clst-fr-sd-register`, `cpt-cf-clst-fr-sd-discover`, `cpt-cf-clst-fr-sd-watch`, `cpt-cf-clst-fr-sd-state`

**Principles**: `cpt-cf-clst-principle-watch-union-shape`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-event-broker` | Registers delivery instances by topic shard and routes by metadata |
| `cpt-cf-clst-actor-platform-gear` | Registers itself and discovers peers for cross-instance routing |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.4
- **Design**: [DESIGN.md](../DESIGN.md) §3.3 (service-discovery contract), §3.1 (entities), §3.8 (metadata is not scoped)
- **ADRs**: [ADR-008](../ADR/008-service-discovery-state-is-intent-not-health.md), [ADR-003](../ADR/003-watch-event-lifecycle-contract.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-sdk-foundation`
  - [x] `p2` - `cpt-cf-clst-feature-cache-primitive`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — backend-determined: discovery is a single set-membership call; result latency depends on the bound backend.
- Reliability — addressed: TTL-bounded heartbeat expiry, topology-watch recovery on lagged/reset, and graceful drain (§2–§3) define behavior under instance loss.

## 2. Actor Flows (CDSL)

### Register an Instance with Metadata

- [x] `p1` - **ID**: `cpt-cf-clst-flow-service-discovery-register`

**Actor**: `cpt-cf-clst-actor-event-broker`

**Success Scenarios**:
- The instance is registered with an address and metadata, defaulting to enabled, and stays discoverable while heartbeating.

**Error Scenarios**:
- Heartbeat stops — the instance disappears from discovery within its TTL window.

**Steps**:
1. [x] - `p1` - Consumer submits a registration with a service name, address, and metadata - `inst-rg-submit`
2. [x] - `p1` - **IF** no instance id is provided - `inst-rg-noid`
   1. [x] - `p1` - The backend assigns an instance id - `inst-rg-assign`
3. [x] - `p1` - SDK registers the instance as enabled with a TTL-bounded heartbeat - `inst-rg-register`
4. [x] - `p1` - **RETURN** a registration handle - `inst-rg-return`

### Discover Instances with a Filter

- [x] `p1` - **ID**: `cpt-cf-clst-flow-service-discovery-discover`

**Actor**: `cpt-cf-clst-actor-event-broker`

**Success Scenarios**:
- A single discovery call returns instances matching a serving-state and metadata filter; fan-out uses set-membership in one call.

**Error Scenarios**:
- No instance matches — an empty result is returned and the caller applies its no-owner policy.

**Steps**:
1. [x] - `p1` - Consumer calls discover with a service name and a filter (default: enabled-only, no metadata constraint) - `inst-ds-call`
2. [x] - `p1` - SDK returns instances matching the serving state and every metadata predicate (AND) - `inst-ds-match`
3. [x] - `p1` - **IF** the caller needs deterministic selection - `inst-ds-order`
   1. [x] - `p1` - Caller sorts client-side (result order is unspecified) - `inst-ds-sort`
4. [x] - `p1` - **RETURN** the matching instances - `inst-ds-return`

### Watch Topology and Recover

- [x] `p1` - **ID**: `cpt-cf-clst-flow-service-discovery-watch`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- The consumer reacts to join/leave/update events and keeps a routing table current.

**Error Scenarios**:
- The watch lags or resets — the consumer re-reads membership via discovery.

**Steps**:
1. [x] - `p1` - Consumer subscribes to the service's topology watch (unfiltered) - `inst-tw-subscribe`
2. [x] - `p1` - Consumer applies its own filter client-side to each change event - `inst-tw-filter`
3. [x] - `p1` - **IF** the watch reports lagged or reset - `inst-tw-lag`
   1. [x] - `p1` - Consumer re-reads current membership via discovery - `inst-tw-reread`
4. [x] - `p1` - **IF** the watch reports a terminal close - `inst-tw-closed`
   1. [x] - `p1` - **RETURN** and stop consuming the watch - `inst-tw-stop`

### Drain on Graceful Shutdown

- [x] `p1` - **ID**: `cpt-cf-clst-flow-service-discovery-drain`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- The instance flips its serving intent to disabled before deregistering, so routers stop sending it new work first.

**Steps**:
1. [x] - `p1` - Consumer sets the instance's serving intent to disabled - `inst-dr-disable`
2. [x] - `p1` - Watchers receive an update event and exclude the instance from new routing - `inst-dr-exclude`
3. [x] - `p1` - Consumer deregisters the instance explicitly - `inst-dr-deregister`

## 3. Processes / Business Logic (CDSL)

### Apply Discovery Filter

- [x] `p1` - **ID**: `cpt-cf-clst-algo-service-discovery-filter`

**Input**: A candidate instance set and a discovery filter

**Output**: The subset matching serving state and all metadata predicates

**Steps**:
1. [x] - `p1` - **FOR EACH** candidate instance - `inst-fl-foreach`
   1. [x] - `p1` - **IF** the instance serving state does not match the state filter - `inst-fl-state`
      1. [x] - `p1` - Exclude the instance - `inst-fl-state-skip`
   2. [x] - `p1` - **FOR EACH** metadata predicate - `inst-fl-meta`
      1. [x] - `p1` - **IF** the instance metadata does not satisfy the predicate (equals or one-of) - `inst-fl-meta-check`
         1. [x] - `p1` - Exclude the instance - `inst-fl-meta-skip`
2. [x] - `p1` - **RETURN** the included instances (order unspecified) - `inst-fl-return`

### Heartbeat Renewal

- [x] `p1` - **ID**: `cpt-cf-clst-algo-service-discovery-heartbeat`

**Input**: A registered instance with a TTL

**Output**: Continued presence in discovery, or TTL-bounded disappearance

**Steps**:
1. [x] - `p1` - Renew the registration on an interval derived from the TTL - `inst-hb-renew`
2. [x] - `p1` - **IF** the instance stops heartbeating - `inst-hb-stop`
   1. [x] - `p1` - The registration expires and the instance disappears from discovery within the TTL - `inst-hb-expire`

## 4. States (CDSL)

### Instance Serving-Intent State Machine

- [x] `p1` - **ID**: `cpt-cf-clst-state-service-discovery-instance`

**States**: Enabled, Disabled

**Initial State**: Enabled

**Transitions**:
1. [x] - `p1` - **FROM** Enabled **TO** Disabled **WHEN** the module flips serving intent to drain - `inst-is-disable`
2. [x] - `p1` - **FROM** Disabled **TO** Enabled **WHEN** the module flips serving intent to serve - `inst-is-enable`

This is module-declared serving intent, not a health observation: a stuck instance cannot flip its own intent; it disappears from discovery only when its TTL-bounded heartbeat stops. External liveness detection is out of scope for this primitive.

## 5. Definitions of Done

### Service-Discovery Backend Trait and Facade

- [x] `p1` - **ID**: `cpt-cf-clst-dod-service-discovery-backend-facade`

The system **MUST** provide the service-discovery backend trait and facade with register, discover, and watch, plus a fluent resolver with capability validation. The backend trait **MUST** be dyn-compatible.

**Implements**:
- `cpt-cf-clst-flow-service-discovery-register`
- `cpt-cf-clst-flow-service-discovery-discover`
- `cpt-cf-clst-algo-service-discovery-heartbeat`

**Touches**:
- Entities: ServiceDiscoveryBackend, ServiceDiscoveryV1, ServiceDiscoveryCapability, ServiceDiscoveryFeatures

### Discovery Types and Filter

- [x] `p1` - **ID**: `cpt-cf-clst-dod-service-discovery-types`

The system **MUST** provide the registration and discovered-instance types, the serving-state enum, the per-key metadata predicate, the serving-state filter, and the extensible discovery filter (default enabled-only; AND-conjoined metadata; explicit opt-in for all states). Result order is unspecified.

**Implements**:
- `cpt-cf-clst-flow-service-discovery-discover`
- `cpt-cf-clst-algo-service-discovery-filter`

**Touches**:
- Entities: ServiceRegistration, ServiceInstance, InstanceState, MetaMatch, DiscoveryFilter, StateFilter

### Topology Watch

- [x] `p1` - **ID**: `cpt-cf-clst-dod-service-discovery-watch`

The system **MUST** provide an unfiltered topology watch yielding join/leave/update changes plus the watch-union lifecycle signals (lagged, reset, closed), so consumers filter client-side and recover by re-reading membership.

**Implements**:
- `cpt-cf-clst-flow-service-discovery-watch`

**Touches**:
- Entities: ServiceWatch, ServiceWatchEvent, TopologyChange

### Registration Handle and Serving Intent

- [x] `p1` - **ID**: `cpt-cf-clst-dod-service-discovery-handle`

The system **MUST** provide a registration handle supporting explicit deregister, metadata update, and serving-intent flip (enabled/disabled), with a no-op drop (no I/O in drop). New registrations default to enabled; modules may register or flip to disabled before exposing themselves to traffic.

**Implements**:
- `cpt-cf-clst-flow-service-discovery-drain`
- `cpt-cf-clst-state-service-discovery-instance`

**Touches**:
- Entities: ServiceHandle

## 6. Acceptance Criteria

- [x] An instance registers with metadata, defaults to enabled, gets an auto-assigned id when none is provided, and disappears within its TTL when heartbeating stops.
- [x] A single discovery call returns instances matching serving state and all metadata predicates; the default filter is enabled-only; all-states requires explicit opt-in.
- [x] The topology watch yields join/leave/update events unfiltered and surfaces lagged/reset/closed signals.
- [x] Serving intent is module-declared (enabled/disabled) and is documented as intent, not health.
- [x] The registration handle supports explicit deregister, metadata update, and serving-intent flip, with no I/O on drop.
