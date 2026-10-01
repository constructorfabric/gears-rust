---
status: proposed
date: 2026-09-30
description: Section plugins serve the schemas of their own sections, the Application Manager fetches them on every seed without reuse, and every registered section is required
---
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

Chosen option: "Schemas served by section plugins, fetched on every seed, every section required", because it is the only option that keeps a single owner for each section's rules, applies a schema change from the next seed without a restart, and makes the whole-manifest check both current and strict. Its cost, one schema call per section plugin per seed and a dependency of seeding on section plugin schema availability, is acceptable for an infrequent, bounded operation and is handled by a defined outcome rather than by weakening validation.

The decision has three parts:

1. **Ownership.** Each section plugin serves the schema for its own section through its section plugin API. The Application Manager owns only the envelope schema and keeps no copy of any section's rules.
2. **Freshness.** On every seed the Application Manager fetches the schema from every section plugin and does not reuse earlier answers. A schema change takes effect on the next seed. Existing registry entries are not re-checked.
3. **Completeness.** Every registered section key is required in a manifest. A missing section, or a key no section plugin registered, makes the whole manifest invalid.

Decision scope: this ADR covers where section schemas come from, when they are read, and whether sections are required. It does not decide how section plugins are discovered ([ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)), the order of validation relative to dispatch ([ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md)), the HTTP mapping of outcomes ([ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md)), or how the schema fingerprint is recorded in audit events ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)).

### Consequences

* The section plugin contract must include a schema operation that the Application Manager calls on every seed and on schema discovery. Section plugin authors own the schema's content and its evolution.
* If any section plugin cannot serve its schema, the seed ends as temporarily unavailable and nothing is dispatched. Schema unavailability applies to seed only. In the precedence set by `cpt-cf-application-manager-fr-outcome-precedence` it shares rank 4 with the audit plugin being unable to accept the request hand-off: after platform access control, registry storage, and not-permitted checks, and before invalid manifest. The request is still handed to the audit plugin.
* The Application Manager needs a JSON Schema validator and must compile each fetched schema per seed. Compiled results are not kept across seeds.
* Discovery serves the same live schemas: one key's schema, and a combined schema built from the envelope schema plus every section plugin's current schema. The combined schema is unavailable when any one section plugin cannot answer. A manifest built from the combined schema passes validation, which makes discovery the contract for seeding callers.
* Each schema used for a seed is identified by a fingerprint so audit readers can tell which schema checked which section.
* Adding a section plugin makes its key required at once. Manifests that lack the new key are invalid after the restart that adds the section plugin, and re-seeding is the seeding caller's task (`cpt-cf-application-manager-fr-plugin-rollout`). Removing a section plugin makes its key unknown.
* Section schemas are JSON Schema Draft 2020-12, and references must stay inside the served document. A schema that declares another dialect, uses an external reference, or does not compile is a section-plugin defect: the request ends as temporarily unavailable with nothing dispatched, and the failure names the key and is raised as an operator signal.
* Section plugin authors must keep the schema operation cheap and side-effect free, because it is on the seed path and is bound by the plugin-call time limit (`cpt-cf-application-manager-fr-bounded-plugin-calls`).
* A schema change can make a previously valid manifest invalid between two seeds. Callers that re-seed on a schedule may see new rejections after a section plugin upgrade; this is intended and reported with the locations of the problems.
* Fetching schemas concurrently is an implementation choice for the [DESIGN](../DESIGN.md); this ADR requires only that each seed sees each section plugin's current schema. Concurrent requests for the same application id are resolved in [DESIGN §4.2](../DESIGN.md#42-concurrent-requests-for-the-same-application): they are left to the section plugins, the system adds no coordination, the last registry writer wins, and both requests are audited.

Impact by concern:

* Performance: one schema call per section plugin per seed. Each call is capped by the one plugin-call time limit, and a seed is bounded by its ordered sequence of capped calls (`cpt-cf-application-manager-nfr-bounded-call-time`, `cpt-cf-application-manager-nfr-bounded-request-time`). Seeds are infrequent, so the cost is accepted.
* Security: schema content is not trusted to grant anything. Access to schema reads passes the same platform access control as other actions, and no section plugin is called before the caller is authorized (`cpt-cf-application-manager-nfr-authorized-before-plugin`). The Application Manager treats a fetched schema as untrusted input to the validator and applies the plugin-call time limit.
* Reliability: an unreachable schema is a retryable, temporarily unavailable outcome with nothing dispatched, never a pass (fail closed on validation). Availability implications are recorded in `cpt-cf-application-manager-nfr-availability`.
* Data: no schema is persisted by the Application Manager; only a schema fingerprint reaches the audit event.
* Integration: no new external dependency; the schema operation is part of the existing section plugin contract.
* Operations: unavailability of a schema names the failing section plugin in the result. It shows in the outcome-category counts (as temporarily unavailable) and, when the schema call timed out, in the plugin-call timeout counts of `cpt-cf-application-manager-nfr-operational-signals`; no separate schema-fetch signal is added.
* Testing: see Confirmation.
* Compliance: not applicable beyond audit, since no personal or tenant data is held in a schema.
* UX: seeding callers get one discoverable, always-current description of a valid manifest and per-location errors.
* Business: platform owners get section plugin delivery independent of Application Manager releases. The trade-off is that a section plugin outage blocks seeds until it recovers.

