# Feature: Registration Helpers, GTS Spec & Observability Contract

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-registration-observability-implemented`

- [x] `p2` - `cpt-cf-clst-feature-registration-observability`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Register and Deregister a Backend per Profile per Primitive](#register-and-deregister-a-backend-per-profile-per-primitive)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Scope-Keyed Registration](#scope-keyed-registration)
  - [Observability Name Emission](#observability-name-emission)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Registration and Deregistration Helpers](#registration-and-deregistration-helpers)
  - [GTS Plugin Specification](#gts-plugin-specification)
  - [Observability Naming Contract](#observability-naming-contract)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Provides the ClientHub registration and deregistration helpers (per profile, per primitive) that the follow-up wiring crate composes, the GTS plugin-spec scaffolding that lets follow-up plugins register and be discovered, and the versioned observability naming contract (stable span/metric/log names plus the cardinality rule) that every follow-up plugin emits against.

### 1.2 Purpose

Registration must be keyed consistently per profile per primitive so consumers resolve the right backend, plugins need a discovery contract, and cluster — as foundational infrastructure every module depends on — needs a stable observability contract so consumer dashboards survive plugin minor versions.

**Requirements**: `cpt-cf-clst-nfr-observability`

**Principles**: `cpt-cf-clst-principle-per-primitive-routing`

This feature contributes to the in-scope `component-sdk`; that component link is tracked in `DECOMPOSITION.md` per the kit reference rules. The wiring orchestration that calls these helpers is a follow-up change and out of scope here.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-host` | Owns the lifecycle that registers and deregisters backends via these helpers |
| `cpt-cf-clst-actor-plugin-author` | Declares a plugin via the GTS spec and emits the contracted observability signals |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.1 (observability NFR)
- **Design**: [DESIGN.md](../DESIGN.md) §3.6 (registration), §3.2 (component model, helpers), §3.4 (GTS dependencies)
- **ADRs**: [ADR-004](../ADR/004-observability-contract.md), [ADR-006](../ADR/006-builder-handle-lifecycle.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-cache-primitive`
  - [x] `p2` - `cpt-cf-clst-feature-leader-election`
  - [x] `p2` - `cpt-cf-clst-feature-distributed-lock`
  - [x] `p2` - `cpt-cf-clst-feature-service-discovery`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — addressed: the cardinality rule keeps operation keys, lock names, and election names out of metric labels (§3), bounding observability cost.
- Reliability — addressed: scope-keyed registration and deregistration make resolution deterministic, reporting profile-not-bound after teardown (§2–§3).

## 2. Actor Flows (CDSL)

### Register and Deregister a Backend per Profile per Primitive

- [x] `p1` - **ID**: `cpt-cf-clst-flow-registration-observability-register`

**Actor**: `cpt-cf-clst-actor-host`

**Success Scenarios**:
- A backend is registered under a profile scope for one primitive and becomes resolvable; deregistration removes it.

**Error Scenarios**:
- After deregistration, a resolve attempt for that profile returns profile-not-bound.

**Steps**:
1. [x] - `p1` - Caller registers a backend for a primitive under the profile scope - `inst-rb-register`
2. [x] - `p1` - Consumers resolving that primitive for the profile receive the registered backend - `inst-rb-resolve`
3. [x] - `p1` - Caller deregisters the backend during shutdown - `inst-rb-deregister`
4. [x] - `p1` - **IF** a consumer resolves after deregistration - `inst-rb-after`
   1. [x] - `p1` - **RETURN** a profile-not-bound error - `inst-rb-unbound`

## 3. Processes / Business Logic (CDSL)

### Scope-Keyed Registration

- [x] `p1` - **ID**: `cpt-cf-clst-algo-registration-observability-scope-register`

**Input**: A primitive backend, a profile, and the primitive kind

**Output**: A scoped registration entry resolvable per profile per primitive

**Steps**:
1. [x] - `p1` - Map the profile to its lookup scope via the profile-scope helper - `inst-sr-scope`
2. [x] - `p1` - Register the backend for the primitive under that scope - `inst-sr-register`
3. [x] - `p1` - On deregister, remove the entry so later resolves report profile-not-bound - `inst-sr-deregister`

### Observability Name Emission

- [x] `p1` - **ID**: `cpt-cf-clst-algo-registration-observability-emit-names`

**Input**: A cluster operation and its outcome

**Output**: Signals using stable, low-cardinality names per the observability contract

**Steps**:
1. [x] - `p1` - Emit spans, metrics, and log events using the stable contracted names - `inst-en-emit`
2. [x] - `p1` - **IF** a field is an operation key, lock name, or election name - `inst-en-highcard`
   1. [x] - `p1` - Keep it out of metric labels; allow it only in trace attributes and log fields - `inst-en-attr`
3. [x] - `p1` - Restrict metric labels to the bounded-cardinality set - `inst-en-labels`

## 4. States (CDSL)

Not applicable — registration helpers, the GTS spec, and the observability naming module introduce no entity lifecycle.

## 5. Definitions of Done

### Registration and Deregistration Helpers

- [x] `p1` - **ID**: `cpt-cf-clst-dod-registration-observability-helpers`

The system **MUST** provide register and deregister helpers for all four primitives that key registration per profile per primitive via the profile scope, such that deregistration causes later resolves to report profile-not-bound.

**Implements**:
- `cpt-cf-clst-flow-registration-observability-register`
- `cpt-cf-clst-algo-registration-observability-scope-register`

**Touches**:
- Entities: register/deregister backend helpers (per primitive)

### GTS Plugin Specification

- [x] `p1` - **ID**: `cpt-cf-clst-dod-registration-observability-gts`

The system **MUST** provide GTS plugin-spec scaffolding for the cluster plugin contract so follow-up plugins can register instances and be discovered.

**Touches**:
- Entities: cluster plugin GTS spec

### Observability Naming Contract

- [x] `p1` - **ID**: `cpt-cf-clst-dod-registration-observability-obs`

The system **MUST** provide a stable observability naming module (span, metric, and log-event name constants) and a versioned observability reference document, including the cardinality rule that operation keys, lock names, and election names never appear as metric labels (only as trace attributes or log fields). Renames are breaking changes.

**Implements**:
- `cpt-cf-clst-algo-registration-observability-emit-names`

**Touches**:
- Entities: observability naming constants, observability reference document

## 6. Acceptance Criteria

- [x] Backends register per profile per primitive and become resolvable; deregistration causes later resolves to report profile-not-bound.
- [x] The GTS plugin spec lets a follow-up plugin register an instance and be discovered.
- [x] The observability naming module defines stable span/metric/log names, and a reference document captures the contract.
- [x] No high-cardinality field (operation key, lock name, election name) appears as a metric label.
