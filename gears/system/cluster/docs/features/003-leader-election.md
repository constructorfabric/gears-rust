# Feature: Leader Election Primitive

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-leader-election-implemented`

- [x] `p2` - `cpt-cf-clst-feature-leader-election`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Participate in Election and Observe Leadership](#participate-in-election-and-observe-leadership)
  - [Graceful Step-Down](#graceful-step-down)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Automatic Claim Renewal](#automatic-claim-renewal)
  - [Validate Election Timing](#validate-election-timing)
- [4. States (CDSL)](#4-states-cdsl)
  - [Leadership State Machine](#leadership-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Leader-Election Backend Trait and Facade](#leader-election-backend-trait-and-facade)
  - [Leadership Watch and Observation](#leadership-watch-and-observation)
  - [Configurable Election Timing](#configurable-election-timing)
  - [Advisory Semantics Documentation](#advisory-semantics-documentation)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Provides named single-leader election with automatic renewal, configurable failover timing, dual observability (event-driven and gate-driven), graceful step-down, and explicitly advisory semantics. It reuses the watch-union event shape established by the cache primitive.

### 1.2 Purpose

Singleton patterns — worker-pool coordination, scheduler election, migration gating — require an at-most-one-leader signal with automatic renewal so consumers don't reimplement heartbeat loops. This feature delivers that signal with tunable timing and a clear advisory boundary.

**Requirements**: `cpt-cf-clst-fr-leader-elect`, `cpt-cf-clst-fr-leader-config`, `cpt-cf-clst-fr-leader-observability`, `cpt-cf-clst-fr-leader-resign`, `cpt-cf-clst-fr-leader-advisory`

**Principles**: `cpt-cf-clst-principle-watch-union-shape`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-event-broker` | Elects a worker-pool leader and reacts to leadership transitions |
| `cpt-cf-clst-actor-platform-gear` | Runs singleton workloads gated on leadership |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.2
- **Design**: [DESIGN.md](../DESIGN.md) §3.3 (leader-election contract, staleness bound, three consumer patterns), §3.1 (entities)
- **ADRs**: [ADR-003](../ADR/003-watch-event-lifecycle-contract.md), [ADR-009](../ADR/009-leader-election-backend-safety.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-sdk-foundation`
  - [x] `p2` - `cpt-cf-clst-feature-cache-primitive`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — backend-determined: failover responsiveness is governed by configurable election timing; round-trip latency depends on the bound backend.
- Reliability — addressed: automatic claim renewal with a missed-renewal budget, transient-error suppression, auto-reenrollment, and explicitly advisory semantics (§2–§3) define behavior under failure.

## 2. Actor Flows (CDSL)

### Participate in Election and Observe Leadership

- [x] `p1` - **ID**: `cpt-cf-clst-flow-leader-election-participate-observe`

**Actor**: `cpt-cf-clst-actor-event-broker`

**Success Scenarios**:
- Exactly one participant observes itself as leader; the leader's claim is renewed automatically.
- A consumer observes transitions event-driven or checks the cached status synchronously.

**Error Scenarios**:
- Leadership is lost transiently — the consumer keeps participating without writing re-enrollment code.

**Steps**:
1. [x] - `p1` - Consumer joins a named election (optionally with custom timing) and receives a leadership watch - `inst-le-join`
2. [x] - `p1` - SDK renews the claim automatically on the derived renewal interval - `inst-le-renew`
3. [x] - `p1` - Consumer awaits the next leadership event, or reads the cached status synchronously inside a loop - `inst-le-observe`
4. [x] - `p1` - **IF** the event reports leadership lost - `inst-le-lost`
   1. [x] - `p1` - Consumer stops leader-only work; the watch auto-reenrolls without consumer code - `inst-le-reenroll`
5. [x] - `p1` - **IF** the next event reports leader or follower - `inst-le-resolved`
   1. [x] - `p1` - Consumer resumes or remains idle accordingly - `inst-le-resume`

### Graceful Step-Down

- [x] `p1` - **ID**: `cpt-cf-clst-flow-leader-election-step-down`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- A planned shutdown releases the claim immediately so a successor is elected within a backend round-trip.

**Steps**:
1. [x] - `p1` - Consumer explicitly resigns from the election - `inst-sd-resign`
2. [x] - `p1` - SDK releases the claim immediately, triggering re-election - `inst-sd-release`
3. [x] - `p1` - **RETURN** acknowledgment that the claim was surrendered - `inst-sd-return`

## 3. Processes / Business Logic (CDSL)

### Automatic Claim Renewal

- [x] `p1` - **ID**: `cpt-cf-clst-algo-leader-election-renewal`

**Input**: An active leadership claim with its timing configuration

**Output**: Sustained leadership, or a leadership-lost transition

**Steps**:
1. [x] - `p1` - Compute the renewal interval as ttl divided by (max-missed-renewals plus one) - `inst-rn-interval`
2. [x] - `p1` - **FOR EACH** renewal tick - `inst-rn-tick`
   1. [x] - `p1` - Attempt to renew the claim - `inst-rn-attempt`
   2. [x] - `p1` - **IF** a transient backend error occurs - `inst-rn-transient`
      1. [x] - `p1` - Retry internally without surfacing a transition - `inst-rn-retry`
   3. [x] - `p1` - **IF** renewals fail past the missed-renewal budget - `inst-rn-exceeded`
      1. [x] - `p1` - Emit a leadership-lost transition - `inst-rn-lost`

### Validate Election Timing

- [x] `p1` - **ID**: `cpt-cf-clst-algo-leader-election-config-validate`

**Input**: A proposed election timing configuration

**Output**: A validated configuration, or an invalid-config error

**Steps**:
1. [x] - `p1` - **IF** ttl is not greater than zero or max-missed-renewals is not greater than zero - `inst-cv-check`
   1. [x] - `p1` - **RETURN** an invalid-config error - `inst-cv-reject`
2. [x] - `p1` - **RETURN** the validated configuration with its derived renewal interval - `inst-cv-ok`

## 4. States (CDSL)

### Leadership State Machine

- [x] `p1` - **ID**: `cpt-cf-clst-state-leader-election-leadership`

**States**: Leader, Follower, Lost

**Initial State**: Follower

**Transitions**:
1. [x] - `p1` - **FROM** Follower **TO** Leader **WHEN** the claim is acquired - `inst-st-acquire`
2. [x] - `p1` - **FROM** Leader **TO** Lost **WHEN** renewals fail past the budget or on graceful shutdown revocation - `inst-st-lose`
3. [x] - `p1` - **FROM** Lost **TO** Follower **WHEN** the watch auto-reenrolls and another participant holds the claim - `inst-st-follower`
4. [x] - `p1` - **FROM** Lost **TO** Leader **WHEN** the watch auto-reenrolls and re-acquires the claim - `inst-st-reacquire`

## 5. Definitions of Done

### Leader-Election Backend Trait and Facade

- [x] `p1` - **ID**: `cpt-cf-clst-dod-leader-election-backend-facade`

The system **MUST** provide the leader-election backend trait and facade with join (with and without custom timing) returning a leadership watch, the **automatic-renewal contract** that backends implement (the SDK defines the renewal cadence and the renewal/transient-retry/missed-budget obligations on the backend trait; the renewal loop itself is a backend responsibility, not SDK code), and a fluent resolver with capability validation. The backend trait **MUST** be dyn-compatible.

**Implements**:
- `cpt-cf-clst-flow-leader-election-participate-observe`
- `cpt-cf-clst-algo-leader-election-renewal`

**Touches**:
- Entities: LeaderElectionBackend, LeaderElectionV1, LeaderElectionCapability, LeaderElectionFeatures

### Leadership Watch and Observation

- [x] `p1` - **ID**: `cpt-cf-clst-dod-leader-election-watch`

The system **MUST** provide the leadership watch with event-driven next-event observation, synchronous cached status and is-leader accessors, explicit resign, and a no-op drop (no I/O in drop). Transient backend errors **MUST NOT** surface as transitions; terminal errors arrive via the closed signal. On graceful shutdown the watch **MUST** deliver leadership-lost then a terminal close.

**Implements**:
- `cpt-cf-clst-flow-leader-election-participate-observe`
- `cpt-cf-clst-flow-leader-election-step-down`
- `cpt-cf-clst-state-leader-election-leadership`

**Touches**:
- Entities: LeaderWatch, LeaderWatchEvent, LeaderStatus

### Configurable Election Timing

- [x] `p1` - **ID**: `cpt-cf-clst-dod-leader-election-config`

The system **MUST** provide an election timing configuration with a reasonable default and rejection of misconfigured values at construction, deriving the renewal interval from ttl and the missed-renewal budget.

**Implements**:
- `cpt-cf-clst-algo-leader-election-config-validate`

**Touches**:
- Entities: ElectionConfig

### Advisory Semantics Documentation

- [x] `p1` - **ID**: `cpt-cf-clst-dod-leader-election-advisory`

The system **MUST** document leader election as advisory coordination (which node should run a workload, not mutual exclusion) and direct consumers needing correctness-critical exclusion to the distributed lock combined with cache compare-and-swap.

**Implements**:
- `cpt-cf-clst-flow-leader-election-participate-observe`

**Touches**:
- Entities: LeaderElectionV1 (documentation)

## 6. Acceptance Criteria

- [x] At most one participant observes itself as leader at any time under a linearizable backend; claims renew automatically.
- [x] Consumers can observe transitions event-driven and check leadership synchronously inside loops.
- [x] Leadership-loss is transient: the watch auto-reenrolls and resolves to leader or follower without re-enrollment code.
- [x] Explicit resign releases the claim immediately so a successor is elected within a backend round-trip.
- [x] Misconfigured timing is rejected at construction; transient backend errors do not surface as transitions.
- [x] Advisory semantics are documented, directing correctness-critical exclusion to lock plus compare-and-swap.
