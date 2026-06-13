---
status: accepted
date: 2026-06-11
---

# ADR-0003: Typed, Registry-Observable Licensing Contracts Validated by the Engine

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Opaque metadata, backend-owned validation](#a-opaque-metadata-backend-owned-validation)
  - [(b) Typed derived GTS contracts validated by the engine](#b-typed-derived-gts-contracts-validated-by-the-engine)
  - [(c) Registered schemas, documentation only](#c-registered-schemas-documentation-only)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-license-resolver-adr-typed-licensing-contracts`

## Context and Problem Statement

License enforcement and license decision happen at different levels. The Gear calling `is_licensed` knows *where* a
check belongs, but only the platform vendor — who composes a concrete platform out of Gears and a licensing backend —
knows *what* must be licensed, and different platform vendors apply different licensing models to the same Gear (one
licenses every LLM model individually, another does not care about models at all). This raises the question: who
defines which Subject/Resource pairs and which of their properties are passed into a license check?

With an opaque `metadata` bag, the check is *callable* but not *governable*: the keys a plugin can rely on exist only
in Gear source code, a platform vendor has no inventory of checks, no schema of available properties, and no
change-control point when a Gear silently adds, renames, or removes a key. How should the check payload represent
Subject, Resource, and their properties so that platform vendors can discover, review, and rely on what Gears pass into
licensing?

## Decision Drivers

* Platform vendors must be able to enumerate the platform's licensing surface (checks, Subject/Resource pairs,
  available properties) from the types registry alone, without reading Gear source code.
* Licensing contracts need their own review and version lifecycle, decoupled from Gear domain types.
* The plugin must be able to trust the shape of what it receives; validation must be uniform, not re-implemented (or
  skipped) per backend.
* Gears must stay free to expose arbitrary, Gear-specific properties, and platform vendors must be free to ignore what
  they do not need.
* Fail-closed semantics: a malformed check must be an error, never a silent evaluation.

## Considered Options

* (a) **Opaque metadata, backend-owned validation**: Subject/Resource as plain identity fields, `metadata` as an
  uninterpreted JSON bag forwarded to the plugin, which owns all validation
* (b) **Typed derived GTS contracts validated by the engine**: licensing base types
  `gts.cf.core.lic.subj.v1~` / `gts.cf.core.lic.res.v1~`; Gears derive concrete Subject/Resource types with
  metadata schemas and an admitted-subjects trait; the engine validates every request against the registered contracts
  before delegation
* (c) **Registered schemas for documentation only**: the request shape of (a), plus registered metadata schemas for
  discoverability — without engine validation

## Decision Outcome

Chosen option: **(b)**. Every Subject and Resource shape used in a check is a registered, versioned GTS type derived
from the licensing base types — the Gear's published licensing contract. The engine validates each request against the
registered contracts before delegating to the plugin and rejects non-conforming requests as fail-closed errors,
distinct from a not-granted decision.

Only (b) makes the licensing surface a real contract: discoverable through the registry (query everything derived from
the base types), reviewable in isolation from other registered types, versioned independently of domain types, and
*enforced*. Option (a) was rejected because it makes the check callable but not governable: every backend validates (or
doesn't) differently, and a Gear can silently change the payload with no review point. Option (c) was rejected because
unvalidated contracts are not contracts — the registered schema and actual behavior drift apart, buying (b)'s
documentation upside while keeping (a)'s integrity downside. Schema-validation performance is not a differentiator:
schemas are resolved from the registry and cached, negligible against the check-latency budget.

### Consequences

* The licensing Gear owns the base types under `gts.cf.core.lic.*`; every licensing-aware Gear registers derived
  Subject/Resource types — its published licensing contract — before it can check.
* Gears pay a projection cost: licensing-relevant fields are copied from domain objects into the licensing types. This
  is the same deliberate boundary cost as DTO and persistence mappings, and it is what decouples domain evolution from
  contract evolution.
* The gateway gains a validation pipeline (structural + subject-compatibility) ahead of plugin delegation; validation
  failures are errors, never not-granted decisions. The engine validates *shape and compatibility* only — what the
  properties mean and what is licensable remain the backend's concern.
* `metadata` stays semantically opaque to the engine but becomes schematized: new properties arrive as optional,
  non-breaking schema fields rather than undocumented keys.
* Compatibility rule: adding an optional property is non-breaking (consumers ignore unknown fields); removing or
  renaming a property, changing its type, or narrowing the admitted subjects requires a new contract version.
* The admitted-subjects constraint is expressed as a GTS trait (`x-gts-traits`) on the derived Resource type — the
  mechanism already used by resource-group's `allowed_parent_types`.

### Confirmation

Confirmed by design and code review (base + derived licensing types registered in the types registry; validation
pipeline in the gateway ahead of delegation) and by tests asserting that non-conforming requests — schema mismatch or
inadmissible subject type — produce validation errors (never `granted: true`, never a plain not-granted decision), and
that conforming requests reach the plugin unchanged.

## Pros and Cons of the Options

### (a) Opaque metadata, backend-owned validation

* Good, because zero coupling and zero engine-side work; maximal short-term flexibility.
* Bad, because the licensing surface is invisible to platform vendors — no inventory, no schema, no change control.
* Bad, because every backend re-implements validation differently, or skips it.

### (b) Typed derived GTS contracts validated by the engine

* Good, because the full licensing surface is enumerable from the registry, reviewable in isolation from other types,
  and versioned.
* Good, because validation is uniform and fail-closed; plugins receive only conforming requests.
* Good, because subject/resource compatibility (admitted subjects) is expressible and checked.
* Good, because metadata stays flexible: Gears expose what they need, platform vendors ignore the rest.
* Bad, because of projection boilerplate, registry governance overhead, and registry schema resolution on the check
  path (mitigated by caching).

### (c) Registered schemas, documentation only

* Good, because cheap: discoverability without engine changes.
* Bad, because nothing keeps schema and behavior in sync — the contract can lie, which is worse than no contract.

## More Information

The admitted-subjects prerequisite is already satisfied by GTS traits (`guidelines/GTS.md` §10): `x-gts-traits-schema`
/ `x-gts-traits` on type schemas, inherited along the derivation chain — resource-group's `allowed_parent_types` is a
working precedent of the same pattern. Base and derived licensing types live under `gts.cf.core.lic.*`.
`cpt-cf-license-resolver-adr-gts-resource-identity` records the identity fields *inside* the contract objects;
`cpt-cf-license-resolver-adr-plugin-delegation` (delegation, no grant store, fail-closed) is untouched and fully
compatible.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-license-resolver-fr-contract-registration` — Subject/Resource shapes as registered, versioned derived types.
* `cpt-cf-license-resolver-fr-contract-discoverability` — the licensing surface is enumerable from the registry, in
  isolation from other types.
* `cpt-cf-license-resolver-fr-request-validation` — engine-side validation ahead of delegation, fail-closed, distinct
  from not-granted.
* `cpt-cf-license-resolver-fr-contract-compatibility` — additive-optional is non-breaking; everything else is a new
  contract version.
* `cpt-cf-license-resolver-fr-evaluation-metadata` — metadata becomes schematized while staying semantically opaque to
  the engine.
* `cpt-cf-license-resolver-principle-validated-contracts` — this ADR is the rationale for that DESIGN principle.
* `cpt-cf-license-resolver-actor-platform-vendor` — the actor this decision serves.
