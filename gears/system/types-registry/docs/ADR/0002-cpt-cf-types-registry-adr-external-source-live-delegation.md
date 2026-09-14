---
status: accepted
date: 2026-07-23
decision-makers: Constructor Fabric Steering Committee
---

# Live External Registry Source Delegation and Tenant Enablement State Model

**ID**: `cpt-cf-types-registry-adr-external-source-live-delegation`

## Table of Contents

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

An active Source Claim must be backed by a plugin implementing ADR-0007's P1 federation and completeness contract in full across the claimed identifier space. That contract is total, so nothing about it is declared at registration; a response failing to satisfy it is rejected and the affected request fails closed.

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

Producing the resolved effective schema and the effective trait artifacts is a **mandatory** part of the contract, not an optional one, and the decision turns on what the alternative leaves a consumer holding.

If a plugin omits the artifacts, a consumer must fetch every base and resolve the chain locally. That means several round trips and a GTS resolution implementation in every consumer — precisely the duplication Types Registry removes.

There is no degraded mode: an absent resolved schema is a dead end, not a smaller answer. Types Registry cannot fill the gap because ADR-0011 keeps external documents out of managed closures and ADR-0014 forbids inspecting their `$schema`.

The obligation is affordable because a plugin is already a full registry adapter responsible for batch resolution, querying, pagination, revision and freshness semantics, Registry Reference retention, and tombstones.

It is also well-posed. ADR-0011's closed boundary keeps an external entity's whole derivation chain inside one Source Claim, served by one plugin. The plugin therefore has every required document and may use its own storage and computation. The obligation applies to the adapter, not necessarily to the vendor catalogue behind it.

Two consequences follow.

Conformance testing must cover **how** the artifacts are computed, not only that they are returned and stable. `$ref` inlining, `allOf` composition along the `$id` chain, trait-value merge under JSON Merge Patch per GTS §9.7.5, and materialization of declared defaults before the completeness check all have specified semantics; a plugin that returns a plausibly-shaped but differently-computed artifact hands consumers a silently wrong schema, which is worse than returning nothing.

The response shape is uniform across origins; its guarantees are not:

| Origin | Meaning of a resolved schema |
|---|---|
| Managed | The closure is Draft-07, every base passed derivation compatibility, and the revision chain is backward compatible. |
| External | The source computed the artifact under rules Types Registry does not validate. |

ADR-0014 forbids inspecting an external dialect, so consumers must be prepared for any dialect GTS admits, including Draft 2019-09 and 2020-12.

Per-level content-model classification is deliberately **not** required of a plugin. It is not a resolution artifact but metadata about the platform's own compatibility policy, which is not enforced on the external side; asking a source to report compliance with a mode the platform does not apply to it would be asking for a number with no meaning. ADR-0003 has since confined that reporting to refusals for managed entities too, so there is no asymmetry left to explain.

Source lifecycle assertions must map to the platform `ACTIVE` or `DELETED` Lifecycle Status. That vocabulary has two values in P1 for every origin: ADR-0008 defers `DEPRECATED` on the external side as well as the managed one, so there is no third value to map onto.

A source may assert deprecation, but Types Registry exposes that entity as `ACTIVE`. This is exact for P1: deprecation discourages adoption while the entity remains usable. ADR-0008 treats deprecation as owner intent, so Types Registry neither requires a successor nor relays the source assertion. The plugin contract must say so explicitly.

Source lifecycle maps as follows:

* an entity may move directly to `DELETED`, with or without prior source-side deprecation;
* a pending source candidate is not an Externally Managed logical entity and must not appear in ordinary resolution or discovery;
* a `DELETED` entity appears only in operations whose contract includes tombstone or history information.

**Exact read is such an operation**, by GTS Identifier or Registry Reference. Its default `lifecycle_status` lets a gear distinguish a retired contract from an identifier never issued (DESIGN §3.3, *Read results*). Reverse resolution likewise returns the tombstone as deleted.

Discovery, search, and query assistance exclude deleted entities because they do not answer about a caller-supplied exact key. The source must never rebind a GTS Identifier or Registry Reference to a different logical entity.

For one exact external entity:

* the same `external_revision` must always identify the same canonical content and `content_hash`;
* changed canonical content must produce a different `external_revision`;
* Types Registry does not assume that revisions are numeric, monotonic, or comparable across entities or sources.

Revision and hash are protocol metadata. Types Registry:

* validates the returned hash against canonical content when present;
* validates revision/hash consistency against caller- or cache-supplied conditional metadata when available;
* exposes both for conditional requests and delegates conditional reads to the owning plugin;
* never persists them as registry state.

Plugin contract tests and source monitoring verify the revision contract across requests without requiring prior values in Types Registry. `cpt-cf-types-registry-fr-cache-freshness-metadata` makes this conditional metadata a P1 obligation.