### Confirmation

* Review of record: no section schema exists in Application Manager configuration or source, and the section plugin contract has a schema operation.
* Integration tests change a test section plugin's schema between two seeds without a restart and confirm the second seed uses the new schema.
* Tests confirm that a second seed calls the schema operation again for every section plugin (no reuse), and that a section plugin unable to serve its schema yields the temporarily unavailable outcome with nothing dispatched.
* Tests confirm that a manifest with a missing section, an unknown key, or a schema violation is rejected as a whole with problem locations, and that no section plugin's apply operation is called.
* A discovery test confirms that a manifest built only from the combined schema passes validation.
* Code review checks that no cache, memoisation or retained compiled schema outlives a seed.

## Pros and Cons of the Options

### Schemas served by section plugins, fetched on every seed, every section required

Each section plugin serves its section's JSON Schema. The Application Manager fetches all of them per seed, validates the envelope and all sections, then dispatches.

* Good, because the section plugin that applies a section is the only source of its rules, so schema and behaviour cannot drift.
* Good, because schema changes ship with the section plugin and apply from the next seed with no Application Manager change or restart.
* Good, because validation always uses current rules, and the audit record can name the exact schema used.
* Good, because required sections give every section plugin data from every application and surface a wrong manifest immediately.
* Good, because discovery and seed validation read the same source.
* Neutral, because adding or removing a section plugin changes what is required, which is deliberate and handled as a rollout rule, not by the Application Manager.
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
* Bad, because the fingerprint recorded in audit would name a schema that was not the section plugin's current one.
* Bad, because discovery would need to choose between live and cached answers and could contradict seed validation.

### Schemas served by section plugins, with optional sections

Schemas are served by section plugins, but a manifest may leave out sections. Omitted sections are skipped.

* Good, because an application can send only what it has, and adding a section plugin breaks no existing manifest.
* Bad, because an omitted section is indistinguishable from a forgotten one, so a wrong manifest can pass silently.
* Bad, because a section plugin cannot tell "no data for this application" from "not sent", which pushes ambiguity into every section plugin.
* Bad, because a re-seed that drops a section would need a rule for what that means (keep, remove, ignore), which contradicts the pass-through principle that the Application Manager gives re-seeds no meaning.
* Bad, because the combined schema would no longer describe one valid manifest shape.

## More Information

* The section plugin contract and its schema operation are described in the PRD as `cpt-cf-application-manager-contract-section-plugin`; the exact trait is a topic for the [DESIGN](../DESIGN.md).
* Review this ADR if seed latency or section plugin schema availability becomes a demonstrated problem, or if the section plugin set stops being small and bounded. Any move to caching would supersede this decision and would need an explicit freshness rule and a matching change to discovery.
* Related decisions: [ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md) (every section plugin participates, one key each, discovered at startup), [ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md) (validate everything, then dispatch), [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md) (audit of requests that end before dispatch), [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md) (the registry stores no manifest, so a schema change never invalidates stored data), [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md) (how the temporarily unavailable outcome is reported over HTTP).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-plugin-served-schemas` — each section plugin is the only source of its section's schema, and a change applies from the next seed.
* `cpt-cf-application-manager-fr-validate-before-dispatch` — schemas are fetched on every seed without reuse before the whole manifest is validated.
* `cpt-cf-application-manager-fr-required-sections` — every registered section key is required, and an unknown key is invalid.
* `cpt-cf-application-manager-fr-manifest-envelope` — the Application Manager owns only the envelope schema.
* `cpt-cf-application-manager-fr-discovery` — discovery serves live schemas and the combined schema from the same source.
* `cpt-cf-application-manager-fr-outcome-precedence` — a schema that cannot be reached shares rank 4 with the audit request hand-off failure, above invalid manifest.
* `cpt-cf-application-manager-fr-outcome-categories` — a schema that cannot be served ends the seed as temporarily unavailable, with nothing dispatched.
* `cpt-cf-application-manager-fr-bounded-plugin-calls` — the schema call is bounded by the plugin-call time limit, and a timed-out schema call ends the seed as temporarily unavailable.
* `cpt-cf-application-manager-fr-plugin-rollout` — adding or removing a section plugin changes what a valid manifest contains.
* `cpt-cf-application-manager-fr-unique-section-keys` — one key per section plugin makes each section's schema unambiguous.
* `cpt-cf-application-manager-nfr-no-partial-on-invalid` — an invalid or unverifiable manifest dispatches nothing.
* `cpt-cf-application-manager-nfr-availability` — seeding depends on section plugin schema availability.
* `cpt-cf-application-manager-contract-section-plugin` — the contract includes a schema operation.
* `cpt-cf-application-manager-actor-section-plugin-author` — owns the schema and its evolution.
* `cpt-cf-application-manager-actor-seeding-caller` — builds manifests from discovery.
* `cpt-cf-application-manager-usecase-first-seed` — validated against current schemas.
* `cpt-cf-application-manager-usecase-reseed` — a schema change applies at the next re-seed.
* `cpt-cf-application-manager-usecase-discovery` — discovery reads live schemas.
* `cpt-cf-application-manager-usecase-plugin-change` — adding or removing a section plugin changes required sections.
