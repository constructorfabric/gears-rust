---
status: accepted
date: 2026-10-20
description: Each section plugin serves its own current schema with a schema version, and every seed fetches the live schemas and validates the whole manifest before it dispatches anything
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

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-plugin-served-section-schemas`

## Context and Problem Statement

Each section must be checked against rules only its section plugin knows. Where do those rules live, and when does the gear read them? The PRD fixes live plugin-served schemas and whole-manifest validation (`cpt-cf-application-manager-fr-plugin-served-schemas`, `cpt-cf-application-manager-fr-validate-before-dispatch`), so a gear-side cache and plugin-only validation are baselines; the open comparison is against schemas registered in the types registry.

## Decision Drivers

* `cpt-cf-application-manager-fr-plugin-served-schemas`, `cpt-cf-application-manager-fr-validate-before-dispatch`
* `cpt-cf-application-manager-nfr-no-partial-on-invalid`
* `cpt-cf-application-manager-fr-discovery`

## Considered Options

* Plugin-served live schemas fetched per seed
* Schemas cached by the gear at startup
* Schemas registered in the types registry (GTS)
* No gear-side validation

## Decision Outcome

Chosen option: "Plugin-served live schemas fetched per seed", because each section's rules have one owner, a schema change applies from the next seed without a restart, and validation always uses current rules.

* Each plugin serves a self-contained Draft 2020-12 schema with a schema version through `schema`; the gear owns only the envelope schema.
* Inside the accepted operation, every seed fetches the schema from every plugin and validates the whole manifest before any dispatch.
* Discovery serves the same live schemas, and seed validation and the combined schema share one composition rule.
* Schema call results and their outcome ranks are defined in [DESIGN §3.3](../DESIGN.md#33-api-contracts).

### Consequences

* Seeding depends on schema availability: an unhealthy plugin blocks seeds until it recovers.
* Each seed pays one schema call per plugin and one compile per section.
* Served schemas are untrusted input, handled by the schema validator ([DESIGN §3.2](../DESIGN.md#32-component-model)).
* Section problems are listed once the envelope is valid, because schemas are fetched only inside the operation.

### Confirmation

* No `apply` call on any section problem, and every problem is listed with its location.
* A table-driven test over the causes of `unusable`, each ending the seed as temporarily unavailable with the key named.
* A schema changed between two seeds is used by the second seed without a restart.

## Pros and Cons of the Options

### Plugin-served live schemas fetched per seed

* Good, because schema and plugin behaviour cannot drift, and discovery and validation read one source.
* Bad, because seeds depend on every plugin's schema availability.

### Schemas cached by the gear at startup

* Good, because seeds are faster and independent of plugin availability.
* Bad, because a schema change needs a gear restart, and the gear keeps a copy against `cpt-cf-application-manager-fr-plugin-served-schemas`.

### Schemas registered in the types registry (GTS)

* Good, because the platform already versions schemas there, visible to other tools.
* Bad, because the registry copy can drift from the plugin's behaviour and needs coordinated releases.
* Bad, because a plugin cannot refuse a schema read for the caller with `NotPermitted`.

### No gear-side validation

* Good, because the gear holds no schema logic.
* Bad, because a bad section is found after earlier sections were applied, against `cpt-cf-application-manager-fr-validate-before-dispatch`.
