---
description: "Why the license resolver, from its own configuration, maps a principal type to a Subject contract type."
status: proposed
date: 2026-11-11
---

# ADR-0004: The Resolver Maps Principal Types to Subject Contract Types

**ID**: `cpt-cf-license-resolver-adr-principal-subject-mapping`

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
  - [(d) Trait on the Subject contract type](#d-trait-on-the-subject-contract-type)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

A **principal** is whoever authenticated. AuthN puts its type, a GTS type id, into `SecurityContext.subject_type` and
its UUID into `subject_id`. Principal types form a GTS hierarchy, and operators derive their own types from it.

The Subject of a check is an instance of a Subject contract type derived from `gts.cf.core.lic.subj.v1~`. Principal
types and Subject contract types are different hierarchies: a GTS type has one parent, and the operator decides which
principals take part in licensing and under which Subject contract type. The most common check is about the principal
itself, but the calling module does not know the operator's Subject contract types. Who maps a principal type to a
Subject contract type, and where is the mapping stored?

## Decision Drivers

* The mapping is operator data: the same module runs under operators with different Subject contract types.
* The mapping is stated once, not in every calling module.
* An operator subtype of a principal type is covered by the entry of its ancestor.
* The plugin receives Subject contract objects only.
* A principal that the operator did not map is not licensed.

## Considered Options

* (a) The resolver maps principal types to Subject contract types from its own configuration.
* (b) Each calling module maps principal types from its own configuration.
* (c) A platform principal projector: mapping instances in types-registry, resolved by a shared library in each
  caller.
* (d) A trait on each Subject contract type that lists the principal types it represents.

## Decision Outcome

Chosen option: (a), because the resolver is the single entry point for license checks, so one configuration serves
every caller and the plugin sees no principals.

A subject in a platform request, the request that a module authenticated by its own service credential sends on the
platform plane (`cpt-cf-license-resolver-adr-two-planes`), has one of two forms, tagged by `kind`:

* **license**: a Subject contract object `{type, id, metadata}`;
* **principal**: a principal type and a principal id. The resolver replaces it with a Subject contract object of the
  mapped type, the same id and empty `metadata`, then validates and delegates it as any other subject.

Mapping lookup for a principal type `P`, from the configuration `principal_subject_types`:

1. an entry with key `P` wins, without a registry lookup;
2. otherwise the resolver resolves `P` in types-registry and takes the nearest ancestor of `P` that has an entry;
3. otherwise the principal is **not mapped**: `P` is absent, or `P` has no entry and is malformed, not registered,
   or has no ancestor with an entry.

An exact entry (step 1) applies to any `P`, registered or not; this is how legacy values such as `user` are mapped.

A principal with a nil id or a nil tenant is not mapped either. A principal that is not mapped yields
`LicenseDecision { granted: false }` with the reason `principal_not_mapped`; the Resource is not validated and the
plugin is not called. A registry failure during the lookup is `ServiceUnavailable`.

### Consequences

* A mapping change needs a configuration change and a restart.
* A mapping change takes effect on each replica as it restarts, so during the restart two checks of the same principal
  can be mapped differently. After a change of the type, the backend matches the principal as the new Subject
  contract type, so grants bound to the old type can stop applying; after the removal of the entry, the principal is not
  mapped and not licensed. An operator therefore adds entries for new principal types freely, and changes or removes an
  existing entry as a migration of its grants: the grants for the new type exist before the change, and the grants for
  the old type are revoked after it.
* The mapped Subject contract type gets empty `metadata`. A type whose schema requires metadata fails validation with
  `InvalidRequest`; the resolver logs it as a likely misconfiguration, throttled.
* The public subject differs from the plugin subject: the plugin keeps receiving a Subject contract object.

### Confirmation

Tests for exact match, nearest ancestor, no entry, absent or unregistered principal type, nil id or tenant, registry
failure, and a mapped type that requires metadata.

## Pros and Cons of the Options

### (a) Mapping in the resolver configuration

* Good, because one configuration serves every caller.
* Good, because the plugin and the stored grants see Subject contract types only.
* Bad, because another module that needs the same mapping, for example to bind a grant to a user, repeats the
  configuration.

### (b) Mapping in every caller

* Good, because the resolver stays unchanged.
* Bad, because the operator repeats the mapping in every calling module.

### (c) Platform principal projector

* Good, because the mapping is discoverable in types-registry for every domain.
* Bad, because it adds a platform mechanism with a snapshot and a refresh task in every caller, before any second
  domain needs it.

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
* `cpt-cf-license-resolver-nfr-fail-closed` — a principal that is not mapped is never licensed.
