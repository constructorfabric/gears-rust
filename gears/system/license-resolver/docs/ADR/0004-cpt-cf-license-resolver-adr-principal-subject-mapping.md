---
status: proposed
date: 2026-11-11
description: "Why the license resolver, from its own configuration, decides which licensing subject type an authenticated principal is checked as."
---

# ADR-0004: The Resolver Maps Principals to Subject Contract Types

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Mapping in the resolver configuration](#a-mapping-in-the-resolver-configuration)
  - [(b) Platform principal projector](#b-platform-principal-projector)
  - [(c) Mapping in every caller](#c-mapping-in-every-caller)
  - [(d) Trait on the Subject contract type](#d-trait-on-the-subject-contract-type)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-license-resolver-adr-principal-subject-mapping`

## Context and Problem Statement

A **principal** is whoever authenticated: a person, a service account, a system actor. AuthN puts its kind, the
**principal type**, into `SecurityContext.subject_type` and its UUID into `subject_id`. Principal types form a GTS
hierarchy shared by the platform (for example `gts.cf.core.identity.principal.v1~cf.core.identity.user.v1~`), and
operators derive their own types from it.

The Subject of a check is an instance of a Subject contract type derived from `gts.cf.core.lic.subj.v1~`. These are
different hierarchies: a GTS type has one parent, and the operator decides which principals take part in licensing and
under which Subject contract type. The most common check is about the principal itself, but the calling module does
not know the operator's Subject contract types. Who maps a principal type to a Subject contract type, and where is the
mapping stored?

## Decision Drivers

* The mapping is operator data: the same module runs under different operators with different Subject contract types.
* The mapping must be stated in one place, not repeated in every calling module.
* An operator subtype of a principal type must be covered by the mapping of its ancestor without a separate entry.
* Backend plugins must keep receiving Subject contract types only.
* A principal that the operator did not map must not be licensed.

## Considered Options

* (a) The resolver maps principal types to Subject contract types from its own configuration; a check names its
  subject either as a Subject contract object or as a principal.
* (b) A platform projector: mapping instances registered in types-registry, resolved by a shared library in each caller.
* (c) Each calling module maps principal types from its own configuration.
* (d) A trait on each Subject contract type that lists the principal types it represents.

## Decision Outcome

Chosen option: (a), because the resolver is the single owner of licensing subjects, so one configuration serves every
caller, and the plugins see no change. Option (b) was rejected at the current stage: the mapping stays with the owner
of the domain subject.

The public check accepts the subject in one of two forms:

* **license subject**: a Subject contract object (`type`, optional `id`, `metadata`), as before;
* **principal**: a principal type and a principal id. The resolver replaces it with a Subject contract object of the
  mapped type, the same id and empty `metadata`, and then validates and delegates as for a license subject.

A second method takes the caller's `SecurityContext` and the Resource only. It builds the principal from the context
(`subject_type`, `subject_id`) and the tenant scope from `subject_tenant_id`, so the request has no subject or context
fields that the caller could set differently.

Mapping lookup for a principal type `P`:

1. an entry with key `P` wins;
2. otherwise the resolver resolves `P` in types-registry and takes the first ancestor of `P`, nearest first, that has
   an entry;
3. otherwise, or when `P` is absent, malformed or not registered, the principal is **not mapped**.

A principal that is not mapped yields `LicenseDecision { granted: false }`; the request is well formed, the operator
only has not licensed that kind of principal. A registry failure during the lookup is `ServiceUnavailable`.

### Consequences

* The public request type differs from the plugin request type: the plugin keeps receiving a request whose subject is a
  Subject contract object. The public request shape changes incompatibly; there are no external callers yet.
* The mapped Subject contract type gets empty `metadata`. A type whose schema requires metadata fails validation with
  `InvalidRequest`, which surfaces the misconfiguration.
* A principal that is not mapped is denied without validating the Resource.
* A mapping change needs a configuration change and a restart.

### Confirmation

Tests for exact match, ancestor match, nearest ancestor wins, no mapping, absent principal type, unregistered
principal type, registry failure, a mapped type that requires metadata, and the self-check method.

## Pros and Cons of the Options

### (a) Mapping in the resolver configuration

* Good, because one configuration serves every caller and no caller knows the operator's types.
* Good, because plugins and stored grants see Subject contract types only.
* Bad, because another module that needs the same mapping (for example to bind a grant to a user) has to go through the
  resolver or repeat the configuration.

### (b) Platform principal projector

* Good, because the mapping is discoverable in types-registry.
* Bad, because it adds a platform mechanism that knows nothing about any domain and needs a snapshot and refresh in
  every caller.

### (c) Mapping in every caller

* Good, because the resolver stays unchanged.
* Bad, because the operator repeats the mapping in every calling module.

### (d) Trait on the Subject contract type

* Good, because the operator configures licensing in types-registry only.
* Bad, because the lookup goes from principal to Subject type, so the resolver has to scan and cache all Subject
  contract types to read the trait.

## More Information

Principal types and their hierarchy: issues #5446 and #5505. Until the platform principal types are registered, the
exact match (step 1) also covers legacy values such as `user`.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-license-resolver-fr-principal-subject` — the principal form of the subject and its mapping.
* `cpt-cf-license-resolver-fr-subject-identity` — a mapped principal becomes a Subject contract object.
* `cpt-cf-license-resolver-nfr-tenant-scoping` — the self-check method takes the tenant from the context.
