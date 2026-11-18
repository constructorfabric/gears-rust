---
description: "Why the quota resolver, from its own configuration, maps a principal type to a quota subject type."
status: proposed
date: 2026-11-11
---

# ADR-0002: The Resolver Maps Principal Types to Quota Subject Types

**ID**: `cpt-cf-quota-resolver-adr-principal-subject-mapping`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Mapping in the resolver configuration](#a-mapping-in-the-resolver-configuration)
  - [(b) Mapping in every caller](#b-mapping-in-every-caller)
  - [(c) Platform principal projector](#c-platform-principal-projector)
  - [(d) Map to backend scopes](#d-map-to-backend-scopes)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

A **principal** is whoever authenticated. AuthN puts its type, a GTS type id, into `SecurityContext.subject_type` and
its UUID into `subject_id`. Principal types form a GTS hierarchy, and operators derive their own types from it.

A **quota subject** is a quota subject type and an opaque id. A quota subject type is a GTS type derived from the base
`gts.cf.core.quota.subj.v1~` that the resolver SDK registers. Principal types and quota subject types are different
hierarchies: a GTS type has one parent, and the operator decides which principals take part in quotas and under which
quota subject type. The most common quota subject is the principal itself, but the calling module does not know the
operator's quota subject types. Who maps a principal type to a quota subject type, and where is the mapping stored?

## Decision Drivers

* The mapping is operator data: the same module runs under operators with different quota subject types.
* The mapping is stated once, not in every calling module.
* An operator subtype of a principal type is covered by the entry of its ancestor.
* The plugin receives quota subjects only.
* A principal that the operator did not map is not charged and gets no lease.
* The principal form does not depend on the backend, so other resolvers can accept the same form.

## Considered Options

* (a) The resolver maps principal types to quota subject types from its own configuration.
* (b) Each calling module maps principal types from its own configuration.
* (c) A platform principal projector: mapping instances in types-registry, resolved by a shared library in each
  caller.
* (d) The resolver maps principal types to backend-specific scopes, which the backend turns into its own subject types.

## Decision Outcome

Chosen option: (a), because the resolver is the single entry point for quota operations, so one configuration serves
every caller, and the plugin sees no principals.

A subject in a platform request has one of two forms, tagged by `kind`:

* **quota**: a quota subject `{type, id}`;
* **principal**: a principal type and a principal id. The resolver replaces it with a quota subject of the mapped type
  and the same id.

Mapping lookup for a principal type `P`, from the configuration `principal_subject_types`:

1. an entry with key `P` wins;
2. otherwise the resolver resolves `P` in types-registry and takes the nearest ancestor of `P` that has an entry;
3. otherwise, or when `P` is absent, malformed, or not registered, the principal is **not mapped**.

A principal with a nil id, or a tenant-plane caller with a nil tenant, is not mapped either. A principal that is not
mapped makes a debit or a lease acquisition denied with the reason `principal_not_mapped`, and a quota state read
empty; the plugin is not called. A registry failure during the lookup is `ServiceUnavailable`.

### Consequences

* A mapping change needs a configuration change and a restart.
* A mapping change takes effect on each replica as it restarts, so a call and its retry can be mapped differently. A
  retry after a change of the type is a new call for the backend and charges the new quota subject once more; a retry
  after the removal of the entry is not mapped. An operator therefore adds entries for new principal types freely, and
  changes or removes an existing entry as a migration of its quotas, at a time with no charges of that type in flight.
* The backend checks that the metric admits the mapped quota subject type. A type the metric does not admit fails in
  the backend as an invalid request; the resolver logs it as a likely misconfiguration, throttled.
* The mapping produces only the principal's own subject. Quotas of the tenant are the backend's concern, given the
  tenant of the request; the SDK registers the tenant subject type so that a tenant quota can be named in a quota
  state result.
* A backend whose own model is not built on quota subject types translates them in its plugin.

### Confirmation

Tests for exact match, nearest ancestor, no entry, absent or unregistered principal type, nil id, registry failure,
and a mapped type the backend rejects.

## Pros and Cons of the Options

### (a) Mapping in the resolver configuration

* Good, because one configuration serves every caller.
* Good, because the plugin and the stored quotas see quota subject types only.
* Good, because the public subject model does not depend on the backend.
* Bad, because another module that needs the same mapping, for example to create a quota for a user, repeats the
  configuration.

### (b) Mapping in every caller

* Good, because the resolver stays a pure pass-through.
* Bad, because the operator repeats the mapping in every calling module.

### (c) Platform principal projector

* Good, because the mapping is discoverable in types-registry for every domain.
* Bad, because it adds a platform mechanism with a snapshot and a refresh task in every caller, before any second
  domain needs it.

### (d) Map to backend scopes

* Good, because a backend that keys quotas by scope needs no translation in its plugin.
* Bad, because the public contract then carries one backend's subject model.
* Bad, because the principal form then depends on the backend, so other resolvers cannot accept the same form.

## More Information

Principal types and their hierarchy: issue #5446.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-quota-resolver-fr-principal-mapping` — the lookup and the not-mapped outcome.
* `cpt-cf-quota-resolver-fr-subject-forms` — the principal form becomes a quota subject.
* `cpt-cf-quota-resolver-nfr-fail-closed` — a principal that is not mapped is never charged or given a lease.
