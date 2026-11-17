---
description: "Product requirements for the quota resolver gear: one quota debit, lease, return, and state contract for platform services and for the authenticated principal itself, delegated to a quota backend plugin."
---

# PRD — Quota Resolver

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
  - [5.1 Quota Operations](#51-quota-operations)
  - [5.2 Subjects and Planes](#52-subjects-and-planes)
  - [5.3 Backend](#53-backend)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Charge a User for Generated Tokens](#charge-a-user-for-generated-tokens)
  - [Show a User Their Remaining Quota](#show-a-user-their-remaining-quota)
  - [Check a User's Quota Before Work](#check-a-users-quota-before-work)
  - [Charge a User from a Background Job](#charge-a-user-from-a-background-job)
  - [Hold and Return Allocated Capacity](#hold-and-return-allocated-capacity)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Quota Resolver is the entry point through which CF modules charge and read quotas. It offers four capabilities:

* **debit**: charge an amount of a metric;
* **lease**: hold capacity, then settle it with commit or release;
* **return**: give back an amount charged earlier: capacity a resource no longer holds, or a charge for work that
  failed;
* **quota state**: read the current state of quotas.

The capabilities are split across two planes:

* the **platform plane** offers all four: a platform service names the tenant and the subjects. A module that serves
  a user request takes that user's principal and tenant from the request's `SecurityContext` and passes them as
  request data;
* the **tenant plane** offers quota state only: the subject is the authenticated principal itself and the tenant is
  that principal's tenant.

A subject is either a quota subject or a principal. The resolver maps a principal to a quota subject from operator
configuration, so no calling module needs to know the operator's quota subject types.

The resolver owns no counters, quotas, or leases. It delegates every operation to a quota backend plugin that it
selects at runtime.

### 1.2 Background / Problem Statement

Many modules charge quotas for the work they do: model tokens, API calls, stored objects. They all need the same entry
point: charge an amount, reserve capacity before work whose cost is known only afterwards and settle it later, and
read what remains. Without one, each module integrates with the quota backend on its own terms and repeats the same
logic.

Two needs shape this entry point:

* The most common subject of a quota is the principal that makes the request, for example a user who spends model
  tokens. A calling module knows the principal type from `SecurityContext`, but it does not know which quota subject
  type the operator uses for that kind of principal. Without a shared mapping, each module repeats the operator's
  mapping.
* A module that serves a user request charges that user, and the user wants to see their own remaining quota. Neither
  should require a policy grant for every user.

### 1.3 Goals (Business Outcomes)

* One quota contract for all consuming modules, independent of the backend that holds the quota state.
* The operator states the principal mapping in one place; no caller code changes when it changes.
* An authenticated user can read their own quota state without any extra policy grant.
* No operation is allowed by default: a missing plugin, an unavailable backend, or a failed mapping lookup ends in an
  error, and a principal that is not mapped ends in a denial for a debit or a lease acquisition and an empty page for
  a quota state read, in 100% of cases.

### 1.4 Glossary

| Term           | Definition                                                                                                                                                              |
|----------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Principal      | Whoever authenticated: a person, a service account, a system actor. `SecurityContext` carries its type (the **principal type**, a GTS type id), its id, and its tenant. |
| Quota subject  | Whom a quota applies to: a quota subject type (a GTS type id derived from the resolver's quota subject base, for example a user or a tenant type) and an opaque id.     |
| Principal form | A subject given as a principal type and a principal id. The resolver maps it to a quota subject.                                                                        |
| Amount         | A positive integer in the smallest unit that the metric defines, for example MiB rather than a fraction of a GB.                                                        |
| Metric         | A registered GTS instance id that names what is counted, for example model tokens.                                                                                      |
| Charge         | An allowed debit or a lease commit, named by the idempotency key of that call.                                                                                          |
| Lease          | A hold on capacity for a bounded time, settled by commit (charge what was used) or release (charge nothing), or released when it expires.                               |
| Decision       | The verdict of a debit or a lease acquisition: allowed or denied. A denial is a successful call, not an error.                                                          |
| Platform plane | Calls authenticated as a platform service (`PlatformSecurityContext`). The service is trusted to name the tenant and the subjects.                                      |
| Tenant plane   | Calls that carry the caller's `SecurityContext`; quota state only. The subject and the tenant come from that context only.                                              |
| Edge           | The api-gateway entry for clients outside the platform. Only routes published there are reachable from outside.                                                         |
| Plugin         | A quota backend implementation, discovered through types-registry, that executes the operations.                                                                        |

## 2. Actors

### 2.1 Human Actors

#### End User

**ID**: `cpt-cf-quota-resolver-actor-end-user`

- **Role**: An authenticated principal that reads the state of its own quotas through the edge.
- **Needs**: Remaining capacity and the next reset time for the metrics it uses.

### 2.2 System Actors

#### Consuming Module

**ID**: `cpt-cf-quota-resolver-actor-consuming-module`

- **Role**: A CF module that charges quotas for the work it does. It calls the platform plane with its own service
credential. To charge the principal whose request it serves, it takes that principal's type, id, and tenant from the
request's `SecurityContext` and passes them as request data.

#### Quota Backend Plugin

**ID**: `cpt-cf-quota-resolver-actor-backend-plugin`

- **Role**: A backend that owns quotas, counters, and leases and executes the delegated operations.

## 3. Operational Concept & Environment

This module introduces no environment constraints beyond project defaults: see
[docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and [guidelines/](../../../../guidelines/).

**Quota kinds.** The backend applies quotas of three kinds. The kind belongs to the quota: a module charges a metric,
and the backend applies every quota on that metric by its kind. The resolver does not read the kind; the module knows
which kind its metric uses and calls the matching operations.

| Kind          | What it bounds                                                            | How a module charges                                                                                                                  | How capacity comes back                                                                                                                                        |
|---------------|---------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Rate limiting | The amount in a moving time window, for example 100 requests a minute     | A debit when it admits the work                                                                                                       | By itself, as the window moves; a return fails with an error                                                                                                   |
| Consumption   | The amount used in a period, for example 1 M tokens a month               | A debit before work whose cost is known, or a lease before work whose cost is known only afterwards, held for the most the work can use and committed for the actual amount | At the period reset. A return gives back a charge for work that failed, in the current period; a released lease and the unused part of a commit charge nothing |
| Allocation    | The amount held at one time, with no period, for example 20 parallel jobs | A lease while the resource is created, committed when it exists, or a debit. Growing a resource charges the added amount the same way | By a return: the whole amount when the resource is deleted, the removed part when it is reduced. A released lease charges nothing                              |

## 4. Scope

### 4.1 In Scope

* Debit of one or several metrics for a set of subjects in one tenant.
* Lease acquisition, commit, and release.
* Return of a charged amount.
* Lease extension.
* Quota state of the quotas that apply to given subjects and metrics.
* Mapping a principal to a quota subject from operator configuration.
* All operations on the platform plane; quota state on the tenant plane.
* Selection of a backend plugin and delegation to it.

### 4.2 Out of Scope

* The backend's other operations, such as quota and policy management: they stay in the backend's own API.
* Storage of quotas, counters, leases, and idempotency records.
* Quota evaluation and arbitration between several quotas.
* Authorization of platform-plane callers by the policy decision point.
* Quota notifications.
* Reconciliation of held allocation with the resources that exist: a resource deleted without a return keeps its
  capacity held until the module returns it or the operator corrects the quota through the backend's own API.

## 5. Functional Requirements

### 5.1 Quota Operations

#### Debit

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-debit`

The system **MUST** charge a positive amount of one metric for a set of subjects in one tenant and return a decision.
An allowed decision means the amount is charged; a denied decision means nothing is charged.

- **Rationale**: Modules charge consumption after or while doing work.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`

#### Lease

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-lease`

The system **MUST** acquire a lease on an amount of one metric for a set of subjects for a requested time, and return
either a lease token and its expiry or a denied decision. The system **MUST** commit a lease for an actual amount not
above the held amount, and release a lease without a charge. A module holds the most that the work can use, for a time
that covers the work, so the commit of completed work is never above the held amount. A commit or release that succeeds
returns no result; one that cannot be applied, for example because the lease has expired or the amount exceeds the held
amount, **MUST** fail with an error and change nothing.

- **Rationale**: Modules that learn the real cost only after the work (for example, generated tokens) must reserve
capacity first. A quota limits work before it starts; it does not record use above its cap. Actual use, including use
above a cap, is metered outside quotas.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`

#### Return

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-return`

The system **MUST** return a positive amount of a charge, named by the charge's idempotency key, in one tenant. The
return reaches the quotas, subjects, and metric that the charge reached, never others. Each quota takes the return by
its kind:

* allocation: the held amount decreases, so the capacity becomes available again;
* consumption: the consumed amount of the period of the charge decreases;
* rate limiting: the return fails.

A module returns allocation when it deletes or reduces a resource, and consumption when work it charged for fails. A
charge **MUST** stay returnable on allocation quotas until all of it is returned, and on consumption quotas until its
period ends, whatever the backend's window for repeated calls. A return that exceeds what remains of the charge, names
an unknown charge, or meets a rate-limiting quota **MUST** fail with an error and change nothing. Each growth of a
resource is a separate charge with its own key, so a module returns every charge of a resource when it deletes the
resource. Release only cancels a lease that is not committed; it does not return an amount after a commit.

- **Rationale**: Allocation capacity stays held after a commit until the module gives it back, and a charge for work
that failed must not count against the period. One return for both kinds keeps the module from knowing which kinds of
quotas the backend applies. Naming the charge keeps a module from returning capacity that another resource holds, and
keeps a return correct when the principal mapping changes after the charge.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`

#### Lease Extension

- [ ] `p2` - **ID**: `cpt-cf-quota-resolver-fr-lease-extension`

The system **MUST** extend an active lease by a requested time. Extension of a lease that has expired, been committed,
or been released **MUST** fail with an error and change nothing. Until extension is available, a module acquires a
lease for a time that covers the whole work.

- **Rationale**: Work whose length is not known in advance, such as creating a resource, can outlive its lease; an
expired lease frees capacity that the work still uses.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`

#### Several Metrics in One Operation

- [ ] `p2` - **ID**: `cpt-cf-quota-resolver-fr-multi-metric`

Debit and lease acquisition **MUST** accept several metrics, each with its own amount, for the same subjects, and apply
all of them or none: if one metric is denied or fails, nothing is charged. A commit **MUST** take the actual amount of
each metric of the lease, and a return the amount of each metric of the charge, also all or none.

- **Rationale**: One resource often needs several metrics together. A partly charged set holds capacity that no work
can use.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`

#### Quota State

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-quota-state`

The system **MUST** return, for one subject and given metrics in one tenant, the state of every quota that the
backend applies to them: cap, consumed amount, remaining amount, and, for a quota with a period, the current period
with its reset time. The
backend decides which quotas apply, including quotas of the tenant; the resolver returns the backend's page unchanged.

- **Rationale**: Modules and users must see remaining capacity before they start work.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`, `cpt-cf-quota-resolver-actor-end-user`

#### Idempotent Writes

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-idempotency`

Debit, lease acquisition, lease extension, commit, release, and return **MUST** require an idempotency key and pass it
to the backend unchanged. A repeated call with the same key that reaches the backend **MUST** return the first result
and charge nothing again. The outcome for a principal that is not mapped is decided again on every call and charges
nothing.

- **Rationale**: Callers retry after timeouts; a retry must not charge twice.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`

### 5.2 Subjects and Planes

#### Subject Forms

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-subject-forms`

A platform-plane request **MUST** accept each subject either as a quota subject or in the principal form.

- **Rationale**: A module charges both principals and subjects that are not principals, such as a device or an agent.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`

#### Principal Mapping

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-principal-mapping`

The system **MUST** map a principal type to a quota subject type from operator configuration: an entry for the type
itself wins, otherwise the entry of its nearest ancestor applies. A principal that is not mapped or has a nil id
**MUST** make a debit or a lease acquisition denied and a quota state read empty, without a call to the backend. A
tenant-plane caller with a nil tenant **MUST** get an empty quota state page, without a call to the backend.

- **Rationale**: The operator decides which principals take part in quotas; the module does not know the operator's
quota subject types.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`, `cpt-cf-quota-resolver-actor-end-user`

#### Platform Plane

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-platform-plane`

The platform plane **MUST** offer debit, lease acquisition, lease extension, commit, release, return, and quota state.
It **MUST** be callable only with a platform service credential and **MUST NOT** be reachable from the edge. The tenant
and the subjects come from the request. A module that charges the principal of a user request passes that principal and
its tenant as request data, taken from the request's `SecurityContext`; the user's `SecurityContext` is neither
forwarded nor synthesized.

- **Rationale**: Only platform services charge quotas, for any subject of any tenant they serve. Passing the user's
principal as data keeps one authentication model for every charge: the platform credential of the calling module.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`

#### Tenant Plane

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-tenant-plane`

The tenant plane **MUST** offer quota state only. It **MUST** take the subject only from the caller's principal and
the tenant only from the caller's tenant; the request **MUST NOT** carry fields that name either. It **MUST** be
reachable from the edge by any authenticated principal.

- **Rationale**: A user reads their own quotas without a policy grant. Only the module that does the work is
authorized to charge, because it decides the metric and the amount; the user is not.
- **Actors**: `cpt-cf-quota-resolver-actor-consuming-module`, `cpt-cf-quota-resolver-actor-end-user`

### 5.3 Backend

#### Plugin Delegation

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-fr-plugin-delegation`

The system **MUST** execute every operation through a backend plugin discovered through types-registry and selected by
vendor and priority, and **MUST NOT** store quota data itself.

- **Rationale**: Quota state belongs to the backend; consuming modules must not depend on a particular backend.
- **Actors**: `cpt-cf-quota-resolver-actor-backend-plugin`

## 6. Non-Functional Requirements

Project baselines: [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and
[guidelines/](../../../../guidelines/).

### 6.1 Gear-Specific NFRs

#### Fail Closed

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-nfr-fail-closed`

The system **MUST NOT** return an allowed decision or a lease when no plugin matches, the backend is unavailable, or
the principal mapping cannot be resolved.

- **Threshold**: 0 allowed decisions across these conditions.
- **Rationale**: Allowing by default lets consumption run past the budget.

#### Tenant Scoping

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-nfr-tenant-scoping`

Every plugin call **MUST** carry exactly one tenant: the request's tenant on the platform plane, the caller's tenant
on the tenant plane.

- **Threshold**: 0 plugin calls with a tenant other than the one this rule names.
- **Rationale**: Quotas are tenant data.

#### Resolver Overhead

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-nfr-overhead`

The time the resolver adds to an operation, excluding the plugin call, **MUST** be at most 5 ms at p95 with warm
caches.

- **Threshold**: ≤ 5 ms p95 at the resolver boundary, plugin time excluded.
- **Rationale**: Debit and lease sit on the request path of the consuming module.

### 6.2 NFR Exclusions

* Data durability and backup: not applicable, the resolver stores no data.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Quota Resolver Client

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-interface-client`

- **Type**: Two Rust traits in the SDK crate, one per plane, each with a REST projection: the platform plane under
`/quota-resolver/v1/platform`, the tenant plane under `/quota-resolver/v1`, as in types-registry.
- **Stability**: unstable.
- **Description**: Platform plane: debit, lease acquire, lease extension, commit, release, return, and quota state.
Tenant plane: quota state,
with the same operation name.
- **Breaking Change Policy**: Major version bump of the contract.

### 7.2 External Integration Contracts

#### Backend Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-quota-resolver-contract-backend-plugin`

- **Direction**: required from the plugin.
- **Protocol/Format**: in-process Rust trait. Requests carry the tenant and quota subjects only, never a principal.
- **Compatibility**: Major version bump of the plugin contract for any breaking change.

## 8. Use Cases

### Charge a User for Generated Tokens

- [ ] `p2` - **ID**: `cpt-cf-quota-resolver-usecase-lease-for-user`

**Actor**: `cpt-cf-quota-resolver-actor-consuming-module`

**Preconditions**:
- The operator maps the user principal type to a quota subject type, and a quota exists for that type and the
  metric.

**Main Flow**:
1. The module receives a user request with the user's `SecurityContext`.
2. It takes the user's principal type, id, and tenant from that context and acquires a lease for the most tokens
   that the request can generate (its token limit), for a time that covers the generation, on the platform plane, with
   the subject in the principal form.
3. It runs the work and commits the lease on the platform plane for the tokens actually used.

**Postconditions**:
- The quotas that the backend applies to the user are charged once for the used tokens.

**Alternative Flows**:
- **Denied**: the module refuses the user request and charges nothing.
- **Work fails**: the module releases the lease, or the lease expires.

### Show a User Their Remaining Quota

- [ ] `p2` - **ID**: `cpt-cf-quota-resolver-usecase-own-quota-state`

**Actor**: `cpt-cf-quota-resolver-actor-end-user`

**Preconditions**:
- The user is authenticated.

**Main Flow**:
1. The user requests their quota state for some metrics through the edge.
2. The system returns the state of the quotas that the backend applies to the user's mapped subject on those metrics.

**Postconditions**:
- Nothing changes.

**Alternative Flows**:
- **Principal not mapped**: the page is empty.

### Check a User's Quota Before Work

- [ ] `p2` - **ID**: `cpt-cf-quota-resolver-usecase-module-reads-user-quota`

**Actor**: `cpt-cf-quota-resolver-actor-consuming-module`

**Preconditions**:
- The module serves a user request with the user's `SecurityContext`.

**Main Flow**:
1. Before it starts the work, the module reads quota state on the tenant plane with the user's `SecurityContext`, for
   the metrics the work consumes.
2. The system returns the state of the quotas that the backend applies to the user's mapped subject on those metrics.
3. The module starts the work, or tells the user that no capacity remains.

**Postconditions**:
- Nothing changes.

**Alternative Flows**:
- **Principal not mapped**: the page is empty.

### Charge a User from a Background Job

- [ ] `p2` - **ID**: `cpt-cf-quota-resolver-usecase-job-debit`

**Actor**: `cpt-cf-quota-resolver-actor-consuming-module`

**Preconditions**:
- The job knows the user's principal type, id, and tenant.

**Main Flow**:
1. Before the work, the job debits on the platform plane with the subject in the principal form.
2. The system maps the principal and delegates.

**Postconditions**:
- The quotas that the backend applies to the user are charged.

**Alternative Flows**:
- **Denied**: the job does not do the work.

### Hold and Return Allocated Capacity

- [ ] `p2` - **ID**: `cpt-cf-quota-resolver-usecase-allocation-lifecycle`

**Actor**: `cpt-cf-quota-resolver-actor-consuming-module`

**Preconditions**:
- Allocation quotas exist on the metrics that a resource needs, configured in the backend outside the resolver.

**Main Flow**:
1. To create a resource, the module acquires a lease on the platform plane for the amount the resource needs.
2. It creates the resource and commits the lease. The commit's idempotency key names the charge, for example the
   resource id.
3. To grow the resource, it charges the added amount the same way, under a new key.
4. To reduce the resource, it returns the removed amount from one or more charges of the resource.
5. To delete the resource, it returns what remains of every charge of the resource.

**Postconditions**:
- The allocation quotas hold exactly the capacity of the existing resources.

**Alternative Flows**:
- **Denied**: the module does not create or grow the resource; nothing is charged.
- **Creation fails**: the module releases the lease, or the lease expires.

## 9. Acceptance Criteria

- [ ] A module charges the user of a request on the platform plane, with the subject in the principal form, without
naming a quota subject type.
- [ ] A user reads their own quota state through the edge; the tenant plane has no debit or lease operation.
- [ ] A principal that is not mapped is denied a debit or a lease acquisition and gets an empty quota state page,
without a backend call.
- [ ] With no plugin or an unavailable backend, no operation returns an allowed decision or a lease.
- [ ] A return of a charge makes the returned amount available again on the allocation and consumption quotas that the
charge reached, also after the backend's window for repeated calls; a return above what remains of the charge, or on a
rate-limiting quota, fails and changes nothing.

## 10. Dependencies

| Dependency            | Description                                                                     | Criticality |
|-----------------------|---------------------------------------------------------------------------------|-------------|
| types-registry        | Plugin discovery; ancestors of principal types for the mapping                  | p1          |
| Quota backend plugin  | Quotas of each kind, counters, leases, idempotency, and evaluation              | p1          |
| AuthN and api-gateway | `SecurityContext` and `PlatformSecurityContext` of the caller; edge publication | p1          |

## 11. Assumptions

* Principal types are GTS types that derive from a platform principal base, so their ancestors resolve in
  types-registry (issue #5446).
* A platform-plane caller is trusted to name the tenant and the subjects, as for other platform-plane resolvers.

## 12. Risks

| Risk                                                   | Impact                                    | Mitigation                                                                                                  |
|--------------------------------------------------------|-------------------------------------------|-------------------------------------------------------------------------------------------------------------|
| A platform service charges the wrong subject           | Wrong quota charged                       | Platform plane only for platform services; the principal of a user request comes from its `SecurityContext` |
| The mapping points to a type the metric does not admit | Every debit for that principal type fails | The backend rejects it as an invalid request; the resolver logs the misconfiguration                        |
| A resource is deleted without a return                 | Its allocation capacity stays held        | The module reconciles its resources; the operator corrects the quota through the backend's own API          |

## 13. Open Questions

None.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
