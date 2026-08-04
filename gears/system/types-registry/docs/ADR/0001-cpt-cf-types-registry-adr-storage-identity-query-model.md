---
status: accepted
date: 2026-07-23
---

# Storage Identity and Query Model for GTS References

**ID**: `cpt-cf-types-registry-adr-storage-identity-query-model`

## Table of Contents

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Terminology](#terminology)
- [Established Constraints](#established-constraints)
- [Decision Drivers](#decision-drivers)
- [Usage Scenarios](#usage-scenarios)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Injectivity is a constraint on identifiers, not a property of the derivation](#injectivity-is-a-constraint-on-identifiers-not-a-property-of-the-derivation)
  - [Query assistance result](#query-assistance-result)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Store GTS Identifier strings](#store-gts-identifier-strings)
  - [Store UUID Registry References returned by Types Registry SDK](#store-uuid-registry-references-returned-by-types-registry-sdk)
  - [UUIDv4 with a persisted global mapping](#uuidv4-with-a-persisted-global-mapping)
  - [Deterministic UUID derived from the GTS Identifier](#deterministic-uuid-derived-from-the-gts-identifier)
- [More Information](#more-information)
  - [Open Design Points](#open-design-points)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Domain gears store objects that conform to GTS Type Schemas or refer to registered GTS Instances. Public requests identify those registry entities with GTS Identifiers and, in P2, may use Alias GTS Identifiers, but persisting long identifier strings creates wide indexes and larger predicates in every domain database.

Most typed-object operations already need Types Registry to retrieve schemas, evaluate tenant availability, or build registry-aware query constraints; P2 also adds Alias resolution. This ADR decides whether domain gears should persist the client-supplied GTS Identifier or a compact UUID registry reference returned by the Types Registry SDK.

In P2, an Alias is a first-class, globally unique GTS Identifier that resolves to a Type Schema or registered Instance. If a client creates an object using an Alias, a later read must return that same Alias GTS Identifier rather than silently replacing it with the target identifier. Alias support is strictly outside P1; the identity model is defined here so P2 does not require changing persisted domain references.

## Scope

This ADR decides:

* the representation persisted by domain gears for client-supplied references to Type Schemas and registered Instances;
* the stability and reverse-resolution guarantees of UUID registry references;
* the query flow for exact GTS Identifiers, wildcard patterns, compatible versions, type hierarchy constraints, and P2 Aliases;
* the authority boundary for Managed and Externally Managed Registry Reference mappings.

The concrete algorithm used inside Types Registry to allocate a UUID is not part of the domain-gear storage contract. Domain gears treat the UUID as opaque and obtain it only through the Types Registry SDK.

Federated source ordering, source claims, plugin fallback, and wildcard pagination are decided by ADR-0007.

## Terminology

| Term | Meaning |
|---|---|
| `input_gts_id` | The literal validated GTS Identifier supplied by a client. It can directly identify an entity or identify an Alias. |
| `target_gts_id` | The GTS Identifier of the Type Schema or registered Instance to which an Alias resolves. |
| Registry Reference | An opaque UUID returned by Types Registry for one exact `input_gts_id` and persisted by a domain gear. |
| Concrete Reference Set | Complete, deduplicated, bounded set of Registry Reference UUIDs selected for one type filter. |
| Availability | The tenant-specific result computed by Types Registry from lifecycle, ownership, dependencies, and, when applicable, external-source state. |

An Alias GTS Identifier and its target GTS Identifier are different identifiers and therefore have different Registry References. Alias resolution may return both references and the target metadata, but the domain object stores the Registry Reference of `input_gts_id` so reverse resolution preserves the identifier selected by the client.

## Established Constraints

* GTS Identifier and wildcard-pattern validation, parsing, and matching must use the platform-approved `gts-rust` implementation.
* Alias resolution, tenant availability, lifecycle, and ownership visibility remain Types Registry responsibilities. Authorization of type use must not be bypassed, and a domain gear may use a cache only with Types Registry-defined freshness and invalidation semantics.
* The owning domain gear defines the read policy for an existing object whose referenced registry entity is unavailable. It must document whether the object is filtered, rejected, or returned with an explicit unavailable status.
* The GTS Identifier namespace is global. Tenant ownership changes management and visibility, not the meaning of an identifier.
* Two tenants cannot register different Type Schemas, Instances, or Alias targets under the same GTS Identifier. A tenant may register a distinct derived type from a visible parent, but cannot redefine the parent's identity. Registration conflicts must not disclose another tenant's ownership or content to an unauthorized caller.
* UUIDs are identifiers, not authorization credentials or tenant-isolation boundaries.
* Types Registry owns Registry Reference mappings and tombstones for Managed Entities and Managed Aliases.
* The owning Registry Source Plugin owns Registry Reference mappings and tombstones for Externally Managed Entities while Types Registry remains the only public resolution API used by regular gears.

## Decision Drivers

* UUID columns and indexes should consume substantially less space than typical GTS Identifier strings.
* Most create, read, bulk, Alias, availability, compatibility, hierarchy, and wildcard scenarios already require Types Registry or a valid cache. See usage scenarios below.
* The same GTS Identifier must resolve to the same Registry Reference across processes, deployments, and Registry Sources without domain gears coordinating ID mappings.
* A Registry Reference must resolve back to the exact GTS Identifier used by the client, including an Alias GTS Identifier.
* Public APIs and logs must remain debuggable through GTS Identifiers even though domain databases store UUIDs.
* Single and bulk operations must avoid one Registry call per object.
* Query support must work consistently across SQLite, PostgreSQL, and MySQL.
* Client and server caches must remain correct in multi-pod deployments and for External Registry Sources.

## Usage Scenarios

| Scenario | Types Registry or valid cache interaction | Domain gear action or policy | Domain DB operation |
|---|---|---|---|
| Create one object of an `input_gts_id` type | Resolve `input_gts_id` to its Registry Reference, target schema, target metadata, and tenant availability. | Validate the payload against the returned schema and apply the gear's write policy. | Store the Registry Reference UUID with the object. |
| Bulk create | Batch-resolve distinct input identifiers, schemas, target metadata, and tenant availability. | Validate each payload and apply the endpoint's defined all-or-nothing or partial-success behavior. | Bulk-store Registry Reference UUIDs. |
| Read one object by object ID | Reverse-resolve the stored UUID to the original GTS Identifier and obtain availability or metadata required by the owning gear's read contract. | Apply the owning gear's unavailable-entity read policy and return the original GTS Identifier. | Read the object and its Registry Reference UUID. |
| Bulk read by object IDs | Batch reverse-resolve the distinct stored UUIDs and batch-evaluate any required availability or metadata. | Apply the owning gear's read policy to each result and return each original GTS Identifier. | Read objects and their Registry Reference UUIDs. |
| List by exact `input_gts_id` values | Batch-resolve the exact identifiers to Registry Reference UUIDs and evaluate availability or visibility required by the list contract. | Validate each input with `gts-rust` and apply the list's unavailable-entity policy. | Use `WHERE registry_reference IN (...)`. |
| List by GTS wildcard pattern | Return a complete, bounded Concrete Reference Set containing every matching Registry Reference UUID under the selected Alias, compatibility, hierarchy, and availability semantics. | Validate the pattern with `gts-rust`, apply the list's unavailable-entity policy, and reject expansion that exceeds documented limits. | Apply the returned UUID set through backend-safe `IN` filtering or equivalent UUID-set matching. |

Payload validation, bulk atomicity, and unavailable-entity response behavior are owning-gear responsibilities. Types Registry supplies identity, schema, availability, and query-planning information; domain gears apply those results to their own transactions and repositories.

## Considered Options

Storage representation:

* Store GTS Identifier strings.
* Store UUID Registry References returned by Types Registry SDK.

Registry Reference allocation:

* UUIDv4 with a persisted global mapping.
* Deterministic UUID derived from the GTS Identifier.

## Decision Outcome

Chosen option: domain gears store only the opaque Registry Reference UUID returned by the Types Registry SDK for the exact `input_gts_id` supplied by the client. They do not derive the UUID themselves and do not persist the GTS Identifier as the type reference.

The allocation model is internal to Types Registry, but it must satisfy the public stability contract. The SDK contract does not expose or require a particular UUID version. The current deterministic `GtsId::to_uuid()` approach is the preferred implementation because it satisfies that stability invariant without transporting allocation state. A different allocation algorithm is permitted only if it preserves all existing mappings and the same global stability guarantees.

Registry Source Plugins must implement the same platform Registry Reference mapping. Types Registry validates every external reverse-resolution result by deriving the returned exact GTS Identifier's Registry Reference and comparing it with the requested UUID.

The Types Registry API must guarantee, directly for Managed Entities and through the owning Registry Source Plugin for Externally Managed Entities:

* the same GTS Identifier always maps to the same UUID across tenants, processes, deployments, Registry Sources, imports, and restores whenever the caller is authorized to resolve it;
* different GTS Identifiers, including an Alias and its target, resolve to different UUIDs, subject to the identifier-profile restriction below;
* the UUID reverse-resolves to the exact original GTS Identifier and, when applicable, separately exposes the Alias target;
* mappings remain resolvable after deprecation or logical deletion while domain references may still exist;
* a logically deleted GTS Identifier cannot be rebound to a new logical entity, with exactly one named exception — the purge operation decided by ADR-0013, which is disabled by default and releases the identifier deliberately; any future restore can restore only the original logical entity and its retained identity history;
* UUID collisions are rejected as conflicts, and so is a divergent definition submitted under an existing identifier — but not a successive one. ADR-0004 narrows this rule: a managed identifier names a mutable logical entity, so definitions that share one admitted revision lineage are revisions rather than conflicts, and a conflict is a definition that shares none;
* single and batch resolution APIs expose the metadata and cache revision required for correct domain-gear operation.

The UUID is opaque at the domain-gear boundary. Domain gears must not call `GtsId::to_uuid()` directly, depend on UUID version bits, or reconstruct the Types Registry allocation algorithm.

### Injectivity is a constraint on identifiers, not a property of the derivation

`GtsId::to_uuid()` returns an explicit UUID tail when the identifier carries one and derives a UUIDv5 otherwise. Where an explicit tail is used, the derivation is a projection rather than a hash: two different GTS Identifiers that embed the same UUID tail produce the same Registry Reference. The invariant that different identifiers resolve to different UUIDs is therefore a requirement on the identifiers admitted, not a guarantee the function provides.

Types Registry closes the gap on the side it controls and states the residual honestly.

* A managed GTS Identifier **MUST NOT** carry an explicit UUID tail. This is part of the managed identity profile alongside ADR-0004's prohibition on minor versions, and it removes the whole class for every identifier Types Registry admits. The UUIDv5 derivation is injective in practice over distinct identifier strings.
* Managed storage carries a uniqueness constraint on the Registry Reference, so a derivation that did collide is rejected at admission rather than silently rebinding a stored domain reference.
* An External Registry Source **MAY** serve identifiers with explicit tails, and Types Registry cannot verify global injectivity across sources: two sources may embed the same UUID in two different identifiers, both results pass Source Claim conformance, and no central index exists to detect it, ADR-0007 having rejected one.
* Where Types Registry observes the collision — two distinct identifiers resolving to one Registry Reference within one operation, or an external reverse-resolution whose returned identifier derives to the requested UUID while a managed entity already holds it — it **MUST** fail with a structured identity-collision error rather than select a winner. Silently choosing one is the failure mode that corrupts persisted domain references.
* A collision that is never co-observed is not detectable. That is an accepted residual of deterministic derivation combined with source-supplied tails, and it is the reason the managed profile excludes them.

For Managed Entities and Managed Aliases, Types Registry is authoritative for forward and reverse resolution and retains durable mappings or tombstones after logical deletion. For Externally Managed Entities, the owning Registry Source Plugin is authoritative for forward and reverse resolution and must retain equivalent mappings or tombstones for as long as platform domain objects may reference them. Types Registry exposes both through the same SDK and REST contracts without persisting external mappings locally.

External Registry Sources cannot provide Aliases and Externally Managed Entities cannot be Alias targets. Alias Registry References are therefore always resolved from managed Types Registry state.

### Query assistance result

Type query assistance returns a Concrete Reference Set. It does not return a normalized database predicate or an opaque executable query plan.

The set:

* contains unique opaque Registry Reference UUIDs only;
* is complete for the validated filter, tenant context, and selected Alias, compatibility, hierarchy, lifecycle, and availability semantics;
* contains only references that are **visible and available to the requesting tenant**, so the set is tenant-specific and two tenants may receive different sets for one filter;
* is semantically unordered, even if Types Registry uses deterministic source-major traversal to build it;
* is bounded by a documented platform maximum, enforced by the registry as the traversal proceeds rather than by whatever client assembles it;
* is never silently truncated;
* is exhaustive for the filter but is **not a snapshot**: it is assembled from a paged traversal, so entities may be registered or deleted between its first page and its last. Pagination is what keeps a deduplicated set from having to be held whole in server memory, and the atomicity given up for that is an accepted trade rather than an omission.

If expansion exceeds the maximum, Types Registry returns `QUERY_EXPANSION_LIMIT_EXCEEDED` and no usable constraint. If a required Registry Source cannot establish its part of the result, Types Registry fails the operation rather than returning a partial set.

Domain gears apply the UUIDs to their own storage. The SDK or gear repository may use backend-safe chunking or an equivalent UUID-set mechanism, but the semantic input remains one complete set. An empty set means the domain query has no matching type references.

Narrowing to available does not take the unavailable-entity policy away from the owning gear, which `cpt-cf-types-registry-fr-tenant-availability` leaves with it — including the option to return an object with an explicit unavailable status. That option belongs to reading a known object, where reverse resolution returns the full snapshot with its availability verdict and reason. A filtered list is a different question, and answering it with rows whose type the tenant may not use would push the filtering back onto every caller.

The response may carry cache or source-freshness metadata, but such metadata does not turn it into an executable plan and cannot replace the concrete UUID members. Because availability is per-tenant and ADR-0010 lets a verdict change with no mutation to the entity, a set can go stale without anything in the registry being written.

### Consequences

* Every domain gear with typed objects depends on the Types Registry SDK for write-time resolution and read-time reverse resolution, with batch APIs and revision-aware caches required to keep that dependency efficient.
* In P2, Alias preservation is achieved by storing the Alias's own Registry Reference. The SDK returns the Alias GTS Identifier on read and may separately return its target identity and metadata.
* Managed Registry identity records require durable local tombstones or an equivalent non-reusable mapping. External plugins carry the same retention obligation for external references.
* Compatibility fixtures must pin representative `GTS Identifier <-> UUID` mappings so an implementation or `gts-rust` upgrade cannot silently change persisted identities.
* External-source consistency belongs to the Registry Source Plugin contract.
* Domain schemas should use native UUID columns where supported and a consistent 16-byte representation where they are not; concrete mappings must be verified for SQLite, PostgreSQL, and MySQL.
* Types Registry outage and stale-cache behavior become part of domain read/write reliability and must have explicit fail-closed, cached-read, timeout, and retry policies.
* Migration from any existing GTS Identifier columns requires batch resolution, backfill, verification, and rollback planning.
* Broad wildcard, compatible-version, or hierarchy filters can exceed the concrete-set limit and must be narrowed by the caller rather than delegated as executable Registry-owned predicates.
* Domain repositories need reusable backend-safe UUID-set filtering so concrete sets do not depend on one database's parameter-count limit.

### Confirmation

The decision is confirmed when:

* the SDK exposes single and batch forward/reverse resolution with cache metadata;
* query assistance returns complete, deduplicated Concrete Reference Sets and never returns normalized predicates, opaque executable plans, truncated sets, or partial pages;
* tests cover empty expansion, duplicate elimination, backend-safe UUID filtering, expansion-limit failure, and required-source failure;
* at least one domain gear design demonstrates all six usage scenarios using only UUID Registry References in its tables;
* P1 tests prove that the same GTS Identifier resolves to the same UUID across tenants and against the pinned fixtures; when P2 Alias support is introduced, tests additionally prove that distinct Alias and target identifiers reverse-resolve independently;
* tests cover managed and external logical deletion/tombstone resolution, local-first plugin fallback, invalid external UUID mappings, UUID collision rejection, cross-tenant identifier conflicts, tenant availability, bulk operations, wildcard semantics, and cache invalidation;
* tests reject registration of a new logical entity under a deleted GTS Identifier while preserving reverse resolution of its previously issued Registry Reference;
* managed registration rejects a GTS Identifier carrying an explicit UUID tail;
* two distinct identifiers deriving to one Registry Reference within one operation produce a structured identity-collision error rather than a selected winner, for the managed-versus-managed, managed-versus-external, and external-versus-external cases;
* representative measurements compare UUID and GTS Identifier column, index, and query costs across SQLite, PostgreSQL, and MySQL.

## Pros and Cons of the Options

### Store GTS Identifier strings

* Good, because rows are self-describing and an exact literal filter can be constructed without Types Registry.
* Good, because the client-supplied Alias can be returned without reverse resolution.
* Bad, because long strings create wider columns, indexes, and `IN` predicates in every domain database.
* Bad, because most scenarios still require Types Registry for schema retrieval, Alias target resolution, tenant availability, or registry-aware query semantics.
* Bad, because each gear must consistently choose DB encodings, lengths, and indexes for GTS Identifiers.

### Store UUID Registry References returned by Types Registry SDK

* Good, because domain tables and indexes use a compact fixed-width value.
* Good, because exact, batch, compatibility, hierarchy, and expanded wildcard queries can share UUID-based repository helpers.
* Good, because the SDK boundary centralizes Alias, availability, federation, and query-planning semantics already needed by most operations.
* Bad, because displaying the original GTS Identifier requires reverse resolution or a valid cache.
* Bad, because Types Registry availability and cache correctness become part of the read path even when the domain object itself is locally available.
* Bad, because the authoritative mapping owner must retain UUID-to-GTS-Identifier identity mappings for as long as any domain object may reference them.

### UUIDv4 with a persisted global mapping

Types Registry can assign a random UUIDv4 once and transport the `GTS Identifier <-> UUID` mapping to every deployment that needs it.

* Good, because references are opaque and decoupled from the identifier's spelling.
* Bad, because independent registration of the same GTS Identifier in different deployments can allocate different UUIDs unless allocation is coordinated.
* Bad, because imports, disaster recovery, federation, and external Registry Sources must preserve and reconcile the mapping.
* Bad, because UUIDv4 alone does not allow two tenants to redefine the same GTS Identifier; that would require a different `(tenant_id, gts_id)` identity model throughout the platform.

### Deterministic UUID derived from the GTS Identifier

Types Registry can derive the reference deterministically, for example through the current `gts-rust` `GtsId::to_uuid()` implementation, which uses an explicit UUID tail when present and otherwise UUIDv5 under the fixed GTS namespace.

* Good, because independent deployments derive the same UUID for the same GTS Identifier without coordinating allocation state.
* Good, because imports, federation, cache keys, and disaster recovery do not need a separately transported surrogate mapping.
* Bad, because the derivation mapping becomes a long-lived compatibility invariant for persisted domain references.
* Bad, because deterministic UUID equality proves identifier equality, not schema-content equality; registration must still reject different definitions under the same identifier.
* Bad, because the explicit-tail path makes the derivation non-injective by construction, so injectivity has to be imposed as a restriction on admitted identifiers and cannot be verified across Registry Sources.

## More Information

### Open Design Points

* Whether exact and wildcard filters use literal identifier semantics, Alias-target equivalence, or both.
* Bulk-operation atomicity, partial-success semantics, and limits.
* Cache invalidation transport and the required consistency guarantee between resolution/schema validation and a domain-object write.
* Managed identity-tombstone retention and disaster-recovery guarantees needed to preserve reverse resolution indefinitely.
* Registry Source Plugin removal and replacement procedures needed to preserve issued external Registry References.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0007**: [0007-cpt-cf-types-registry-adr-federated-source-routing-query.md](./0007-cpt-cf-types-registry-adr-federated-source-routing-query.md)
- **ADR-0004**: [0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md](./0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md) — decides what counts as a conflict between two definitions under one managed identifier, and makes the identifier a mutable logical entity.
- **ADR-0013**: [0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md](./0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md) — decides the purge operation that is the single named exception to the identifier non-rebinding guarantee.

This decision directly addresses the following requirements or design elements:

* `cpt-cf-types-registry-fr-aliasing` - P2 Aliases are globally unique GTS Identifiers and retain distinct Registry References from their targets.
* `cpt-cf-types-registry-fr-id-resolution` - Stable forward and reverse UUID resolution is the core integration contract.
* `cpt-cf-types-registry-fr-type-query-assistance` - Domain gears need UUID constraints suitable for their own databases.
* `cpt-cf-types-registry-fr-tenant-availability` - Type usability is tenant-specific and must be evaluated consistently.
* `cpt-cf-types-registry-fr-cache-freshness-metadata` - Validatable resolution depends on stable references, revisions, and invalidation semantics.
* `cpt-cf-types-registry-db-requirements` - Storage requirements are derived from this ADR.
