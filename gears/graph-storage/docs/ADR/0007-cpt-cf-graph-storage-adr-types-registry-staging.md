---
status: proposed
date: 2026-10-30
decision-makers: V. Shandyba (Graph Storage gear owner); stage 2 and 3 need the types-registry owners
review-evidence: tests/types_registry.rs against the real registry; a seeding probe of producer types on main (see Confirmation)
---

# ADR-0007: Graph types move to the platform types-registry in stages

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The stages](#the-stages)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [1. Registry authoritative now](#1-registry-authoritative-now)
  - [2. Gear catalogue authoritative for good](#2-gear-catalogue-authoritative-for-good)
  - [3. Staged](#3-staged)
- [Open questions for the types-registry owners](#open-questions-for-the-types-registry-owners)
- [What this amends in the documentation](#what-this-amends-in-the-documentation)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-graph-storage-adr-types-registry-staging`

## Context and Problem Statement

types-registry is the platform's one governed registry for type contracts
"instead of bespoke per-gear type-registration mechanisms" (types-registry PRD
§ 1.3). Graph Storage was designed as a client of it: DESIGN described the
gear's `gts_type` table as a per-tenant projection of the registry, and the
base ontology as published to it at startup.

None of it was built. The gear declares `deps = [authz_resolver]`; readiness
reports `types_registry` as `not_implemented`; ingest validates against
`gts_type` only, and producers register their types with the gear only. The
gear also carries its own copy of the registry's rules: chain validation, the
`BACKWARD` compatibility check of types-registry ADR-0003
(`domain/evolution.rs`) and revisions after its ADR-0005. Two consequences:

- **Graph types are invisible to the platform.** Neither the base ontology
  `gts.cf.core.graph.*` nor any producer's type resolves in another gear, the
  PDP, a UI or CI, and a producer cannot derive a graph type in the registry
  because its base is not there.
- **The rules can drift.** The registry's admission is moving on (gts-rust
  0.12, managed admission, revisions in its own store); the gear's copy is
  maintained separately.

What stopped the gear from using the registry when it was built still
partly holds. The registry holds **global** entities only until its P1
(gears-rust #4628), and graph types are registered per tenant; it has no
validation hooks for a gear's own rules (`index`, `full_text_search`,
`vector_search`, `scope_managed`, edge endpoint constraints, migrating stored
rows) until its P2; and a 10,000-item batch must not make a registry round
trip per item.

What does not hold any more: the registry now seeds the `toolkit-gts`
inventory into its database at startup through its own admission path, and
admits the gear's nine base schemas unchanged (see Confirmation).

## Decision Drivers

- The platform has one type namespace; a graph type that only one gear can
  resolve is not a platform type.
- No stage may add a runtime dependency the registry cannot yet honour, or
  change the tenant-isolation model the gear documents.
- Ingest keeps validating from a local, interned, chain-resolved copy
  (`gts_type`): a 4-byte reference per row, no registry call per item.
- Existing tenants' types keep their identity through every stage.

## Considered Options

1. **Registry authoritative now**: register every producer type in the
   registry, and treat `gts_type` as a projection, from the next release.
2. **Gear catalogue authoritative for good**: record graph-storage as an
   exception to the registry and stop describing it as a client.
3. **Staged**: publish what the registry can hold today, keep the gear's
   catalogue authoritative for producer types until the registry has tenants,
   and move the rules as the registry gains the hooks for them.

## Decision Outcome

Chosen option: **3, staged.** Option 1 would make a tenant's type a global
entity visible to every tenant and published with platform credentials on a
tenant's behalf — a change to the isolation model nobody has agreed to — and
would make the registry refuse the type changes ADR-0006 admits on the data
(`revalidate`) or with a migration. Option 2 keeps graph types out of the
platform namespace for good, which is the thing to fix.

### The stages

| Stage | When | What | Runtime dependency on the registry |
|---|---|---|---|
| 0 | now | This ADR; DESIGN, PRD and the `gts_type` doc say the gear's catalogue is authoritative for producer types in the interim | none |
| 1 | now | The nine base schemas and the gear's permissions (one `AuthzPermissionV1` per resource and action the PEP checks) are submitted to the `toolkit-gts` inventory (`src/gts.rs`), which types-registry seeds at its own startup | **none**: the registry seeds, the gear does not call it |
| 2 | after types-registry P1 (#4628) | Producer types are tenant-owned registry entities; `gts_type` becomes their projection; existing tenants' types are imported with their identity; readiness reports the registry | yes, with the gear serving from its projection while the registry is unreachable |
| 3 | after types-registry P2 | The graph rules become the gear's validation hook at admission, and the gear's copy of the compatibility check (`domain/evolution.rs`) goes | yes |

### Consequences

- Good: the base ontology and the permissions resolve platform-wide from
  stage 1, so a grant can name `gts.cf.toolkit.authz.permission.v1~cf.core.graph.*`
  and a producer can derive from `gts.cf.core.graph.*` in the registry.
- Good: stage 1 adds no deploy order and no readiness dependency.
- Bad, accepted for stage 1: a bad inventory entry stops the registry's
  startup, and the platform with it. `tests/types_registry.rs` seeds the real
  registry with everything the gear submits and fails on a refusal or on a
  stored schema that differs from the one the gear validates against.
- Bad, until stage 2: producer types are still registered with the gear only,
  and the gear still carries its own copy of the registry's rules.
- Neutral: a producer outside the `cf` vendor needs the registry's vendor
  policy (`allowed_vendors`) to cover it in stage 2; the registry refuses such
  types today (see Confirmation).

### Confirmation

- `tests/types_registry.rs` initialises the real types-registry gear on an
  in-memory database, runs its startup seeding over the whole inventory, and
  reads back the nine base schemas (equal to `BASE_SCHEMAS`) and the five
  permissions. Removing one schema from the inventory, or altering one, fails
  it.
- A seeding probe on `main` (2026-10-30, not committed) admitted the nine base
  schemas byte-identical and the three example producer types in
  `schemas/examples/` — graph traits, payload schemas and edge endpoint
  constraints — under vendor `cf`; under vendor `acme` the registry's vendor
  policy refused them.

## Pros and Cons of the Options

### 1. Registry authoritative now

- Good, because graph types become platform types at once, and the gear's
  copy of the registry's rules can go.
- Bad, because before the registry's P1 a tenant's type would be a global
  entity, visible to every tenant and published with platform credentials on
  the tenant's behalf.
- Bad, because the registry decides on schemas alone and would refuse the
  changes ADR-0006 admits on the data or with a migration.

### 2. Gear catalogue authoritative for good

- Good, because nothing changes for producers or for deployment.
- Bad, because graph types stay outside the platform namespace, and the gear
  keeps a per-gear registration mechanism the registry exists to replace.

### 3. Staged

- Good, because what the registry can hold today is published today, with no
  runtime dependency, and nothing is promised that the registry cannot honour.
- Bad, because until stage 2 producer types live in the gear only and the
  rules stay duplicated, and the stages depend on another team's releases.

## Open questions for the types-registry owners

Stage 2 and 3 are not this gear's alone to decide:

1. **Tenancy.** Is P1 tenant ownership the path for graph types, or should
   graph types of the `cf` vendor (Studio's, for instance) be registered as
   global entities before P1?
2. **Write path.** Does a producer register in the registry and the gear
   follow a change notification, or does the gear stay the front door —
   checking its rules, submitting to admission and writing its projection?
3. **Synchronous registration.** Admission is asynchronous (`202` and an
   operation); the gear's `POST /types` answers synchronously. Is awaiting the
   operation within the request deadline the intended pattern?
4. **Admission grounds.** ADR-0006 admits a change the schemas cannot prove
   compatible when every stored row validates (`revalidate`), and a change
   carried by a migration. How does a registry that decides on schemas alone
   admit those — a hook, an override, or a new major version?
5. **Import.** How are existing tenants' types (about 186 per Studio tenant)
   imported with their identity unchanged?
6. **Inventory format.** Stage 1 submits raw JSON through
   `InventoryTypeSchema` rather than `#[gts_type_schema]` structs, because the
   schemas are documents producers read. Is that acceptable as a lasting
   form?

## What this amends in the documentation

- DESIGN § `gts_type`, § Base Ontology Publication, the health matrix row
  "Types registry", and the Ontology Registry boundaries: the interim stated,
  the stages referenced.
- PRD actor *Types Registry Gear* and § Dependencies.
- The `gts_type` entity's doc comment.

## More Information

- gears-rust #5134 (the decision this answers), #5012 (tracker), #4619
  (registry P0), #4628 (registry P1), #4873 (plugins as GTS instances).
- types-registry PRD § 1.3, § 4.2; ADR-0003 (compatibility), ADR-0005
  (revisions).

## Traceability

- **PRD**: `cpt-cf-graph-storage-actor-types-registry`,
  `cpt-cf-graph-storage-fr-type-registration`
- **DESIGN**: `cpt-cf-graph-storage-dbtable-gts-type`
- **Related ADR**: `cpt-cf-graph-storage-adr-type-evolution`
