# Feature: Smoke Tests (in-process stub backends)

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-smoke-tests-implemented`

- [x] `p2` - `cpt-cf-clst-feature-smoke-tests`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [In-Process Stub Model](#in-process-stub-model)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [In-Process Stub Backends](#in-process-stub-backends)
  - [Resolution and Capability-Mismatch Coverage](#resolution-and-capability-mismatch-coverage)
  - [Watch Lifecycle Coverage](#watch-lifecycle-coverage)
  - [Coordination Behavior Coverage](#coordination-behavior-coverage)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Verifies the SDK contract end-to-end against minimal in-process stub backends with no external infrastructure — exercising resolution, capability-mismatch failure, every watch lifecycle variant, CAS conflict, single-leader under contention, lock release-on-timeout, scoping, and the prefix-watch polyfill. It establishes the cross-backend behavioral baseline that any conforming backend must reproduce.

### 1.2 Purpose

The contract must be provable without spinning up Postgres, Redis, or K8s, so every cluster-aware behavior is exercisable on a developer laptop. These smoke tests verify API shape and the happy-and-error paths the stubs can emit; distributed-correctness under partition or clock skew is verified per-plugin in follow-up changes.

**Requirements**: `cpt-cf-clst-nfr-cross-backend-stability`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-platform-gear` | Represents the consumer code exercised by the smoke tests |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.1 (cross-backend stability NFR), §9 (acceptance criteria)
- **Design**: [DESIGN.md](../DESIGN.md) §1.2 (verification approach), §6 (smoke tests verify API shape, not distributed correctness)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-cache-primitive`
  - [x] `p2` - `cpt-cf-clst-feature-leader-election`
  - [x] `p2` - `cpt-cf-clst-feature-distributed-lock`
  - [x] `p2` - `cpt-cf-clst-feature-service-discovery`
  - [x] `p2` - `cpt-cf-clst-feature-sdk-default-backends`
  - [x] `p2` - `cpt-cf-clst-feature-scoping-polyfill`
  - [x] `p2` - `cpt-cf-clst-feature-watch-auto-restart`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — not applicable: these are contract smoke tests, not performance benchmarks.
- Reliability — addressed at the contract level: watch-lifecycle signals (lagged/reset/closed) and coordination recovery are exercised; distributed-correctness under partition or clock skew is verified per-plugin in follow-up changes.

## 2. Actor Flows (CDSL)

Not applicable — this is a test-only feature with no actor-facing interaction. The behaviors under test are described as definitions of done and acceptance criteria, and the stub model is described as a process below.

## 3. Processes / Business Logic (CDSL)

### In-Process Stub Model

- [x] `p1` - **ID**: `cpt-cf-clst-algo-smoke-tests-stub-model`

**Input**: A test scenario driving the SDK contract

**Output**: Deterministic contract behavior without external infrastructure

**Steps**:
1. [x] - `p1` - Back the cache with a single in-memory state map and a single monotonic version source - `inst-sm-state`
2. [x] - `p1` - Drive watch events through a single ordered channel so per-key ordering is observable - `inst-sm-channel`
3. [x] - `p1` - Induce lifecycle signals (lagged, reset, closed) deterministically to exercise recovery - `inst-sm-signals`
4. [x] - `p1` - Treat the stub as a contract fixture, not a production backend (one clock, one state map) - `inst-sm-fixture`

## 4. States (CDSL)

Not applicable — the smoke tests exercise the primitive features' state machines; they define no new entity lifecycle.

## 5. Definitions of Done

### In-Process Stub Backends

- [x] `p1` - **ID**: `cpt-cf-clst-dod-smoke-tests-stubs`

The system **MUST** provide minimal in-process stub backends (a memory cache backend and siblings) used solely for contract smoke tests, explicitly documented as not constituting a production backend.

**Implements**:
- `cpt-cf-clst-algo-smoke-tests-stub-model`

**Touches**:
- Entities: in-process stub backends (test-only)

### Resolution and Capability-Mismatch Coverage

- [x] `p1` - **ID**: `cpt-cf-clst-dod-smoke-tests-resolution`

The smoke tests **MUST** verify per-primitive resolution succeeds against a bound backend and that a declared capability unmet by the backend fails startup with a capability-not-met error naming the primitive, requirement, and provider.

**Touches**:
- Entities: smoke test suite

### Watch Lifecycle Coverage

- [x] `p1` - **ID**: `cpt-cf-clst-dod-smoke-tests-watch`

The smoke tests **MUST** verify that all three watches surface event, lagged, reset, and closed signals, that per-key ordering is preserved, and that delivery is at most once.

**Touches**:
- Entities: smoke test suite

### Coordination Behavior Coverage

- [x] `p1` - **ID**: `cpt-cf-clst-dod-smoke-tests-coordination`

The smoke tests **MUST** verify CAS conflict surfacing, single-leader under multi-task contention, lock release on timeout and explicit release, composable scoping prefix translation, and the prefix-watch polyfill diff.

**Touches**:
- Entities: smoke test suite

## 6. Acceptance Criteria

- [x] The full smoke-test suite passes against the in-process stub backends with no external dependencies.
- [x] Resolution succeeds for bound backends; capability mismatch fails startup with a specific error.
- [x] All three watches surface event/lagged/reset/closed with preserved per-key ordering and at-most-once delivery.
- [x] CAS conflict, single-leader-under-contention, lock release-on-timeout, scoping, and the polyfill are each exercised.
- [x] The stub backends are documented as contract fixtures, not production backends.
