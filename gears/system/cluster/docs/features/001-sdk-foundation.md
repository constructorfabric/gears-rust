# Feature: SDK Foundation & Shared Contract

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-sdk-foundation-implemented`

- [x] `p2` - `cpt-cf-clst-feature-sdk-foundation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Declare and Resolve a Cluster Profile](#declare-and-resolve-a-cluster-profile)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Profile-Scope Mapping](#profile-scope-mapping)
  - [Provider Error Retryability Classification](#provider-error-retryability-classification)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Crate Scaffold and Workspace Wiring](#crate-scaffold-and-workspace-wiring)
  - [Unified Error Model and Retryability Classification](#unified-error-model-and-retryability-classification)
  - [Typed Profile Marker and Scope Helpers](#typed-profile-marker-and-scope-helpers)
  - [Dyn-Compatibility Assertion Harness](#dyn-compatibility-assertion-harness)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Establishes the `cf-gears-cluster-sdk` crate (lib `cluster_sdk`) and the shared contract foundation every cluster primitive builds on: the unified error model with programmatic retryability classification, the typed profile marker, the profile-scope and name-validation helpers, and the dyn-compatibility assertion harness.

### 1.2 Purpose

The four coordination primitives need a single, stable, serde-free, dyn-safe foundation so the public contract can evolve independently of any backend. This feature provides the cross-cutting types and helpers that the cache, leader-election, lock, and service-discovery features all depend on, and it removes magic-string profile names by construction.

**Requirements**: `cpt-cf-clst-fr-validation-typed-profile`, `cpt-cf-clst-nfr-error-retryability`, `cpt-cf-clst-nfr-plugin-stability`

This feature realizes the in-scope design component `component-sdk` (foundation slice) and applies the facade-plus-backend-trait pattern established in DESIGN §3.6; per the kit reference rules those design-component and sequence links are tracked in `DECOMPOSITION.md`, not duplicated here.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-platform-gear` | Declares a typed profile once and resolves primitives against it |
| `cpt-cf-clst-actor-plugin-author` | Relies on the stable, serde-free, dyn-compatible contract surface |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) §3.1 (domain model), §3.6 (resolution pattern)
- **ADRs**: [ADR-005](../ADR/005-facade-plus-backend-trait-pattern.md), [ADR-007](../ADR/007-capability-typing-and-profile-resolution.md)
- **Dependencies**: None (foundation feature)

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — not applicable: the foundation defines cross-cutting types and helpers with no I/O or throughput dimension.
- Reliability — addressed: the provider-error retryability classification (§3) gives consumers a programmatic basis for retry and recovery decisions; per-backend reliability is out of scope here.

## 2. Actor Flows (CDSL)

### Declare and Resolve a Cluster Profile

- [x] `p1` - **ID**: `cpt-cf-clst-flow-sdk-foundation-declare-profile`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- The profile string exists in exactly one place in the consumer crate (the marker), never re-typed at call sites.
- Resolution maps the profile to a stable lookup scope shared by all primitives.

**Error Scenarios**:
- A profile name that violates the cluster name rule is rejected before any lookup.

**Steps**:
1. [x] - `p1` - Consumer defines a zero-sized marker and implements the profile marker trait with its profile `NAME` once - `inst-define-marker`
2. [x] - `p1` - Consumer passes the profile marker (a type, not a string) at a resolver call site - `inst-pass-marker`
3. [x] - `p1` - SDK reads the marker's `NAME` and maps it to a lookup scope via the profile-scope helper - `inst-map-scope`
4. [x] - `p1` - **IF** the profile name violates the cluster name rule - `inst-validate`
   1. [x] - `p1` - **RETURN** an invalid-name error naming the offending value - `inst-invalid`
5. [x] - `p1` - **RETURN** the resolved scope used for per-primitive backend lookup - `inst-return-scope`

## 3. Processes / Business Logic (CDSL)

### Profile-Scope Mapping

- [x] `p1` - **ID**: `cpt-cf-clst-algo-sdk-foundation-profile-scope`

**Input**: A profile name string from a profile marker

**Output**: A stable lookup scope, or an invalid-name error

**Steps**:
1. [x] - `p1` - Validate the profile name against the cluster name rule - `inst-ps-validate`
2. [x] - `p1` - **IF** invalid - `inst-ps-if-invalid`
   1. [x] - `p1` - **RETURN** an invalid-name error - `inst-ps-return-invalid`
3. [x] - `p1` - Compose the canonical scope form - `inst-ps-compose`
4. [x] - `p1` - **RETURN** the composed scope - `inst-ps-return`

### Provider Error Retryability Classification

- [x] `p1` - **ID**: `cpt-cf-clst-algo-sdk-foundation-error-classify`

**Input**: A backend/provider error surfaced through the contract

**Output**: A structured retryability classification consumers can branch on without parsing strings

**Steps**:
1. [x] - `p1` - **IF** the error is a lost/dropped connection - `inst-ec-conn`
   1. [x] - `p1` - Classify as connection-lost (retryable after reconnect) - `inst-ec-conn-set`
2. [x] - `p1` - **IF** the error is a timeout - `inst-ec-timeout`
   1. [x] - `p1` - Classify as timeout (retryable) - `inst-ec-timeout-set`
3. [x] - `p1` - **IF** the error is an authentication failure - `inst-ec-auth`
   1. [x] - `p1` - Classify as auth-failure (not retryable) - `inst-ec-auth-set`
4. [x] - `p1` - **IF** the error is resource exhaustion - `inst-ec-exhaust`
   1. [x] - `p1` - Classify as resource-exhausted (retryable with backoff) - `inst-ec-exhaust-set`
5. [x] - `p1` - **ELSE** - `inst-ec-else`
   1. [x] - `p1` - Classify as other (not retryable) - `inst-ec-other-set`
6. [x] - `p1` - **RETURN** the classification - `inst-ec-return`

## 4. States (CDSL)

Not applicable — the foundation defines cross-cutting types and helpers with no entity lifecycle. Entity state machines belong to the leader-election and service-discovery features.

## 5. Definitions of Done

### Crate Scaffold and Workspace Wiring

- [x] `p1` - **ID**: `cpt-cf-clst-dod-sdk-foundation-crate-scaffold`

The system **MUST** create the `cf-gears-cluster-sdk` crate (lib `cluster_sdk`) at `gears/system/cluster/cluster-sdk/`, registered as a workspace member, depending only on `tokio`, `tokio_util`, `async-trait`, and platform crates — with no dependency on serde.

**Constraints**: `cpt-cf-clst-constraint-no-serde`

**Touches**:
- Entities: cluster-sdk crate

### Unified Error Model and Retryability Classification

- [x] `p1` - **ID**: `cpt-cf-clst-dod-sdk-foundation-error-model`

The system **MUST** provide a unified error type covering the full contract variant set (including capability-not-met, profile-not-bound, profile-not-specified, shutdown, and a structured provider variant) with **no** not-started variant, plus a provider-error-kind classification enabling programmatic retryability decisions.

**Implements**:
- `cpt-cf-clst-algo-sdk-foundation-error-classify`

**Touches**:
- Entities: ClusterError, ProviderErrorKind

### Typed Profile Marker and Scope Helpers

- [x] `p1` - **ID**: `cpt-cf-clst-dod-sdk-foundation-profile`

The system **MUST** provide the profile marker trait, the profile-scope helper, and name validation so consumers reference profiles by a typed identifier defined once and the profile string never appears a third time.

**Implements**:
- `cpt-cf-clst-flow-sdk-foundation-declare-profile`
- `cpt-cf-clst-algo-sdk-foundation-profile-scope`

**Touches**:
- Entities: ClusterProfile, profile-scope helper

### Dyn-Compatibility Assertion Harness

- [x] `p1` - **ID**: `cpt-cf-clst-dod-sdk-foundation-dyn-compat`

The system **MUST** provide a compile-time dyn-compatibility assertion harness applied per backend trait, so any future change that breaks dyn-compatibility fails the build, supporting a stable plugin contract across versions.

**Constraints**: `cpt-cf-clst-constraint-dyn-compat`

**Touches**:
- Entities: dyn-compat assertion harness

## 6. Acceptance Criteria

- [x] The `cf-gears-cluster-sdk` crate builds as a workspace member with no serde dependency.
- [x] A consumer can declare a profile once via the marker trait and resolve against it without re-typing the profile string.
- [x] An invalid profile name is rejected with a specific invalid-name error before any backend lookup.
- [x] Backend errors are classified into the structured retryability kinds (connection-lost, timeout, auth-failure, resource-exhausted, other) without string parsing.
- [x] The dyn-compatibility assertion harness fails the build if any backend trait becomes dyn-incompatible.
