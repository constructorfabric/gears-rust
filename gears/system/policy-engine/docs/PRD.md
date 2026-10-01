---
description: "Product requirements for the Policy Engine: versioned Rego deny-rule bundles assigned to tenants and evaluated as the admission-control engine plugin."
---

<!-- cpt:
version: 1.0.0
status: draft
module: policy-engine
system: cf
-->

# PRD — Policy Engine

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
  - [5.1 Policy Content](#51-policy-content)
  - [5.2 Assignment and Inheritance](#52-assignment-and-inheritance)
  - [5.3 Evaluation](#53-evaluation)
  - [5.4 Management](#54-management)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Module-Specific NFRs](#61-module-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Author and Activate a Policy Bundle](#author-and-activate-a-policy-bundle)
  - [Evaluate an Admission Request](#evaluate-an-admission-request)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Policy Engine stores tenant-authored policy and evaluates it for the platform's admission gate. Policy is written as
Rego documents that define a boolean `deny` rule, grouped into versioned bundles and assigned to tenants. It serves
exactly two surfaces: the `admission-control` engine plugin, which is the only decision surface, and a management API
(Rust client and REST) for authoring, validating, activating and assigning bundles.

### 1.2 Background / Problem Statement

Enforcing gears (Infrastructure Resource Manager first) call `admission-control` before an operation. Platform-wide
rules are built into that gate; tenant-specific rules cannot be, because tenants author and change them at runtime. The
gate therefore delegates to one pluggable engine. Policy Engine is that engine: it finds the policy assigned to the
resource's tenant and its ancestors, evaluates it in isolation and answers permit or deny, failing closed on any error.

### 1.3 Goals (Business Outcomes)

- Tenant-controlled guardrails: tenants and their administrators author deny rules without a platform release.
- Safe rollout: a rule can run in shadow mode and report what it would have denied before it is enforced.
- Deterministic, bounded evaluation: the same input and content give the same answer within a fixed time budget.
- Fail closed: 0 permits are produced by an error, timeout or unreadable content.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Bundle | A named, tenant-owned container of versions. |
| Version | An immutable-once-active snapshot of a bundle's documents; state `draft`, `active` or `superseded`. |
| Document | A named Rego source defining the boolean `deny` rule, scoped by `resource_types` and `actions`. |
| Assignment | The link of a bundle to a tenant, with an `enforce` flag; inherited by descendant tenants. |
| Shadow denial | A denial by a non-enforcing assignment; reported but never refuses. |
| Barrier | A self-managed tenant boundary that tenant-resolver ancestry traversal does not cross by default. |

## 2. Actors

### 2.1 Human Actors

#### Policy Author

**ID**: `cpt-cf-policy-engine-actor-policy-author`

- **Role**: Authors documents, validates drafts and proposes versions.

#### Tenant Policy Administrator

**ID**: `cpt-cf-policy-engine-actor-tenant-policy-admin`

- **Role**: Activates versions and assigns bundles within the tenant subtree the platform authorizes them for.

#### Platform Operator

**ID**: `cpt-cf-policy-engine-actor-platform-operator`

- **Role**: Configures timeouts, cache and content limits, and selects the engine in `admission-control`.

### 2.2 System Actors

#### Admission Gateway

**ID**: `cpt-cf-policy-engine-actor-admission-gateway`

- **Role**: `admission-control`; calls this gear as its engine plugin and is its only decision consumer.

#### Enforcing Gear

**ID**: `cpt-cf-policy-engine-actor-enforcing-gear`

- **Role**: A gear whose operations are gated (IRM first); reaches this gear only through the gateway.

#### Hierarchy Provider

**ID**: `cpt-cf-policy-engine-actor-hierarchy-provider`

- **Role**: `tenant-resolver`; supplies tenant ancestry and reachability.

#### Types Registry

**ID**: `cpt-cf-policy-engine-actor-types-registry`

- **Role**: `types-registry`; confirms that concrete resource type ids named by documents exist.

## 3. Operational Concept & Environment

The gear runs in-process with its consumers, persists content through `toolkit-db` (Postgres or SQLite), and runs no
background tasks. Project-wide runtime conventions are in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md).
Policy Engine evaluates on the shared Rego facility (`toolkit-policy-evaluation`), which also runs `admission-control`
built-in policies.

## 4. Scope

### 4.1 In Scope

- Bundles, versions and documents, with the draft, active, superseded lifecycle.
- Content validation at validate and activate time.
- Assignment to tenants with inheritance along the ancestry, barriers respected, and enforce or shadow mode.
- Evaluation as the `admission-control` engine plugin, with tenant boundary check and fail-closed semantics.
- Management API (`PolicyManagementClientV1` and REST) authorized by three permissions.

### 4.2 Out of Scope

- Platform-wide rules: these are `admission-control` built-in policies, which also apply without an engine.
- Any decision API other than the engine plugin; callers reach policy through `admission-control`.
- Recording decisions, violations, audit export or refusal events: `admission-control` publishes refusal events.
- Permit-side outcomes: documents can only deny.
- Policy languages other than Rego, batch evaluation, and dry-run or version comparison tooling.

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration, e2e) targeting 90%+ code
> coverage unless otherwise specified.

### 5.1 Policy Content

#### Version Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-lifecycle-states`

A bundle **MUST** hold numbered versions in state draft, active or superseded; at most one draft and one active version
exist per bundle; activating a draft supersedes the previous active version; only a draft can be changed or deleted,
and an active or superseded version is immutable.

- **Actors**: `cpt-cf-policy-engine-actor-policy-author`, `cpt-cf-policy-engine-actor-tenant-policy-admin`

#### Documents

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-bundle-composition`

A version **MUST** hold uniquely named documents; each document carries Rego source defining the boolean rule `deny`
(true denies; false or undefined does not; any other value is an evaluation error), `resource_types` (GTS patterns; a
concrete id is a pattern without wildcard) and `actions` (empty means all).

- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

#### Content Validation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-content-validation`

Validate and activate **MUST** apply the same checks and report every finding against its document: the source
compiles and defines `deny`; source and AST depth and rule-dependency guards pass; no determinism or resource-heavy
builtin is referenced; at least one valid GTS pattern is listed; every concrete type id exists in `types-registry`; the
version respects the configured document count and document size limits. Saving an incomplete draft is allowed;
activating a failing one is refused. The registry is consulted only at validate and activate, never per request.

- **Actors**: `cpt-cf-policy-engine-actor-policy-author`, `cpt-cf-policy-engine-actor-types-registry`

### 5.2 Assignment and Inheritance

#### Tenant Assignment

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-tenant-assignment`

The system **MUST** assign a bundle to a tenant at most once; an assignment applies to that tenant and is inherited by
every descendant tenant along the ancestry. Only a bundle's active version is ever evaluated; a bundle without an active
version contributes nothing.

- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`

#### Barriers Always Respected

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-inheritance-barriers`

Inheritance **MUST** stop at self-managed tenant barriers; no assignment and no setting reaches across one. Rules that
must apply platform-wide belong in `admission-control` built-in policies, not in assignments.

- **Actors**: `cpt-cf-policy-engine-actor-hierarchy-provider`

#### Enforce Flag and Shadow Mode

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-non-enforcing-assignment`

An assignment **MUST** carry an `enforce` flag, default true. A denial from a non-enforcing assignment is a shadow
denial and **MUST NOT** refuse the operation. The flag can be changed without changing content.

- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`

### 5.3 Evaluation

#### Admission Engine Plugin

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-admission-engine`

The system **MUST** register as an `admission-control` engine plugin and answer each evaluation with a permit or a
denial. It **MUST** expose no other decision surface and no batch call; enforcing gears reach it only through the
gateway.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`, `cpt-cf-policy-engine-actor-enforcing-gear`

#### Tenant Boundary Check

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-cross-tenant`

The caller's tenant **MUST** be the resource tenant or an ancestor of it, barriers respected; otherwise the system
**MUST** deny with reason `TENANT_BOUNDARY` and evaluate nothing.

- **Actors**: `cpt-cf-policy-engine-actor-hierarchy-provider`

#### Applicable Set

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-applicable-set`

The applicable documents **MUST** be those of the active versions of bundles assigned along the tenant chain whose
`resource_types` match the request's resource type and whose `actions` are empty or contain the action. Every
applicable document is evaluated; none is skipped after a denial.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Per-Request Loading and Compile Cache

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-active-content-loading`

Active content **MUST** be read from the store on every request, so an activation, assignment or unassignment takes
effect on the next request. Compiled documents **MAY** be cached by version id, which is safe because active versions
are immutable; the cache is bounded.

- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

#### Evaluation Input

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-evaluation-input`

Rego input **MUST** be `action`, `resource` (`type`, `id`, `tenant_id`), `properties` and `subject` (`id`,
`tenant_id`). The subject comes from the caller's `SecurityContext` only; an anonymous context is an invalid request.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Any Enforced Deny Wins

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-denial-precedence`

The result **MUST** be a denial when at least one document of an enforcing assignment denies, and a permit otherwise;
no ordering or priority between documents or bundles exists.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Denial Reasons

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-denial-reason`

A policy denial **MUST** carry reason `POLICY_DENIED` and name every denying document by bundle, version, document id
and document name.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Shadow Denials Reported

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-shadow-denials`

Shadow denials **MUST** be returned to the gate alongside either a permit or a denial, identified the same way as
denials.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Evaluation Isolation and Timeouts

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-evaluation-isolation`

Evaluation **MUST** see only the request input and the time; determinism and resource-heavy builtins are refused; a
panic is contained; all documents of a request share one wall-clock budget, and tenant-resolver calls and registry
lookups each have their own bound. Exceeding a bound is a failure, not a denial. Budgets are operator-configurable.

- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

#### Denial Versus Failure

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-denial-versus-failure`

The system **MUST** fail closed and keep a policy denial distinguishable from a failure (unavailable, timeout,
internal, invalid request); no failure is ever reported as a permit, and no setting converts one into a permit.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

### 5.4 Management

#### Management Authorization

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-admin-authorization`

Every management operation **MUST** be authorized against the caller's own context for exactly one of three
permissions, none implying another: `read` (read content), `author` (create and update bundles, create, replace,
delete and validate drafts), `publish` (activate, assign, update assignment, unassign). Assigning needs a target tenant
reachable from the caller with barriers respected; content the caller may not see looks absent.

- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`, `cpt-cf-policy-engine-actor-policy-author`

#### Retry-Safe Management

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-idempotent-management`

Management calls **MUST** be safe to retry without keys: a second open draft, a duplicate bundle name or a duplicate
assignment is a conflict; activating an active version and unassigning a missing assignment succeed as no-ops.

- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide NFRs defined in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md)
> and [guidelines/](../../../../guidelines/). Document only module-specific NFRs here.

### 6.1 Module-Specific NFRs

#### Fail-Closed Determinism

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-fail-closed`

Every error path **MUST** refuse.

- **Threshold**: 0 permits across injected failures: evaluation error, timeout, hierarchy or registry outage, content that does not compile, anonymous context.

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-tenant-isolation`

Content visibility, management and decisions **MUST** stay within the tenant boundary.

- **Threshold**: 0 cross-tenant reads, writes or decision influences in the isolation suite, including barrier boundaries.

#### Decision Latency

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-decision-latency`

A decision **MUST** complete within 25 ms at p95, measured at the gear boundary, under normal load.

- **Threshold**: p95 of 25 ms; defaults are 5 ms evaluation and 20 ms per hierarchy call.

#### Availability

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-nfr-availability`

The gear **MUST NOT** reduce the availability of its host process: no failure originating in the gear stalls or
terminates the host.

- **Threshold**: 0 host-fatal outcomes in the fault-injection set of `cpt-cf-policy-engine-nfr-fail-closed`.

### 6.2 NFR Exclusions

- Decision durability and audit trails: N/A here; refusal events are published by `admission-control`.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Policy Management Client

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-interface-management-client`

- **Type**: Rust trait (`PolicyManagementClientV1`, in ClientHub)
- **Stability**: unstable
- **Description**: Bundles, versions (draft, replace, delete, validate, activate) and assignments.

#### Policy Administration REST API

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-interface-rest-api`

- **Type**: REST under `/policy-engine/v1`
- **Stability**: unstable
- **Description**: `/bundles`, `/bundles/{id}`, `/bundles/{id}/versions[/{version}]` with `validate` and `activate`,
  and `/assignments[/{id}]`. List endpoints use standard toolkit OData paging. No decision endpoint exists.

### 7.2 External Integration Contracts

#### Admission Engine Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-contract-admission-engine`

- **Direction**: provided to `admission-control`
- **Protocol/Format**: `AdmissionEnginePluginClientV1::evaluate`, registered as a GTS plugin instance.
- **Compatibility**: Governed by the `admission-control` SDK version.

#### GTS Registrations

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-contract-gts`

- **Direction**: provided to `types-registry`
- **Protocol/Format**: Resource types for bundle, bundle version and assignment, the engine plugin instance, and the
  `read`, `author` and `publish` permission instances.
- **Compatibility**: GTS-versioned (`v1`).

#### Tenant Hierarchy Read

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-contract-hierarchy-read`

- **Direction**: required from `tenant-resolver`
- **Protocol/Format**: Ancestor chain and reachability, both with barrier mode respect.
- **Compatibility**: Tracks the `tenant-resolver` SDK.

## 8. Use Cases

### Author and Activate a Policy Bundle

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-usecase-activate-bundle`

**Actor**: `cpt-cf-policy-engine-actor-policy-author`

**Preconditions**:

- The author holds `author`, the administrator `publish`.

**Main Flow**:

1. The author creates a bundle and a draft and saves documents.
2. The author validates the draft and fixes any findings.
3. The administrator activates the draft; the previous active version becomes superseded.
4. The administrator assigns the bundle to a tenant, in shadow mode first if desired.

**Postconditions**:

- The next evaluation for that tenant and its descendants uses the new version.

**Alternative Flows**:

- **Validation fails**: activation is refused with every finding; the draft stays editable.

### Evaluate an Admission Request

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-usecase-admit-operation`

**Actor**: `cpt-cf-policy-engine-actor-admission-gateway`

**Preconditions**:

- The engine is selected in `admission-control`.

**Main Flow**:

1. The gateway calls `evaluate` with the operation.
2. The gear checks the tenant boundary and loads the assigned active documents.
3. It evaluates the matching documents and returns a permit or a denial, with any shadow denials.

**Postconditions**:

- No state changed.

**Alternative Flows**:

- **Dependency failure or timeout**: a failure is returned; the gateway refuses.

## 9. Acceptance Criteria

- [ ] A draft validates and activates under identical checks; a failing draft never activates.
- [ ] A second open draft is a conflict; re-activating and re-unassigning succeed without effect.
- [ ] An enforced denial in any assigned ancestor bundle refuses; a shadow denial never refuses and is reported.
- [ ] A caller outside the resource tenant's ancestry gets `TENANT_BOUNDARY`.
- [ ] Assignments do not reach across a barrier.
- [ ] Every injected dependency failure, timeout or evaluation error yields a failure, never a permit.
- [ ] Each management operation is refused without its permission.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `admission-control` | Consumes the engine plugin contract | p1 |
| `tenant-resolver` | Ancestry and reachability | p1 |
| `types-registry` | Concrete type id checks at validate and activate; GTS registration | p1 |
| `authz-resolver` | Authorizes management operations | p1 |
| `toolkit-db` | Persistence of bundles, versions, documents and assignments | p1 |
| `toolkit-policy-evaluation` | Shared Rego compile, guards, denylists and timeout | p1 |

## 11. Assumptions

- Enforcing gears supply complete operation properties; a missing property evaluates as absent.
- Policy volume per tenant is small, so a per-request indexed read is affordable.
- Consumers enforce the verdicts `admission-control` returns.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| A regorus upgrade adds a non-deterministic builtin | Determinism lost silently | The regorus version is pinned exactly; denylists live in the shared library |
| Hierarchy outage | Every evaluation fails, so gated operations are refused | Fail closed by design; bounded timeout |
| A document that cannot compile reaches an active version | Affected tenants are refused | Activation runs the same validation; a compile failure at evaluation is a failure, not a permit |

## 13. Open Questions

- Does any enforcing gear need policy applied across a barrier? Today platform-wide rules go to `admission-control` built-ins.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **Consumer**: [admission-control](../../admission-control/docs/PRD.md)