### Platform admission

The External Registry Source remains responsible for source-owned schema, instance, evolution, and derivation validation. Types Registry validates every live result's GTS envelope, Registry Reference mapping, source-pattern claim, authorization, tenant visibility, and lifecycle exposure before returning it as usable. It validates nothing about the content, and there is no further platform check a result must satisfy: ADR-0011 closes the boundary, so no Managed Entity can depend on an external one, and a consumer reading an external entity directly decides for itself what it needs of it.

Any source assertion used for admission must be bound to the returned `external_revision` and `content_hash`. P1 does not persist external admission receipts. A stateful admission mechanism for external entities requires a future ADR.

External Registry Sources cannot provide Aliases, and Externally Managed Entities cannot be Alias targets.

### What the plugin is asked, and what it must not answer

A plugin call carries the plane-appropriate authenticated context through `SourceSecurityContext`: `Tenant(&SecurityContext)` on the tenant plane or `Platform(&PlatformSecurityContext)` on the platform plane. The wrapper preserves ToolKit's two authenticated context types across the in-process P1 plugin call. The call also carries the Context Tenant the question is about; it is **optional**, because a platform-plane read may ask no tenant-specific question at all.

A plugin **MAY** add checks, but they **MAY only deny**. Narrowing makes an entity invisible, indistinguishable from absence. Widening would let an unoperated source override a platform authorization decision and is forbidden.

The ownership assertion does not need this directional rule. There the source describes content it owns; here it would be deciding platform access.

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
* Source-internal dependency handling and tooling remain source-owned. Types Registry neither reconstructs them from projections nor exposes cross-source dependency or reverse-impact queries (ADR-0011).
* The source ordering and query contract become correctness-critical and are decided in ADR-0007.

### Confirmation

This decision is confirmed when:

* Types Registry storage contains no external definition, identifier, revision, content-hash, dependency, lifecycle, tenant-state, or UUID-mapping value in any column;
* a source result that does not satisfy ADR-0007's contract as applicable to the returned identifier is rejected as an invalid source response rather than interpreted, and the affected request fails closed;
* managed entities continue to resolve entirely from Types Registry storage in P1, and Managed Aliases do so when Alias support is introduced in P2;
* external single and batch forward/reverse resolution are delegated through the normal Types Registry API;
* every external response carries revision and hash metadata, returned content hashes are verifiable, and conditional requests reject inconsistent revision/hash pairs;
* plugin contract tests prove that repeated results with the same external revision have the same canonical content and hash;
* integration tests distinguish `NOT_FOUND`, `SOURCE_UNAVAILABLE`, and invalid plugin responses;
* P1 registry entity list and search tests prove that a selected source failure returns no partial result page;
* plugin-owned tombstones preserve external reverse resolution after logical deletion;
* plugins reject rebinding a deleted external GTS Identifier or Registry Reference to a different logical entity;
* a source assertion of deprecation is accepted, the entity is exposed as `ACTIVE`, and no result of any origin carries a `DEPRECATED` status in P1;
* a plugin that does not return the resolved effective schema and trait artifacts for a Type Schema result has that response rejected as invalid, and conformance tests exercise `$ref` inlining, `allOf` chain composition, RFC 7396 trait merge, and default materialization rather than only the stability of the returned bytes;
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

Live delegation is chosen against projection on the grounds that a second serving copy of externally authoritative state introduces synchronization, invalidation, and stale-read policy as correctness concerns. Two observations would reopen it: measurement showing that plugin latency puts federated resolution outside the NFR budget in a realistic deployment, or a source contract that made change notification reliable enough for a projection to be invalidated rather than polled. Neither is available today, and note that reopening it does not reopen ADR-0011 — a projection of external content would still not permit a reference across the boundary.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0007**: [0007-cpt-cf-types-registry-adr-federated-source-routing-query.md](./0007-cpt-cf-types-registry-adr-federated-source-routing-query.md)
- **ADR-0011**: [0011-cpt-cf-types-registry-adr-managed-external-boundary.md](./0011-cpt-cf-types-registry-adr-managed-external-boundary.md)
- **ADR-0008**: [0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md](./0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md) — makes deprecation owner intent rather than a consequence of succession, and defers the status out of P1 on the external side as well as the managed one, which is why the lifecycle mapping here has two values and a source assertion of deprecation is accepted but not relayed.
- **ADR-0012**: [0012-cpt-cf-types-registry-adr-write-path-admission-protocol.md](./0012-cpt-cf-types-registry-adr-write-path-admission-protocol.md) — decides the question this ADR leaves open, making Registry Source Plugin configuration a Managed registered Instance governed by the ordinary write path.
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
