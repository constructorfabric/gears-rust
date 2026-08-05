---
status: accepted
date: 2026-07-23
decision-makers: Constructor Fabric Steering Committee
---

# Live External Registry Source Delegation and Tenant Enablement State Model

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [External entity ownership](#external-entity-ownership)
  - [Live external response contract](#live-external-response-contract)
  - [The plugin resolves its own type chains](#the-plugin-resolves-its-own-type-chains)
  - [Platform admission](#platform-admission)
  - [What the plugin is asked, and what it must not answer](#what-the-plugin-is-asked-and-what-it-must-not-answer)
  - [Tenant enablement and availability](#tenant-enablement-and-availability)
  - [Source failures](#source-failures)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Always delegate externally managed definitions and tenant state](#always-delegate-externally-managed-definitions-and-tenant-state)
  - [Project definitions and tenant state locally](#project-definitions-and-tenant-state-locally)
  - [Project definitions locally and read tenant state live](#project-definitions-locally-and-read-tenant-state-live)
  - [Allow each plugin to choose projection or live delegation](#allow-each-plugin-to-choose-projection-or-live-delegation)
- [More Information](#more-information)
  - [What later decisions settled](#what-later-decisions-settled)
  - [What would reopen this decision](#what-would-reopen-this-decision)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-types-registry-adr-external-source-live-delegation`

## Context and Problem Statement

Types Registry must expose one platform-facing contract over both locally managed registry entities and entities whose source of truth is an external vendor registry or contract catalog. The platform must decide whether it replicates externally managed definitions into local projections or resolves them live through Registry Source Plugins.

Every registry entity may also have tenant-specific enablement state. For Externally Managed Entities, both definition content and tenant enablement remain authoritative in the External Registry Source. `DELETED` is a lifecycle status of the entity, not a Tenant Enablement State.

This ADR selects the data-ownership boundary. Ordered source routing, UUID resolution, wildcard federation, and pagination are defined by ADR-0007.

## Scope

This ADR decides:

* whether Types Registry persists external Type Schema or registered Instance content;
* which component owns external definition caching, querying, revision history, and Registry Reference retention;
* the response metadata required from Registry Source Plugins;
* how externally managed tenant enablement state is obtained;
* the platform-admission and source-unavailability boundary.

This ADR does not define plugin ordering, GTS pattern intersection, federated pagination, or concrete SDK method signatures.

## Decision Drivers

* External Registry Sources remain authoritative for their definitions, revisions, lifecycle assertions, and tenant enablement states.
* Types Registry should not maintain synchronization workers, projection invalidation, or replicated external revision history when a source plugin can provide live registry semantics.
* Regular gears must continue to use one Types Registry API and must not consume Registry Source Plugins directly.
* Plugins may use source-native caches and indexes, but their results must conform to one platform contract.
* Registry Reference resolution must remain stable after external deletion while platform domain objects may still store the reference.
* Tenant-specific usability decisions must fail closed when authoritative external state cannot be obtained.
* Platform visibility, authorization, identifier validation, and availability semantics must not be bypassed by a plugin.

## Considered Options

* Always delegate externally managed definitions and tenant state.
* Project definitions and tenant state locally.
* Project definitions locally and read tenant state live.
* Allow each plugin to choose projection or live delegation.

## Decision Outcome

Chosen option: always delegate externally managed definitions and tenant state to the owning Registry Source Plugin.

### External entity ownership

ADR-0011 states the rule this enumeration expresses — Types Registry persists no state whose authority belongs to a source — and, because it closes the managed–external boundary in both directions, the rule and the enumeration below agree with no exception. Types Registry does not persist externally managed:

* Type Schema or registered Instance content;
* external entity identifiers;
* external revisions or content hashes;
* definition, dependency, lifecycle, or search projections;
* UUID-to-GTS-Identifier mappings;
* source-owned tenant enablement state;
* source-owned caches, revision history, or tombstones.

No item above is narrowed, and the one exception that could be argued for does not arise. That exception would be an external entity identifier held as the label on a dependency edge toward a Managed Entity, to make deletion safety decidable locally. Under ADR-0011's closed boundary no Externally Managed Entity may depend on a Managed Entity, so there is no external dependent to label, and nothing about an external entity appears in Types Registry storage.

The owning Registry Source Plugin is responsible for all of that state. Managed Type Schemas, registered Instances, revisions, dependencies, Registry Reference mappings, and tenant enablement remain fully owned and persisted by Types Registry. Managed Aliases follow the same ownership rule when the strictly P2 Alias capability is introduced.

Registry Source Plugin registration and routing configuration are Managed registered Instances of platform-defined control-plane types, validated by a built-in validator inside Types Registry rather than by a P2 Validation Hook (ADR-0012). They are not external entity projections and not platform configuration files: Source Claim invariants are statements about registry state and can only be checked against it at write time.

An active Source Claim must satisfy ADR-0007's P1 minimum capability and completeness profile for every claimed entity kind. Types Registry rejects plugin configuration activation when an applicable mandatory capability is missing.

### Live external response contract

Every live external entity result must include at least:

* the exact canonical GTS Identifier;
* entity kind and canonical content — the authored document, in the same slot a Managed Entity's authored document occupies;
* for a Type Schema, the resolved effective schema and the effective trait artifacts, computed by the plugin;
* an opaque `external_revision`;
* a canonical `content_hash`;
* the ownership scope: platform-wide, or the identifier of the one tenant that owns the entity. This is mandatory, so there is no default to get wrong; an absent scope, or one naming a tenant the platform does not know, is an `INVALID_SOURCE_RESPONSE`. The plugin states this flat fact only — Types Registry expands it into the descendant-visibility relation itself, and the assertion confers no authority, since no write path reaches an external entity (ADR-0009);
* tenant enablement state when the operation requires tenant-specific availability.

### The plugin resolves its own type chains

Producing the resolved effective schema and the effective trait artifacts is a **mandatory** capability, not an optional one, and the decision turns on what the alternative leaves a consumer holding.

If a plugin could omit them, a consumer needing the effective form of an external type would have to fetch every base in the chain and resolve it locally — several round trips and a reimplementation of GTS resolution in every consumer, which is the duplication Types Registry exists to remove. There is no degraded mode here: an absent resolved schema is a dead end rather than a smaller answer, because Types Registry will not produce one either. ADR-0011 forbids external documents from entering a managed resolution closure and ADR-0014 forbids reading their `$schema`, so the platform cannot fill the gap on the plugin's behalf.

The obligation is affordable because a plugin is already a full registry adapter rather than a translation shim, and this ADR's own consequences say so: batch resolving, querying, pagination, revision and freshness semantics, Registry Reference retention, and tombstones are all on it already. It is also well-posed because ADR-0011 closes the boundary: an externally managed entity's whole derivation chain lies inside one Source Claim and is served by one plugin, so the plugin holds every document it needs. The plugin may keep its own storage and computation to do it; the obligation is on the plugin, not on the vendor catalogue behind it.

Two consequences follow.

Conformance testing must cover **how** the artifacts are computed, not only that they are returned and stable. `$ref` inlining, `allOf` composition along the `$id` chain, trait-value merge under JSON Merge Patch per GTS §9.7.5, and materialization of declared defaults before the completeness check all have specified semantics; a plugin that returns a plausibly-shaped but differently-computed artifact hands consumers a silently wrong schema, which is worse than returning nothing.

The shape is uniform across origins and the guarantee is not, and that must be documented rather than inferred. For a Managed Entity a resolved schema means the closure was Draft-07 throughout, every base was checked for derivation compatibility, and the revision chain is backward compatible. For an Externally Managed Entity it means the source computed it under its own rules, of which Types Registry validates none. A practical corollary: the dialect may be anything GTS admits, since ADR-0014 forbids the platform from inspecting it, so a consumer validating against an external resolved schema must be prepared for Draft 2019-09 or 2020-12.

Per-level evolvability and the frozen-chain state are deliberately **not** required of a plugin. They are not resolution artifacts but metadata about the platform's own compatibility policy, which is not enforced on the external side; asking a source to report compliance with a mode the platform does not apply to it would be asking for a number with no meaning.

Source lifecycle assertions must map to the platform `ACTIVE` or `DELETED` Lifecycle Status. That vocabulary has two values in P1 for every origin: ADR-0008 defers `DEPRECATED` on the external side as well as the managed one, so there is no third value to map onto.

A source may still assert that an entity is deprecated, and Types Registry exposes such an entity as `ACTIVE`. That is not an approximation: a deprecated entity discourages new adoption and is otherwise fully usable, so `ACTIVE` says what is true of it in P1. Types Registry must not require the source to identify a successor, because ADR-0008 decides that deprecation is owner intent rather than a consequence of version succession. The assertion is not relayed to consumers, and the plugin contract must state that plainly, because a vendor whose registry deprecates types will otherwise assume the signal reaches them. A source may transition an entity directly to `DELETED` whether or not it previously deprecated it. A source-side pending candidate is not an Externally Managed logical entity and must not be returned through ordinary federation resolving or discovery. A `DELETED` source entity may be returned only by an operation whose contract explicitly includes tombstone or history information. **The exact read is such an operation**, whether the caller supplies a GTS Identifier or a Registry Reference: `lifecycle_status` sits in its default field set precisely so that a tombstone can be read, which is how a gear holding a stored reference distinguishes a retired contract from an identifier that never existed (DESIGN §3.3, *Read results*). The restriction therefore bites on discovery, search, and query assistance, which exclude deleted entities entirely — none of them is answering about a key the caller named. Reverse resolution of a deleted entity likewise succeeds and reports it deleted; that is what the source's retained tombstone is for. The source must never rebind its GTS Identifier or Registry Reference to a different logical entity.

For one exact external entity:

* the same `external_revision` must always identify the same canonical content and `content_hash`;
* changed canonical content must produce a different `external_revision`;
* Types Registry does not assume that revisions are numeric, monotonic, or comparable across entities or sources.

Revision and hash are protocol metadata. Types Registry validates the returned hash against canonical content when that content is present, and validates revision/hash consistency against caller- or cache-supplied conditional metadata when available. Cross-request conformance of a plugin's revision contract is verified by plugin contract tests and source monitoring; it does not require Types Registry to persist prior values. Types Registry exposes the metadata for conditional requests, which `cpt-cf-types-registry-fr-cache-freshness-metadata` makes a P1 obligation rather than a permission, and delegates the conditional read itself to the owning plugin through the capability required above. It does not persist the metadata as registry state.

### Platform admission

The External Registry Source remains responsible for source-owned schema, instance, evolution, and derivation validation. Types Registry validates every live result's GTS envelope, Registry Reference mapping, source-pattern claim, authorization, tenant visibility, and lifecycle exposure before returning it as usable. It validates nothing about the content, and there is no further platform check a result must satisfy: ADR-0011 closes the boundary, so no Managed Entity can depend on an external one, and a consumer reading an external entity directly decides for itself what it needs of it.

Any source assertion used for admission must be bound to the returned `external_revision` and `content_hash`. P1 does not persist external admission receipts. A stateful admission mechanism for external entities requires a future ADR.

External Registry Sources cannot provide Aliases, and Externally Managed Entities cannot be Alias targets.

### What the plugin is asked, and what it must not answer

A plugin call carries the `SecurityContext` — the platform rule for every in-process call, and plugins are in-process in P1 — together with the tenant the question is about. That tenant is **optional**, because a platform-plane read has no tenant and therefore asks no tenant-specific question at all.

A plugin **MAY** apply its own checks on top of the platform's, and those checks **MAY only deny**. Narrowing is safe: the worst outcome is an entity the caller cannot see, which is indistinguishable from one that does not exist. Widening is not available to a plugin, because access is a platform decision and remains one; a source that could grant what Types Registry refused would place an authorization outcome in a component the platform does not operate. This is the same directional rule the ownership assertion above does *not* need — there a source speaks about its own content, here it would be speaking about a platform decision.

A plugin does not supply a `resource_version` and Types Registry does not ask for one. That value exists solely as the optimistic-concurrency precondition of a write, PRD §4.2 keeps authoritative management of external sources out of scope, and a token supplied for an operation that does not exist would be a lever attached to nothing — a plugin returning a constant would look like concurrency control while detecting no conflict. Freshness is carried uniformly across both origins by the validator instead.

### Tenant enablement and availability

The division of labour is that the plugin supplies **inputs** and Types Registry reaches the **verdict**. A plugin returns its lifecycle assertion, the ownership scope, and tenant enablement state; it does not compute Tenant Availability State. Three reasons: the plugin does not hold every input, since visibility and authorization are evaluated here from what it returns; ADR-0010 requires `AVAILABLE` to mean one thing across origins, which per-plugin computation would erode; and the decision to fail closed when a state cannot be confirmed belongs to whoever composes the verdict.

Types Registry obtains externally managed Tenant Enablement State from the owning plugin at decision time. The plugin may satisfy the request from its own correctly invalidated cache, but cache correctness is part of the plugin contract.

Registry Source Plugins must provide efficient batch tenant-state lookup or include authoritative tenant state in batch entity results. If a plugin cannot confirm the required state, enabled-only operations fail closed.

Lifecycle Status and Tenant Enablement State remain separate dimensions. Types Registry computes the platform-facing Tenant Availability State after applying source state, lifecycle, dependencies, and visibility.

### Source failures

`NOT_FOUND`, `FORBIDDEN`, `SOURCE_UNAVAILABLE`, and `INVALID_SOURCE_RESPONSE` are distinct outcomes. Types Registry must not convert source unavailability into `NOT_FOUND`, `AVAILABLE`, or an empty authoritative query result.

All P1 Types Registry operations that require the source fail closed. In particular, registry entity list and search operations return a source failure without a partial result page when any selected source is unavailable or returns an invalid or incomplete response.

### Consequences

* Types Registry has no external definition synchronization, projection tables, projection revisions, invalidation workers, or full-reconciliation jobs.
* External resolving and discovery depend on plugin latency and availability.
* Registry Source Plugins become full registry adapters rather than thin import connectors.
* Each plugin must implement platform-compatible batch resolving, querying, pagination, revision/freshness, Registry Reference retention, and tenant-state behavior.
* Plugin caches must remain correct across plugin instances, pods, and data centers according to the source contract.
* External entity history and reverse resolution may be unavailable if a plugin is removed without migration; plugin removal and replacement therefore require preservation of issued Registry References and tombstones.
* Cross-source dependency and impact queries must be federated through plugin capabilities; Types Registry does not reconstruct them from local projections.
* The source ordering and query contract become correctness-critical and are decided in ADR-0007.

### Confirmation

This decision is confirmed when:

* Types Registry storage contains no external definition, identifier, revision, content-hash, dependency, lifecycle, tenant-state, or UUID-mapping value in any column;
* plugin configuration activation rejects a Source Claim/entity-kind pair that does not satisfy ADR-0007's applicable mandatory capability profile;
* managed entities continue to resolve entirely from Types Registry storage in P1, and Managed Aliases do so when Alias support is introduced in P2;
* external single and batch forward/reverse resolution are delegated through the normal Types Registry API;
* every external response carries revision and hash metadata, returned content hashes are verifiable, and conditional requests reject inconsistent revision/hash pairs;
* plugin contract tests prove that repeated results with the same external revision have the same canonical content and hash;
* integration tests distinguish `NOT_FOUND`, `SOURCE_UNAVAILABLE`, and invalid plugin responses;
* P1 registry entity list and search tests prove that a selected source failure returns no partial result page;
* plugin-owned tombstones preserve external reverse resolution after logical deletion;
* plugins reject rebinding a deleted external GTS Identifier or Registry Reference to a different logical entity;
* a source assertion of deprecation is accepted, the entity is exposed as `ACTIVE`, and no result of any origin carries a `DEPRECATED` status in P1;
* a plugin that does not return the resolved effective schema and trait artifacts for a claimed Type Schema kind fails Source Claim activation, and conformance tests exercise `$ref` inlining, `allOf` chain composition, RFC 7396 trait merge, and default materialization rather than only the stability of the returned bytes;
* tenant-aware operations obtain authoritative source state and fail closed when it cannot be confirmed;
* a response omitting the ownership scope, or naming a tenant the platform does not know, is rejected as an invalid source response rather than exposed;
* a plugin-side check can make an entity invisible to a caller and cannot make one visible that platform policy refused;
* no plugin supplies a `resource_version`, and no operation accepts one for an externally managed entity;
* regular gears never access Registry Source Plugins directly.

## Pros and Cons of the Options

### Always delegate externally managed definitions and tenant state

Types Registry resolves and queries external entities live through Registry Source Plugins. Plugins own definition storage, caches, indexes, revision history, Registry Reference mappings, tombstones, and tenant state.

* Good, because the External Registry Source remains the only source of truth for external definitions and tenant state.
* Good, because Types Registry needs no synchronization workers, projection reconciliation, or external-state invalidation protocol.
* Good, because stale local tenant state cannot incorrectly expose an entity that the authoritative source has disabled or deleted.
* Bad, because resolving, discovery, and tenant-aware availability depend on plugin latency and availability.
* Bad, because every plugin must implement a complete platform-compatible registry, query, retention, and failure contract.
* Neutral, because source-native caches remain permitted, but their correctness belongs to the plugin rather than Types Registry.

### Project definitions and tenant state locally

Types Registry stores both external definitions and per-tenant state and uses plugins only to refresh missing or stale data.

* Good, because most resolving and query operations can be served from local storage with predictable latency.
* Good, because Types Registry can apply one local indexing and query model to managed and externally managed entities.
* Bad, because Types Registry maintains a second serving copy of externally authoritative definitions and tenant state.
* Bad, because synchronization, invalidation, reconciliation, and stale-read policy become correctness-critical.
* Bad, because stale projected tenant state can expose an entity after the authoritative source disables or deletes it.
* Neutral, because the External Registry Source remains authoritative even though Types Registry serves its local projection.

### Project definitions locally and read tenant state live

Types Registry stores and indexes external definitions but always asks plugins for current tenant state.

* Good, because exact, wildcard, dependency, and discovery queries can use platform-owned local indexes.
* Good, because tenant availability is evaluated against current source-owned tenant state.
* Bad, because definition synchronization and projection invalidation are still required.
* Bad, because tenant-aware operations still depend on plugin latency and availability after local candidate selection.
* Bad, because definition freshness and tenant-state freshness can diverge and require two consistency models.
* Neutral, because definition content and tenant state intentionally use separate freshness lifecycles.

### Allow each plugin to choose projection or live delegation

Each source advertises its preferred storage mode and Types Registry supports both execution models.

* Good, because each source can select the model best matched to its native query, caching, and change-notification capabilities.
* Good, because sources can be integrated incrementally without requiring one universal storage strategy.
* Bad, because Types Registry must implement, secure, and test both projection and live-delegation paths.
* Bad, because freshness, failure, pagination, and cache guarantees become plugin-specific.
* Bad, because equivalent public requests may have different correctness and availability behavior depending on the owning source.
* Neutral, because regular gears still use the same public Types Registry API while the internal execution model varies by plugin.

## More Information

### What later decisions settled

Two questions this ADR left open have since been answered, and the answers narrow it rather than amend it.

ADR-0011 closed the managed–external boundary in both directions. That is what lets the persistence prohibition above stand with **no** exception. The one exception that could have been argued for — an external identifier held as the label on a dependency edge toward a Managed Entity — would record a relationship the closed boundary makes unrepresentable, so it does not arise, and no external identifier is admitted into any column, dependency edges included.

ADR-0012 decided that Registry Source Plugin configuration is a Managed registered Instance of a platform-defined control-plane type, governed by the ordinary write path and validated by a built-in in-process validator rather than by a P2 Validation Hook. This ADR states that conclusion; the decision itself belongs there.

### What would reopen this decision

Live delegation is chosen against projection on the grounds that a second serving copy of externally authoritative state introduces synchronization, invalidation, and stale-read policy as correctness concerns. Two observations would reopen it: measurement showing that plugin latency puts federated resolution outside the NFR budget in a realistic deployment, or a source capability contract that made change notification reliable enough for a projection to be invalidated rather than polled. Neither is available today, and note that reopening it does not reopen ADR-0011 — a projection of external content would still not permit a reference across the boundary.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0007**: [0007-cpt-cf-types-registry-adr-federated-source-routing-query.md](./0007-cpt-cf-types-registry-adr-federated-source-routing-query.md)
- **ADR-0011**: [0011-cpt-cf-types-registry-adr-managed-external-boundary.md](./0011-cpt-cf-types-registry-adr-managed-external-boundary.md)
- **ADR-0008**: [0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md](./0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md) — makes deprecation owner intent rather than a consequence of succession, and defers the status out of P1 on the external side as well as the managed one, which is why the lifecycle mapping here has two values and a source assertion of deprecation is accepted but not relayed.
- **ADR-0012**: [0012-cpt-cf-types-registry-adr-write-path-admission-protocol.md](./0012-cpt-cf-types-registry-adr-write-path-admission-protocol.md) — decides the question this ADR leaves open, making Registry Source Plugin configuration a Managed registered Instance governed by the ordinary write path.
- **Design note**: [../design-notes/registry-federation-external-sources.md](../design-notes/registry-federation-external-sources.md)
- **ToolKit plugins**: [../../../../../docs/TOOLKIT_PLUGINS.md](../../../../../docs/TOOLKIT_PLUGINS.md)

This decision directly addresses:

* `cpt-cf-types-registry-fr-registry-federation` - selects live source delegation rather than local definition projection.
* `cpt-cf-types-registry-fr-externally-managed-entities` - defines the external data-ownership and platform-admission boundary.
* `cpt-cf-types-registry-fr-id-resolution` - delegates external Registry Reference resolution while preserving one public API.
* `cpt-cf-types-registry-fr-type-query-assistance` - requires plugin-owned query capability behind Types Registry.
* `cpt-cf-types-registry-fr-tenant-availability` - obtains authoritative external tenant state at decision time.
* `cpt-cf-types-registry-fr-tenant-enablement` - keeps external tenant enablement source-owned while Types Registry owns managed tenant enablement when that post-P1 capability is introduced.
* `cpt-cf-types-registry-fr-cache-freshness-metadata` - uses plugin-supplied revision and hash as the external validator without persisting external cache state.
* `cpt-cf-types-registry-contract-toolkit-plugins` - preserves plugin isolation behind Types Registry.
