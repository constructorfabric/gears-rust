---
description: "Product requirements for the Policy Engine: configured built-in policies and versioned Rego deny-rule bundles assigned to tenants, evaluated as the default admission-control engine plugin."
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
- [14. Changes from the Previous PRD](#14-changes-from-the-previous-prd)
- [15. Traceability](#15-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Policy Engine evaluates platform and tenant policy for the platform's admission gate, as its default engine. Policy is
written as Rego documents that define a boolean `deny` rule. Platform rules are built-in policies from the gear's
configuration and apply to every tenant; tenant rules are grouped into versioned bundles and assigned to tenants. It
serves exactly two surfaces: the `admission-control` engine plugin, which is the only decision surface, and a
management API (Rust client and REST) for authoring, validating, activating and assigning bundles.

### 1.2 Background / Problem Statement

Enforcing gears (Infrastructure Resource Manager first) call `admission-control` before an operation. The gate makes
no policy decision itself; it delegates to one pluggable engine. Policy Engine is the default engine: it evaluates the
platform's built-in policies, then the policy assigned to the resource's tenant and its ancestors, in isolation, and
answers permit or deny, failing closed on any error. Tenant rules are authored and changed at runtime; built-in
policies change only with the configuration, and are the only rules that reach tenants behind a barrier.

### 1.3 Goals (Business Outcomes)

- Platform guardrails: built-in policies apply to every tenant, self-managed ones included, and tenant policy cannot
  override them.
- Tenant-controlled guardrails: tenants and their administrators author deny rules without a platform release.
- Safe rollout: a rule can run in shadow mode and report what it would have denied before it is enforced.
- Deterministic, bounded evaluation: the same input and content give the same answer within a fixed time budget.
- Fail closed: 0 permits are produced by an error, timeout or unreadable content.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Built-in policy | A Rego document from the gear's configuration, scoped like a document, applied to every tenant before tenant policy. |
| Bundle | A named, tenant-owned container of versions. |
| Version | An immutable-once-active snapshot of a bundle's documents; state `draft`, `active` or `superseded`. |
| Document | A named Rego source defining the boolean `deny` rule, scoped by resource types and actions. |
| Assignment | The link of a bundle to a tenant, enforcing or in shadow mode; inherited by descendant tenants. It applies only while its owner tenant (the assigning administrator's tenant) still reaches the target tenant with barriers respected; otherwise it is dormant. |
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

- **Role**: Configures built-in policies, evaluation and call bounds, caching and content limits, and selects the
  engine in `admission-control`.

### 2.2 System Actors

#### Admission Gateway

**ID**: `cpt-cf-policy-engine-actor-admission-gateway`

- **Role**: `admission-control`; calls this gear as its engine plugin and is its only decision consumer.

#### Enforcing Gear

**ID**: `cpt-cf-policy-engine-actor-enforcing-gear`

- **Role**: A gear whose operations are gated (IRM first); reaches this gear only through the gateway.

#### Hierarchy Provider

**ID**: `cpt-cf-policy-engine-actor-hierarchy-provider`

- **Role**: `tenant-resolver`; supplies tenant ancestry (decisions) and reachability (management).

#### Types Registry

**ID**: `cpt-cf-policy-engine-actor-types-registry`

- **Role**: `types-registry`; confirms that concrete resource type ids named by documents and built-in policies exist.

## 3. Operational Concept & Environment

The gear runs in-process with its consumers, persists content through `toolkit-db` (Postgres or SQLite), and runs no
background tasks. Project-wide runtime conventions are in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md).
Policy Engine evaluates on the shared Rego facility (`toolkit-policy-evaluation`).

## 4. Scope

### 4.1 In Scope

- Built-in policies from configuration, validated at startup and evaluated before tenant policy for every tenant.
- Bundles, versions and documents, with the draft, active, superseded lifecycle.
- Content validation at validate and activate time.
- Assignment to tenants with inheritance along the ancestry, barriers respected, and enforce or shadow mode.
- Evaluation as the `admission-control` engine plugin, with tenant boundary check and fail-closed semantics.
- Management API (Rust client and REST) authorized by three permissions.

### 4.2 Out of Scope

- A runtime API for built-in policies: they change only with the configuration.
- Any decision API other than the engine plugin; callers reach policy through `admission-control`.
- Recording decisions, violations, audit export or refusal events: `admission-control` publishes refusal events.
- Permit-side outcomes: documents can only deny.
- Policy languages other than Rego, batch evaluation, and version comparison tooling.
- Deferred to p2: administration audit trail, idempotency keys, deprecation, and ancestor guardrails across a barrier
  (5.2, 5.4).

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
(true denies; false or undefined does not; any other value is an evaluation error), the resource types it applies to
(GTS patterns; a concrete id is a pattern without wildcard) and the actions it applies to (none listed means all).

- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

#### Content Validation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-content-validation`

Validate and activate **MUST** apply the same checks and refuse invalid content alike, reporting every finding
against its document. The checks: the source compiles and defines `deny`; the shared library's structural guards
pass; no determinism or resource-heavy builtin is referenced; at least one valid GTS pattern is listed; every
concrete type id exists in `types-registry`; the version respects the configured document count and document size limits; document names are printable and fit in a
denial, and resource types and actions are bounded in number and length. Saving an incomplete draft is allowed, but
not one that breaks a bound or names a document unprintably; activating a failing one is refused. The registry is
consulted only at validate and activate, never per request.

- **Actors**: `cpt-cf-policy-engine-actor-policy-author`, `cpt-cf-policy-engine-actor-types-registry`

### 5.2 Assignment and Inheritance

#### Tenant Assignment

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-tenant-assignment`

The system **MUST** assign a bundle to a tenant at most once; an assignment applies to that tenant and is inherited by
every descendant tenant along the ancestry. Only a bundle's active version is ever evaluated; a bundle without an active
version contributes nothing. An assignment applies only while its owner tenant (the tenant of the administrator who made
it) still reaches the target tenant with barriers respected (`cpt-cf-policy-engine-fr-inheritance-barriers`).

- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`

#### Barriers Always Respected

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-inheritance-barriers`

Inheritance **MUST** stop at self-managed tenant barriers; in this release no assignment and no setting reaches across
one. Rules that must apply platform-wide belong in built-in policies (`cpt-cf-policy-engine-fr-builtin-policies`), not
in assignments.

An assignment applies only while its owner tenant still reaches its target tenant with barriers respected. When a tenant
becomes self-managed, assignments made by ancestors above the new barrier **MUST** stop applying immediately: they
become dormant and apply again if the barrier is removed. A dormant assignment can be removed by an administrator of the
target tenant (`publish` on the target tenant, target reachable from the caller with barriers respected); the owner
above the barrier is refused with `TENANT_BOUNDARY`. Unassigning never reports success for an assignment the caller can
see but did not delete; one the caller cannot see is reported as absent. Any change the caller may not make to an
assignment it can see (as the listing shows it) **MUST** be refused with `CAPABILITY_DENIED`, never reported absent.

Assignments **MUST** be listable, filtered by bundle or by target tenant, and every assignment **MUST** say whether it is
dormant. A caller sees the assignments of bundles it may read at its tenants (barriers ignored) made from its side of
every barrier, so an owner sees an assignment that stopped applying, and the dormant assignments at tenants it reaches
on which it holds `publish`, so an administrator of the target tenant can find what it may remove. A listing filter
naming content the caller cannot see yields no rows; a read of an assignment the caller would not see in the listing
reports it absent. A listing for a caller whose subtree is too large to evaluate is refused with `SUBTREE_TOO_LARGE`,
never answered partially; reading one assignment is not limited.

- **Actors**: `cpt-cf-policy-engine-actor-hierarchy-provider`, `cpt-cf-policy-engine-actor-tenant-policy-admin`

#### Ancestor Guardrail Across a Barrier

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-barrier-guardrail-reach`

An ancestor's guardrail assignment **SHOULD** be able to keep applying to a tenant that has become self-managed, so that
a self-managed tenant is not exempt from an ancestor's guardrail. This is deferred: in this release assignments never
cross a self-managed barrier (`cpt-cf-policy-engine-fr-inheritance-barriers`), and platform-wide guardrails are expressed
as built-in policies, which are configured and evaluated for every tenant.

- **Rationale**: Removing it from p1 leaves operator guardrails to built-in policies; whether tenant-authored policy
  should ever cross a barrier is open (section 13).
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`, `cpt-cf-policy-engine-actor-hierarchy-provider`

#### Enforce Flag and Shadow Mode

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-non-enforcing-assignment`

An assignment **MUST** either enforce, the default, or run in shadow mode. A denial from a non-enforcing assignment is
a shadow denial and **MUST NOT** refuse the operation. The mode can be changed without changing content.

- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`

### 5.3 Evaluation

#### Built-in Policies

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-builtin-policies`

Built-in policies **MUST** come from the gear's configuration only, each with a unique id and the form of a document
(`cpt-cf-policy-engine-fr-bundle-composition`). The matching built-ins **MUST** be evaluated before the tenant boundary
check and before any tenant policy, in configuration order, for every request whatever the caller's and the resource's
tenants; the first denial **MUST** deny with reason `BUILTIN_POLICY_DENIED`, and no tenant policy can override it. A
built-in that cannot be evaluated is a failure, never a permit.

- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`, `cpt-cf-policy-engine-actor-admission-gateway`

#### Built-in Policy Validation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-builtin-validation`

Startup **MUST** fail on a built-in policy with a blank or duplicate id or any finding of
`cpt-cf-policy-engine-fr-content-validation` (the built-ins counting as one version), so a built-in that is accepted
compiles and never fails a request for its content.

- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`, `cpt-cf-policy-engine-actor-types-registry`

#### Admission Engine Plugin

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-admission-engine`

The system **MUST** register as an `admission-control` engine plugin and answer each evaluation with a permit or a
denial. It **MUST** expose no other decision surface and no batch call; enforcing gears reach it only through the
gateway.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`, `cpt-cf-policy-engine-actor-enforcing-gear`

#### Tenant Boundary Check

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-cross-tenant`

The caller's tenant **MUST** be the resource tenant or an ancestor of it, barriers respected, derived from the same
ancestor chain that selects the policies, so a concurrent barrier change cannot give an inconsistent answer; otherwise
the system **MUST** deny with reason `TENANT_BOUNDARY` and evaluate no tenant policy.

- **Actors**: `cpt-cf-policy-engine-actor-hierarchy-provider`

#### Applicable Set

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-applicable-set`

The applicable documents **MUST** be those of the active versions of bundles assigned along the tenant chain (by
assignments that currently apply) whose
resource types match the request's resource type and whose actions are unlisted or include the action. Every
applicable document is evaluated; none is skipped after a denial.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Changes Take Effect on the Next Request

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-active-content-loading`

An activation, assignment, assignment update or unassignment **MUST** take effect on the next evaluation request, with
no restart and no propagation delay.

- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

#### Evaluation Input

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-evaluation-input`

Rego input **MUST** be `action`, `resource` (`type`, `id`, `tenant_id`), `properties` and `subject` (`id`,
`tenant_id`), the same for built-in policies and documents. The subject comes from the caller's security context only;
an anonymous context is an invalid request.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Any Enforced Deny Wins

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-denial-precedence`

Past the built-in policies, the result **MUST** be a denial when at least one document of an enforcing assignment
denies, and a permit otherwise; no ordering or priority between documents or bundles exists.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Denial Reasons

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-denial-reason`

A policy denial **MUST** carry reason `POLICY_DENIED` and name the denying documents by bundle, version, document id and
document name, as many as the admission gate accepts per result: past its bound, shadow denials are dropped first,
then denials, the drop is logged, and the outcome never changes. A built-in denial **MUST** carry reason
`BUILTIN_POLICY_DENIED` and no references, like any other engine denial; the denying built-in's id is logged with the
correlation id.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Shadow Denials Reported

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-shadow-denials`

Shadow denials **MUST** be returned to the gate alongside either a permit or a denial, identified the same way as
denials.

- **Actors**: `cpt-cf-policy-engine-actor-admission-gateway`

#### Evaluation Isolation and Timeouts

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-evaluation-isolation`

Evaluation **MUST** see only the request input and the time; determinism and resource-heavy builtins are refused; a
panic is contained; all documents of a request, built-in policies included, share one wall-clock budget, and
tenant-resolver calls, policy store reads and registry lookups each have their own bound. Exceeding a bound is a
failure, not a denial. Budgets are operator-configurable within fixed maxima.

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
delete and validate drafts), `publish` (activate, assign, update assignment, unassign; also listing and reading the
dormant assignments the caller may remove). Assigning needs a target tenant
reachable from the caller with barriers respected; unassigning needs `publish` on the target tenant, reachable from the
caller with barriers respected, so an administrator of the target tenant can remove an assignment whose owner is above a
barrier; content the caller may not see looks absent. Bundle names must be
printable, and bundle names and descriptions are bounded in length.

- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`, `cpt-cf-policy-engine-actor-policy-author`

#### Retry-Safe Management

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-retry-safe-management`

Management calls **MUST** be safe to retry without keys: a second open draft, a duplicate bundle name or a duplicate
assignment is a conflict; activating an active version and unassigning a missing assignment succeed as no-ops.
Unassigning succeeds only for an assignment that was deleted, does not exist, or is not visible to the caller; one the
caller can see but may not remove is refused.

- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

#### Dry Runs of Publish Operations

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-publish-dry-run`

Activate, assign, update assignment and unassign **MUST** each offer a dry run that runs every check of the operation
against the same state and writes nothing, so that clients never replicate those checks. A dry run requires the
permission the operation requires and answers exactly what the operation would: success (a no-op success included), or
the operation's own error with the same masking, so it never reveals what the operation would hide. It is advisory: it
reserves nothing, and the operation checks again when it runs, so a concurrent writer can still make it fail.

- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`

#### Administration Audit

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-administration-audit`

Deferred. The system **SHOULD** record every change to bundles, versions and assignments as an administration event,
retained at least as long as the content it describes, carrying the acting subject and tenant, the time, the operation,
and the identity of the content affected. A full trail depends on the future platform Audit gear. In this release the
gear records attribution only: an assignment stores `created_by` and `updated_by` (actor subject ids) beside
`created_at` and `updated_at`; assign, update-assignment and unassign each emit an info-level structured log line with
the actor, assignment id, bundle id and target tenant; version activation records `activated_by`.

- **Rationale**: Two commitments depend on attribution no other requirement delivers: every policy change carries an
  author, and every state change is attributable to an actor. Retention has to outlast the content, because the
  question an auditor asks is who put a rule in force, and that question outlives every decision the rule produced.
- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`, `cpt-cf-policy-engine-actor-platform-operator`

#### Idempotency Keys

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-idempotent-management`

Deferred. The system **SHOULD** accept a client-supplied idempotency key on every management operation that creates
content or changes lifecycle or assignment state (create bundle, create draft, activate, delete draft, assign, update
assignment, unassign), following the platform's REST convention: the `Idempotency-Key` header on REST and an explicit
parameter on the management client. A repeat with the same key and request content returns the original result marked
as a replay and creates nothing new; the same key with different content is a conflict; keys are retained for at least
the platform's one-hour replay window, scoped to the calling tenant. The key record, the entity it protects and any
administration event (`cpt-cf-policy-engine-fr-administration-audit`) are written in one transaction. Until then,
retries are handled by `cpt-cf-policy-engine-fr-retry-safe-management`.

- **Rationale**: A create whose response was lost is retried by every reasonable client; without a key the retry
  yields a second draft or a duplicate caught only by a unique constraint, and, once audited, two events where one change
  was intended. The platform already settles the header, window and replay marker, so the gear adopts the convention.
  Writing the key in the same transaction is what makes the guarantee hold: a key committed separately could be lost
  while the change survives.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`, `cpt-cf-policy-engine-actor-tenant-policy-admin`

#### Deprecation

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-deprecation`

Deferred. The system **SHOULD** allow an active version to be deprecated so that its bundle stops contributing to
evaluation on the next request, while the bundle and its history are retained. In this release a bundle is withdrawn by
unassigning it, and versions only move draft, active, superseded.

- **Rationale**: Withdrawing policy must be as controlled as activating it, and must not destroy the record of what was
  in force.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide NFRs defined in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md)
> and [guidelines/](../../../../guidelines/). Document only module-specific NFRs here.

### 6.1 Module-Specific NFRs

#### Fail-Closed Determinism

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-fail-closed`

Every error path **MUST** refuse.

- **Threshold**: 0 permits across injected failures: evaluation error of a built-in or a document, timeout, hierarchy or registry outage, content that does not compile, anonymous context.

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-tenant-isolation`

Content visibility, management and decisions **MUST** stay within the tenant boundary.

- **Threshold**: 0 cross-tenant reads, writes or decision influences in the isolation suite, including barrier boundaries.

#### Decision Latency

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-decision-latency`

A decision **MUST** complete within 25 ms at p95, measured at the gear boundary, under normal load.

- **Threshold**: p95 of 25 ms.

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

- **Type**: Rust client trait in ClientHub, in the gear's SDK
- **Stability**: unstable
- **Description**: Bundles, versions (draft, replace, delete, validate, activate) and assignments, and a dry run of
  each publish operation (`check_publish`). Errors are typed;
  each carries a stable code that also travels on the wire. Bundles, versions and assignments are cursor-paged
  (OData `$top` and cursor; default page 25, maximum 100); bundles are filterable by `name` and `owner_tenant_id`, so a
  bundle can be found by its name; versions are listed newest ordinal first; assignments are filterable by `bundle_id` and `tenant_id` and
  carry a `dormant` flag.

#### Policy Administration REST API

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-interface-rest-api`

- **Type**: Versioned REST API with a published OpenAPI document
- **Stability**: unstable
- **Description**: The management client's operations over bundles, versions and assignments; lists, versions included, are cursor-paged. The
  publish operations take `dry_run=true`, answering `204` when the operation would succeed and its problem
  otherwise; validation answers the same way. No decision endpoint exists.

### 7.2 External Integration Contracts

#### Admission Engine Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-contract-admission-engine`

- **Direction**: provided to `admission-control`
- **Protocol/Format**: The `admission-control` engine plugin contract, in-process, registered as a GTS plugin instance.
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
- **Protocol/Format**: Ancestor chain (one call per decision) and reachability (management), both respecting barriers.
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

1. The gateway asks the engine to evaluate the operation.
2. The gear evaluates the matching built-in policies; the first denial is returned at once.
3. It resolves the tenant chain, checks the tenant boundary against it and loads the assigned active documents.
4. It evaluates the matching documents and returns a permit or a denial, with any shadow denials.

**Postconditions**:

- No state changed.

**Alternative Flows**:

- **Dependency failure or timeout**: a failure is returned; the gateway refuses.

## 9. Acceptance Criteria

- [ ] A built-in denial refuses for every tenant, behind a barrier included, before any tenant policy is read.
- [ ] A built-in that fails validation fails startup.
- [ ] A draft validates and activates under identical checks; a failing draft never activates.
- [ ] A second open draft is a conflict; re-activating and re-unassigning succeed without effect.
- [ ] An enforced denial in any assigned ancestor bundle refuses; a shadow denial never refuses and is reported.
- [ ] A caller outside the resource tenant's ancestry gets `TENANT_BOUNDARY`.
- [ ] Assignments do not reach across a barrier; when a tenant becomes self-managed, assignments of ancestors above the
  new barrier stop applying at once, and the target tenant's administrator can remove them while the owner gets
  `TENANT_BOUNDARY`.
- [ ] Assignment changes record `created_by` and `updated_by` and log the actor, assignment id, bundle id and target tenant.
- [ ] Versions are listed newest first in pages of 25 by default, 100 at most.
- [ ] Every injected dependency failure, timeout or evaluation error yields a failure, never a permit.
- [ ] Each management operation is refused without its permission.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `admission-control` | Consumes the engine plugin contract | p1 |
| `tenant-resolver` | Ancestry (decisions) and reachability (management) | p1 |
| `types-registry` | Concrete type id checks at startup (built-ins), validate and activate; GTS registration | p1 |
| `authz-resolver` | Authorizes management operations | p1 |
| `toolkit-db` | Persistence of bundles, versions, documents and assignments | p1 |
| `toolkit-policy-evaluation` | Shared Rego compilation, guards, denylists and evaluation budget | p1 |

## 11. Assumptions

- Enforcing gears supply complete operation properties; a missing property evaluates as absent.
- Policy volume per tenant is small, so a per-request indexed read is affordable.
- Consumers enforce the verdicts `admission-control` returns.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| A Rego engine upgrade adds a non-deterministic builtin | Determinism lost silently | The engine version is pinned exactly; denylists live in the shared library |
| Hierarchy outage | Every evaluation fails, so gated operations are refused | Fail closed by design; bounded timeout |
| A document that cannot compile reaches an active version | Affected tenants are refused | Activation runs the same validation; a compile failure at evaluation is a failure, not a permit |
| A broad built-in policy | Every tenant is refused alike | Built-ins change only with the configuration; their denials name the reason `BUILTIN_POLICY_DENIED` and are logged with the policy id |

## 13. Open Questions

- Does any enforcing gear need tenant-authored policy applied across a barrier (`cpt-cf-policy-engine-fr-barrier-guardrail-reach`)? Today only built-in policies cross one.

## 14. Changes from the Previous PRD

The previous PRD was cut down to the default admission-control engine. Removed or deferred:

- Decision records and violations projection: removed; `admission-control` publishes refusal events.
- Obligations and permit-side outcomes: removed; documents only deny.
- Batch evaluation: removed; the engine answers one request at a time.
- Trigger, phase and attribute filter matching: removed; documents match on resource type and action only.
- Subject-based evaluation: removed; the subject is rule input from the security context, not a matching dimension.
- Version comparison tooling: removed; shadow mode is the way to try a rule. Dry runs of the publish operations
  (`cpt-cf-policy-engine-fr-publish-dry-run`) check whether an operation would be accepted, not what a rule denies.
- Staged rollout beyond shadow mode: removed; an assignment enforces or runs in shadow.
- Any decision API other than the admission-control plugin: removed; no decision endpoint exists.
- `cpt-cf-policy-engine-fr-administration-audit`: moved to p2; full trail awaits the platform Audit gear, attribution
  fields and log lines exist meanwhile.
- `cpt-cf-policy-engine-fr-idempotent-management`: moved to p2; retries are conflicts or no-ops without keys
  (`cpt-cf-policy-engine-fr-retry-safe-management`).
- `cpt-cf-policy-engine-fr-deprecation`: moved to p2; withdrawal is by unassigning.
- Ancestor guardrail across a barrier: moved to p2 (`cpt-cf-policy-engine-fr-barrier-guardrail-reach`); built-in
  policies cover platform-wide guardrails.

## 15. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **Consumer**: [admission-control](../../admission-control/docs/PRD.md)
