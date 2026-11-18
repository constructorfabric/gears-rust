---
description: "Why the quota resolver owns no quota state and delegates every operation to a backend plugin selected through types-registry."
status: proposed
date: 2026-11-11
---

# ADR-0001: The Resolver Delegates Quota Operations to a Backend Plugin

**ID**: `cpt-cf-quota-resolver-adr-plugin-delegation`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Separate resolver with a plugin contract](#a-separate-resolver-with-a-plugin-contract)
  - [(b) Extend the backend API](#b-extend-the-backend-api)
  - [(c) Client library in every module](#c-client-library-in-every-module)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Consuming modules need one entry point to debit, lease, and read quotas for principals and for other subjects. The
quota state (quotas, counters, leases, idempotency, evaluation) belongs to a quota backend. Where do the entry point,
principal mapping, and the tenant-plane operations live, and how do they reach the quota state?

## Decision Drivers

* Consuming modules must not depend on one quota backend.
* Quota state must have one owner; no second store of counters or leases.
* The backend's own API keeps its management surface and its authorization model.
* The pattern must match the other platform resolvers, so that operators and module authors meet one model.

## Considered Options

* (a) A separate resolver gear with a plugin contract that a quota backend implements.
* (b) Add principal mapping and tenant-plane operations to the quota backend's own API.
* (c) A client library that each module links to map principals and call the backend.

## Decision Outcome

Chosen option: (a), because it keeps one quota contract for consumers that does not name the backend, leaves the
backend's own API and authorization model as they are, and follows the license-resolver and authz-resolver pattern.

The resolver selects the plugin through types-registry by vendor and priority, as license-resolver does. The plugin
receives the tenant and quota subjects only; principal mapping happens before delegation.

### Consequences

* The SDK crate defines the two public contracts, the plugin contract, their data types, the quota subject base and
  tenant types, and the plugin GTS type.
* A backend registers a plugin in its process. The plugin converts the resolver requests to backend operations and
  backend results to resolver results.
* The operations exposed are the subset consumers need: debit, lease, return, and quota state. The backend's other
  operations, such as quota and policy management, stay in its own API and can be added to the resolver later without
  breaking it.
* With no plugin, every operation fails with an error; nothing is allowed by default.

### Confirmation

Code review of the dependency graph: consuming modules depend on the resolver SDK only. Tests for plugin selection and
for every operation with no plugin.

## Pros and Cons of the Options

### (a) Separate resolver with a plugin contract

* Good, because consumers depend on a small contract that does not change with the backend.
* Good, because the backend's own API and its authorization model stay unchanged.
* Bad, because the data types of the operations are defined twice, in the resolver SDK and in the backend, with a
  conversion in the plugin.

### (b) Extend the backend API

* Good, because there is no new gear and no conversion.
* Bad, because the backend would serve end users at the edge next to its management surface.
* Bad, because consumers stay tied to one backend.

### (c) Client library in every module

* Good, because there is no extra hop.
* Bad, because the mapping configuration repeats in every module, and the tenant-plane quota state read has no service
  to live in.

## More Information

License-resolver and authz-resolver use the same plugin selection.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-quota-resolver-fr-plugin-delegation` — the resolver delegates to a selected plugin and stores nothing.
* `cpt-cf-quota-resolver-nfr-fail-closed` — no plugin ends in an error.
* `cpt-cf-quota-resolver-contract-backend-plugin` — the plugin contract carries quota subjects only.
