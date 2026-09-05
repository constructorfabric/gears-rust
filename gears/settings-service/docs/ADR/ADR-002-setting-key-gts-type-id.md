---
status: accepted
date: 2026-09-05
decision-makers: Constructor Fabric Steering Committee
---

# ADR-002: Setting Key Is a GTS Type Identifier

**ID**: `cpt-cf-settings-service-adr-setting-key-gts-type-id`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Each setting is its own registered GTS type](#each-setting-is-its-own-registered-gts-type)
  - [The setting key is a GTS instance identifier](#the-setting-key-is-a-gts-instance-identifier)
  - [Admin settings are not GTS-registered; key is a category path](#admin-settings-are-not-gts-registered-key-is-a-category-path)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Events, policies, and audit records at platform level may reference only entities that carry a Global Type System identity, and that must hold for both **module-contributed** and **admin-authored** settings. An earlier decision (ADR-001, now retired) made the setting key a GTS *instance* identifier — `<value-type>~<instance-id>` — so that no type had to be registered per setting.

Two of the three premises that decision rested on no longer hold, and the design has since taken the option it rejected. This record states why, so the reversal is traceable rather than implied by the design text alone.

How should a setting be identified so that both authoring parties are uniformly GTS-referenceable **and policy-addressable**, now that the Registry supports run-time registration and the gear can own the vendor?

## Decision Drivers

* Events, policies, and audit may reference only GTS-identified entities, for **both** authoring parties.
* A **policy must be able to name one setting** as its resource (`cpt-cf-settings-service-fr-per-setting-access`). Policy resources are GTS *types*; an instance identifier cannot be a policy target.
* The Registry now accepts run-time registration (`TypesRegistryClient::register_type_schemas`, idempotent, per-item results), which removes the constraint that only a code-derived catalog could be registered.
* An admin-authored setting still has no natural GTS vendor of its own; whatever model is chosen must supply one without inventing a pseudo-vendor.
* The value's *shape* must keep coming from a curated, reviewed catalog; the service must not become a second type system.
* The leaf name is unique within its category and the full key globally unique, enforced in the Settings DB.

## Considered Options

* **Each setting is its own registered GTS type** — a gear-owned abstract base plus one derived type per setting, registered when the declaration is created.
* **The setting key is a GTS instance identifier** — the retired ADR-001 model: only value types are registered.
* **Admin settings are not GTS-registered; key is a category path** — module settings stay GTS types, admin settings use `<category-chain>/<leaf>`.

## Decision Outcome

Chosen option: **"Each setting is its own registered GTS type"**, because it is the only option under which a setting is both GTS-referenceable and a valid **policy resource** for both authoring parties, and because the two objections ADR-001 raised against it have been answered by the platform rather than argued away.

The key is `gts.cf.core.settings.setting_type.v1~<vendor>.<package>.<category>.<name>.v1~`:

1. The **abstract base** `gts.cf.core.settings.setting_type.v1~` is owned by the Settings gear, defined in its SDK, and registered at gear init. It is not part of `libs/toolkit-gts`.
2. The **derived half** is the setting. It carries exactly four name tokens before its version — `<vendor>.<package>.<category>.<name>` — with no `gts.` prefix of its own. The **category is always the third token**: a module supplies its own half and the category is read from `<namespace>`; an admin setting is composed as `<vendor>.settings.<category>.<name>.v1`, the admin supplying `<vendor>` and `<name>`.
3. The trailing `~` makes the key a **type**, not an instance — which is what lets a policy name a single setting.
4. Both authoring paths call `register_type_schemas` **before** inserting the declaration row, composing the concrete schema from the base and the setting's `value_type_id`. The composed schema carries **no `default`**: the Schema Default lives in the `default_value` column alone. Registration is idempotent, so a retried create re-registers the same type rather than minting a second one.
5. The value's **shape** remains a separate, toolkit-owned catalog type (`gts.cf.toolkit.settings.type_*~`) named by `value_type_id`. Identity and shape are two objects on purpose; the service still never invents value shapes.
6. Uniqueness stays in the Settings DB: `uq_declaration_key` on `key`, `UNIQUE(category_id, leaf_slug)` among active declarations.

### Consequences

* Every setting key is a valid GTS type id, so declaration events and audit reference module and admin settings uniformly, and a policy can target exactly one setting. There is no admin-vs-module referenceability asymmetry.
* Setting types and value types both occupy the Registry. Per-tenant values and overrides stay in the Settings DB, off the Registry hot path; nothing on the read path asks the Registry.
* Declaration creation depends on the Registry: the base must be registered first (the Registry rejects a child whose parent is absent with `FailedPrecondition`), and a create that fails to register leaves no declaration behind.
* Retiring a declaration does **not** unregister its type — the Registry has no unregister operation — which is what makes re-declare-to-revive a lookup rather than a re-mint.
* The category slug is load-bearing: it is the third token of every key declared under it, so renaming a category would re-key every one of them. Category `key` is therefore refused on update, and `algo-key-construction` in the declarations FEATURE composes keys on these terms.
* The SDK's setting-key value object must treat the derived half as a **type** (trailing `~` required) and read the value type from `value_type_id` rather than from the key's left half. ADR-001's SDK contract is superseded on both points.

### Confirmation

* SDK unit tests on the setting-key value object assert the `<base>~<derived>~` shape, the mandatory trailing `~` on the derived half, the four-token derived grammar with the category in third position, GTS grammar rejection via the platform identifier library, and byte-identical round-tripping.
* Declaration-creation tests assert that `register_type_schemas` is called before the row is inserted, that the composed schema carries no `default`, and that a registration failure leaves no row.
* Database constraints `uq_declaration_key` and `UNIQUE(category_id, leaf_slug)` enforce uniqueness independently of application code.
* Design and code review confirm that value shapes are never composed by this gear and that no read-path code consults the Registry for identity.

## Pros and Cons of the Options

### Each setting is its own registered GTS type

A gear-owned abstract base, one derived type per setting, registered when the declaration is created.

* Good, because every setting is GTS-referenceable **and** policy-addressable, for both authoring parties.
* Good, because the vendor problem dissolves: the base supplies the gear-owned root, the derived half carries the author's own vendor, and no pseudo-vendor is invented.
* Good, because the Registry now supports exactly this: idempotent run-time registration with per-item results.
* Neutral, because the Registry holds one type per setting. ADR-001 counted this as over-registration; it is the price of policy addressability, and the read path never pays it.
* Bad, because declaration creation acquires a Registry dependency and fails closed on a Registry outage.
* Bad, because there is no unregister, so retired settings leave their types resolvable forever.

### The setting key is a GTS instance identifier

The retired model: `<value-type>~<instance-id>`, only value types registered.

* Good, because the Registry stays a small curated catalog regardless of how many settings exist.
* Good, because the value type is recoverable from the key itself.
* Bad, because an instance identifier is **not a policy resource**: per-setting access control cannot name it.
* Bad, because two of its three founding premises are false today — the Registry does support run-time registration, and the gear can own the vendor.
* Bad, because it embeds the value type in the identity, so a breaking value-shape change is a new setting rather than an evolution of the same one.

### Admin settings are not GTS-registered; key is a category path

Module settings remain GTS types; admin settings use `<category-chain>/<leaf>`.

* Good, because it avoids the vendor problem for operator-created settings.
* Bad, because it abandons referenceability and policy addressability for admin settings entirely.
* Bad, because every downstream consumer must handle two key shapes.

## More Information

Supersedes ADR-001 *Setting Key Is a GTS Instance Identifier* (retired; the decision it recorded is reversed by this one). Of ADR-001's three objections to the chosen option, two were premises about the platform that have since changed — Registry run-time registration exists, and the vendor is supplied by a gear-owned base — and the third, one Registry entry per setting, is accepted as the cost of making a setting a policy resource.

The design records the decision on its own terms in DESIGN.md §6 *Open Questions* ("Setting identity — RESOLVED") and specifies it in §3 *Setting key by author* and §4.7 *When the type is registered*.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-settings-service-constraint-key-is-gts-type-id` — this ADR is the rationale for that design constraint
* `cpt-cf-settings-service-constraint-gts-value-validation` — identity and value shape are separate objects; the shape still comes from the curated catalog
* `cpt-cf-settings-service-fr-per-setting-access` — a setting is a GTS type so that a policy can name it as its resource
* `cpt-cf-settings-service-fr-settings-category-model` — the category slug is the third token of every key declared under it, and per-category leaf uniqueness is enforced in the Settings DB
* `cpt-cf-settings-service-fr-typed-value-validation` — the value type validated against is `value_type_id`, no longer derived from the key
* `cpt-cf-settings-service-fr-module-contributed-declarations` — module and admin settings share one key shape and one registration path
* `cpt-cf-settings-service-nfr-versatility-gts-scope-class` — new settings register their own type; the curated value-type catalog needs no core change
* `cpt-cf-settings-service-principle-consume-gts` — the gear registers setting types but composes no value shapes, and nothing on the read path asks the Registry
* `cpt-cf-settings-service-design-settings-service` — identity model for the declaration entity
