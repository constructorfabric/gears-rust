# Feature: Watch Auto-Restart Combinator

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-watch-auto-restart-implemented`

- [x] `p2` - `cpt-cf-clst-feature-watch-auto-restart`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Wrap a Watch for Transparent Reconnection](#wrap-a-watch-for-transparent-reconnection)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Classify Terminal Close Retryability](#classify-terminal-close-retryability)
  - [Backoff and Resubscribe](#backoff-and-resubscribe)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Retry Policy](#retry-policy)
  - [Restarting Watch Combinator](#restarting-watch-combinator)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Ships one canonical, opt-in watch-restart combinator for all three watch types. It turns retryable terminal closes into transparent reconnection with backoff, synthesizes a reset on each successful resubscribe so the consumer re-reads state, and propagates non-retryable closes unchanged. Consumers wanting a custom loop can still consume the raw watch stream.

### 1.2 Purpose

Without an SDK-shipped combinator, every consumer reinvents the same restart loop with inconsistent backoff and retryability classification, producing thundering-herd reconnect storms against a recovering backend. A single combinator parameterized by one policy type eliminates this class of regression.

**Requirements**: `cpt-cf-clst-fr-watch-auto-restart`

**Principles**: `cpt-cf-clst-principle-watch-union-shape`

The related uniform watch-lifecycle-signals requirement (`cpt-cf-clst-fr-watch-lifecycle-signals`) is covered by the cache primitive feature, which defines the union event shape this combinator consumes.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-platform-gear` | Wraps any watch to get transparent reconnection |
| `cpt-cf-clst-actor-event-broker` | Keeps shard/topology watches alive across transient outages |
| `cpt-cf-clst-actor-oagw` | Maintains cache watches with consistent backoff |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.7
- **Design**: [DESIGN.md](../DESIGN.md) §3.1 (RetryPolicy, RestartingWatch), §3.9 (auto-restart combinator, retryability table), §3.3 (auto-restart entry point)
- **ADRs**: [ADR-003](../ADR/003-watch-event-lifecycle-contract.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-cache-primitive`
  - [x] `p2` - `cpt-cf-clst-feature-leader-election`
  - [x] `p2` - `cpt-cf-clst-feature-service-discovery`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — addressed: jittered exponential backoff prevents thundering-herd reconnect storms against a recovering backend (§3).
- Reliability — addressed: terminal-close retryability classification and transparent reconnection with a synthesized reset (§2–§3) are the core behavior of this feature.

## 2. Actor Flows (CDSL)

### Wrap a Watch for Transparent Reconnection

- [x] `p1` - **ID**: `cpt-cf-clst-flow-watch-auto-restart-wrap`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- A retryable terminal close is recovered by reconnecting with backoff and emitting a reset; the consumer re-reads state and continues.

**Error Scenarios**:
- A non-retryable close (auth failure, other, shutdown, capability-not-met) propagates unchanged and the consumer stops.
- The retry cap is exhausted — the most recent close propagates unchanged.

**Steps**:
1. [x] - `p1` - Consumer wraps any of the three watches with the auto-restart combinator and a retry policy - `inst-ar-wrap`
2. [x] - `p1` - Consumer awaits events as if from the raw watch - `inst-ar-next`
3. [x] - `p1` - **IF** a terminal close is retryable - `inst-ar-retryable`
   1. [x] - `p1` - Reconnect after backoff and emit a reset on successful resubscribe - `inst-ar-reset`
4. [x] - `p1` - **IF** a terminal close is non-retryable - `inst-ar-nonretryable`
   1. [x] - `p1` - **RETURN** the close to the consumer unchanged - `inst-ar-propagate`
5. [x] - `p1` - **IF** the retry cap is exhausted - `inst-ar-cap`
   1. [x] - `p1` - **RETURN** the most recent close unchanged - `inst-ar-cap-propagate`

## 3. Processes / Business Logic (CDSL)

### Classify Terminal Close Retryability

- [x] `p1` - **ID**: `cpt-cf-clst-algo-watch-auto-restart-classify`

**Input**: A terminal close payload

**Output**: Retryable or non-retryable classification

**Steps**:
1. [x] - `p1` - **IF** the close is a provider connection-lost, timeout, or resource-exhausted error - `inst-cl-retry`
   1. [x] - `p1` - Classify as retryable - `inst-cl-retry-set`
2. [x] - `p1` - **IF** the close is a provider auth-failure or other error - `inst-cl-prov-no`
   1. [x] - `p1` - Classify as non-retryable - `inst-cl-prov-no-set`
3. [x] - `p1` - **IF** the close is shutdown, capability-not-met, or a lock/leader terminal signal - `inst-cl-term`
   1. [x] - `p1` - Classify as non-retryable - `inst-cl-term-set`

### Backoff and Resubscribe

- [x] `p1` - **ID**: `cpt-cf-clst-algo-watch-auto-restart-backoff`

**Input**: A retry policy (initial backoff, max backoff, jitter, optional retry cap)

**Output**: A resubscribe attempt schedule honoring the policy

**Steps**:
1. [x] - `p1` - Start from the initial backoff and grow toward the maximum backoff - `inst-bo-grow`
2. [x] - `p1` - Apply jitter to each delay to avoid thundering-herd reconnects - `inst-bo-jitter`
3. [x] - `p1` - **IF** a retry cap is set and reached - `inst-bo-cap`
   1. [x] - `p1` - Stop retrying and propagate the most recent close - `inst-bo-stop`
4. [x] - `p1` - **RETURN** a reset to the consumer on each successful resubscribe - `inst-bo-reset`

## 4. States (CDSL)

Not applicable — the combinator is a stateless wrapper over a base watch driven by the retry policy; it introduces no entity lifecycle.

## 5. Definitions of Done

### Retry Policy

- [x] `p1` - **ID**: `cpt-cf-clst-dod-watch-auto-restart-policy`

The system **MUST** provide a retry policy with initial backoff, maximum backoff, jitter factor, and an optional retry cap, whose default is exponential backoff from one second to thirty seconds with full jitter and no retry cap.

**Implements**:
- `cpt-cf-clst-algo-watch-auto-restart-backoff`

**Touches**:
- Entities: RetryPolicy

### Restarting Watch Combinator

- [x] `p1` - **ID**: `cpt-cf-clst-dod-watch-auto-restart-combinator`

The system **MUST** provide a restarting-watch combinator available for all three watch types via a single uniform policy type, reading retryability from the provider error classification, emitting a reset on each successful resubscribe, and propagating non-retryable and shutdown closes unchanged. The raw watch stream **MUST** remain consumable without the combinator.

**Implements**:
- `cpt-cf-clst-flow-watch-auto-restart-wrap`
- `cpt-cf-clst-algo-watch-auto-restart-classify`

**Touches**:
- Entities: RestartingWatch

## 6. Acceptance Criteria

- [x] The combinator is available for cache, leader-election, and service-discovery watches via one uniform policy type.
- [x] Retryable terminal closes (connection-lost, timeout, resource-exhausted) trigger reconnect with backoff and a synthesized reset.
- [x] Non-retryable closes (auth-failure, other, shutdown, capability-not-met, lock/leader terminal) propagate unchanged.
- [x] The default policy is one second to thirty seconds exponential backoff with full jitter and no cap; an exhausted cap propagates the most recent close.
- [x] Consumers can still consume the raw watch stream without the combinator.
