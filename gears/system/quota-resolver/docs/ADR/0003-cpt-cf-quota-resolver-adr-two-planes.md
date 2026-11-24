---
description: "Why the quota resolver charges only on a trusted platform plane with the principal passed as data and offers a read-only tenant plane at the edge."
status: proposed
date: 2026-11-11
---

# ADR-0003: Platform and Tenant Planes

**ID**: `cpt-cf-quota-resolver-adr-two-planes`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Platform-plane writes, principal as data](#a-platform-plane-writes-principal-as-data)
  - [(b) Writes on both planes, user context forwarded](#b-writes-on-both-planes-user-context-forwarded)
  - [(c) Caller security context in the plugin contract](#c-caller-security-context-in-the-plugin-contract)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Two kinds of callers charge and read quotas. A platform service acts for any subject of any tenant it serves, for
example a background job. A module that serves a user request charges that user, and the user wants to see their own
quotas. A backend that authorizes every call through the policy decision point against the calling service would need
a policy grant for each module and each kind of subject it charges. Which plane carries which operation, how does
a module charge the user of a request, and which operations are reachable from the edge?

## Decision Drivers

* A platform service names the tenant and the subjects; it is trusted, as on the platform plane of other resolvers.
* A call on behalf of the caller must not let the caller name another subject or tenant.
* A user reads their own quotas without a policy grant.
* Only the module that does the work is authorized to charge, because it decides the metric and the amount; the user
  whose work is charged is not.
* A charge has one authentication model, whoever its subject is.

## Considered Options

* (a) Writes on the platform plane only, the user's principal passed as request data; the tenant plane offers quota
  state only and is published at the edge.
* (b) Writes on both planes: on the tenant plane the module forwards the user's `SecurityContext`, and the
  tenant-plane write routes are internal.
* (c) As (a), but the resolver passes caller security context through the plugin contract and prescribes a policy
  check in the backend.

## Decision Outcome

Chosen option: (a). Each plane is its own contract with its own base path, as in types-registry
(`PlatformTypesRegistryApi` under `/types-registry/v2/platform`, `TypesRegistryApi` under `/types-registry/v2`).

| Plane    | Contract                   | Base path                     | Operations                                          | Subject and tenant    | Edge          |
|----------|----------------------------|-------------------------------|-----------------------------------------------------|-----------------------|---------------|
| Platform | `PlatformQuotaResolverApi` | `/quota-resolver/v1/platform` | debit, acquire, commit, release lease; return; quota state | from the request      | not published |
| Tenant   | `QuotaResolverApi`         | `/quota-resolver/v1`          | quota state                                         | from the context only | published     |

`PlatformQuotaResolverApi` takes `PlatformSecurityContext`; `QuotaResolverApi` takes `SecurityContext`, and its
request has no subject or tenant field.

A module that serves a user request and charges that user calls the platform plane with its own platform credential.
It takes the user's principal type, id, and tenant from the incoming `SecurityContext` and passes them as request
data: the subject in the principal form, the tenant as a request field. The user's `SecurityContext` is
neither forwarded to the resolver nor synthesized by it. The resolver maps the principal as in
`cpt-cf-quota-resolver-adr-principal-subject-mapping`.

On the tenant plane the subject is the caller's principal, mapped the same way, and the tenant is the caller's tenant.

The resolver performs no policy check. It passes the platform identity that it receives from the caller to the plugin, only
for the backend's record of operations. It trusts platform-plane callers
for the target data they provide and trusts the backend to decide which quotas apply to the fixed tenant and subjects.

This is an exception to the toolkit's two-plane rule (`cpt-cf-adr-two-plane-auth`, "Which plane?"), which puts
operations within a tenant, writes included, on the tenant plane with a policy check. The toolkit has no way to
authorize a trusted service that charges many tenants: a tenant `SecurityContext` binds one tenant, and the policy
decision point does not evaluate `PlatformSecurityContext`. The exception ends when the toolkit evaluates policies for
platform-plane callers, keyed on the calling service and set at platform level, so the charged tenant cannot deny its
own charge. The resolver then adopts that check without changing its contracts.

### Consequences

* Only platform services can charge. A module charges the user of a request with the same call it uses for any other
  subject; the SDK offers a helper that builds the principal subject and the tenant from a `SecurityContext`.
* The resolver cannot check that a principal passed as data is the user of the module's request; it trusts the
  platform-plane caller, as for any other subject.
* Until the exception ends, any service with a platform credential can charge, return, or read quotas of any subject
  in any tenant. No component checks which service may do so.
* The backend records the platform identity with every charge, but this is not an audit log. The identity names the
  calling service only when the caller runs in another process. In one process, which is the default deployment, the
  platform gives a caller no identity of its own: the resolver receives a marker that names no module
  ([gears-rust#6036](https://github.com/constructorfabric/gears-rust/issues/6036)). The record names the charging
  module in every deployment when the platform closes that gap; the resolver's contracts do not change then.
* A backend that also has its own API gains a second entry into its operations: the in-process plugin. The backend's
  own policy check covers only its API; the plugin is reachable by any module of the same process and is not
  authorized. This adds no access: a module of the process can already call the resolver's platform plane, which
  reaches the same operations. The platform does not treat the process as a security boundary.
* An authenticated user receives the state of the quotas that the backend applies to the mapped subject and requested
  metrics, including quotas of the tenant. The resolver returns the backend's page unchanged.

### Confirmation

OpenAPI: `QuotaResolverApi` has only the quota state route, with `x-toolkit-visibility: exposed`; every
`PlatformQuotaResolverApi` route is internal. Tests: a tenant-plane request cannot change the subject or the tenant;
the resolver returns the plugin's quota-state page unchanged.

## Pros and Cons of the Options

### (a) Platform-plane writes, principal as data

* Good, because a user cannot charge, by any route.
* Good, because every charge is authenticated the same way, by the platform credential of the calling module.
* Good, because the tenant plane is read-only and safe to publish at the edge.
* Bad, because the resolver trusts the module to pass the right principal.
* Bad, because no component authorizes the calling service, which is an exception to `cpt-cf-adr-two-plane-auth`.

### (b) Writes on both planes, user context forwarded

* Good, because the resolver takes the principal from an authenticated context, not from request data.
* Bad, because a charge has two authentication models, and a user's bearer token would reach write routes that only
  their being internal keeps away from the user.
* Bad, because the contract doubles: every write exists on both planes.

### (c) Caller security context in the plugin contract

* Good, because the resolver can prescribe one explicit policy-check mechanism for every backend.
* Bad, because the plugin contract would carry the caller's `SecurityContext` and leak backend authorization concerns
  into the resolver.
* Bad, because every module could require policy grants for every user it serves.

## More Information

Principal types and their hierarchy: issue #5446.

Two-plane authentication: [ADR-0008](../../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-quota-resolver-fr-platform-plane` — every write is on the platform plane; the principal is request data.
* `cpt-cf-quota-resolver-fr-tenant-plane` — quota state only; subject and tenant come from the context; published at
  the edge.
* `cpt-cf-quota-resolver-nfr-tenant-scoping` — the tenant plane takes the tenant from the context.
