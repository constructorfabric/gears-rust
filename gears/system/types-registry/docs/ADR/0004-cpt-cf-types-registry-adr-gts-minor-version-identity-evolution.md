---
status: accepted
date: 2026-07-26
decision-makers: Constructor Fabric Steering Committee
---

# GTS Minor-Version and Identity-Evolution Policy

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Managed identity rules](#managed-identity-rules)
  - [What a managed version family is](#what-a-managed-version-family-is)
  - [Lifecycle of family members](#lifecycle-of-family-members)
  - [Reference and derivation rules](#reference-and-derivation-rules)
  - [Exact resolution versus patterns](#exact-resolution-versus-patterns)
  - [Externally managed entities](#externally-managed-entities)
  - [What counts as an identifier conflict](#what-counts-as-an-identifier-conflict)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Mandatory minor version, immutable entity](#mandatory-minor-version-immutable-entity)
  - [Mandatory minor version, mutable entity](#mandatory-minor-version-mutable-entity)
  - [No minor version, immutable entity](#no-minor-version-immutable-entity)
  - [No minor version, mutable logical entity](#no-minor-version-mutable-logical-entity)
  - [Two family-level evolution modes](#two-family-level-evolution-modes)
- [More Information](#more-information)
  - [Industry Practice](#industry-practice)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-types-registry-adr-gts-minor-version-identity-evolution`

## Context and Problem Statement

GTS identifiers permit a major version and an optional minor version. Types Registry must decide whether managed GTS Type Schemas and registered GTS Instances require a minor version, allow each owning gear to choose whether to use one, or prohibit minor versions in the platform-managed identity profile.

The choice is coupled to identity mutability:

* an immutable minor-versioned entity can publish a new compatible definition under a new minor GTS ID;
* a mutable major-only entity can publish a compatible definition without changing its GTS ID;
* an incompatible definition must remain distinguishable from the old contract through a new major GTS ID.

Optional minor versions appear flexible but give otherwise similar GTS IDs different stability semantics. A `$ref` may be immutable and pinned in one family but mutable and floating in another. A major-only GTS ID can also be both a concrete identifier and, in pattern matching, a selector that covers minor-versioned candidates. This ambiguity affects exact resolution, deterministic Registry References, caches, derived types, federation, and compatibility queries.

This ADR establishes the platform-facing identity policy. The enforced Type Schema Evolution Compatibility mode is decided by ADR-0003. Internal revision representation and retention are decided separately by ADR-0005 and ADR-0006. The lifecycle of the members of a version family — how many may be usable at once, and whether deprecation exists — is decided by ADR-0008.

## Scope

This ADR decides:

* whether minor versions are allowed in newly registered managed GTS identifiers;
* whether compatible content updates preserve the GTS ID;
* when a new major GTS ID is required;
* what constitutes a managed version family, including for chained identifiers with more than one versioned segment;
* whether derived types and references are rewritten automatically;
* how exact identity resolution differs from compatible-version and wildcard matching;
* how the policy applies to externally managed entities;
* what counts as a conflict between two definitions submitted under one managed GTS identifier.

This ADR does not define database tables, revision retention, rollback APIs, or the concrete compatibility algorithm.

## Decision Drivers

* Domain gears need stable Registry References and should not migrate stored references for every compatible contract update.
* A GTS ID must have one predictable stability meaning for platform-managed entities.
* Automatic cloning of derived types or rewriting of `$ref` targets would publish owner-visible contracts without owner intent.
* Compatibility and dependency checks should be centralized in Types Registry rather than reimplemented by every gear.
* Exact identifier resolution must remain deterministic and must not silently select a different minor version.
* External Registry Sources may already have authoritative versioning semantics that Types Registry must preserve.
* The platform-approved `gts-rust` implementation currently supports minor versions and assumes versioned IDs are normally immutable; adopting a different managed profile must be explicit.

## Considered Options

* Mandatory minor version, immutable entity.
* Mandatory minor version, mutable entity.
* No minor version, immutable entity.
* No minor version, mutable logical entity.
* Two family-level evolution modes.

## Decision Outcome

Chosen option for Types Registry-managed entities: no minor version and a mutable logical entity within one major GTS identity.

### Managed identity rules

* Newly registered managed GTS Type Schemas and registered GTS Instances must omit the minor version from every platform-owned concrete GTS ID segment. P2 managed GTS Aliases use the same rule when introduced.
* A managed GTS ID identifies one logical entity within one major version, not one immutable content snapshot.
* A content update that is backward compatible under ADR-0003 preserves the same GTS ID and Registry Reference.
* A backward-incompatible Type Schema change requires a new major GTS ID.
* Registered Instance content may change under the same major-only GTS ID according to ADR-0006; schema compatibility terminology does not apply to successive Instance values.
* Minor-version policy is not configurable per managed gear, tenant, Type Schema family, or update request.

### What a managed version family is

A GTS Identifier can carry a version in more than one position. For a chained identifier such as `A.v1~B.v1~`, both `A` and `B` are versioned, so "the version family" needs a definition before the lifecycle rules below can be applied.

A managed version family is identified by the canonical GTS Identifier with the **major version of its last segment removed** and the trailing `~` of a Type Identifier normalized away, with every preceding segment held exactly as written.

```text
family(gts.acme.crm.customer.type.v1~)                 = (gts.acme.crm.customer.type)
family(gts.acme.crm.customer.type.v2~)                 = (gts.acme.crm.customer.type)          -- same family
family(gts.cf.core.events.type.v1~acme.crm.order.type.v1~)  = (gts.cf.core.events.type.v1~, acme.crm.order.type)
family(gts.cf.core.events.type.v1~acme.crm.order.type.v2~)  = (gts.cf.core.events.type.v1~, acme.crm.order.type)  -- same family
family(gts.cf.core.events.type.v2~acme.crm.order.type.v1~)  = (gts.cf.core.events.type.v2~, acme.crm.order.type)  -- DIFFERENT family
```

The consequences are deliberate:

* A major bump of a base type says nothing about anything derived from it. Types derived from the `v1` base keep their own lifecycle, which is required because their owners may be other gears or other tenants and this ADR already forbids publishing owner-visible contracts without owner intent.
* Adopting a new base major means admitting a **new logical entity in a new family**. `gts.cf.core.events.type.v2~acme.crm.order.type.v1~` does not succeed `gts.cf.core.events.type.v1~acme.crm.order.type.v1~`; the two are unrelated by version succession even though their identifiers look like siblings.
* A derived entity's status is independent of the status of the base it chains through; every non-deleted base remains a valid reference and derivation target.
* Version succession is therefore always a change in exactly one identifier segment — the last one. Types Registry never infers succession across a difference in any preceding segment.
* Normalizing the kind marker away makes a family name **exclusive across kinds**: a derived Type Schema `gts.A~acme.crm.order.type.v1~` and a well-known registered Instance `gts.A~acme.crm.order.type.v1` map to the same family, and Types Registry admits whichever arrives first and refuses the other. This is a managed-profile restriction that GTS itself does not impose, alongside the prohibitions on minor versions here and on an explicit UUID tail in ADR-0001. It is adopted because the two identifiers differ by one character while denoting entirely unrelated things, nothing needs both — an Instance of that derived type is `gts.A~acme.crm.order.type.v1~<segment>`, not the colliding form — and because a family groups Version Successors, which are by definition of one kind. Keeping the marker in the key would instead let both families exist and force a shared owner on them, rejecting the second registrant over a family it may not be able to see.

### Lifecycle of family members

What happens to the members of a family over time — whether more than one may be usable at once, how a consumer learns which is newest, and whether deprecation exists — is decided by [ADR-0008](./0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md). Only one property is stated here, because it is a property of identity rather than of lifecycle: admitting a compatible internal content revision preserves the logical entity's Lifecycle Status, since the revision is not a new member of the family.

### Reference and derivation rules

* A managed `$ref` to a major-only GTS Type Schema is a floating reference to the current admitted revision of that logical entity.
* A reference remains valid for as long as its target is not deleted; no lifecycle metadata rewrites or invalidates a reference.
* Updating a referenced or base Type Schema does not rewrite the `$ref` string or the dependent GTS ID.
* Types Registry must revalidate affected registered dependency closure before activating the new base or referenced revision under ADR-0005.
* Types Registry must not automatically create a new derived Type Schema when a base Type Schema changes.
* Tooling may generate a candidate derived definition for owner review, but registration and activation require an explicit owner operation.

### Exact resolution versus patterns

* Exact resolution is literal: it resolves only the entity whose canonical GTS ID equals the supplied identifier.
* Exact resolution must never use minor-version flexibility, compatible-version expansion, or implicit pattern coverage.
* Compatible-version, hierarchy, and wildcard queries are separate operations and may return multiple exact identifiers or Registry References.
* A major-only ID used as a GTS pattern may cover minor-versioned externally managed candidates according to `gts-rust`; that behavior does not change exact resolution.

### Externally managed entities

The managed major-only policy does not rewrite authoritative external identities.

* An External Registry Source may expose minor-versioned GTS IDs, major-only GTS IDs, or another source-owned revision convention.
* Types Registry preserves and returns the exact external GTS ID without storing or normalizing it.
* Types Registry does not synthesize a major-only GTS ID for `v1.0`, automatically advance references to `v1.1`, or claim that a source-owned immutable ID is mutable.
* Every live plugin response must provide an opaque `external_revision` and canonical `content_hash`.
* The same `external_revision` for one exact entity must always identify the same content and hash, and changed canonical content must produce a different revision.
* Types Registry does not require or persist an external versioning profile and does not interpret source revision ordering.
* The External Registry Source remains responsible for its evolution and compatibility rules; Types Registry applies only the federation response checks defined by ADR-0002 and makes no compatibility claim about source-owned content.
* The managed version-family definition is a property of Types Registry storage and is not imposed on an External Registry Source. How source lifecycle assertions map onto the platform model is decided by ADR-0008.

### What counts as an identifier conflict

A managed GTS ID names a mutable logical entity, so two differing definitions under one identifier are not conflicting by that fact alone. What separates the two cases is revision lineage:

* sequential managed definitions admitted through ADR-0005 or ADR-0006 are revisions of one logical identity and retain the same Registry Reference. A later definition differing from an earlier one is evolution, not an identity collision;
* concurrent definitions that share no admitted revision lineage are conflicts;
* a stale registrar cannot replace the current revision with an older or divergent definition, which the per-candidate precondition of ADR-0012 enforces rather than this rule.

This concerns only which submissions conflict. Registry Reference representation and the exact forward and reverse identity guarantees are decided by ADR-0001 and are untouched by identity mutability: the reference names the logical entity, and every revision of it resolves under the same reference.

### Consequences

* Gear-owned domain rows do not require reference migration for compatible managed Type Schema evolution.
* A Registry Reference identifies the logical entity, not the schema revision used at one historical moment.
* Resolution results and caches must include revision or freshness metadata and cannot assume that a major-only managed GTS ID is immutable.
* The platform must adapt or wrap `gts-rust` behavior that assumes versioned IDs are append-only and resolved schemas can be cached forever.
* Type Schema and Instance revisions become correctness mechanisms rather than public GTS version components.
* Keying a family on the last identifier segment means version succession never crosses a derivation chain. Adopting a new base major produces an entity in a different family, so a derivation chain can hold simultaneously active entities that look like version siblings. Diagnostics, discovery, and documentation must present the family key rather than let readers infer succession from identifier similarity.
* Managed and external entities expose different revision ownership: Types Registry owns managed revision history, while a Registry Source Plugin supplies live opaque revisions for external entities.
* Existing managed minor-versioned entities, if any, require a separately planned migration and coexistence policy; this ADR governs new registration and does not silently rename existing identities.

### Confirmation

This decision is confirmed when:

* managed registration rejects concrete GTS IDs containing a minor version;
* exact resolution is tested separately from pattern and compatible-version resolution;
* a compatible managed update preserves the GTS ID and Registry Reference while changing the current internal revision;
* an incompatible update under the same managed GTS ID is rejected and requires a new major identity;
* the family key is computed from the last identifier segment only, so `A.v1~B.v1~` and `A.v2~B.v1~` resolve to different families and neither succeeds the other;
* a derived Type Schema and a well-known registered Instance whose identifiers differ only by the trailing `~` resolve to one family, and the second of them to be registered is refused whatever the order of arrival and whatever their owners;
* admitting an internal content revision changes no Lifecycle Status;
* reference validation accepts every visible and tenant-available non-deleted target, whatever its Lifecycle Status;
* dependent schemas and references are revalidated without automatic ID or `$ref` rewriting;
* external minor-versioned identities are resolved live without normalization or synthetic managed IDs;
* plugin contract tests reject a source that returns different canonical content or content hashes for the same external revision, without requiring Types Registry to persist external revision history;
* tests distinguish a valid sequential revision of one identity from a divergent definition that shares no admitted revision lineage with it.

## Pros and Cons of the Options

### Mandatory minor version, immutable entity

Backward-compatible changes create a new minor GTS ID; incompatible changes create a new major GTS ID.

* Good, because every GTS ID identifies one immutable definition.
* Good, because references and resolved schemas are reproducible and simple to cache.
* Good, because it follows the current `gts-rust` append-only version assumption.
* Bad, because compatible changes create new identities, Registry References, query results, and migration choices.
* Bad, because derived types and references remain pinned and require explicit owner-driven successors when adoption is desired.

### Mandatory minor version, mutable entity

* Good, because a family retains explicit minor labels.
* Bad, because the label no longer identifies immutable content.
* Bad, because there is no clear rule for when to increment the minor version.
* Bad, because it combines floating references with version-shaped identifiers and provides the weakest mental model.

### No minor version, immutable entity

Any content change creates a new major GTS ID.

* Good, because identity and content remain immutable.
* Bad, because compatible and incompatible changes both cause major-version churn.
* Bad, because compatibility can still be computed but is not reflected usefully in the public identity model.

### No minor version, mutable logical entity

Compatible changes update the current definition without changing the GTS ID; incompatible changes create a new major GTS ID.

* Good, because stored Registry References and exact `$ref` strings remain stable across compatible evolution.
* Good, because consumers see a major-version channel instead of a sequence of compatible public identities.
* Good, because the Types Registry compatibility and dependency engine owns evolution complexity centrally.
* Bad, because references become floating with respect to internal revisions.
* Bad, because caches, validation, dependency closure, concurrency, and diagnostics require revision-aware behavior.
* Bad, because it intentionally differs from the current `gts-rust` assumption that versioned IDs are immutable and safely cacheable forever.

### Two family-level evolution modes

Types Registry could support two explicit managed evolution modes:

* `FLOATING_MAJOR`: a major-only GTS ID such as `gts.acme.crm.customer.type.v1~` identifies a mutable logical entity. Compatible changes create retained internal revisions under the same GTS ID and Registry Reference.
* `PINNED_MINOR`: minor-versioned GTS IDs such as `gts.acme.crm.customer.type.v1.0~` and `gts.acme.crm.customer.type.v1.1~` identify separate immutable logical entities. A compatible change creates a higher-minor Version Successor with a new Registry Reference; an incompatible change creates a higher-major Version Successor.

The mode would be selected when the first entity in a managed major family is admitted and would then be immutable for that family.

* Good, because each owner gets the model their contract needs: an API-version channel where stored references should survive compatible evolution, an immutable schema-registry identity where consumers need reproducible resolution and explicit adoption.
* Good, because publishing a new pinned minor leaves existing references and derived types on the previous minor untouched, so a successor never mutates the dependency closure of its predecessor.
* Bad, because it produces two admission, compatibility, and lifecycle models inside one registry. Almost every other contract — comparison baseline, dependency revalidation, `latest` semantics, Concrete Reference Set expansion — would need defined behaviour in both, and Managed Instances and P2 Aliases would need their own mode rules because Instance values have no compatibility relation.
* Bad, because the owner must make an effectively irreversible identity decision at first admission, before the consumers, derived types, data lifetime, and deployment topology that would inform it are known.
* Bad, because `PINNED_MINOR` does not actually deliver immutability in a chained type system: `base.v1~derived.v1.0~` still changes when the floating `base.v1~` advances, so it would have to promise immutability only for an entity's own canonical content or require its entire reference closure to be pinned too.

This option is not selected for P1. It may be reconsidered when a concrete scenario requires public pinned identities or reproducibility that retained internal revisions and explicit revision provenance cannot provide; that reconsideration must first settle effective-contract pinning and whether the option applies beyond Type Schemas.

## More Information

### Industry Practice

* [Google AIP-185](https://google.aip.dev/185) requires Google APIs to expose a major version such as `v1`, not minor or patch versions, and updates the major channel in place with compatible functionality.
* [Kubernetes API deprecation policy](https://kubernetes.io/docs/reference/using-api/deprecation-policy/) preserves API elements and significant behavior within an existing API version and introduces a new version for incompatible evolution.
* [GitHub REST API breaking-change policy](https://docs.github.com/en/rest/about-the-rest-api/breaking-changes) applies additive changes to supported API versions and places breaking changes in a new API version.
* [Confluent Schema Registry](https://docs.confluent.io/platform/current/schema-registry/fundamentals/schema-evolution.html) represents the alternative immutable-history model: a stable subject owns monotonically increasing schema versions checked under a compatibility policy.

The selected managed model follows major-version API channels externally while ADR-0005 retains immutable internal schema revisions in the style of schema registries.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0003**: [0003-cpt-cf-types-registry-adr-type-schema-evolution-compatibility.md](./0003-cpt-cf-types-registry-adr-type-schema-evolution-compatibility.md)
- **ADR-0005**: [0005-cpt-cf-types-registry-adr-type-schema-revisions.md](./0005-cpt-cf-types-registry-adr-type-schema-revisions.md)
- **ADR-0006**: [0006-cpt-cf-types-registry-adr-registered-instance-revisions.md](./0006-cpt-cf-types-registry-adr-registered-instance-revisions.md)
- **ADR-0007**: [0007-cpt-cf-types-registry-adr-federated-source-routing-query.md](./0007-cpt-cf-types-registry-adr-federated-source-routing-query.md)
- **ADR-0008**: [0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md](./0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md)

This decision directly addresses:

* `cpt-cf-types-registry-fr-gts-validation` - defines the platform profile for managed GTS version semantics.
* `cpt-cf-types-registry-fr-validate-schema-compat` - maps compatible managed changes to in-place revisions and incompatible changes to a new major identity.
* `cpt-cf-types-registry-fr-id-resolution` - separates exact identity resolution from pattern and compatible-version expansion.
* `cpt-cf-types-registry-fr-ref-tracking` - makes dependent revalidation mandatory when a floating managed reference changes revision.
* `cpt-cf-types-registry-fr-lifecycle` - defines the version family that lifecycle transitions operate on; the transitions themselves are decided by ADR-0008.
* `cpt-cf-types-registry-fr-externally-managed-entities` - preserves authoritative external minor and revision semantics.
