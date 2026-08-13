---
status: accepted
date: 2026-07-12
decision-makers: Constructor Fabric Steering Committee
---

# ADR-001: Setting Key Is a GTS Instance Identifier

**ID**: `cpt-cf-settings-service-adr-setting-key-gts-instance-id`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Each setting is its own registered GTS type](#each-setting-is-its-own-registered-gts-type)
  - [Admin settings are not GTS-registered; key is a category path](#admin-settings-are-not-gts-registered-key-is-a-category-path)
  - [The setting key is a GTS instance identifier](#the-setting-key-is-a-gts-instance-identifier)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Events, policies, and audit records at platform level may reference only entities that carry a Global Type System identity. A setting is such an entity: an apply event names the settings it changed, an audit record names the setting mutated, and a policy may gate on one. So a setting needs a GTS identity — but the platform's GTS Registry is an in-memory, code-derived inventory, and a setting is created at run time by an operator, not at build time by a module author.

How should a setting be identified so that both **module-contributed** and **admin-authored** settings are GTS-referenceable, without minting a registered GTS type per setting and without inventing a pseudo-vendor for operator-created content?

## Decision Drivers

* Events, policies, and audit may reference only GTS-identified entities, and that must hold for **both** authoring parties, not just modules.
* The GTS Registry holds a code-derived inventory; registering one type per setting is neither required by the GTS specification nor supported by the current Registry implementation.
* An admin-authored setting has **no natural GTS vendor** — it is created at run time by an operator, so there is no authoring party to name.
* The PRD requires the setting's leaf name to be unique within its category, and the full key to be globally unique.
* The value's shape must be validated against a curated, reusable catalog rather than a bespoke type per setting.
* The declaration and its value are separate concerns; identity belongs to the declaration and must not depend on any stored value.

## Considered Options

* **Each setting is its own registered GTS type** — mint and register a GTS type per setting.
* **Admin settings are not GTS-registered; key is a category path** — module settings stay GTS types, admin settings use `<category-chain>/<leaf>`.
* **The setting key is a GTS instance identifier** — one uniform model for both authoring parties.

## Decision Outcome

Chosen option: **"The setting key is a GTS instance identifier"**, because it is the only option that gives *every* setting a valid GTS identity — satisfying the referenceability driver for both authoring parties — while keeping the Registry to a small curated catalog rather than one entry per setting.

The GTS specification makes the type/instance split explicit: a **type** ends with `~` and must be registered; an **instance** has no trailing `~` and its registration is not mandated. The reference implementation confirms the pattern — the `resource-group` gear registers types and stores concrete groups as unregistered rows keyed inside the gear.

1. A setting's `key` is a GTS **instance** identifier `<value-type>~<setting-instance-id>`, the same shape for module and admin settings.
   * **Left** (`<value-type>`, ends with `~`): a curated value type from the catalog `gts.cf.settings.types.*~`. This is the **only** part registered in GTS. It defines the shape.
   * **Right** (`<setting-instance-id>`, no trailing `~`): the setting's own instance id, authored by the deploying party.
   * Only the **first** segment carries the `gts.` prefix; the segment after `~` does not repeat it. Each segment carries exactly **four name tokens** before its version — `vendor.package.namespace.type.vMAJOR[.MINOR]` — which is what bounds the shapes below. A worked key:

     ```text
     gts.cf.settings.types.bool_flag.v1~acme.settings.network.enable_proxy.v1
      seg1 vendor=cf   package=settings ns=types   type=bool_flag   (a type)
                            seg2 vendor=acme package=settings ns=network type=enable_proxy
     ```
2. The setting is a GTS instance and is **not** registered. Only value types occupy the Registry. Per-tenant values, overrides, and cascade stay in the Settings DB, off the Registry hot path.
3. **Admin instance id**: `<vendor>.settings.<category>.<name>.v1` — the admin supplies `<vendor>` and the leaf `<name>`; `<category>` is the slug of the category the setting is created in.
4. **Module instance id**: the module supplies its own GTS instance id; the reconciler extracts the category from the namespace segment at the `<category>` position. No per-setting type is registered — the module references a catalog value type through the key's left half.
5. **Uniqueness lives in the Settings DB**: `key` is globally unique (`uq_declaration_key`) and the leaf name is unique within its category (`UNIQUE(category_id, leaf_slug)`). There is no separate `gts_type_id` column — the value type is literally the left half of the key.
6. **Category rename or move is a re-key, for both authoring parties**, because the category segment is embedded in every key. The old key does not resolve; there is no succession or redirect.
7. A **breaking value-shape change** is a new MAJOR of the value type — the key's left half — and therefore a new key, meaning a new setting. Compatible changes keep the key.

### Consequences

* Every setting key is a valid GTS instance id, so events, policies, and audit reference module and admin settings uniformly, with no asymmetry between the two authoring parties.
* The Registry holds only a small curated value-type catalog. Adding a setting never adds a Registry entry.
* A value type must be registered **before** a setting referencing it can be created, so declaration creation is fail-closed on a Registry outage.
* Re-categorizing a setting is a breaking re-key. This is accepted: it mirrors GTS identifier immutability, and a reader gets a clean signal rather than a silent redirect to different content.
* The category slug becomes load-bearing beyond grouping, so it must be validated against the GTS grammar at category creation and treated as immutable thereafter.
* The four-token grammar caps how much structure a key may carry. `settings` occupies the package position and the category the namespace position, which leaves no room for a nested category path — flat categories are therefore a consequence of the identifier grammar, not only a product simplification.
* Grammar validation belongs to the platform GTS identifier library, not to this service. Re-implementing it produces a second source of truth that drifts: an early hand-rolled validator here accepted five-token segments the platform validator rejects, and nothing caught it until the two were compared.
* No `gts_type_id` column exists on the declaration; any code needing the value type derives it from the key's left half.
* Admin key composition needs a `<vendor>` from the operator, since the instance id carries one. This is supplied input, not a platform-invented pseudo-vendor.

### Confirmation

* SDK unit tests on the setting-key value object assert the `<value-type>~<instance-id>` split, the trailing-`~` rule on each half, GTS grammar rejection, byte-identical round-tripping, and that re-categorizing produces a different key.
* The SDK delegates grammar validation to the platform GTS identifier library, so conformance to this decision is checked by that library rather than by a local copy of the rules. A key shape that this ADR permits but the library rejects is a defect in the ADR.
* Database constraints `uq_declaration_key` and `UNIQUE(category_id, leaf_slug)` enforce the uniqueness rules independently of application code.
* Design and code review confirm that no per-setting GTS type is registered and that no `gts_type_id` column is introduced.

## Pros and Cons of the Options

### Each setting is its own registered GTS type

Mint a GTS type per setting and register it in the Registry.

* Good, because every setting is trivially GTS-referenceable.
* Good, because the value shape and the identity are the same object, so there is nothing to keep in sync.
* Bad, because it over-registers: the Registry would hold one entry per setting rather than a curated catalog.
* Bad, because it requires a GTS **vendor** per setting, which an admin-authored setting does not have.
* Bad, because the Registry is a code-derived in-memory inventory and does not support run-time per-instance registration.

### Admin settings are not GTS-registered; key is a category path

Module settings remain registered GTS types; admin settings use a human-readable `<category-chain>/<leaf>` key.

* Good, because it removes the pseudo-vendor problem for operator-created settings.
* Good, because the key is human-readable and mirrors the category tree directly.
* Neutral, because uniqueness moves into the Settings DB, which is where it ends up under any option.
* Bad, because it abandons the referenceability driver for admin settings — events and audit would reference them by opaque string only.
* Bad, because it creates a deliberate asymmetry between the two authoring parties, so every downstream consumer must handle two key shapes.

### The setting key is a GTS instance identifier

One uniform `<value-type>~<setting-instance-id>` model for both authoring parties; only value types are registered.

* Good, because it satisfies the referenceability driver for every setting without exception.
* Good, because the Registry stays a small curated catalog independent of how many settings exist.
* Good, because the value type is recoverable from the key itself, so no denormalized type column can drift.
* Neutral, because uniqueness is enforced by the Settings DB rather than by the Registry.
* Bad, because the embedded category makes re-categorization a breaking re-key.
* Bad, because a Registry outage blocks creating a setting that references an unseen value type.

## More Information

Open at the time of acceptance: reader support for an old key after a breaking value-type MAJOR change.

Upstream source: `ADR-settings-declaration-key-gts-type-202606301553`, `vhp-architecture` repository.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-settings-service-constraint-key-is-gts-instance-id` — this ADR is the rationale for that design constraint
* `cpt-cf-settings-service-fr-settings-category-model` — the category slug is embedded in the key, and per-category leaf uniqueness is enforced in the Settings DB
* `cpt-cf-settings-service-fr-typed-value-validation` — the value type validated against is the key's left half
* `cpt-cf-settings-service-fr-module-contributed-declarations` — module and admin settings share one key shape, so the reconciler and the admin path agree on identity
* `cpt-cf-settings-service-principle-consume-gts` — the service consumes curated value types and registers nothing itself
* `cpt-cf-settings-service-design-settings-service` — identity model for the declaration entity
