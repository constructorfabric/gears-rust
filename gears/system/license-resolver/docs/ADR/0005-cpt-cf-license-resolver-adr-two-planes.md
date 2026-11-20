---
description: "Why the license resolver offers a trusted platform plane with the subject and tenant as request data and a tenant plane, open to any authenticated caller, for the caller itself."
status: accepted
date: 2026-11-11
decision-makers: license-resolver maintainers
---

# ADR-0005: Platform and Tenant Planes

**ID**: `cpt-cf-license-resolver-adr-two-planes`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Two planes, principal as data on the platform plane](#a-two-planes-principal-as-data-on-the-platform-plane)
  - [(b) One plane, user context forwarded](#b-one-plane-user-context-forwarded)
  - [(c) Platform plane only](#c-platform-plane-only)
  - [(d) Tenant plane only, S2S token per tenant](#d-tenant-plane-only-s2s-token-per-tenant)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The platform authenticates a call on one of two planes
([ADR-0008](../../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md)): on the **tenant plane** the
call carries the caller's `SecurityContext`, built from its bearer token; on the **platform plane** it carries a
`PlatformSecurityContext`, built from the calling module's service credential, with no tenant. The **edge** is the
api-gateway that external clients reach. A platform-plane route accepts only a service credential, and external
clients hold none, so from outside only the tenant-plane routes are reachable. Who can call a route follows from its
context type, not from its visibility: `#[exposed]` ([ADR-0003](../../../../../docs/arch/toolkit-oop/ADR/0003-cpt-cf-adr-gateway-abstraction.md)) only lists a route for the api-gateway to forward
to a module that runs in its own process, and in a single-process deployment every route shares the api-gateway
listener.

Two kinds of callers check licenses. A platform service, a module that calls with its own service credential rather
than a user's token, checks any subject of any tenant it serves, for example
a tenant before a background job or the user of the request it handles. An end user, for example a UI that hides
unlicensed features, asks whether they themselves are licensed. Which plane carries which check, how does a module
check the user of its request, and which check is reachable from the edge?

## Decision Drivers

* A platform service names the tenant and the subject; it is trusted, as on the platform plane of other resolvers.
* A check on behalf of the caller must not let the caller name another subject or tenant.
* A user checks their own license without a policy grant.
* A check has one authentication model per plane, whoever its subject is.
* The operation context, inputs that describe the operation being checked beyond its subject and resource, cannot be
  built from a `SecurityContext`, so the caller passes it on both planes.

## Considered Options

* (a) Two planes: a platform plane whose request names the tenant and the subject, the user's principal passed as
  request data; a tenant plane for the caller itself, open to any authenticated caller.
* (b) One plane: the module forwards the user's `SecurityContext`, and the resolver takes the subject and tenant from
  it.
* (c) The platform plane only: an end user reaches a check through a module that proxies it.
* (d) The tenant plane only, for platform services too: a service gets a tenant `SecurityContext` from a token that the
  identity provider (IdP) issues for the service's own client id and secret (OAuth2 client credentials, S2S), and a
  policy check authorizes it, as ADR-0008 describes for tenant-scoped work.

## Decision Outcome

Chosen option: (a). Each plane is its own contract with its own base path, as in types-registry
(`PlatformTypesRegistryApi` under `/types-registry/v2/platform`, `TypesRegistryApi` under `/types-registry/v2`).

| Plane    | Contract                     | Path                                        | Subject and tenant    | Callers                  | Visibility |
|----------|------------------------------|---------------------------------------------|-----------------------|--------------------------|------------|
| Platform | `PlatformLicenseResolverApi` | `/license-resolver/v1/platform/is-licensed` | from the request      | platform services only   | internal   |
| Tenant   | `LicenseResolverApi`         | `/license-resolver/v1/is-licensed`          | from the context only | any authenticated caller | exposed    |

`PlatformLicenseResolverApi` takes `PlatformSecurityContext`; `LicenseResolverApi` takes `SecurityContext`, and its
request has no subject or tenant field.

A module that checks the user of its request calls the platform plane with its own platform credential. It takes the
user's principal type, id, and tenant from the incoming `SecurityContext` and passes them as request data: the
subject in the principal form, the tenant as a request field. The user's `SecurityContext` is neither forwarded to the
resolver nor synthesized by it. The resolver maps the principal as in
`cpt-cf-license-resolver-adr-principal-subject-mapping`.

On the tenant plane the subject is the caller's principal, mapped the same way, and the tenant is the caller's tenant.
The tenant plane requires authentication only, no policy permission: the caller learns only its own license status in
its own tenant.

Both requests carry the operation context, which the caller fills.

The resolver performs no policy check and passes no caller identity to the plugin. It trusts platform-plane callers
for the subject and tenant they provide.

This is an exception to the toolkit's two-plane rule (ADR-0008, "Which plane?"), which puts
operations within a tenant, service-to-service calls included, on the tenant plane with a policy check. The rule asks
whether an operation is within a tenant, not whether it changes data, so a read-only check falls under it too. The
toolkit has no way to authorize a trusted service that checks many tenants (option (d)): a tenant `SecurityContext`
binds one tenant, ADR-0008 forbids building one by hand for a target tenant, and the policy decision point does not
evaluate `PlatformSecurityContext`. The exception ends when the toolkit evaluates policies for platform-plane callers,
keyed on the calling service and set at platform level
([#6037](https://github.com/constructorfabric/gears-rust/issues/6037)). The resolver then adopts that check without
changing its contracts.

### Consequences

* A module checks the user of a request with the same call it uses for any other subject; the SDK offers a helper
  that builds the principal subject and the tenant from a `SecurityContext`.
* The resolver cannot check that a principal passed as data is the user of the module's request; it trusts the
  platform-plane caller, as for any other subject.
* A tenant-plane answer is a hint: a UI may hide a feature by it, but a module never accepts it as proof of a license.
  A module that gates an operation builds the check's resource and `metadata` from its own data, not from user input.
* Until the exception ends, any service with a service credential can learn whether any subject in any tenant is
  licensed. No component checks which service may do so, and the backend cannot record which service made a check.
* A backend that also has its own API gains a second entry into its grant data: the in-process plugin. The backend's
  own policy check covers only its API; the plugin is reachable only through the resolver and is not authorized.
* The path `/license-resolver/v1/is-licensed` was the platform route before; it now serves the tenant plane, and the
  OpenAPI change is breaking. There are no callers of the old route yet.

### Confirmation

OpenAPI: the `LicenseResolverApi` route has `x-toolkit-visibility: exposed`; the `PlatformLicenseResolverApi` route is
internal. Tests: a tenant-plane request cannot change the subject or the tenant; a principal on the platform plane is
checked as its mapped subject in the requested tenant.

## Pros and Cons of the Options

### (a) Two planes, principal as data on the platform plane

* Good, because a platform service checks any subject, a user included, with one call.
* Good, because the tenant plane is safe to open to any authenticated caller: its caller cannot name another subject
  or tenant.
* Good, because the planes match types-registry.
* Bad, because the resolver trusts the module to pass the right principal.
* Bad, because no component authorizes the calling service, which is an exception to the toolkit's two-plane rule
  (ADR-0008).

### (b) One plane, user context forwarded

* Good, because the resolver takes the principal from an authenticated context, not from request data.
* Bad, because a platform service without a user, or one checking a tenant or another subject, has no context to
  forward.
* Bad, because forwarding the user's bearer token makes every check depend on the edge credential reaching the
  resolver.

### (c) Platform plane only

* Good, because the resolver has one contract and no edge surface.
* Bad, because every UI that hides unlicensed features needs a module that proxies the check.

### (d) Tenant plane only, S2S token per tenant

* Good, because every check passes a policy check, as ADR-0008 asks for tenant-scoped work.
* Bad, because a client-credentials request names no tenant: the token yields the context of the one tenant that the
  IdP binds to the client. A service that checks many tenants needs an IdP client per tenant or a token exchange per
  tenant, which the platform does not have, and pays an IdP round trip per tenant.
* Bad, because building a tenant `SecurityContext` by hand instead is what ADR-0008 forbids as the seam where
  cross-tenant leaks start.

## More Information

Principal types and their hierarchy: issue #5446.

Authorization of platform-plane callers, the exit condition of the exception:
[#6037](https://github.com/constructorfabric/gears-rust/issues/6037).

Two-plane authentication: [ADR-0008](../../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-license-resolver-fr-is-licensed-check` — the check on each plane.
* `cpt-cf-license-resolver-fr-principal-subject` — the principal as request data on the platform plane; the caller's
  principal on the tenant plane.
* `cpt-cf-license-resolver-nfr-tenant-scoping` — the tenant from the request on the platform plane, from the context on
  the tenant plane.
