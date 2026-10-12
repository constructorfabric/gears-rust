---
description: "Product requirements for Admission Control: the single admit/refuse gate over one optional engine plugin."
---

<!-- cpt:
version: 1.0.0
status: draft
module: admission-control
system: cf
-->

# PRD — Admission Control

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
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Admission Decision](#51-admission-decision)
  - [5.2 Engine](#52-engine)
  - [5.3 Failure, Events and Configuration](#53-failure-events-and-configuration)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Module-Specific NFRs](#61-module-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Gate a Resource Operation](#gate-a-resource-operation)
  - [Substitute the Admission Engine](#substitute-the-admission-engine)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Admission Control is the single admission gate of the platform. An enforcing gear describes one intended operation
(who acts is taken from its `SecurityContext`) and receives one answer: admitted or refused. The gate validates the
request, then consults at most one pluggable admission engine, which makes every policy decision. The Infrastructure
Resource Manager (IRM) is the first enforcing gear and reaches policy only through this gate; the `policy-engine` gear is the default engine
plugin.

### 1.2 Background / Problem Statement

Without a shared gate each enforcing gear would embed its own policy checks and failure handling, and tenant policy
could not be applied uniformly. One gate gives one contract, one fail-closed rule and one refusal trail, while the
policy store and evaluation stay behind a replaceable engine plugin.

### 1.3 Goals (Business Outcomes)

- One admission contract for every enforcing gear, with identity that cannot be spoofed by the caller.
- Fail closed: a gate that cannot decide refuses, in 100% of engine-absent, engine-failure and timeout cases.
- Every refusal and every shadow finding is published as an event, best effort: publication never delays or fails an
  admission, and an event is dropped (and counted) only when the queue is full or the broker is absent or failing.

### 1.4 Glossary

| Term             | Definition                                                                                                      |
|------------------|-----------------------------------------------------------------------------------------------------------------|
| Admission        | The gate's decision on one intended operation; the verdict is admitted or refused.                              |
| Engine           | The optional plugin that evaluates tenant policy; selected by vendor and optionally a pinned instance.          |
| Refusal cause    | Why an operation was refused: engine policy, request too large, invalid request, or could not run.              |
| Shadow denial    | A denial from a non-enforcing policy assignment; reported in events but never refuses the operation.            |
| Correlation id   | Identifier the gate mints per operation; ties the verdict, engine call and events together.                     |

## 2. Actors

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-admission-control-actor-platform-operator`

- **Role**: Configures engine selection, timeouts and size bounds; a bad configuration fails startup.

#### Security Auditor

**ID**: `cpt-cf-admission-control-actor-security-auditor`

- **Role**: Reviews the published refusal and shadow events to see what was refused and why.

### 2.2 System Actors

#### Enforcing Gear

**ID**: `cpt-cf-admission-control-actor-enforcing-gear`

- **Role**: A gear that gates its operations, IRM first. Calls `AdmissionClientV1.admit` with its `SecurityContext` and
  honours the verdict.

#### Admission Engine

**ID**: `cpt-cf-admission-control-actor-admission-engine`

- **Role**: The engine plugin implementing `AdmissionEnginePluginClientV1`. "Admission engine" (or "engine") names
  this role throughout; `policy-engine` names one gear that fills it, the default.

#### Types Registry

**ID**: `cpt-cf-admission-control-actor-types-registry`

- **Role**: Lists engine plugin instances and registers the refusal event type.

#### Event Broker

**ID**: `cpt-cf-admission-control-actor-audit-sink`

- **Role**: Receives refusal events on the audit topic. Optional: when absent, events are dropped.

## 3. Operational Concept & Environment

The gate runs in-process, holds no persistent state and has no REST surface. Runtime, lifecycle and integration patterns
follow [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and
[guidelines/](../../../../guidelines/).

## 4. Scope

### 4.1 In Scope

- One admission operation returning admitted or refused, with a refusal cause.
- Selection and invocation of exactly one optional engine plugin under a timeout.
- Best-effort publication of refusal and shadow events.
- Startup validation of the configuration.

### 4.2 Out of Scope

- Policy storage, authoring and evaluation of tenant policy (owned by the engine, e.g. `policy-engine`).
- Batch admission, deferral, obligations and modification of the caller's request.
- Policies evaluated by the gate itself: every policy decision, platform rules included, is the engine's.
- Operational REST routes and refusal counters.
- Authentication and tenant hierarchy resolution (the engine resolves tenancy).

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration) targeting 90%+ code
> coverage unless otherwise specified.

### 5.1 Admission Decision

#### Single Admission Interface

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-admission-interface`

The system **MUST** offer one operation, `admit`, that takes an admission request and returns admitted or refused; it
**MUST NOT** modify the request or return anything beyond the verdict. The only calls that get an error
(`AdmissionError`) instead of a verdict are those that fail authentication or argument validation (an anonymous context
or an invalid identifier, below).

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Identity From SecurityContext

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-request-authenticity`

The subject and its tenant **MUST** come only from the caller's `SecurityContext`; the request has no subject fields. An
anonymous context (nil subject or subject tenant) **MUST** be rejected as unauthenticated, not refused.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Identifier Validation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-identifier-validation`

`enforcing_gear` and `action` **MUST** match `^[a-z0-9][a-z0-9._:-]{0,127}$` and `resource_type` **MUST** be a GTS type
id of at most 256 bytes; otherwise the call is rejected as an invalid argument that never echoes the value.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Request Size Bounds

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-request-bounds`

A request whose property count or serialized property size exceeds the configured bound, or whose properties nest
deeper than the fixed depth bound, **MUST** be refused as too-large before any policy runs.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-platform-operator`

#### Decision Order

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-decision-order`

The gate **MUST** authenticate the call, validate its identifiers and check its size bounds before calling the engine,
and **MUST** call the engine at most once per admission; only a call that passes every check reaches the engine.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-admission-engine`

#### Refusal Cause

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-refusal-cause`

Every refusal **MUST** carry the correlation id and exactly one cause: engine policy (reason code and policy references), request too large (which bound), invalid request (the engine judged the
properties invalid) or could not run (which condition). A denied operation and a failed check **MUST** stay
distinguishable, and only a failure that may pass on retry **MUST** be reported as retryable: a defect in the gate or in
the gate–engine contract is could-not-run `internal` and is not retryable.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

### 5.2 Engine

#### Engine Selection

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-engine-selection`

At most one engine **MUST** be in use, selected by `vendor` and optionally a pinned `instance_id`; the engine is
optional. A configured engine that cannot be resolved **MUST** fail startup.

- **Actors**: `cpt-cf-admission-control-actor-admission-engine`, `cpt-cf-admission-control-actor-types-registry`

### 5.3 Failure, Events and Configuration

#### Fail Closed

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-fail-closed`

When a valid call within bounds finds no engine, or the engine cannot be resolved, is unavailable, times out or fails, the
operation **MUST** be refused as could-not-run with the matching condition. The one exception is an engine that judges
the caller's properties invalid: the operation **MUST** then be refused as invalid request, which is not retryable.
Resolving and calling the engine **MUST** run under the configured timeout.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-admission-engine`

#### Refusal Events

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-refusal-events`

The gate **MUST** publish one refusal event per (operation, policy) pair for every refusal and one per shadow denial
(marked not enforced), never for admissions. Events carry the correlation id, subjects, cause and property names
(never values). Publication **MUST** be best-effort: a full queue or an absent or failing broker drops the event and
never delays or alters a verdict.

- **Actors**: `cpt-cf-admission-control-actor-security-auditor`, `cpt-cf-admission-control-actor-audit-sink`

#### Startup Configuration Validation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-configuration-validation`

Startup **MUST** fail on unknown configuration keys or a numeric setting of zero (a zero timeout or bound would refuse
every request while startup succeeded).

- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide NFRs are defined in
> [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and
> [guidelines/](../../../../guidelines/). Only module-specific NFRs are listed here.

### 6.1 Module-Specific NFRs

#### Fail-Closed Determinism

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-fail-closed`

No failure of the engine, the event path or the broker **MUST** ever produce an admission that the engine did not grant.

- **Threshold**: 0 admissions on engine-absent, engine-failure and timeout paths.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

#### Gate Overhead

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-overhead`

Validation, size check and verdict construction **MUST** add at most 5 ms p95 (10 ms p99) to an admission,
excluding the engine call, which is bounded by `engine_timeout_ms`.

- **Threshold**: 5 ms p95 / 10 ms p99 gate overhead; engine bound 100 ms by default.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

### 6.2 NFR Exclusions

- Availability SLOs: inherited from the platform; the gate is stateless and in-process.
- Event delivery guarantees: events are best-effort by design.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Admission Client

- [ ] `p1` - **ID**: `cpt-cf-admission-control-interface-admission-client`

`AdmissionClientV1::admit(ctx, &AdmissionRequest) -> Result<Verdict, AdmissionError>`, registered in ClientHub. The
request is `enforcing_gear`, `action`, `resource_type`, optional `resource_id`, `resource_tenant_id` and `properties`.

#### Admission Engine Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-admission-control-interface-engine-plugin`

`AdmissionEnginePluginClientV1::evaluate(ctx, &EngineRequest) -> Result<EngineResult, EngineFailure>`, registered in
ClientHub scoped by the engine's GTS instance. The result is permit or deny, each with shadow denials; a failure is
unavailable, timeout, internal, invalid-request (the caller's properties) or contract-violation (what the gate sent). The contract is stable: a breaking change to it is a major version of
the SDK.

### 7.2 External Integration Contracts

#### GTS Registration

- [ ] `p1` - **ID**: `cpt-cf-admission-control-contract-gts`

The gate owns the engine plugin spec `gts.cf.toolkit.plugins.plugin.v1~cf.core.admission_control.engine.v1~`, the error
resource type `gts.cf.core.admission_control.admission.v1~` and the refusal event type.

#### Refusal Event Contract

- [ ] `p1` - **ID**: `cpt-cf-admission-control-contract-refusal-event`

One event type on the audit topic `gts.cf.core.events.topic.v1~cf.core.admission_control.audit.v1`, published under
the gate's own identity; the payload is listed in DESIGN.md.

## 8. Use Cases

### Gate a Resource Operation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-usecase-gate-operation`

1. IRM builds an `AdmissionRequest` and calls `admit` with its `SecurityContext`.
2. The gate validates identifiers and size.
3. If both pass, the gate calls the engine under the timeout.
4. The gate publishes any refusal or shadow events and returns admitted or refused; IRM proceeds only on admitted.

### Substitute the Admission Engine

- [ ] `p2` - **ID**: `cpt-cf-admission-control-usecase-substitute-engine`

An operator links another engine plugin and sets `engine.vendor` (and optionally `instance_id`); after restart the gate
uses it with no change to enforcing gears.

## 9. Acceptance Criteria

- [ ] An oversized request is refused without calling the engine.
- [ ] With no engine, every valid operation within bounds is refused as could-not-run.
- [ ] An engine timeout or failure refuses; it never admits.
- [ ] An anonymous context and an invalid identifier are errors, not verdicts.
- [ ] One event is published per refusing policy and per shadow denial, none for admissions.
- [ ] A full queue or absent broker does not change any verdict.
- [ ] An unknown key, a zero numeric setting or an unresolved engine fails startup.

## 10. Dependencies

| Dependency                   | Description                                                         | Criticality |
|------------------------------|---------------------------------------------------------------------|-------------|
| `types-registry`             | Engine discovery, event type registration                           | p1          |
| Admission engine (`policy-engine` by default) | Evaluates tenant policy                            | p2          |
| `event-broker`               | Receives refusal events                                             | p3          |

## 11. Assumptions

- Enforcing gears pass the end caller's `SecurityContext` unchanged.
- The engine resolves tenancy and applies tenant policy; the gate does not.
- IRM is the first enforcing gear; further gears adopt the same client.

## 12. Risks

| Risk                               | Impact                                   | Mitigation                                                   |
|------------------------------------|------------------------------------------|--------------------------------------------------------------|
| No engine configured               | Every valid operation is refused         | Fail closed by design; startup warning                       |
| Engine slower than its bound       | Operations refused as could-not-run      | Bounded call; metric on engine latency                       |
| Events dropped under load          | Gaps in the refusal trail                | Dropped-events metric; events never gate admissions          |

## 13. Open Questions

- Should a dropped-event alert threshold be recommended for operators?

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
