---
status: accepted
date: 2026-11-18
---

# Quota Resolver Plugin Path Without a Policy Check

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Trusted admission mode behind the plugin](#a-trusted-admission-mode-behind-the-plugin)
  - [(b) Plugin calls the QE API with its own credential](#b-plugin-calls-the-qe-api-with-its-own-credential)
  - [(c) A separate copy of the operations](#c-a-separate-copy-of-the-operations)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-resolver-plugin-path`

## Context and Problem Statement

The quota resolver delegates debit, lease, return, and quota state to a backend plugin in process. Its plugin contract
carries the fixed tenant and quota subjects, and the caller's platform identity for the record only; the resolver
runs no policy check
([ADR-0003](../../../quota-resolver/docs/ADR/0003-cpt-cf-quota-resolver-adr-two-planes.md)). QE admits every operation
through the policy decision point (PDP) and binds the returned access scope to storage. How does the QE plugin run QE
operations without a caller to authorize?

## Decision Drivers

* One implementation of each operation: replay, evaluation, and storage behave the same on both paths.
* QE's own API keeps its PDP check.
* One operation never spans tenants; the caller names the tenant.
* The path without a PDP check is reachable only through the plugin.

## Considered Options

* (a) A trusted admission mode that only the plugin uses.
* (b) The plugin calls QE's own in-process API with a `SecurityContext` of its own and a PDP grant.
* (c) The plugin calls a separate copy of the operations without admission.

## Decision Outcome

Chosen option: (a).

* Admission has two modes. **PDP mode** serves QE's REST and in-process API, as today. **Trusted mode** checks the
  target's shape, calls no PDP, and returns an access scope that names the request's tenant only. Policy management in
  trusted mode is always refused.
* The gear builds the operations twice over the same storage, engines, and caches: once over PDP mode for its API,
  once over trusted mode for the plugin. The trusted operations are reachable only through the plugin client.
* The plugin implements `QuotaResolverPluginClient` and registers itself as a quota resolver plugin instance in
  ClientHub. It converts resolver requests to QE requests and QE results to resolver results.
* The plugin path has no `SecurityContext`: the plugin receives none and builds none. Trusted admission takes the
  tenant from the request.
* Admission in both modes returns the access scope and the **actor**: the identity that the operation log and the
  events name as the cause of the operation. In PDP mode the actor is the authenticated caller. In trusted mode it is
  the platform identity of the resolver's caller, which the plugin request carries; QE does not authorize by it. The
  operations take the actor and the access scope, and none takes a `SecurityContext`; that is what lets one
  implementation serve both modes.

### Consequences

* This is an exception to `cpt-cf-quota-enforcement-fr-authorization` for the plugin path only. When the resolver
  authorizes its platform-plane callers, calls that come through the resolver are authorized there; the plugin client
  itself stays without a policy check. Until then no component checks which module may charge, return, or read the
  quotas of any tenant on this path. This is accepted: it is the exception that the resolver records for its
  platform plane (`cpt-cf-quota-resolver-adr-two-planes`), and the plugin path adds no access to it.
* The trust boundary of the plugin path is the process. Any module in the process can get the plugin client from
  ClientHub and is trusted like a caller of the resolver's platform plane: it can act on any tenant it names. This
  adds no access, because the same module can call the resolver's platform plane, which reaches the same operations.
  The platform does not treat the process as a security boundary, so QE adds no check of its own here.
* The actor that the operation log records on this path is not an audited identity. It names the calling service
  only when the resolver's caller runs in another process. In one process the platform gives a caller no identity of
  its own, and the log then names no module
  ([gears-rust#6036](https://github.com/constructorfabric/gears-rust/issues/6036)). QE records whatever the
  resolver passes and needs no change when the platform closes that gap.
* Storage binds every query of an operation to the tenant-only scope, so one operation never spans tenants.
* Debit, lease, rollback, and snapshot through QE's own API still need a PDP grant. New consumers use the resolver.

### Confirmation

Tests: the plugin path makes no PDP call and reaches only the request's tenant; QE's API still calls the PDP; policy
management in trusted mode is refused.

## Pros and Cons of the Options

### (a) Trusted admission mode behind the plugin

* Good, because every operation keeps one implementation.
* Good, because the bypass is visible in one place: the admission mode.
* Bad, because any module in the process can call the plugin client.

### (b) Plugin calls the QE API with its own credential

* Good, because QE needs no second mode.
* Bad, because the plugin has no `SecurityContext` to pass. The resolver receives a platform security context, which
  the PDP does not evaluate, and a gear must not build a `SecurityContext` by hand: only authentication produces one
  ([two-plane authentication](../../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md)).
* Bad, because the allowed way, a token that QE gets for itself from the identity provider on every replica, makes
  the PDP authorize QE itself, not the module that charges: one more call per debit that always gives one answer.

### (c) A separate copy of the operations

* Good, because the API path does not change.
* Bad, because two copies of replay and evaluation drift apart.

## More Information

Quota resolver plugin contract: [DESIGN](../../../quota-resolver/docs/DESIGN.md), §3.2 Backend Plugin.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-quota-enforcement-fr-authorization`
* `cpt-cf-quota-enforcement-fr-tenant-isolation`
* `cpt-cf-quota-enforcement-nfr-authorization`
* `cpt-cf-quota-enforcement-principle-pdp-authorized-attribution`
