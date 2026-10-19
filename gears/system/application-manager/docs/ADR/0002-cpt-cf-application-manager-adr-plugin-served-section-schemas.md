---
status: accepted
date: 2026-10-20
description: Each section plugin serves its own current schema with a schema version through the schema call, the gear keeps no stored schemas, and every seed fetches the live schemas and validates the whole manifest before it dispatches anything
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Section Plugins Serve Their Own Live Schemas, Fetched on Every Seed

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Plugin-served live schemas fetched per seed](#plugin-served-live-schemas-fetched-per-seed)
  - [Schemas cached by the gear at startup](#schemas-cached-by-the-gear-at-startup)
  - [Schemas registered in the types registry (GTS)](#schemas-registered-in-the-types-registry-gts)
  - [No gear-side validation](#no-gear-side-validation)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-plugin-served-section-schemas`

## Context and Problem Statement

A manifest carries one section per section plugin, and each section must be checked against rules that only the domain of its section plugin knows. Where do those rules live, and when does the gear read them? The answer decides whether a schema change needs a gear release or restart, whether callers and section plugins can drift apart, and whether a bad manifest is caught before anything is dispatched.

## Decision Drivers

* The rules for a section should exist in one place, owned by the section plugin that applies the section, so schema and behaviour cannot drift (`cpt-cf-application-manager-fr-plugin-served-schemas`).
* A section plugin should be able to change its schema without a change or restart of the gear (`cpt-cf-application-manager-fr-validate-before-dispatch`).
* A bad manifest must be rejected as a whole before any dispatch, so nothing is half-applied (`cpt-cf-application-manager-nfr-no-partial-on-invalid`).
* Seeding callers need one discoverable description of a valid manifest (`cpt-cf-application-manager-fr-discovery`).
* A stale schema must never be the reason a manifest passes or fails. Correctness is preferred over seed latency, because seeds are infrequent and the section plugin set is small and bounded.
* The gear must stay free of domain logic and of knowledge of consumers.

## Considered Options

* Plugin-served live schemas fetched per seed
* Schemas cached by the gear at startup
* Schemas registered in the types registry (GTS)
* No gear-side validation

## Decision Outcome

Chosen option: "Plugin-served live schemas fetched per seed", because it keeps one owner for each section's rules, applies a schema change from the next seed without a restart, and makes the whole-manifest check current. It costs one schema call per section plugin per seed and makes seeding depend on schema availability. That cost is acceptable for an infrequent, bounded operation. The PRD fixes live plugin-served schemas and whole-manifest validation (`cpt-cf-application-manager-fr-plugin-served-schemas`, `cpt-cf-application-manager-fr-validate-before-dispatch`), so "Schemas cached by the gear at startup" and "No gear-side validation" are recorded as baselines, and the comparison that decides is the one against schemas registered in the types registry (GTS).

The decision has these parts:

1. **Ownership.** Each section plugin serves the current schema of its own section through the `schema` call. The schema is a self-contained JSON Schema Draft 2020-12 document and comes with a schema version that the plugin declares (`cpt-cf-application-manager-entity-schema-version`). The gear owns only the envelope schema and stores no section schema.
2. **Per-seed validation.** Inside the accepted operation, every seed fetches the live schema from every section plugin, validates the envelope and every section before anything is dispatched, and dispatches only when the whole manifest is valid. A schema change takes effect on the next seed.
3. **Schema call results.** A `schema` call that returns `NotPermitted` gives not permitted. Any other error kind, or a transport error, gives the reason `unreachable`. A timeout gives `timed_out`, a refusal by the in-flight cap gives `not_sent`, and a schema that cannot be used gives `unusable`.
4. **Outcome ranks.** A schema that is not served or is unusable ends the seed as temporarily unavailable (rank 5). A `NotPermitted` answer from a plugin's `schema` ends it as not permitted (rank 6). A section problem ends it as invalid manifest (rank 7). Nothing is dispatched in any of these cases.
5. **Discovery.** An authorized caller with the `read_schema` permission reads the section keys, one live schema, or the combined schema, each with its schema version. The caller reads the expected schemas up front through discovery and builds the manifest from them. When one plugin's call gives an unavailable reason and another's gives `NotPermitted`, a read answers 503 and not 403, the same precedence as the seed.

Decision scope: this ADR covers where section schemas come from and when they are read. It does not decide how section plugins are discovered ([ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)), how the audit ledger records events ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-ledger.md)), or how permission is decided ([ADR 0005](./0005-cpt-cf-application-manager-adr-platform-level-access.md)). Outcome precedence, placement and HTTP mapping are set only in [DESIGN §3.3](../DESIGN.md#33-api-contracts).

### Consequences

* The section plugin contract includes the `schema` call. The gear calls it in each seed operation and in each schema read. Section plugin authors own the schema's content and its evolution, change the schema version whenever the content changes, and keep the call cheap and free of side effects.
* The `schema` call is bound by the plugin-call time limit (`cpt-cf-application-manager-fr-bounded-plugin-calls`), because it is on the seed path.
* If any section plugin does not serve a usable schema, the seed ends as temporarily unavailable with nothing dispatched, and the report names the key and the reason. Seeding depends on section plugin schema availability (`cpt-cf-application-manager-nfr-availability`).
* The schema validator compiles each served schema as Draft 2020-12 and keeps no compiled schema beyond one operation or one read. A schema that declares another dialect, refers outside the served document, exceeds the size cap, carries a missing, empty or too long schema version, or fails to compile is `unusable`.
* Served schemas are untrusted input, so the validator uses a linear-time regex engine with size limits and compiles off the async executor.
* Seed validation and the combined schema from discovery follow one composition rule, so a manifest built from the combined schema passes validation.
* The combined read fails when any one section plugin cannot answer. The key list is read from memory and calls no plugin.
* The schema version of each validated section is recorded as sent in the report and the finished event. The gear never compares versions.
* The envelope is checked before the operation is accepted, and the schemas are fetched only inside the accepted operation. Section problems are therefore listed only once the envelope is valid. This is the staged problem listing in [DESIGN §4.6](../DESIGN.md#46-platform-deviations-and-dependencies).
* A schema change can make a previously valid manifest invalid between two seeds. A caller that re-seeds after a section plugin upgrade may see new rejections, reported with the locations of the problems.
* A caller without the `read_schema` permission, or without the seed permission, causes no `schema` call.

### Confirmation

* Review: no section schema exists in gear configuration or source, and no schema or compiled schema outlives one operation or one read.
* A counting test confirms that no `apply` call is made on any section problem, and that the report lists every problem with its location.
* Tests confirm that a schema that is not served or is unusable ends the seed as temporarily unavailable (rank 5).
* A table-driven test over the causes of `unusable`: a schema that declares another dialect, refers outside the served document, exceeds the size cap, carries a missing, empty or too long schema version, or fails to compile. Each case ends the seed as temporarily unavailable with the reason `unusable` and the key named.
* A test confirms that a `NotPermitted` answer from a `schema` call ends the seed as not permitted (rank 6) and a read as 403, and that an unavailable plugin together with a `NotPermitted` one gives 503.
* An integration test changes a test section plugin's schema between two seeds without a restart and confirms that the second seed calls `schema` again and uses the new schema.
* A discovery test confirms that each schema is returned with its schema version, and that a manifest built only from the combined schema passes validation.

## Pros and Cons of the Options

### Plugin-served live schemas fetched per seed

Each section plugin serves its section's schema through `schema`. In every seed operation, the gear fetches the schema from every section plugin, validates the envelope and all sections, and dispatches only when the whole manifest is valid.

* Good, because the section plugin that applies a section is the only source of its rules, so schema and behaviour cannot drift.
* Good, because a schema change ships with the section plugin and applies from the next seed with no gear change or restart.
* Good, because validation always uses current rules, and the recorded schema version names the schema used.
* Good, because discovery and seed validation read the same source.
* Good, because the whole manifest is checked before the first dispatch, so a bad manifest leaves nothing half-applied.
* Bad, because every seed depends on the schema availability of the section plugins, so one unhealthy plugin blocks seeds until it recovers.
* Bad, because each seed pays one schema call per plugin and one schema compile per section.

### Schemas cached by the gear at startup

Section plugins serve schemas once at startup, and the gear validates against the kept copies.

* Good, because seeds are faster and do not depend on section plugin availability.
* Good, because startup would surface an unusable schema at deploy time.
* Bad, because a schema change needs a restart of the gear, which breaks independent section plugin evolution.
* Bad, because a seed can be validated against rules the plugin has since changed, so a manifest may pass or fail wrongly.
* Bad, because the gear would keep a copy of each section's rules, against `cpt-cf-application-manager-fr-plugin-served-schemas`.
* Bad, because discovery would have to choose between live and kept answers and could contradict seed validation.

### Schemas registered in the types registry (GTS)

Each section plugin registers its schema as a GTS type, and the gear reads it from the types registry.

* Good, because the platform already has a registry and a versioning scheme for schemas.
* Good, because schemas would be visible to other platform tools.
* Bad, because the registry copy is a second place for the rules, so it can drift from the behaviour of the plugin that applies the section.
* Bad, because a schema change needs a registry update coordinated with the plugin release.
* Bad, because the gear would read a registry entry that the caller identity and plugin-level permission cannot shape, so a plugin could not refuse a schema read with `NotPermitted`.

### No gear-side validation

The gear passes each section to its plugin, and each plugin validates on `apply`.

* Good, because the gear holds no validator and no schema logic.
* Good, because plugin availability affects only the sections dispatched, not a separate schema step.
* Bad, because a problem in one section is found only after earlier sections have been applied, so a bad manifest can leave an install half-applied.
* Bad, because callers have no combined description of a valid manifest to build from, so discovery cannot offer a reliable contract.
* Bad, because each plugin would repeat envelope checks and report problems in its own format, so the gear could not list every problem at once.

## More Information

* The section plugin contract is `cpt-cf-application-manager-contract-section-plugin`, and its exact trait is in the [DESIGN](../DESIGN.md#33-api-contracts).
* Review this ADR if seed latency or schema availability becomes a demonstrated problem, or if the section plugin set stops being small and bounded.
* Related decisions: [ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md) (every section plugin participates, one key each, discovered once at startup), [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-ledger.md) (the audit ledger), [ADR 0005](./0005-cpt-cf-application-manager-adr-platform-level-access.md) (platform-level permission, including `read_schema`).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-plugin-served-schemas` — each section plugin is the only source of its section's schema, and a change applies from the next seed.
* `cpt-cf-application-manager-fr-validate-before-dispatch` — every seed fetches the live schemas and validates the whole manifest before any dispatch.
* `cpt-cf-application-manager-fr-required-sections` — the validator requires exactly the key set and names each missing or unknown key.
* `cpt-cf-application-manager-fr-manifest-envelope` — the gear owns only the envelope schema.
* `cpt-cf-application-manager-fr-discovery` — discovery serves live schemas with their versions, and the combined schema from the same source.
* `cpt-cf-application-manager-fr-outcome-categories` — a schema not served or unusable gives temporarily unavailable, a `NotPermitted` schema gives not permitted, and a section problem gives invalid manifest.
* `cpt-cf-application-manager-fr-bounded-plugin-calls` — the `schema` call is bound by the plugin-call time limit.
* `cpt-cf-application-manager-nfr-bounded-request-time` — the schema fetch runs inside the background operation, so the immediate answer stays within the platform request deadline.
* `cpt-cf-application-manager-nfr-no-partial-on-invalid` — an invalid or unverifiable manifest dispatches nothing.
* `cpt-cf-application-manager-nfr-availability` — seeding depends on section plugin schema availability.
* `cpt-cf-application-manager-contract-section-plugin` — the contract includes the `schema` call.
* `cpt-cf-application-manager-entity-schema-version` — the schema version is declared by the section plugin and recorded as sent.
* `cpt-cf-application-manager-principle-validate-then-dispatch` — dispatch starts only after every live schema and the whole-manifest validation have passed.
* `cpt-cf-application-manager-component-schema-validator` — compiles the served schemas, composes the combined schema, and collects every problem.
* `cpt-cf-application-manager-component-section-plugin-router` — fetches the schemas concurrently and classifies the `schema` call results.
* `cpt-cf-application-manager-component-read-service` — serves the live schemas and the combined schema for discovery.
* `cpt-cf-application-manager-actor-seeding-caller` — builds manifests from discovery.
* `cpt-cf-application-manager-usecase-first-seed` — validated against current schemas.
* `cpt-cf-application-manager-usecase-reseed` — a schema change applies at the next re-seed.
* `cpt-cf-application-manager-usecase-discovery` — the caller reads the expected schemas up front.
