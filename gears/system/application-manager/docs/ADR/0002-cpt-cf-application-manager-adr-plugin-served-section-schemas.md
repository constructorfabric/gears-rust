---
status: proposed
date: 2026-09-30
description: Section plugins serve the schemas of their own sections, the Application Manager fetches them on every seed without reuse, and every registered section is required
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Section Plugins Serve Their Own Schemas, Fetched on Every Seed, With Every Section Required


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Schemas served by section plugins, fetched on every seed, every section required](#schemas-served-by-section-plugins-fetched-on-every-seed-every-section-required)
  - [Schemas held in Application Manager configuration](#schemas-held-in-application-manager-configuration)
  - [Schemas served by section plugins, cached by the Application Manager](#schemas-served-by-section-plugins-cached-by-the-application-manager)
  - [Schemas served by section plugins, with optional sections](#schemas-served-by-section-plugins-with-optional-sections)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-plugin-served-section-schemas`

## Context and Problem Statement

A manifest carries one section per section plugin, and each section must be checked against rules that only its section plugin's domain knows. Where do those rules live, when does the Application Manager read them, and may a manifest leave a section out? The answer decides whether a schema change needs an Application Manager release or restart, whether callers and section plugins can drift apart, and whether a bad manifest is caught before anything is dispatched.

## Decision Drivers

* Rules for a section should exist in one place, owned by the section plugin that applies the section, so schema and behaviour cannot drift (`cpt-cf-application-manager-fr-plugin-served-schemas`).
* A section plugin should be able to change its schema without a change or restart of the Application Manager (`cpt-cf-application-manager-fr-validate-before-dispatch`).
* A bad manifest must be rejected as a whole before any dispatch, so nothing is half-applied (`cpt-cf-application-manager-nfr-no-partial-on-invalid`).
* Seeding callers need one discoverable description of a valid manifest (`cpt-cf-application-manager-fr-discovery`).
* Every section plugin should be able to rely on receiving data from every application, and a wrong manifest should be visible at once (`cpt-cf-application-manager-fr-required-sections`).
* A stale schema must never be the reason a manifest passes or fails; correctness is preferred over seed latency, since seeds are infrequent and the section plugin set is small and bounded.
* The Application Manager must stay free of domain logic and of knowledge of consumers.

## Considered Options

* Schemas served by section plugins, fetched on every seed, every section required
* Schemas held in Application Manager configuration
* Schemas served by section plugins, cached by the Application Manager
* Schemas served by section plugins, with optional sections


## Decision Outcome

Chosen option: "Schemas served by section plugins, fetched on every seed, every section required", because it is the only option that keeps one owner for each section's rules, applies a schema change from the next seed without a restart, and makes the whole-manifest check both current and strict. Its cost is one schema call per section plugin per seed and a dependency of seeding on schema availability. That cost is acceptable for an infrequent, bounded operation.

The decision has three parts:

1. **Ownership.** Each section plugin serves the schema for its own section through its section plugin API, with a schema version it declares (`cpt-cf-application-manager-entity-schema-version`). The Application Manager owns only the envelope schema and keeps no copy of any section's rules.
2. **Freshness.** On every seed the Application Manager fetches the schema from every section plugin and does not reuse earlier answers. A schema change takes effect on the next seed.
3. **Completeness.** Every registered section key is required in a manifest. A missing section, or a key no section plugin registered, makes the whole manifest invalid (`cpt-cf-application-manager-fr-required-sections`).

Decision scope: this ADR covers where section schemas come from, when they are read, and whether sections are required. It does not decide how section plugins are discovered ([ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)) or how audit events are delivered ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)). Outcome precedence, placement and HTTP mapping are set only in [DESIGN §3.3](../DESIGN.md#33-api-contracts).

### Consequences

* The section plugin contract includes a schema operation that the Application Manager calls on every seed and on schema discovery. Section plugin authors own the schema's content and its evolution.
* If any section plugin cannot serve a usable schema, the seed ends as temporarily unavailable and nothing is dispatched. Where this outcome ranks and how it maps to HTTP are set in [DESIGN §3.3](../DESIGN.md#33-api-contracts).
* The Application Manager needs a JSON Schema validator and compiles each fetched schema per seed. Compiled results are not kept across seeds.
* Discovery serves the same live schemas: one key's schema, and a combined schema built from the envelope schema plus every section plugin's current schema. The combined schema is unavailable when any one section plugin cannot answer. A manifest built from the combined schema passes validation (`cpt-cf-application-manager-fr-discovery`).
* The schema version of each schema used for a seed is recorded as sent, so readers can tell which schema checked which section. The Application Manager never compares versions.
* Section schemas are JSON Schema Draft 2020-12, and references stay inside the served document. A schema that declares another dialect, uses an external reference, or does not compile is a section-plugin defect. The seed ends as temporarily unavailable with nothing dispatched, and the failure names the key.
* Section plugin authors must keep the schema operation cheap and side-effect free, because it is on the seed path and is bound by the plugin-call time limit (`cpt-cf-application-manager-fr-bounded-plugin-calls`).
* A schema change can make a previously valid manifest invalid between two seeds. Callers that re-seed may see new rejections after a section plugin upgrade. This is intended and is reported with the locations of the problems.
* Adding or removing a section plugin changes which keys are required. Section plugin changes need a restart ([DESIGN §2.2](../DESIGN.md)), so the change shows in the first seed after it.
* Concurrent requests for the same application id are not coordinated by schema fetching. One open operation is allowed per application, and a second request is refused as conflict ([DESIGN §4.1](../DESIGN.md#41-concurrent-requests-for-the-same-application)).

### Confirmation

* Review: no section schema exists in Application Manager configuration or source, and no cache or retained compiled schema outlives a seed.
* Integration tests change a test section plugin's schema between two seeds without a restart and confirm the second seed calls the schema operation again and uses the new schema.
* Tests confirm that a section plugin unable to serve its schema ends the seed as temporarily unavailable with nothing dispatched.
* Tests confirm that a missing section, an unknown key, or a schema violation rejects the whole manifest with problem locations and calls no apply operation.
* A discovery test confirms that a manifest built only from the combined schema passes validation.

## Pros and Cons of the Options

### Schemas served by section plugins, fetched on every seed, every section required

Each section plugin serves its section's JSON Schema. The Application Manager fetches all of them per seed, validates the envelope and all sections, then dispatches.

* Good, because the section plugin that applies a section is the only source of its rules, so schema and behaviour cannot drift.
* Good, because schema changes ship with the section plugin and apply from the next seed with no Application Manager change or restart.
* Good, because validation always uses current rules, and the recorded schema version names the schema used.
* Good, because required sections give every section plugin data from every application and surface a wrong manifest immediately.
* Good, because discovery and seed validation read the same source.
* Neutral, because adding or removing a section plugin changes what is required, which is deliberate and follows from the restart rule for plugin changes.
* Bad, because every seed depends on every section plugin's schema being available, so one unhealthy section plugin blocks all seeds until it recovers.
* Bad, because each seed pays one schema call per section plugin and one schema compile per section.

### Schemas held in Application Manager configuration

The operator supplies section schemas in the Application Manager's configuration, keyed by section key.

* Good, because validation does not depend on section plugin availability.
* Good, because schema reads are fast and simple.
* Bad, because the rules live apart from the section plugin that applies them, so schema and behaviour drift and two releases must be kept in step.
* Bad, because a schema change needs a configuration change and, in practice, a restart, which breaks independent section plugin evolution.
* Bad, because the Application Manager would hold domain knowledge that belongs to section plugins, against `cpt-cf-application-manager-fr-plugin-served-schemas`.
* Bad, because a section plugin could be registered without a matching schema, or the reverse, and the mismatch would show up only at seed time.

### Schemas served by section plugins, cached by the Application Manager

Section plugins serve schemas, but the Application Manager keeps them for reuse, refreshed by time or by invalidation.

* Good, because seeds are faster and tolerate a section plugin that is briefly unavailable.
* Neutral, because a cache is bounded by the small section plugin set, so memory is not the concern.
* Bad, because a seed can be validated against a stale schema, so a manifest may pass or fail against rules the section plugin no longer holds.
* Bad, because a cache needs an invalidation or expiry rule and a way for section plugins to signal change, which adds state, a new failure mode and behaviour that is hard to test.
* Bad, because the recorded schema version would name a schema that was not the section plugin's current one.
* Bad, because discovery would need to choose between live and cached answers and could contradict seed validation.

### Schemas served by section plugins, with optional sections

Schemas are served by section plugins, but a manifest may leave out sections. Omitted sections are skipped.

* Good, because an application can send only what it has, and adding a section plugin breaks no existing manifest.
* Bad, because an omitted section is indistinguishable from a forgotten one, so a wrong manifest can pass silently.
* Bad, because a section plugin cannot tell "no data for this application" from "not sent", which pushes ambiguity into every section plugin.
* Bad, because a re-seed that drops a section would need a rule for what that means (keep, remove, ignore), which contradicts the pass-through principle that the Application Manager gives manifest content no meaning.
* Bad, because the combined schema would no longer describe one valid manifest shape.


## More Information

* The section plugin contract is described in the PRD as `cpt-cf-application-manager-contract-section-plugin`. The exact trait is in the [DESIGN](../DESIGN.md).
* Review this ADR if seed latency or section plugin schema availability becomes a demonstrated problem, or if the section plugin set stops being small and bounded. A move to caching would supersede this decision and would need an explicit freshness rule and a matching change to discovery.
* Related decisions: [ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md) (every section plugin participates, one key each, discovered at startup), [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md) (audit events through a transactional outbox).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-plugin-served-schemas` — each section plugin is the only source of its section's schema, and a change applies from the next seed.
* `cpt-cf-application-manager-fr-validate-before-dispatch` — schemas are fetched on every seed without reuse before the whole manifest is validated.
* `cpt-cf-application-manager-fr-required-sections` — every registered section key is required, and an unknown key is invalid.
* `cpt-cf-application-manager-fr-manifest-envelope` — the Application Manager owns only the envelope schema.
* `cpt-cf-application-manager-fr-discovery` — discovery serves live schemas and the combined schema from the same source.
* `cpt-cf-application-manager-fr-outcome-categories` — a schema that cannot be served ends the seed as temporarily unavailable, with nothing dispatched.
* `cpt-cf-application-manager-fr-bounded-plugin-calls` — the schema call is bounded by the plugin-call time limit.
* `cpt-cf-application-manager-fr-unique-section-keys` — one key per section plugin makes each section's schema unambiguous.
* `cpt-cf-application-manager-nfr-no-partial-on-invalid` — an invalid or unverifiable manifest dispatches nothing.
* `cpt-cf-application-manager-nfr-availability` — seeding depends on section plugin schema availability.
* `cpt-cf-application-manager-contract-section-plugin` — the contract includes a schema operation.
* `cpt-cf-application-manager-entity-schema-version` — the schema version is declared by the section plugin and recorded as sent.
* `cpt-cf-application-manager-actor-seeding-caller` — builds manifests from discovery.
* `cpt-cf-application-manager-usecase-first-seed` — validated against current schemas.
* `cpt-cf-application-manager-usecase-reseed` — a schema change applies at the next re-seed.
