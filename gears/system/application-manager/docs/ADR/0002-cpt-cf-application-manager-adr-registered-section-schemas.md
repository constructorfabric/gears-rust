---
status: accepted
date: 2026-10-27
description: Each section plugin registers its section schema as a GTS section type, and the gear reads and compiles all of them once at startup, then validates the whole manifest before it dispatches anything
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Section Schemas Registered as GTS Section Types, Read Once at Startup

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Registered section types read at startup](#registered-section-types-read-at-startup)
  - [Registered section types read on every seed](#registered-section-types-read-on-every-seed)
  - [Plugin-served schemas fetched on every seed](#plugin-served-schemas-fetched-on-every-seed)
  - [No gear-side validation](#no-gear-side-validation)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-registered-section-schemas`

## Context and Problem Statement

Each section must be checked against rules only its section plugin knows. Where do those rules live, and when does the gear read them? The PRD requires registered section schemas and whole-manifest validation before any dispatch (`cpt-cf-application-manager-fr-registered-section-schemas`, `cpt-cf-application-manager-fr-validate-before-dispatch`). The open question is when the gear reads the registered schemas, and whether the plugin is asked at all.

## Decision Drivers

* `cpt-cf-application-manager-fr-registered-section-schemas`, `cpt-cf-application-manager-fr-validate-before-dispatch`
* `cpt-cf-application-manager-nfr-no-partial-on-invalid`
* `cpt-cf-application-manager-fr-discovery`
* `cpt-cf-application-manager-fr-unique-section-keys`
* `cpt-cf-application-manager-fr-startup-checks`

## Considered Options

* Registered section types read at startup
* Registered section types read on every seed
* Plugin-served schemas fetched on every seed
* No gear-side validation

## Decision Outcome

Chosen option: "Registered section types read at startup", because each section's rules have one owner and one registered copy, seeds and discovery do not depend on plugin or registry availability, and an unusable schema or a key conflict fails at deploy time.

* Each plugin registers a derived type of `gts.cf.core.application_manager.section.v1~`, whose schema is the section content schema (JSON Schema Draft 2020-12). The gear owns only the envelope schema.
* Each plugin instance references its exact section type version through `properties.section_type` (an `x-gts-ref`).
* The section key is the section type id without its minor version, for example `gts.cf.core.application_manager.section.v1~acme.iam.roles.v1`. The schema version is the full section type id.
* At `start` the gear resolves every section type and compiles every schema once. The compiled schemas stay fixed with the plugin set until the next start.
* Every seed validates the whole manifest against those schemas before any dispatch. Discovery serves the same schemas, and seed validation and the combined schema share one composition rule.
* Schema checks and outcome details are in [DESIGN §3.2](../DESIGN.md#32-component-model).

### Consequences

* A schema change is a new section type version, and takes effect after a gear restart. A major version gives a new key, which manifests must adopt.
* Seeds and discovery make no registry call and no plugin call for schemas.
* An unusable or unresolvable section schema fails startup.
* One compile per section per process.
* Registered schemas are untrusted input, handled by the schema validator.
* A plugin cannot vary its schema by caller.

### Confirmation

* A table-driven startup test over the causes of an unusable schema, each failing startup with its reason.
* No `apply` call on any section problem, and every problem is listed with its location.
* No types-registry call and no plugin call on the seed and discovery paths.
* Two minor versions of one section type register under one key.

## Pros and Cons of the Options

### Registered section types read at startup

* Good, because each section's rules have one owner and one registered copy, visible to other tools.
* Good, because seeds and discovery do not depend on registry or plugin availability, and each schema is compiled once.
* Good, because an unusable schema or a key conflict fails at deploy time.
* Bad, because a schema change needs a gear restart.
* Bad, because a plugin cannot vary its schema by caller.

### Registered section types read on every seed

* Good, because a schema change applies without a restart.
* Bad, because the registry sits on every seed and discovery path.
* Bad, because the schema can change between validation runs.

### Plugin-served schemas fetched on every seed

* Good, because the plugin can vary its schema by caller.
* Bad, because seeds depend on every plugin's availability.
* Bad, because each seed pays a schema call per plugin.
* Bad, because the gear cannot check schemas at deploy time.

### No gear-side validation

* Good, because the gear holds no schema logic.
* Bad, because a bad section is found after earlier sections were applied, against `cpt-cf-application-manager-fr-validate-before-dispatch`.
