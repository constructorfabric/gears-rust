---
status: accepted
date: 2026-10-12
decision-makers: Constructor Fabric Steering Committee
---

# ADR-002: Resource Group Types Live in Types Registry

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Type model](#type-model)
  - [Declaration and registration](#declaration-and-registration)
  - [Stored references](#stored-references)
  - [Resolution and validation](#resolution-and-validation)
  - [Evolution](#evolution)
  - [Errors](#errors)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Types in Types Registry, RG keeps only references](#types-in-types-registry-rg-keeps-only-references)
  - [Keep local type tables and synchronise with a saga](#keep-local-type-tables-and-synchronise-with-a-saga)
  - [RG REST facade over Types Registry](#rg-rest-facade-over-types-registry)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-resource-group-adr-p1-types-in-types-registry`

## Context and Problem Statement

[ADR-001](./ADR-001-gts-type-system.md) chose GTS as the type system for resource groups and, as part of that decision, a local type registry inside Resource Group (RG): three tables (`gts_type`, `gts_type_allowed_parent`, `gts_type_allowed_membership`), SMALLINT surrogate keys, a type CRUD REST API and SDK surface, and an init-time bootstrap that other gears used to register their types.

This local registry duplicates what Types Registry already provides for every GTS type: storage and validation of Type Schemas, derivation checks, compatibility rules, and a stable reference for domain gears to persist. Two catalogues of the same GTS identifiers drift apart, force every consumer to know which one is authoritative, and make RG responsible for schema semantics it does not own. Types Registry ADR-0001 already decides that domain gears persist the UUID Registry Reference returned by Types Registry rather than a GTS identifier or a gear-local surrogate. ADR-0003 and ADR-0004 of Types Registry give a compatibility guarantee within a major version that makes most of RG's type-update safety checks redundant.

The question: should RG keep owning type storage and CRUD, or should types become ordinary Types Registry entities with RG keeping only references?

## Decision Drivers

* One source of truth for GTS Type Schemas; no gear-local copy to synchronise
* Persist the platform-standard identity of a type (Registry Reference, UUID) instead of a local surrogate
* Remove RG-owned type CRUD, bootstrap and seeding, which are pure duplication
* Keep RG hot paths (create group, add member) free of avoidable cost and keep checks outside the database transaction
* Rely on the compatibility guarantees of Types Registry instead of re-validating existing data on every type change
* Gears are not yet deployed to production, so a clean schema change without data migration is acceptable

## Considered Options

* **Option 1: Types live in Types Registry; RG stores only the Registry Reference** — types are derived Type Schemas registered through Types Registry; RG reads them through the Types Registry SDK
* **Option 2: Keep local type tables and synchronise them with Types Registry through a saga** — RG remains the type owner and mirrors changes into Types Registry
* **Option 3: RG REST facade over Types Registry** — RG keeps its `/types` REST surface and forwards it to Types Registry

## Decision Outcome

Chosen option: "Types live in Types Registry; RG stores only the Registry Reference", because it removes the duplicated catalogue entirely, uses the identity and compatibility model Types Registry already guarantees, and leaves RG with only group and membership semantics.

### Type model

An RG type is a GTS Type Schema registered in Types Registry and derived from the abstract base type `gts.cf.core.rg.type.v1~`. Placement rules are trait values (`x-gts-traits`) of the derived type:

* `can_be_root` — whether groups of the type may have no parent (default false)
* `allowed_parent_types` — GTS identifiers of RG types allowed as parent; a type listing its own identifier allows nesting under itself (default empty)
* `allowed_membership_types` — GTS identifiers of resource types allowed as members (default empty)

The trait schema and the defaults are declared on the base type. The JSON Schema of a group's `metadata` is the `metadata` property of the derived type schema. The base type is abstract: groups are never created with it.

Whether a type is a tenant type is not a trait. A type whose GTS identifier starts with the tenant type identifier `gts.cf.core.rg.type.v1~cf.core._.tenant.v1~` is a tenant type; a group of such a type starts a new tenant scope.

### Declaration and registration

Some types are declared statically by the gear that owns them and reach Types Registry through the static type inventory. The platform tenant type (`gts.cf.core.rg.type.v1~cf.core._.tenant.v1~`: may be a root, may nest under itself, takes no member types, metadata `status`, `self_managed`, `custom_domain`) is declared in the Resource Group SDK. Account Management declares `gts.cf.core.rg.type.v1~cf.core.am.user.v1~` (member handle) and `gts.cf.core.rg.type.v1~cf.core.am.user_group.v1~` (user-group container) in its own SDK and no longer registers anything at startup. Any other type is registered through the Types Registry API; the deployment's registration policy governs which vendors may derive types from the RG base type.

RG no longer has type endpoints (`/types-registry/v1/types*`), type methods in its SDK, a type service, type seeding or the init-time bootstrap surface.

### Stored references

`resource_group.gts_type_id` and `resource_group_membership.gts_type_id` hold the Types Registry Registry Reference (UUID) of the type. There is no foreign key. `resource_group.is_tenant` is derived from the type identifier prefix at creation; a group's type never changes, so the flag never changes. A partial unique index over tenant root groups guarantees at most one tenant root in the deployment. The tables `gts_type`, `gts_type_allowed_parent`, `gts_type_allowed_membership` and the `gts_type_path` domain are dropped. The migration recreates the RG tables without carrying data over, because the gears were not deployed to production.

### Resolution and validation

RG resolves types through the Types Registry SDK (the platform contract, in-process local client). Type lookups happen before the database transaction starts, so the transaction never waits on a remote call. Each lookup reads effective traits and the resolved schema in one fixed projection. Types named by GTS id and by Registry Reference travel together in one batched request, so an operation whose keys are known up front makes a single request; a listed page adds one more only for types the filter resolution did not already return. RG has no cache of its own; caching belongs to the Types Registry SDK, where it is planned.

Group creation checks that the type exists, that its traits allow the requested placement (root, or the parent's type in `allowed_parent_types`), and that `metadata` is valid against the type schema. Membership creation checks that the member resource type is in `allowed_membership_types` of the group's type. An unknown type is an error; it is never silently skipped. Native SecureORM group predicates compare the membership type column with the member type's Registry Reference directly, with no subquery to a local type table.

### Evolution

Types Registry guarantees that within a major version a type only evolves in a backward-compatible way, and that the Registry Reference stays the same across such revisions. RG therefore does not re-validate existing groups when a type evolves, and the former "hierarchy safety" checks on type update no longer exist. Narrowing placement rules is not a compatible change: it requires a new major version, which is a new identifier with a new Registry Reference, and existing groups keep their original type.

### Errors

An unknown group type on create is 404 (type not found). An unknown membership resource type is 400. Types Registry unavailable is 503 with a retry hint. A stored type reference that Types Registry cannot resolve is 500, because it is a data integrity problem rather than a client error. `$orderby` on type fields is rejected with 400.

### Consequences

* RG depends on Types Registry availability for group creation, membership creation and metadata validation; reads of existing groups and memberships that do not need type data are unaffected
* Until the Types Registry SDK offers caching, every such write costs one lookup; this cost is accepted and bounded to the write path, outside the transaction
* Group and membership rows grow: the type column is 16 bytes (UUID) instead of 2 bytes (SMALLINT), including in every index that contains it; in exchange the three type tables and their indexes are gone
* No local type CRUD, seeding, bootstrap or compatibility safety checks to maintain; type governance (who may register which type) moves to the Types Registry deployment policy
* Exactly one tenant root group is enforced by a partial unique index rather than application logic
* Existing data is not migrated; any environment with RG data must be recreated
* Temporary: until Types Registry delivers its own publication of the RG inventory (Types Registry P0 task T31), the Types Registry gear seeds the `gts.cf.core.rg.*` inventory into its database and publishes the local contracts itself; this is removed when that task lands

### Confirmation

* Integration tests: groups and memberships are created against a Types Registry that holds the tenant, user and user-group types; unknown types map to the errors above
* Migration check: the schema after migration has no type tables, `is_tenant` on groups, and the tenant-root partial unique index

## Pros and Cons of the Options

### Types in Types Registry, RG keeps only references

* Good, because there is a single catalogue of GTS Type Schemas and no synchronisation problem
* Good, because the stored identity is the platform-standard Registry Reference
* Good, because compatibility guarantees of Types Registry replace RG's own hierarchy-safety logic
* Good, because RG loses the type CRUD, seeding and bootstrap code
* Bad, because RG writes depend on Types Registry being reachable
* Bad, because the type column grows from 2 to 16 bytes per row

### Keep local type tables and synchronise with a saga

This was the earlier plan. RG would remain the owner of type definitions and mirror every change into Types Registry through a multi-step saga with compensation.

* Good, because RG would keep DB-enforced referential integrity for types and no runtime dependency on Types Registry for placement checks
* Bad, because two stores must stay consistent, and a failed step leaves them diverged until compensated
* Bad, because the duplicated type model, CRUD and safety checks stay in RG
* Bad, because RG would still hold a gear-local surrogate for a type that already has a platform identity

### RG REST facade over Types Registry

RG would keep its type REST surface and forward each call to Types Registry.

* Good, because existing clients of the RG type endpoints would keep working
* Bad, because it adds a second public entry point for the same entity, with its own authorization and error mapping that must track Types Registry
* Bad, because RG would still need to model type updates it no longer owns
* Bad, because there are no deployed clients to protect

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Supersedes in part**: [ADR-001](./ADR-001-gts-type-system.md) (type storage, surrogate keys, junction tables, type CRUD)
- **Types Registry**: [ADR-0001 storage identity](../../../types-registry/docs/ADR/0001-cpt-cf-types-registry-adr-storage-identity-query-model.md), [ADR-0003 schema evolution compatibility](../../../types-registry/docs/ADR/0003-cpt-cf-types-registry-adr-type-schema-evolution-compatibility.md), [ADR-0004 minor-version identity evolution](../../../types-registry/docs/ADR/0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md)
- Migration DDL: [migration.sql](../migration.sql)
