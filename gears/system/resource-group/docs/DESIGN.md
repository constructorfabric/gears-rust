<!-- Created: 2026-03-06 by Constructor Tech -->
<!-- Updated: 2026-04-20 by Constructor Tech -->

# Technical Design - Resource Group (RG)


<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
  - [3.8 Query Profile Enforcement](#38-query-profile-enforcement)
  - [3.9 Error Mapping](#39-error-mapping)
- [4. Additional Context](#4-additional-context)
  - [Non-Applicable Design Domains](#non-applicable-design-domains)
  - [Performance Architecture](#performance-architecture)
  - [Security Architecture](#security-architecture)
  - [Reliability Architecture](#reliability-architecture)
  - [Data Governance](#data-governance)
  - [API Versioning](#api-versioning)
  - [Observability](#observability)
  - [Architecture Evolution: RG as Persistent Storage for Types Registry](#architecture-evolution-rg-as-persistent-storage-for-types-registry)
  - [Known Technical Debt](#known-technical-debt)
  - [Open Questions](#open-questions)
  - [4.1 Database Size Analysis & Production Projections](#41-database-size-analysis--production-projections)
  - [4.2 Testing Architecture](#42-testing-architecture)
- [5. Traceability](#5-traceability)

<!-- /toc -->

> **Abbreviation**: Resource Group = **RG**. Used throughout this document.

## 1. Architecture Overview

### 1.1 Architectural Vision

RG is a generic hierarchy and membership gear.

It provides:

- group types resolved from Types Registry (GTS Type Schemas with placement traits)
- strict forest entity topology
- closure-table hierarchy read model
- membership links between groups and resources
- read interfaces consumable by external gears/plugins

RG is intentionally policy-agnostic:

- no AuthZ policy evaluation
- no decision semantics
- no SQL filter generation

The architecture consists of:

- **RG Resolver SDK** — read and write trait contracts (`ResourceGroupClient`, `ResourceGroupReadHierarchy`) and the statically declared tenant RG type
- **RG Gear (Gateway)** — routes requests to built-in or vendor-specific provider
- **RG Plugin** — full service with database, REST API, seeding, and domain logic; group types are read from Types Registry

Deployments use either: (RG Plugin + RG Service) or (Vendor RG Plugin + Vendor RG Service) — both behind the same SDK contracts.

AuthZ can operate without RG. RG is an optional PIP data source for AuthZ plugin logic.

For AuthZ-facing deployments aligned with current platform architecture, `ownership-graph` is the required profile; provider selection (built-in provider or vendor-specific backend) is deployment-specific.

### 1.2 Architecture Drivers

#### Functional Drivers


| Requirement                                                   | Design Response                                                                                                                       |
| ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-resource-group-fr-rest-api`                           | REST API layer with OperationBuilder and OData query support.                                                                         |
| `cpt-cf-resource-group-fr-odata-query`                        | OData `$filter` and cursor-based pagination (`cursor`, `limit`) on all list endpoints. Ordering is undefined but consistent. No `$orderby`. |
| `cpt-cf-resource-group-fr-list-groups-depth`                  | Dedicated depth endpoint (`/{group_id}/hierarchy`) returns hierarchy with relative depth and depth-based filtering.                       |
| `cpt-cf-resource-group-fr-types-in-types-registry`            | Types are GTS Type Schemas derived from `gts.cf.core.rg.type.v1~` and owned by Types Registry; RG has no type storage, CRUD, seeding or bootstrap. Placement rules are trait values (`can_be_root`, `allowed_parent_types`, `allowed_membership_types`). |
| `cpt-cf-resource-group-fr-resolve-types`                      | `TypeCatalog` port backed by the Types Registry SDK; lookups happen before the DB transaction; unknown type is an error (404/400), registry outage maps to 503, unresolvable stored reference to 500. |
| `cpt-cf-resource-group-fr-store-type-reference`               | Group and membership rows store the Registry Reference UUID without a foreign key; no re-validation of existing groups when a type evolves (compatibility guaranteed within a major version). |
| `cpt-cf-resource-group-fr-seed-groups`                        | Group data seeding is optional and deployment-specific (plugin data migration, manual DB admin, or RG API). Validates parent-child links and type compatibility when performed. Groups MUST be seeded sequentially (parents before children) due to hierarchy dependencies. |
| `cpt-cf-resource-group-fr-seed-memberships`                   | Membership data seeding is optional and deployment-specific (plugin data migration, manual DB admin, or RG API). Validates group existence and tenant compatibility when performed. Memberships have no interdependencies and SHOULD be seeded in parallel. |
| `cpt-cf-resource-group-fr-manage-entities`                    | Entity service with create/get/update/move/delete operations.                                                                         |
| `cpt-cf-resource-group-fr-enforce-forest-hierarchy`           | Domain invariants + cycle checks before writes. Multiple roots permitted (forest).                                                    |
| `cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`     | Entity create/move validates that at most one root group is of a tenant type (GTS id starts with `TENANT_RG_TYPE_PATH`; `is_tenant` is derived at creation); second tenant root rejected with `TenantRootAlreadyExists` (409 Conflict); the `uq_rg_tenant_root` partial unique index backs the check against races. |
| `cpt-cf-resource-group-fr-validate-parent-type`               | Entity create/move validates parent-child compatibility against the parent type's `allowed_parent_types` / `can_be_root` read from Types Registry. |
| `cpt-cf-resource-group-fr-delete-entity-no-active-references` | Delete orchestration applies reference-policy checks before entity removal and closure mutation.                                      |
| `cpt-cf-resource-group-fr-tenant-scope-ownership-graph`       | Ownership-graph profile enforces tenant-hierarchy-compatible parent-child and membership writes, with tenant-scoped AuthZ query path. |
| `cpt-cf-resource-group-fr-manage-membership`                  | Membership service provides deterministic add/remove lifecycle operations.                                                            |
| `cpt-cf-resource-group-fr-query-membership-relations`         | Membership read API supports indexed lookups by group and by resource.                                                                |
| `cpt-cf-resource-group-fr-closure-table`                      | Hierarchy service backed by `resource_group_closure`.                                                                                 |
| `cpt-cf-resource-group-fr-query-group-hierarchy`              | Hierarchy read paths return ancestors/descendants ordered by depth.                                                          |
| `cpt-cf-resource-group-fr-subtree-operations`                 | Subtree move/delete executes closure recalculation inside one transaction boundary.                                                   |
| `cpt-cf-resource-group-fr-query-profile`                      | Optional profile guard checks for depth/width on writes and query paths; limits can be disabled.                                      |
| `cpt-cf-resource-group-fr-profile-change-no-rewrite`          | Profile updates are treated as guardrails only and never rewrite historical hierarchy rows.                                           |
| `cpt-cf-resource-group-fr-reduced-constraints-behavior`       | Tightened profiles allow full reads but reject writes that create/increase depth or width violations.                                 |
| `cpt-cf-resource-group-fr-integration-read-port`              | Read-only consumer contract for hierarchy/membership access.                                                                          |
| `cpt-cf-resource-group-fr-no-authz-and-sql-logic`             | Hard separation: RG returns data only; AuthZ/PEP own constraints/SQL.                                                                 |
| `cpt-cf-resource-group-fr-deterministic-errors`               | Unified error mapper translates domain/infrastructure failures to stable public categories.                                           |
| `cpt-cf-resource-group-fr-force-delete`                       | Delete orchestration supports optional `force` parameter for cascade deletion of subtree and memberships.                             |
| `cpt-cf-resource-group-fr-jwt-auth`                           | Public RG HTTP/gRPC endpoints authenticate every request via JWT and run `PolicyEnforcer`. The AuthZ plugin's in-process `ResourceGroupReadHierarchy` reader is hierarchy-only and uses `AccessScope::allow_all()`, so it does **not** invoke `PolicyEnforcer` — this resolves the AuthZ ↔ RG circular dependency; the plugin still emits its own AuthZ constraints from the returned hierarchy. |
| `cpt-cf-resource-group-fr-dual-auth-modes` _(p2 — deferred, not implemented yet)_ | **Future / not implemented yet.** RG Gateway will additionally support MTLS (hierarchy-only, AuthZ-bypassed) for service-to-service callers when the AuthZ plugin is split out of the RG process. Tracked as `p2`. |


#### NFR Allocation


| NFR ID                                                | NFR Summary                     | Allocated To                                | Design Response                                | Verification      |
| ----------------------------------------------------- | ------------------------------- | ------------------------------------------- | ---------------------------------------------- | ----------------- |
| `cpt-cf-resource-group-nfr-hierarchy-query-latency`   | hierarchy reads p95 < 250ms     | hierarchy read paths + closure indexes      | indexed ancestor/descendant lookups            | benchmark suite   |
| `cpt-cf-resource-group-nfr-membership-query-latency`  | low-latency membership reads    | membership service + indexes                | direct lookup by group/resource keys           | benchmark suite   |
| `cpt-cf-resource-group-nfr-transactional-consistency` | transactional write consistency | transaction boundary in persistence adapter | canonical + closure updates commit together    | integration tests |
| `cpt-cf-resource-group-nfr-deterministic-errors`      | stable failures                 | unified error mapper                        | all domain/infra failures mapped to SDK errors | unit tests        |
| `cpt-cf-resource-group-nfr-production-scale`          | projected production volumes    | schema design + index strategy              | composite indexes, partitioning candidate for membership table (~455M rows, ~110 GB) | capacity planning |
| `cpt-cf-resource-group-nfr-compatibility`             | API and SDK compatibility       | SDK trait contracts + REST versioning       | path-based API versioning, trait backward compat | integration tests |
| `cpt-cf-resource-group-nfr-data-lifecycle`            | data lifecycle on deprovisioning | force delete cascade + tenant scope        | cascade-delete memberships/groups via force delete on tenant removal | integration tests |


#### Architecture Decision Records

| ADR ID | Title |
| --- | --- |
| `cpt-cf-resource-group-adr-p1-gts-type-system` | [GTS Type System for Resource Group](ADR/ADR-001-gts-type-system.md) — GTS chained type paths, traits pattern, barrier semantics (storage parts superseded by ADR-002) |
| `cpt-cf-resource-group-adr-p1-types-in-types-registry` | [Resource Group Types Live in Types Registry](ADR/ADR-002-types-in-types-registry.md) — types are Types Registry entities, RG stores the Registry Reference |

#### Key Compatibility Anchors


| Document                                          | Constraint                                                                                                  |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `docs/arch/authorization/DESIGN.md`               | AuthZ plugin can consume RG data as PIP input; PEP compiles constraints to SQL.                             |
| `docs/arch/authorization/RESOURCE_GROUP_MODEL.md` | AuthZ usage expects tenant-scoped groups with tenant-hierarchy-aware validation for graph/membership links. |
| `gears/system/authz-resolver/docs/PRD.md`       | AuthZ resolver contract unchanged; extension through plugin behavior only.                                  |
| `gears/system/authn-resolver/docs/PRD.md`       | no AuthN/AuthZ responsibility mixing.                                                                       |


### 1.3 Architecture Layers


| Layer                  | Responsibility                                    | Technology                    |
| ---------------------- | ------------------------------------------------- | ----------------------------- |
| REST API Layer         | HTTP endpoints with OData query support           | OperationBuilder + REST handlers |
| SDK API Layer          | expose entity/membership + read contracts    | Rust SDK traits + ClientHub   |
| Domain Layer           | validate type compatibility (types from Types Registry) and forest invariants | domain services               |
| Hierarchy Engine       | closure-table updates/queries and profile checks  | domain service + repositories |
| Integration Read Layer | read-only hierarchy queries for AuthZ plugin      | `ResourceGroupReadHierarchy`  |
| Persistence Layer      | transactional storage and indexing                | SQL + SeaORM repositories     |


## 2. Principles & Constraints

### 2.1 Design Principles

#### Policy-Agnostic Core

- [x] `p1` - **ID**: `cpt-cf-resource-group-principle-policy-agnostic`

RG handles graph/membership data only.

#### Strict Forest Integrity

- [x] `p1` - **ID**: `cpt-cf-resource-group-principle-strict-forest`

Hierarchy guarantees single parent and cycle prevention for all writes. Multiple roots are allowed (RG is a forest, not a single tree).

#### Tenant Root Uniqueness

- [x] `p1` - **ID**: `cpt-cf-resource-group-principle-tenant-root-uniqueness`

The forest **MAY** contain multiple root groups, but at most **one** of them **MUST** be a tenant-type group (GTS id starts with `TENANT_RG_TYPE_PATH`). That unique tenant root is the **main tenant**; every other tenant lives as its sub-tenant. Non-tenant roots (e.g. auxiliary ownership forests that are not part of the tenant tree) may coexist alongside the main tenant root and carry the main tenant's `tenant_id`, but they are not tenants themselves. This invariant aligns the RG forest with the AuthZ/TR single-root tenant tree: `TENANT_MODEL.md`'s single-root-tree is realized as exactly the tenant-root subtree inside the RG forest. Enforcement at create/move time rejects a second tenant-type root with `TenantRootAlreadyExists` (409 Conflict); a partial unique index on `resource_group` (`is_tenant` where `parent_id IS NULL`) guarantees the invariant against concurrent writers.

#### Type Governance in Types Registry

- [x] `p1` - **ID**: `cpt-cf-resource-group-principle-dynamic-types`

Group types are owned by Types Registry, not by RG. A type is a GTS Type Schema derived from the abstract base `gts.cf.core.rg.type.v1~`; its placement rules are trait values (`x-gts-traits`: `can_be_root`, `allowed_parent_types`, `allowed_membership_types`, defaults false / empty / empty, declared as the trait schema on the base). Types are declared statically (the tenant type in the RG SDK, the AM user and user-group types in the AM SDK) or registered through the Types Registry API; the deployment's registration policy governs which vendors may derive from the RG base type. Types Registry guarantees that within a major version a type only evolves backward-compatibly, so RG does not re-validate existing groups on type evolution; narrowing placement rules requires a new major (new id, new Registry Reference).

**ADRs**: `cpt-cf-resource-group-adr-p1-gts-type-system`, `cpt-cf-resource-group-adr-p1-types-in-types-registry`

**Derived type fields — metadata object, JSONB storage (v1)**: Derived type fields (e.g. `barrier`, `custom_domain`, `category`) are nested inside a `metadata` object in API requests and responses. In the database, they are stored in a `metadata` JSONB column on `resource_group`. For chained types (e.g., `gts.cf.core.rg.type.v1~y.core.tn.tenant.v1~`), the type schema defines `properties.metadata` with `additionalProperties: false` for field isolation. Group `metadata` is validated against the resolved schema of the group's type, obtained from Types Registry; an unknown type is an error (it is never silently skipped). Advanced validation (e.g., Starlark-based cross-field rules) is out of scope for v1.

#### Query Profile as Guardrail

- [x] `p1` - **ID**: `cpt-cf-resource-group-principle-query-profile-guardrail`

`(max_depth, max_width)` is a service profile controlling write admissibility and SLO classification.

#### Tenant Scope for Ownership Graph

- [x] `p1` - **ID**: `cpt-cf-resource-group-principle-tenant-scope-ownership-graph`

In ownership-graph usage, groups are tenant-scoped and links must be tenant-hierarchy-compatible (same-tenant or allowed related-tenant link per tenant hierarchy rules).

#### Barrier as Data (Not Enforcement)

- [x] `p1` - **ID**: `cpt-cf-resource-group-principle-barrier-as-data`

`barrier` is not a dedicated database column. For GTS types that support barrier semantics (e.g. tenant types), `barrier` is stored inside the `metadata` JSONB field as `metadata.self_managed` (boolean). **ADRs**: `cpt-cf-resource-group-adr-p1-gts-type-system`. **RG stores and returns it without enforcement** — RG does not filter, restrict, or alter query results based on the barrier value. RG stores it in `metadata` and returns it in API responses within the `metadata` object, nothing more.

Barrier enforcement is a **joint responsibility of Tenant Resolver and AuthZ**:

- **RG does not enforce barriers**: all RG queries (hierarchy, groups, memberships) return data regardless of barrier values. If a caller has an `AccessScope` that includes `tenant_id = T7`, RG will return T7's data even if T7 has `metadata.self_managed = true`.
- **Tenant Resolver enforces barriers in hierarchy traversal**: TR SDK defines `BarrierMode` (`Respect` / `Ignore`). The static-tr-plugin's `collect_descendants` skips barrier tenants (`self_managed = true`) and their entire subtrees. `collect_ancestors` from a barrier tenant returns empty. RG's `metadata.self_managed` maps to TR's `TenantInfo.self_managed`.
- **AuthZ integrates barriers into SQL constraints**: the `in_tenant_subtree` predicate supports `barrier_mode: "all"` (default, reads `metadata->>'barrier'` from the closure query) or `"none"` (ignores barriers for billing, provisioning).
- **Each layer is vendor-replaceable**: vendors can implement custom TR plugins and AuthZ plugins with different barrier semantics. RG remains policy-agnostic.

This aligns with the core constraint "No AuthZ Decision Logic" — RG is a data source, TR + AuthZ are the policy engines.

**Actual barrier behavior** (Tenant Resolver `BarrierMode::Respect`, default):
- Barrier tenant and its subtree are **completely invisible** to parent — skipped in `collect_descendants`.
- Barrier tenant cannot see parent chain — `collect_ancestors` returns empty.
- Barrier tenant sees its own subtree normally via its own scope.
- Nested barriers are allowed (partner with barrier → customer with barrier).
- `BarrierMode::Ignore` bypasses barriers (platform-admin, billing, provisioning).

### 2.2 Constraints

#### No AuthZ Decision Logic

- [x] `p1` - **ID**: `cpt-cf-resource-group-constraint-no-authz-decision`

RG cannot return allow/deny decisions.

#### No SQL/ORM Filter Generation

- [x] `p1` - **ID**: `cpt-cf-resource-group-constraint-no-sql-filter-generation`

RG cannot generate SQL fragments or access-scope objects.

#### Database-Agnostic Persistence

- [x] `p1` - **ID**: `cpt-cf-resource-group-constraint-db-agnostic`

RG persistence layer uses SeaORM abstractions and standard SQL. The gear **MUST NOT** depend on vendor-specific SQL extensions or features of a particular RDBMS. Any SQL-compatible database supported by SeaORM can be used as the storage backend.

**Implementation note**: the reference DDL (`migration.sql`) uses plain types supported by both PostgreSQL and SQLite (UUID/TEXT, partial unique index). RG stores no GTS type path in the database, so no type path format validation is needed in the persistence layer; type identifiers are validated by Types Registry.

#### Type References Are Registry References

- [x] `p1` - **ID**: `cpt-cf-resource-group-constraint-surrogate-ids-internal`

`gts_type_id` columns on `resource_group` and `resource_group_membership` hold the Types Registry Registry Reference (UUID). They are opaque and **MUST NOT** appear in any API response, SDK type, REST contract, or OpenAPI schema. All external interfaces (REST API, SDK traits) use GTS type paths (strings) exclusively; RG resolves paths to and from Registry References through the Types Registry SDK. There is no foreign key and no gear-local surrogate.

**Strict type existence validation**: every GTS type path used in an RG operation **MUST** be known to Types Registry. This applies to:
- `type` in group create requests (`POST /groups`); a group's type never changes, so `PUT /groups/{id}` does not change it
- `resource_type` in membership operations (`POST /memberships/...`)

If a group type does not exist the operation **MUST** return `TypeNotFound` (404); if a membership resource type does not exist it **MUST** return a validation error (400). The allowed parent and member types are checked from the type's traits (`allowed_parent_types`, `allowed_membership_types`).

**RG type prefix requirement (`gts.cf.core.rg.type.v1~`)**:
- **`type` in group operations** **MUST** have the `gts.cf.core.rg.type.v1~` prefix. Rejected without it.
- **`allowed_parent_types`** entries **MUST** be derived from `gts.cf.core.rg.type.v1~` — parent groups are always RG types (enforced by Types Registry through the trait schema).
- **`allowed_membership_types`** have **no prefix requirement** — membership resource types are external domain types (e.g., `gts.cf.core.am.user.v1~`) that do not need to be RG types.

This ensures the hierarchy is always governed by the RG type contract, while membership resources can be any registered GTS type.

**ADRs**: `cpt-cf-resource-group-adr-p1-types-in-types-registry`

#### Profile Change Safety

- [x] `p1` - **ID**: `cpt-cf-resource-group-constraint-profile-change-safety`

Reducing enabled `max_depth`/`max_width` cannot rewrite existing rows. Writes that worsen violation are rejected until external migration runs. Limits may also be disabled.

#### Non-Applicable Constraint Categories

- **Regulatory constraints**: Not applicable at gear level — RG does not handle PII directly. Regulatory requirements are platform-level (see PRD section 6.6).
- **Vendor/licensing constraints**: SeaORM is Apache-2.0 licensed. No vendor lock-in — database is pluggable via `cpt-cf-resource-group-constraint-db-agnostic`.
- **Data residency constraints**: Not applicable at gear level — data residency is a deployment-specific concern handled by infrastructure (database placement, region selection).
- **Resource constraints**: Not applicable — resource constraints (budget, team, timeline) are project-level and tracked in project management tools.

## 3. Technical Architecture

### 3.1 Domain Model

**Planned locations**:

- `gears/system/resource-group/resource-group-sdk/src/models.rs` — SDK models and DTOs
- `gears/system/resource-group/resource-group-sdk/src/api.rs` — SDK trait contracts
- `gears/system/resource-group/resource-group-sdk/src/error.rs` — SDK error types
- `gears/system/resource-group/resource-group/src/domain/` — domain services and invariants
- `gears/system/resource-group/resource-group/src/api/` — REST API handlers

**Core entities**:


| Entity                    | Description                                                         |
| ------------------------- | ------------------------------------------------------------------- |
| `ResourceGroupType`       | type as read from Types Registry (GTS id, Registry Reference, placement traits, metadata schema); not stored by RG |
| `ResourceGroupEntity`     | group node with optional parent, stored in `resource_group` table   |
| `ResourceGroupMembership` | resource-to-group many-to-many link, qualified by `resource_type`   |
| `ResourceGroupClosure`    | ancestor-descendant-depth projection                                |
| `ResourceGroupError`      | deterministic public error taxonomy                                 |

**Entity relationships**:

- `ResourceGroupType` * ←→ * `ResourceGroupType` via the `allowed_parent_types` trait (M:N self-referential, held in Types Registry: which types can be parents of which)
- `ResourceGroupType` * ←→ * resource types via the `allowed_membership_types` trait (held in Types Registry: which resource types can be members of groups of which type)
- `ResourceGroupEntity` * → 1 `ResourceGroupType` (each group has one type, referenced by Registry Reference with no foreign key)
- `ResourceGroupEntity` * → 0..1 `ResourceGroupEntity` (optional parent — forest root has no parent)
- `ResourceGroupEntity` 1 ←→ * `ResourceGroupMembership` (group has many members)
- `ResourceGroupEntity` 1 ←→ * `ResourceGroupClosure` (as ancestor and as descendant)

**Value objects**: `GtsTypePath` (validated GTS type path string), `ListQuery` (OData filter + pagination params), `PageInfo` (cursor pagination metadata), `Page<T>` (paginated response wrapper).

### 3.2 Component Model

```mermaid
graph TD
    A[Domain Client / General Consumer] --> B[ResourceGroupClient]
    X[AuthZ Plugin] --> Z[ResourceGroupReadHierarchy]
    B --> D[RG Gear]
    Z --> D
    D --> E[Type Catalog]
    D --> F[Entity Service]
    D --> G[Hierarchy Service]
    D --> H[Membership Service]
    E --> TR[Types Registry SDK]
    F --> E
    H --> E
    F --> I
    G --> I
    H --> I
    I --> J[(SQL DB)]
```

AuthZ plugin depends only on the narrow `ResourceGroupReadHierarchy` trait (hierarchy-only). All other consumers (domain clients, general consumers) use `ResourceGroupClient` (full CRUD including reads).



#### RG Gear (Gateway)

- [x] `p1` - **ID**: `cpt-cf-resource-group-component-gear`

Responsibilities:

- wire services and repositories
- register public clients in ClientHub
- expose REST API endpoints under `/api/resource-group/v1/` (groups, memberships)
- load query profile config
- route `ResourceGroupReadHierarchy` calls to built-in data path or configured vendor-specific plugin path

Boundaries:

- no business rule implementation
- no authz decision logic

#### Type Catalog

- [x] `p1` - **ID**: `cpt-cf-resource-group-component-type-catalog`

Responsibilities:

- resolve GTS id strings and Registry References to type definitions (effective traits plus resolved schema) through the Types Registry SDK, in one fixed projection
- run before any database transaction of a write starts, so no transaction waits on a remote call
- resolve every key known up front in one batched request per operation; a list page adds a request only for types its filter resolution did not return
- report unknown types as absent (callers map them to 404 or 400) and registry failures as unavailability (503)

Boundaries:

- read-only: RG never creates, updates or deletes types
- no local cache; caching belongs to the Types Registry SDK

#### Entity Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-component-entity-service`

Responsibilities:

- create/get/update/move/delete entities
- validate parent type compatibility on create and move using the type's `allowed_parent_types` and `can_be_root`; a group's type never changes after creation
- derive `is_tenant` from the type id prefix at creation and reject a second tenant root
- validate group `metadata` against the schema of the group's type
- orchestrate subtree operations

#### Hierarchy Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-component-hierarchy-service`

Responsibilities:

- maintain closure table rows
- serve ancestor/descendant queries ordered by depth
- enforce depth/width rules on writes

#### Membership Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-component-membership-service`

Responsibilities:

- add/remove/list membership links
- guard deletion with active-reference checks

#### Integration Read Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-component-integration-read-service`

Responsibilities:

- expose read-only graph/membership queries for external consumers
- remain protocol-neutral and authz-agnostic

#### Persistence Adapter

- [x] `p1` - **ID**: `cpt-cf-resource-group-component-persistence-adapter`

Responsibilities:

- transactional persistence
- index-aware query execution
- consistent canonical + closure updates
- support canonical persistence strategy

Each repository MUST be defined as a trait first (e.g., `GroupRepositoryTrait`, `MembershipRepositoryTrait`, `ClosureRepositoryTrait`) and injected into domain services as `Arc<dyn Trait>`. This enables:
- unit testing via in-memory trait implementations without database; types are supplied by a mock Types Registry
- swappable storage backends without touching domain layer
- clear contract boundary between domain and infrastructure

Boundaries:

- no domain decisions
- no API semantics

### 3.3 API Contracts

**Core API** (`ResourceGroupClient`, stable):


| Method | Returns | Description |
| ------ | ------- | ----------- |
| `create_group` / `update_group` | `ResourceGroup` | group lifecycle |
| `get_group` | `ResourceGroup` | get group by ID |
| `list_groups` | `Page<ResourceGroup>` | list groups with OData query |
| `delete_group` | `()` | delete group (non-cascade; cascade only via REST) |
| `list_group_depth` | `Page<ResourceGroupWithDepth>` | traverse hierarchy from reference group with relative depth |
| `add_membership` | `ResourceGroupMembership` | add membership |
| `remove_membership` | `()` | remove membership |
| `list_memberships` | `Page<ResourceGroupMembership>` | list memberships with OData query |


SDK models are defined in `resource-group-sdk/src/models.rs` and are aligned with REST API schemas. Key SDK types:

| SDK Type | REST Schema | Description |
| -------- | ----------- | ----------- |
| `ResourceGroup` | `Group` | group identity (`id`, `type`, `name`), `hierarchy` context, `metadata` (type-specific fields) |
| `ResourceGroupWithDepth` | `GroupWithDepth` | same as `ResourceGroup` plus `hierarchy.depth` (relative distance) |
| `ResourceGroupMembership` | `Membership` | `group_id`, `resource_type`, `resource_id` |
| `Page<T>` / `PageInfo` | `*Page` | cursor-based pagination wrapper |

The SDK has no type model and no type methods: group types are Types Registry entities, read through the Types Registry SDK. The SDK only declares the platform tenant type and the type path constant `TENANT_RG_TYPE_PATH`.

Core API trait (`ResourceGroupClient`) is defined in `resource-group-sdk/src/api.rs`. All methods accept `SecurityContext` as the first argument for tenant-scoped authorization.

Core API usage pattern: resolve `dyn ResourceGroupClient` from `ClientHub`, build `SecurityContext` with `subject_id` + `subject_tenant_id`, then call trait methods with the appropriate request structs. All operations accept `SecurityContext` as the first argument for tenant-scoped authorization.

Membership write semantics for AuthZ-facing profile:

- membership operations are keyed by `(group_id, resource_type, resource_id)`
- in `ownership-graph` mode, add/remove validates tenant scope via caller `SecurityContext` effective scope and target group tenant
- membership tenant scope is derived from the target group's `tenant_id` via `group_id` (JOIN, not stored on membership row)
- tenant-incompatible membership writes fail deterministically (`TenantIncompatibility` error mapping)
- no policy decision fields are produced by RG for these operations

Platform-admin provisioning exception:

- privileged platform-admin calls that create/manage tenant hierarchies through `ResourceGroupClient` may run without caller tenant scoping
- this exception applies to provisioning/management operations only, not AuthZ query path
- data invariants remain strict: parent-child and membership links must satisfy tenant hierarchy compatibility rules

#### REST API Endpoints

Base path: `/api/resource-group/v1` (groups, memberships). There are no type endpoints: types are managed through the Types Registry API.

| Method | Base | Path | Operation | Description |
| ------ | ---- | ---- | --------- | ----------- |
| GET | resource-group | `/groups` | `listGroups` | List groups with OData query |
| POST | resource-group | `/groups` | `createGroup` | Create group (`tenant_id` derived from `SecurityContext` effective tenant scope) |
| GET | resource-group | `/groups/{group_id}` | `getGroup` | Get group by ID |
| PUT | resource-group | `/groups/{group_id}` | `updateGroup` | Full replace of group (including parent move) |
| DELETE | resource-group | `/groups/{group_id}` | `deleteGroup` | Delete group (optional `?force=true`) |
| GET | resource-group | `/groups/{group_id}/hierarchy` | `listGroupHierarchy` | Traverse hierarchy from reference group with relative depth |
| GET | resource-group | `/memberships` | `listMemberships` | List memberships with OData query |
| POST | resource-group | `/memberships/{group_id}/{resource_type}/{resource_id}` | `addMembership` | Add membership |
| DELETE | resource-group | `/memberships/{group_id}/{resource_type}/{resource_id}` | `deleteMembership` | Remove membership |

Query support on all list endpoints:

- `$filter` — OData field-specific operators (eq, ne, in)
- `limit` — page size (1..200, default 25)
- `cursor` — opaque token from previous response for next/previous page
- Ordering is undefined but consistent — no `$orderby`

Group list (`listGroups`) `$filter` fields: `type` (eq, ne, in), `hierarchy/parent_id` (eq, ne, in — direct parent only, depth=1; for ancestor traversal use `listGroupHierarchy`), `tenant_id` (eq, ne, in — narrows the AuthZ-scoped result to a specific tenant; cannot widen access), `id` (eq, ne, in), `name` (eq, ne, in).

Group depth (`listGroupHierarchy`) `$filter` fields: `hierarchy/depth` (eq, ne, gt, ge, lt, le), `type` (eq, ne, in).

Membership list `$filter` fields: `resource_id` (eq, ne, in), `resource_type` (eq, ne, in), `group_id` (eq, ne, in).

REST API field projection notes:

- Group responses (`Group` schema) do not include `created_at`/`updated_at` timestamps. These fields exist in the database for audit purposes but are not exposed in API responses.
- Membership list responses (`Membership` schema) do not include `tenant_id`. Memberships are always scoped to a single tenant; tenant scope is derived from the group's `tenant_id` via `group_id` JOIN and is not stored on the membership row itself.

**Integration read API** (stable):

`ResourceGroupReadHierarchy` is a narrow read-only contract for in-process plugin consumers — the AuthZ resolver plugin, the tenant-resolver RG plugin, and an in-process AuthZ PDP. It carries hierarchy walks (`get_group_descendants` / `get_group_ancestors`), flat OData-filtered group listing (`list_groups`), single-group existence lookup (`get_group`), and membership listing (`list_memberships`). All other consumers use `ResourceGroupClient`, which includes the same reads plus full CRUD.

These reads are resolved **unscoped** — they bypass `PolicyEnforcer`. A consumer that *is* the PDP cannot route reads back through the PEP without re-entering and recursing into itself, so implementations resolve them with no tenant `AccessScope`; the caller supplies any subject/tenant `OData` filter and owns tenant scoping.

| Trait | Method | Description |
| ----- | ------ | ----------- |
| `ResourceGroupReadHierarchy` | `get_group_descendants(ctx, group_id, query)` | descendant hierarchy walk (depth ≥ 0); matches REST `GET /groups/{group_id}/hierarchy` — OData `$filter` (depth, type), cursor-based pagination (`cursor`, `limit`) |
| `ResourceGroupReadHierarchy` | `get_group_ancestors(ctx, group_id, query)` | ancestor hierarchy walk (depth ≤ 0); same shape as the descendant walk |
| `ResourceGroupReadHierarchy` | `list_groups(ctx, query)` | flat OData-filtered group listing; enables batch reads (`id in (…)`) for the tenant-resolver RG plugin's `get_tenants(&[TenantId])` |
| `ResourceGroupReadHierarchy` | `get_group(ctx, id)` | single-group existence + tenant-ownership lookup; backs PDP scope validation (`/tenants/{t}/resourceGroups/{rg}`) |
| `ResourceGroupReadHierarchy` | `list_memberships(ctx, query)` | membership listing; backs PDP group-membership resolution. Caller MUST supply a subject-scoped filter (`resource_id eq '<subject_id>'`) |

Integration read models reuse the same SDK structs defined above:

- `get_group_descendants` / `get_group_ancestors` return `Page<ResourceGroupWithDepth>` (matches REST `GroupWithDepthPage`); `list_groups` and `get_group` return `Page<ResourceGroup>` / `ResourceGroup`
- `list_memberships` (on both `ResourceGroupClient` and `ResourceGroupReadHierarchy`) returns `Page<ResourceGroupMembership>` (matches REST `MembershipPage` — no `tenant_id`; tenant scope is available from group data the caller already has via the hierarchy reads)

Integration read trait hierarchy (defined in `resource-group-sdk/src/api.rs`):

| Trait | Extends | Methods | Used by |
| ----- | ------- | ------- | ------- |
| `ResourceGroupReadHierarchy` | — | `get_group_descendants`, `get_group_ancestors`, `list_groups`, `get_group`, `list_memberships` (all unscoped / PEP-bypassing) | AuthZ resolver plugin, tenant-resolver RG plugin, in-process AuthZ PDP |
| `ResourceGroupClient` | — | full CRUD (groups, memberships, hierarchy) | General consumers |

> The previously planned two-tier split — a separate `ResourceGroupReadPluginClient` extending `ResourceGroupReadHierarchy` with `list_memberships` — was collapsed. `list_memberships` and `get_group` now live directly on `ResourceGroupReadHierarchy`, and the vendor-specific plugin gateway resolves `dyn ResourceGroupReadHierarchy`. A vendor backend replaces the registered `ResourceGroupReadHierarchy` implementation at gear init rather than implementing a distinct plugin trait.

ClientHub registration: single implementation (`RgService`), registered as both `dyn ResourceGroupClient` and `dyn ResourceGroupReadHierarchy`. AuthZ plugin resolves `dyn ResourceGroupReadHierarchy`, general consumers resolve `dyn ResourceGroupClient`.

Plugin gateway routing notes:

- `ResourceGroupClient` is the full read+write contract for entity/membership lifecycle and hierarchy queries (used by domain clients and general consumers)
- `ResourceGroupReadHierarchy` is the narrow read-only contract for in-process plugin consumers (AuthZ resolver plugin, tenant-resolver RG plugin, in-process AuthZ PDP); its reads are resolved unscoped (bypass `PolicyEnforcer`)
- both are registered in ClientHub backed by the same implementation
- gear service resolves configured provider:
  - built-in provider: serve reads from local RG persistence path
  - vendor-specific provider: resolve plugin instance by configured vendor; the vendor's `dyn ResourceGroupReadHierarchy` implementation replaces the registered one at gear init (there is no separate plugin trait)
- plugin registration is scoped (GTS instance ID), same pattern as tenant-resolver/authz-resolver gateways
- `SecurityContext` is forwarded without policy interpretation in gateway layer (including plugin path)

Returned models are generic graph/membership objects. They do not encode AuthZ decisions or SQL semantics.

Tenant projection rule for integration reads:

- hierarchy reads (`list_group_depth`) return `ResourceGroupWithDepth` which includes `tenant_id` per group — callers use this to validate tenant scope
- membership reads (`list_memberships`) return `ResourceGroupMembership` without `tenant_id` — callers derive tenant scope from group data already obtained via hierarchy reads
- rows from hierarchy reads can legitimately contain different `tenant_id` values when caller effective scope spans tenant hierarchy levels
- this keeps RG policy-agnostic while allowing external PDP logic to validate tenant ownership before producing group-based constraints

Caller identity propagation rule (aligned with Tenant Resolver pattern):

- integration read methods accept caller `SecurityContext` (`ctx`) as the first argument
- RG gateway preserves `ctx` across provider routing (for plugin path, `ctx` is passed through to selected plugin unchanged) without converting it into policy decisions
- plugin implementations decide how/if `ctx` affects read access semantics (for example tenant-scoped visibility or auditing)
- this keeps RG data-only while preserving caller identity required by AuthZ plugin/PDP flows
- for AuthZ query path, reads are tenant-scoped by effective scope derived from caller `SecurityContext.subject_tenant_id`; non-tenant-scoped provisioning exception does not apply

#### Integration Read Schemas (AuthZ-facing)

The integration read contract returns **data rows only** (no policy/decision fields). Schemas match REST API models exactly.

`list_group_depth(ctx, group_id, query)` returns `Page<ResourceGroupWithDepth>` (matches REST `GET /groups/{group_id}/hierarchy` → `GroupWithDepthPage`):


| Field         | Type        | Required | Description                                                                  |
| ------------- | ----------- | -------- | ---------------------------------------------------------------------------- |
| `id`          | UUID        | Yes      | Group identifier                                                             |
| `type`        | string      | Yes      | GTS chained type path                                                        |
| `name`        | string      | Yes      | Display name                                                                 |
| `metadata`     | object | No   | Type-specific fields nested in `metadata` object (e.g. `metadata.self_managed`, `metadata.category`). Stored in DB `metadata` JSONB. Schema defined by chained GTS type. |
| `hierarchy`   | object      | Yes      | RG hierarchy context                                                         |
| `hierarchy.parent_id` | UUID / null | No | Parent group (null for root groups)                                    |
| `hierarchy.tenant_id` | UUID  | Yes    | Tenant scope (can differ per row under tenant hierarchy scope)               |
| `hierarchy.depth` | INT     | Yes      | Relative distance from reference group (`0` = self, positive = descendants, negative = ancestors) |

OData filters for `list_group_depth`: `hierarchy/depth` (eq, ne, gt, ge, lt, le), `type` (eq, ne, in). Pagination: `cursor`, `limit`. Uses OData nested path syntax (e.g., `$filter=hierarchy/depth ge 0 and type eq '...'`).

`list_memberships(ctx, query)` returns `Page<ResourceGroupMembership>` (matches REST `GET /memberships` → `MembershipPage`):


| Field           | Type   | Required | Description                           |
| --------------- | ------ | -------- | ------------------------------------- |
| `group_id`      | UUID   | Yes      | Group identifier                      |
| `resource_type` | string | Yes      | Resource type classification          |
| `resource_id`   | string | Yes      | Resource identifier                   |

OData filters for `list_memberships`: `group_id` (eq, ne, in), `resource_type` (eq, ne, in), `resource_id` (eq, ne, in). Pagination: `cursor`, `limit`.

Membership rows do not include `tenant_id`. Callers derive tenant scope from group data obtained via `list_group_depth`.

Tenant consistency behavior for integration reads:

- hierarchy rows include `tenant_id` per group — callers validate row scope against effective tenant scope before generating AuthZ group constraints
- membership rows are keyed by `group_id` — callers map `group_id → tenant_id` from hierarchy data
- in AuthZ query path, mixed-tenant rows are valid when each row tenant is inside effective tenant scope resolved from `ctx`

#### Integration Read Examples

Examples below assume caller effective tenant scope includes:

- `11111111-1111-1111-1111-111111111111` (tenant `T1`, subject tenant)
- `77777777-7777-7777-7777-777777777777` (tenant `T7`, related tenant in hierarchy scope)

Data shape used by all examples (same tenant/group/resource IDs as below):

```text
tenant T1 (11111111-1111-1111-1111-111111111111)
├── department D2 (22222222-2222-2222-2222-222222222222)
│   ├── branch B3 (33333333-3333-3333-3333-333333333333)
│   │   └── resource R4
│   └── resource R5
├── resource R4
├── resource R6
└── tenant T7 (77777777-7777-7777-7777-777777777777)
    └── resource R8
tenant T9 (99999999-9999-9999-9999-999999999999)
└── resource R0
```

Client initialization: AuthZ plugin resolves `dyn ResourceGroupReadHierarchy` from ClientHub (hierarchy only); general consumers resolve `dyn ResourceGroupClient` (full CRUD). Both build `SecurityContext` with `subject_id` + `subject_tenant_id` for tenant-scoped operations.

**Integration read examples** — see [openapi.yaml](./openapi.yaml) for full request/response examples with realistic data.

- `list_group_depth(ctx, D2, filter="hierarchy/depth ge 0")` — returns descendants: D2 at depth 0, B3 at depth 1.
- `list_group_depth(ctx, B3, filter="hierarchy/depth ge -10 and hierarchy/depth le 0")` — returns ancestor chain: T1 at depth -2, D2 at depth -1, B3 at depth 0.
- `list_memberships(ctx, filter="group_id in (...)")` — returns membership links `(group_id, resource_type, resource_id)` for requested groups.

### 3.4 Internal Dependencies


| Dependency           | Purpose                                     |
| -------------------- | ------------------------------------------- |
| `resource-group-sdk` | contracts/models/errors                     |
| `toolkit/client_hub`  | inter-gear client registration and lookup |


### 3.5 External Dependencies


| Dependency                            | Interface                       | Purpose                                                       |
| ------------------------------------- | ------------------------------- | ------------------------------------------------------------- |
| SQL database                          | SeaORM repositories             | durable canonical + closure storage                           |
| Types Registry SDK                    | `PlatformTypesRegistryApi` (in-process local client) | read group types (effective traits + resolved schema) and Registry References before the DB transaction; no RG-side cache |
| AuthZ Resolver SDK                    | `PolicyEnforcer` / `AuthZResolverApi` | AuthZ evaluation for JWT-authenticated RG API requests (write + read) |
| Vendor-specific RG backend (optional) | `ResourceGroupReadHierarchy`    | alternative hierarchy/membership source for integration reads (vendor impl replaces the registered `ResourceGroupReadHierarchy` at gear init) |
| AuthZ plugin consumer (optional)      | `ResourceGroupReadHierarchy`    | read group context in PDP logic (narrow reads: hierarchy + listing + single-group + memberships, resolved unscoped; in-process via `ClientHub` — `p1`; MTLS transport — `p2`, deferred / not implemented yet) |
| General consumers (optional)          | `ResourceGroupClient`           | full read+write access to entities/memberships/hierarchy |


### 3.6 Interactions & Sequences

#### Create Resource Group With Parent

**ID**: `cpt-cf-resource-group-seq-create-entity-with-parent`

Tenant Administrator creates a child resource group (e.g. department, branch) under an existing parent group via REST API `POST /groups`. Other callers — Instance Administrator (REST API) and Apps (`ResourceGroupClient` SDK) — follow the same internal flow.

```mermaid
sequenceDiagram
    participant TA as Tenant Admin (REST API)
    participant ES as Entity Service
    participant TC as Type Catalog (Types Registry SDK)
    participant HS as Hierarchy Service
    participant DB as Persistence

    TA->>ES: create_entity(type, parent)
    ES->>TC: resolve group type (traits, metadata schema)
    TC-->>ES: type definition (or not found / unavailable)
    ES->>ES: validate metadata against type schema
    ES->>DB: begin tx (SERIALIZABLE)
    ES->>HS: load current hierarchy snapshot in tx
    ES->>ES: validate parent type compatibility in tx
    ES->>HS: validate cycle/depth/width in tx
    ES->>DB: insert resource_group row (is_tenant derived from type id)
    HS->>DB: insert closure self row
    HS->>DB: insert ancestor-descendant rows
    DB-->>ES: commit
    alt serialization conflict
        ES->>DB: rollback
        ES->>ES: retry create_entity (bounded retry policy)
    end
    ES-->>TA: resource group created
```



#### Move Resource Group Subtree

**ID**: `cpt-cf-resource-group-seq-move-subtree`

Tenant Administrator moves a resource group (and its entire subtree) to a new parent within the same tenant via REST API `PUT /groups/{group_id}`. Other callers — Instance Administrator (REST API) and Apps (`ResourceGroupClient` SDK) — follow the same internal flow.

```mermaid
sequenceDiagram
    participant TA as Tenant Admin (REST API)
    participant ES as Entity Service
    participant TC as Type Catalog (Types Registry SDK)
    participant HS as Hierarchy Service
    participant DB as Persistence

    TA->>ES: move_entity(node, new_parent)
    ES->>TC: resolve involved types (before the transaction)
    ES->>DB: begin tx (SERIALIZABLE)
    ES->>HS: load current hierarchy snapshot in tx
    ES->>HS: validate not-in-subtree (cycle check) in tx
    ES->>HS: validate parent type/depth/width in tx
    HS->>DB: delete affected closure paths
    HS->>DB: insert rebuilt closure paths
    DB-->>ES: commit
    alt serialization conflict
        ES->>DB: rollback
        ES->>ES: retry move_entity (bounded retry policy)
    end
    ES-->>TA: success
```



Write-concurrency rule for hierarchy mutations (`create/move/delete`):

- authoritative invariant checks MUST run inside the same write transaction that applies closure/entity mutations
- a write transaction whose invariant check reads a predicate over rows the write itself does not lock MUST use `SERIALIZABLE` isolation, to prevent a phantom read between that check and the mutation under concurrent hierarchy mutations — rewriting closure rows across a subtree (`move`, a parent-changing `update`, a **force** delete) is one case of this, and so is `create`'s depth/width check, which reads the parent's depth and sibling count without locking either. A write that instead locks whatever its decision was read from — one row by primary key, or a row lock such as `SELECT ... FOR UPDATE` — does not need it — see the retry-policy section for which operation is which
- serialization conflicts are handled by bounded retry with deterministic error mapping when retries are exhausted

#### AuthZ + RG + SQL Responsibility Split

**ID**: `cpt-cf-resource-group-seq-authz-rg-sql-split`

```mermaid
sequenceDiagram
    participant PEP as Domain PEP
    participant AZ as AuthZ Resolver Plugin
    participant RG as ResourceGroupReadHierarchy
    participant CMP as PEP Constraint Compiler
    participant DB as Domain DB

    PEP->>AZ: evaluate(subject, action, resource, context)
    AZ->>RG: list_group_depth(tenant_id, ...)
    RG-->>AZ: graph data only
    AZ-->>PEP: decision + constraints
    PEP->>CMP: compile constraints
    CMP->>DB: execute scoped SQL
```



This is the fixed boundary:

- RG returns graph data only.
- AuthZ plugin creates constraints.
- PEP/compiler creates SQL.

#### gear initialization Order

**ID**: `cpt-cf-resource-group-seq-init-order`

RG Management API depends on AuthZ SDK; AuthZ plugin depends on RG Access API SDK. This circular dependency is resolved by phased initialization:

```
Phase 1 (SystemCapability):
  1. RG gear init (deps: [authz_resolver, types_registry]; the Types Registry platform API is resolved from ClientHub, so Types Registry initializes first)
     → registers ResourceGroupClient in ClientHub
     → registers ResourceGroupReadHierarchy in ClientHub
     → REST/gRPC endpoints NOT yet accepting traffic

  2. AuthZ Resolver init (deps: [types-registry])
     → registers AuthZResolverApi in ClientHub
     → plugin discovery is lazy (first evaluate() call)

Phase 2 (ready):
  3. RG Gear starts accepting REST/gRPC traffic
     → write operations call PolicyEnforcer → AuthZResolverApi (available since step 2)
     → seed operations run as pre-deployment step with system SecurityContext (bypass AuthZ)

  4. AuthZ plugin on first evaluate() call
     → lazy-discovers RG plugin via types-registry
     → calls ResourceGroupReadHierarchy (available since step 1)
```

There is no deadlock: RG registers its read clients before AuthZ initializes, and AuthZ registers its client before RG starts accepting write traffic. Seed operations run as a pre-deployment step with a system-level `SecurityContext` and bypass the AuthZ evaluation path.

#### End-to-End Authorization Flow (Example)

**ID**: `cpt-cf-resource-group-seq-e2e-authz-flow`

Concrete example: a user of tenant `T1` requests a list of courses. The tenant hierarchy grants access to courses in `T1` and its child tenant `T7`.

```text
Tenant hierarchy:
  tenant T1 (11111111-...)
  └── tenant T7 (77777777-...)
  tenant T9 (99999999-...)
```

```mermaid
sequenceDiagram
    participant U as User (tenant T1)
    participant GW as API Gateway
    participant AN as AuthNResolverClient
    participant CS as Courses Service
    participant PE as PolicyEnforcer
    participant AZ as AuthZ Resolver Plugin
    participant RG as ResourceGroupReadHierarchy
    participant DB as Courses DB

    U->>GW: GET /api/lms/v1/courses (JWT: subject_id, subject_tenant_id=T1)
    GW->>AN: authenticate(bearer_token)
    AN-->>GW: SecurityContext {subject_id, subject_tenant_id=T1, token_scopes}
    GW->>CS: handler(ctx: SecurityContext)

    CS->>PE: access_scope(ctx, COURSE, "list", None)
    PE->>AZ: evaluate(SecurityContext, EvaluationRequest)
    Note right of AZ: subject.properties.tenant_id = T1<br/>action.name = "list"<br/>resource.type = "gts.cf.lms.course.v1~"<br/>context.require_constraints = true<br/>context.supported_properties = ["owner_tenant_id"]

    AZ->>RG: list_group_depth(system_ctx, T1, filter: "hierarchy/depth ge 0 and type eq 'tenant'")
    RG-->>AZ: [{T1, depth:0, metadata:{}}, {T7, depth:1, metadata:{self_managed:true}}]
    Note right of AZ: AuthZ policy logic (not RG):<br/>T7.metadata.self_managed=true, caller≠T7<br/>→ exclude T7 scope from AccessScope

    AZ-->>PE: decision=true, constraints=[{owner_tenant_id IN (T1)}]
    PE->>PE: compile_to_access_scope()
    PE-->>CS: AccessScope {owner_tenant_id IN (T1)}

    CS->>DB: SELECT * FROM courses WHERE owner_tenant_id IN (T1)
    DB-->>CS: courses from T1 only (T7 invisible due to barrier)
    CS-->>U: 200 OK [{...T1 courses...}]
```

Step-by-step:

1. **AuthN** — API Gateway extracts JWT bearer token, calls `AuthNResolverClient.authenticate()`. The authn plugin validates the token and returns a `SecurityContext` with `subject_id`, `subject_tenant_id = T1`, and `token_scopes`. Gateway injects `SecurityContext` into request extensions.

2. **Domain service** — Courses handler receives the request with `SecurityContext`. Before querying the database, it calls `PolicyEnforcer.access_scope(&ctx, &COURSE_RESOURCE, "list", None)` to obtain row-level access constraints.

3. **AuthZ evaluation** — `PolicyEnforcer` builds an `EvaluationRequest` (subject with `tenant_id = T1`, action `"list"`, resource type `"gts.cf.lms.course.v1~"`, `require_constraints = true`, `supported_properties = ["owner_tenant_id"]`) and calls `AuthZResolverApi.evaluate(ctx, request)`, passing the caller's `SecurityContext` ahead of the `EvaluationRequest` and receiving `Result<EvaluationResponse, CanonicalError>`.

4. **Hierarchy resolution** — The AuthZ plugin calls `ResourceGroupReadHierarchy.list_group_depth()` with `tenant_id = T1` and a depth filter to resolve the tenant hierarchy. RG returns `[T1 (depth 0), T7 (depth 1)]` — the accessible tenant subtree. The plugin does NOT see `T9` because it is outside `T1`'s hierarchy.

5. **Barrier filtering (TR + AuthZ, not RG)** — The AuthZ plugin calls Tenant Resolver `get_descendants(T1, BarrierMode::Respect)`. TR skips T7 entirely (`self_managed = true`) and returns `[T1]`. Alternatively, AuthZ queries `tenant_closure` with `AND barrier = 0` — same result. RG played no role in this filtering.

6. **Constraint generation** — After barrier filtering, the AuthZ plugin produces constraints: `owner_tenant_id IN (T1)` (T7 and its subtree excluded). This is returned in `EvaluationResponse` with `decision = true`.

7. **Constraint compilation** — `PolicyEnforcer` calls `compile_to_access_scope()` which converts the PDP constraints into an `AccessScope` with `ScopeFilter::in("owner_tenant_id", [T1])`.

8. **SQL execution** — Courses service applies the `AccessScope` via SecureORM, which appends `WHERE owner_tenant_id IN ('T1')` to the query. The user sees courses from T1 only. T7's courses, groups, and memberships are invisible.

Key separation of concerns:

| Component | Knows about | Does NOT know about |
| --------- | ----------- | ------------------- |
| Courses service | course domain, SQL schema | tenant hierarchy, access policies |
| AuthZ plugin | access policies, tenant hierarchy (via RG) | courses, SQL schema |
| RG | hierarchy data, group membership | courses, access policies, SQL |

#### RG Authentication Modes: JWT vs MTLS

**ID**: `cpt-cf-resource-group-seq-auth-modes`

RG Gear exposes its REST/gRPC API with **two authentication modes**. The mode determines whether the request passes through AuthZ evaluation.

##### Mode 1: JWT (public API — all endpoints) — implemented (`p1`)

Standard user/service requests authenticated via JWT bearer token. **All** RG REST API endpoints are available. Every request goes through AuthZ evaluation via `PolicyEnforcer`, same as any other domain service (e.g. courses). JWT token lifecycle (expiry, refresh, revocation) is managed by the AuthN Resolver and API Gateway — RG delegates token validation entirely to the platform authentication layer and does not implement its own session timeout or token renewal logic.

Applies to:
- `GET /api/resource-group/v1/groups` — list/get groups
- `POST/PUT/DELETE /api/resource-group/v1/groups/{group_id}` — group lifecycle
- `GET /api/resource-group/v1/groups/{group_id}/hierarchy` — hierarchy traversal
- `GET /api/resource-group/v1/memberships` — list memberships
- `POST/DELETE /api/resource-group/v1/memberships/{...}` — membership lifecycle

In the current monolith deployment, the AuthZ plugin reads hierarchy via the in-process
`ResourceGroupReadHierarchy` trait registered in `ClientHub`. That path
**bypasses `PolicyEnforcer` invocation** by construction — the plugin
cannot evaluate itself (circular dependency), so the trait
implementation in `RgReadService` delegates to unscoped reads
(`AccessScope::allow_all()`) without going through `PolicyEnforcer`.
The bypass is on the *invocation* side only: AuthZ enforcement is
still produced by the plugin **after** it receives the hierarchy data,
in the form of tenant / subtree constraints attached to the original
caller's request. The `dyn ResourceGroupReadHierarchy` type narrows
the surface to hierarchy-only operations, so the plugin cannot reach
non-hierarchy reads or any write paths through this channel.

##### Mode 2: MTLS (private API — hierarchy endpoint only) — deferred (`p2`, not implemented yet)

> **Status: `p2` — designed, not implemented yet.** The MTLS path is the
> service-to-service authentication mode planned for a future microservice
> deployment that splits the AuthZ plugin out of the RG process. The
> current monolith uses the in-process `ResourceGroupReadHierarchy` path
> described above and does not need this transport. The design is kept
> here so the future split has a documented contract; do not implement
> this in the current iteration.

Service-to-service requests authenticated via mutual TLS client certificate. Used exclusively by AuthZ plugin to read tenant hierarchy. **Only one endpoint** is available in MTLS mode:

- `GET /api/resource-group/v1/groups/{group_id}/hierarchy` — hierarchy traversal

All other endpoints return `403 Forbidden` in MTLS mode. This is enforced by RG gateway-level allowlist, not by AuthZ evaluation.

MTLS requests **bypass AuthZ evaluation entirely** — no `PolicyEnforcer` call, no `access_evaluation_request`. This is critical because:
1. AuthZ plugin **is the caller** — it cannot evaluate itself (circular dependency)
2. MTLS certificate identity is a trusted system principal — access is granted by transport-level authentication
3. The single allowed endpoint returns read-only hierarchy data — minimal attack surface

##### Authentication Decision Flow (`p2` — covers the future MTLS path)

> **Status: `p2` — applies only when the deferred MTLS path lands.** Today
> all traffic goes through the JWT branch.

RG Gateway receives requests from two types of callers and routes them through different authentication paths:

- **JWT path** (`p1`) — Admin (Instance/Tenant) or App sends a request with a bearer token. RG Gateway delegates authentication to AuthN Resolver, then runs AuthZ evaluation via `PolicyEnforcer` before executing the query.
- **MTLS path** (`p2`, deferred) — AuthZ Plugin (in microservice deployment) sends a request with a client certificate. RG Gateway verifies the certificate against a trusted CA bundle, checks the endpoint allowlist, and executes directly without AuthZ evaluation.

```mermaid
flowchart TD
    REQ["Incoming request to RG REST API<br/>(from Admin, App, or AuthZ Plugin)"] --> AUTH_CHECK{RG Gateway:<br/>authentication method?}

    AUTH_CHECK -->|"JWT bearer token<br/>(Admin / App) — p1"| JWT_PATH[AuthN Resolver validates JWT]
    JWT_PATH --> SEC_CTX[SecurityContext extracted from token]
    SEC_CTX --> AUTHZ["RG Gateway calls<br/>PolicyEnforcer.access_scope()"]
    AUTHZ --> CONSTRAINTS[RG Gateway applies<br/>AccessScope to query]
    CONSTRAINTS --> EXEC["RG Service executes<br/>query with SQL predicates"]

    AUTH_CHECK -->|"MTLS client cert<br/>(AuthZ Plugin) — p2 deferred"| MTLS_PATH["RG Gateway verifies client cert<br/>against trusted CA bundle"]
    MTLS_PATH --> ENDPOINT_CHECK{RG Gateway:<br/>endpoint in MTLS allowlist?}
    ENDPOINT_CHECK -->|"Yes: /groups/{id}/hierarchy"| SYSTEM_CTX["RG Gateway creates<br/>System SecurityContext"]
    SYSTEM_CTX --> EXEC_DIRECT["RG Hierarchy Service executes<br/>directly — no AuthZ evaluation"]
    ENDPOINT_CHECK -->|No: any other endpoint| REJECT[403 Forbidden]

    style REJECT fill:#f66,color:#fff
    style EXEC_DIRECT fill:#6b6,color:#fff
    style EXEC fill:#6b6,color:#fff
```

##### Sequence: MTLS request from AuthZ plugin (`p2` — deferred, not implemented yet)

**ID**: `cpt-cf-resource-group-seq-mtls-authz-read`

> **Status: `p2` — design retained for the future microservice split. Not
> implemented in the current monolith.**

```mermaid
sequenceDiagram
    participant AZ as AuthZ Plugin
    participant RG_GW as RG Gateway
    participant RG_SVC as RG Hierarchy Service
    participant DB as RG Database

    AZ->>RG_GW: GET /groups/{T1}/hierarchy (MTLS cert)
    RG_GW->>RG_GW: verify client certificate
    RG_GW->>RG_GW: check endpoint allowlist → ✓ /groups/{id}/hierarchy

    Note over RG_GW: MTLS mode: skip AuthZ evaluation

    RG_GW->>RG_SVC: list_group_depth(system_ctx, T1, query)
    RG_SVC->>DB: SELECT rg.*, c.depth FROM resource_group_closure c JOIN resource_group rg ON c.descendant_id = rg.id WHERE c.ancestor_id = T1
    DB-->>RG_SVC: [{T1, depth:0}, {T7, depth:1}]
    RG_SVC-->>RG_GW: Page<ResourceGroupWithDepth>
    RG_GW-->>AZ: 200 OK [{T1, depth:0}, {T7, depth:1}]
```

##### Sequence: JWT request from user to RG (same AuthZ flow as any domain service)

**ID**: `cpt-cf-resource-group-seq-jwt-rg-request`

```mermaid
sequenceDiagram
    participant U as User (tenant T1)
    participant GW as API Gateway
    participant AN as AuthN Resolver
    participant RG_GW as RG Gateway
    participant PE as PolicyEnforcer
    participant AZ as AuthZ Plugin
    participant RG_HIER as RG ReadHierarchy (internal)
    participant RG_SVC as RG Service
    participant DB as RG Database

    U->>GW: GET /api/resource-group/v1/groups?$filter=... (JWT)
    GW->>AN: authenticate(bearer_token)
    AN-->>GW: SecurityContext {subject_id, tenant_id=T1}
    GW->>RG_GW: handler(ctx)

    Note over RG_GW: JWT mode: run AuthZ evaluation

    RG_GW->>PE: access_scope(ctx, RESOURCE_GROUP, "list")
    PE->>AZ: evaluate(EvaluationRequest)
    AZ->>RG_HIER: list_group_depth(system_ctx, T1, ...) [in-process via ClientHub; MTLS variant: p2 — deferred]
    RG_HIER-->>AZ: [{T1, depth:0, barrier:false}, {T7, depth:1, barrier:true}]
    Note over AZ: TR: BarrierMode::Respect → T7 skipped<br/>AuthZ: exclude T7 from AccessScope
    AZ-->>PE: decision=true, constraints=[owner_tenant_id IN (T1)]
    PE-->>RG_GW: AccessScope {owner_tenant_id IN (T1)}

    RG_GW->>RG_SVC: list_groups(ctx, query, access_scope)
    RG_SVC->>DB: SELECT * FROM resource_group WHERE tenant_id IN (T1) AND ...
    DB-->>RG_SVC: groups
    RG_SVC-->>RG_GW: Page<ResourceGroup>
    RG_GW-->>U: 200 OK
```

Note: when a user calls RG REST API with JWT, the AuthZ flow is **identical** to any other domain service (courses, users, etc.):
1. API Gateway authenticates JWT → `SecurityContext`
2. RG gateway calls `PolicyEnforcer.access_scope()` → AuthZ evaluates → constraints returned
3. RG applies `AccessScope` to its own query via SecureORM (SecureORM maps AuthZ property `owner_tenant_id` to actual column `tenant_id` in `resource_group` table)
4. AuthZ plugin internally reads hierarchy via `ResourceGroupReadHierarchy` — in-process via `ClientHub` (`p1`) today, with the MTLS transport variant (`p2`, deferred / not implemented yet) reserved for the future microservice split — this internal read bypasses AuthZ

The key insight: RG is simultaneously a **consumer** of AuthZ (for its own JWT-authenticated endpoints) and a **data provider** for AuthZ (via in-process ClientHub hierarchy reads; an MTLS variant is `p2` — deferred). The bypass at the trait/transport level prevents the circular call.

##### MTLS Configuration and Certificate Verification (`p2` — deferred, not implemented yet)

> **Status: `p2` — designed, not implemented yet.** Configuration shape
> retained for the future microservice split; do not wire this in the
> current monolith.

MTLS authentication will be configured at the RG gateway level and includes two parts: certificate trust and endpoint allowlist.

**Certificate verification process** (performed by RG Gateway on every MTLS request):

1. RG Gateway extracts the client certificate from the TLS handshake.
2. Certificate is validated against the trusted CA bundle (`ca_cert`): signature chain, expiration, revocation status.
3. Client identity (certificate `CN` / `SAN`) is matched against `allowed_clients` list. If the client is not in the list, the request is rejected with `403 Forbidden`.
4. Only after identity verification, the endpoint is checked against `allowed_endpoints`. If the endpoint is not in the allowlist, `403 Forbidden` is returned.

```yaml
gears:
  resource_group:
    mtls:
      enabled: true
      # Trusted CA bundle for verifying client certificates.
      # In production: internal PKI CA that issues service certificates.
      ca_cert: /etc/ssl/certs/internal-ca.pem
      # Clients allowed to connect via MTLS (matched by certificate CN).
      allowed_clients:
        - cn: authz-resolver-plugin
      # Endpoints reachable via MTLS. All other endpoints return 403.
      allowed_endpoints:
        - method: GET
          path: /api/resource-group/v1/groups/{group_id}/hierarchy
```

Only explicitly listed method+path combinations are reachable via MTLS. Any request to an unlisted endpoint returns `403 Forbidden` regardless of certificate validity. Similarly, a valid certificate from a client not in `allowed_clients` is rejected.

##### In-Process vs Out-of-Process

| Deployment | AuthZ → RG hierarchy read | Auth mechanism | Status |
| ---------- | ------------------------- | -------------- | ------ |
| Monolith (single process) | `hub.get::<dyn ResourceGroupReadHierarchy>()` — direct in-process call via ClientHub | No network auth needed — trusted in-process call, system `SecurityContext` | `p1` — implemented |
| Microservices (separate processes) | gRPC/REST call to RG service | MTLS client certificate — only `/groups/{id}/hierarchy` endpoint allowed | `p2` — deferred, not implemented yet |

In both cases, the AuthZ plugin uses `ResourceGroupReadHierarchy` trait. The trait implementation is either a direct local call (monolith — `p1`) or an MTLS-authenticated remote call (microservices — `p2`, deferred / not implemented yet). The RG gateway applies the same allowlist logic in both cases — but in monolith mode, the in-process ClientHub path skips the gateway entirely (no HTTP, no MTLS, no allowlist check needed — the type system enforces that only `list_group_depth` is callable via `dyn ResourceGroupReadHierarchy`).

### 3.7 Database schemas & tables

Group types are not stored in RG. They are GTS Type Schemas in Types Registry; the `gts_type_id` columns below hold the Types Registry Registry Reference (UUID) of the type, with no foreign key. The former tables `gts_type`, `gts_type_allowed_parent`, `gts_type_allowed_membership` and the `gts_type_path` domain no longer exist (see ADR-002).

#### Table: `resource_group`


| Column        | Type        | Description                                       |
| ------------- | ----------- | ------------------------------------------------- |
| `id`          | UUID        | entity ID (PK, default `gen_random_uuid()`)       |
| `parent_id`   | UUID NULL   | parent entity (FK to `resource_group.id`)         |
| `gts_type_id` | UUID        | Types Registry Registry Reference of the group type (no FK) |
| `is_tenant`   | BOOLEAN     | `true` when the type id starts with `TENANT_RG_TYPE_PATH`; derived at creation, never changes |
| `name`        | TEXT        | display name                                      |
| `metadata`    | JSONB NULL  | type-specific fields for group instance, validated against the schema of the group's type from Types Registry. For types supporting barrier semantics, includes `metadata.self_managed` (boolean). |
| `tenant_id`   | UUID        | tenant scope                                      |
| `created_at`     | TIMESTAMPTZ | creation time                                     |
| `updated_at`    | TIMESTAMPTZ | update time (nullable)                            |


Constraints:

- FK `parent_id` → `resource_group(id)` ON UPDATE CASCADE ON DELETE RESTRICT

Indexes:

- `(parent_id)`
- `(name)`
- `(gts_type_id, id)` — composite for type-scoped queries
- `(tenant_id)` — SecureORM tenant scope filtering
- UNIQUE `(is_tenant)` WHERE `parent_id IS NULL AND is_tenant` (`uq_rg_tenant_root`) — at most one tenant root in the whole forest

#### Table: `resource_group_membership`


| Column          | Type        | Description                                |
| --------------- | ----------- | ------------------------------------------ |
| `group_id`      | UUID        | group entity ID (FK to `resource_group.id`)|
| `gts_type_id`   | UUID        | Types Registry Registry Reference of the member resource type (no FK) |
| `resource_id`   | TEXT        | caller-defined resource identifier         |
| `created_at`       | TIMESTAMPTZ | creation time                              |

Tenant scope is not stored on membership rows. It is derived from `resource_group.tenant_id` via JOIN on `group_id` — including the "one tenant per resource" check, which asks whether any membership of the pair belongs to another tenant and is answered by that JOIN with `LIMIT 1`, inside the same `SERIALIZABLE` transaction as the insert it gates.

Constraints/indexes:

- UNIQUE `(group_id, gts_type_id, resource_id)`
- FK `group_id` → `resource_group(id)` ON UPDATE CASCADE ON DELETE RESTRICT
- index `(gts_type_id, resource_id)` — for reverse lookups by resource
- in ownership-graph usage, tenant scope is validated against operation context for tenant-scoped callers via the referenced group's `tenant_id`

#### Table: `resource_group_closure`


| Column          | Type    | Description                |
| --------------- | ------- | -------------------------- |
| `ancestor_id`   | UUID    | ancestor (parent on path), FK to `resource_group(id)` |
| `descendant_id` | UUID    | descendant (child on path), FK to `resource_group(id)` |
| `depth`         | INTEGER | distance, 0 for self       |


Constraints/indexes:

- PRIMARY KEY `(ancestor_id, descendant_id)`
- FK `ancestor_id` → `resource_group(id)` ON UPDATE CASCADE ON DELETE RESTRICT
- FK `descendant_id` → `resource_group(id)` ON UPDATE CASCADE ON DELETE RESTRICT
- index `(descendant_id)` — for ancestor lookups
- index `(ancestor_id, depth)` — for descendant queries with depth filtering
- self-row required for each entity (`ancestor_id = descendant_id`, `depth = 0`)

Compatibility note:

- AuthZ predicates require only `ancestor_id/descendant_id`.
- `depth` is additional metadata for ordered hierarchy reads.

### 3.8 Query Profile Enforcement

Config:

- `max_depth`: optional positive integer, default `10` (recommended for default performance profile), configurable without hard upper bound; `null`/absent disables depth limit
- `max_width`: optional positive integer; `null`/absent disables width limit

Enforcement rules:

- reads are not truncated when stored data already violates tightened profile
- writes are rejected when they create/increase violation for enabled limits:
  - `DepthLimitExceeded`
  - `WidthLimitExceeded`

Profile reduction for enabled limits requires external operator migration to restore compliance.

Ownership-graph tenant enforcement:

- parent-child edges must be tenant-hierarchy-compatible (same-tenant or allowed related-tenant link)
- membership tenant scope is derived from the target group's `tenant_id`; tenant-scoped callers must stay within effective tenant scope from `subject_tenant_id`
- platform-admin provisioning calls may bypass caller-tenant scope checks, but cannot create tenant-incompatible links
- violations return deterministic conflict/validation errors

### 3.9 Error Mapping


| Failure                     | Public Error               |
| --------------------------- | -------------------------- |
| invalid input               | `Validation`               |
| missing type/entity         | `NotFound` (unknown group type on create: 404 type not found) |
| unknown membership resource type | `Validation` (HTTP 400) |
| Types Registry unavailable  | `ServiceUnavailable` (HTTP 503 with retry hint) |
| stored type reference that Types Registry cannot resolve | `Internal` (HTTP 500, data inconsistency) |
| second tenant-type root rejected | `TenantRootAlreadyExists` (409 Conflict) — enforces `cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness` |
| invalid parent type         | `InvalidParentType`        |
| cycle attempt               | `CycleDetected`            |
| active references on delete | `ConflictActiveReferences` (response body MUST include list of blocking entities — children and/or memberships — so the caller can display what prevents deletion) |
| depth/width violation       | `LimitViolation`           |
| tenant-incompatible parent/child/membership write | `TenantIncompatibility` |
| second tenant-type root rejected | `TenantRootAlreadyExists` (HTTP 409 Conflict) |
| infra timeout/unavailable   | `ServiceUnavailable`       |
| unexpected failure          | `Internal`                 |


## 4. Additional Context

### Non-Applicable Design Domains

- **Usability (UX)**: Not applicable — RG is a backend infrastructure gear; no frontend architecture or user-facing UI.
- **Compliance (COMPL)**: Not applicable — compliance controls are platform-level; RG does not own regulated data directly. Consuming gears and AuthZ are responsible for compliance architecture.
- **Operations (OPS)**: RG follows standard Gears deployment, logging, and monitoring patterns. No RG-specific deployment topology, observability, or SLO architecture beyond platform defaults.
- **Event Architecture (INT-003)**: Not applicable in v1 — RG does not publish domain events. Consumers read current state via SDK traits. Domain events (group lifecycle, membership changes) are a candidate for future versions if consumer demand arises.

### Performance Architecture

RG relies on database-level performance rather than application-level caching:

- **No application-level cache**: hierarchy and membership reads go directly to PostgreSQL. Performance is ensured by indexed closure table lookups (btree indexes on `ancestor_id`, `descendant_id`, `depth`). The closure table pattern eliminates N+1 queries by design — a single SQL query returns the complete ancestor/descendant set.
- **Connection pooling**: handled by platform database infrastructure (connection pool configuration is deployment-specific).
- **Scalability approach**: vertical scaling of the database instance is the primary strategy. Horizontal read replicas can be added for read-heavy AuthZ query paths. `resource_group_membership` partitioning is a candidate optimization for production scale (see PRD Open Questions).
- **Application-tier horizontal scaling**: RG is a stateless service with no in-process caches, sessions, or local state. Multiple RG instances can run behind the platform load balancer without session affinity. Scaling the application tier is a simple matter of increasing replica count — the platform orchestration layer handles load distribution. All coordination is delegated to PostgreSQL (SERIALIZABLE transactions for write consistency, connection pool for concurrency control).
- **Query cost protection**: all list endpoints enforce `limit` (max 200 per page). Unbounded hierarchy traversals are bounded by the `max_depth` query profile, but only where one is configured: `null`/absent disables the depth limit (§3.8), so unless a deployment sets it, traversal depth is unbounded. Database-level query timeout (`statement_timeout`) and lock wait (`lock_timeout`) are PostgreSQL runtime parameters, set per connection through the database config's `params` map — any key `toolkit-db` does not recognize is passed to the server as a runtime option. No platform default is set in this repository, so unless a deployment supplies one, neither is bounded. API-layer rate limiting is handled by the API gateway — RG does not implement its own throttling.
- **Closure write amplification bounds**: subtree move operations update `O(N × D)` closure rows where N = subtree size and D = depth. With `max_depth = 10` and typical organizational hierarchies (width >> depth), expected subtree sizes for move operations are under 10K nodes. For larger subtrees, SERIALIZABLE isolation + bounded retry (max 3) prevents runaway transactions. No hard cap on subtree size is enforced — `max_depth` and `max_width` provide indirect bounds.
- **Optimistic concurrency**: v1 uses last-write-wins semantics for `PUT /groups/{id}`. ETag-based optimistic concurrency control is a candidate for future versions if concurrent update conflicts become a production concern.
- **Latency budget** (target p95 < 250 ms for hierarchy queries, default profile `max_depth = 10`):

| Layer | Budget | Notes |
| ----- | ------ | ----- |
| SQL execution | ≤ 150 ms | Indexed closure table query; measured < 0.5 ms for 100K groups in test env |
| Domain logic | ≤ 20 ms | Validation, mapping, profile checks |
| REST serialization | ≤ 50 ms | JSON encoding, OData parsing |
| Overhead | ≤ 30 ms | Connection pool, middleware, network |

### Security Architecture

- **Data protection**: encryption at rest and in transit is handled at platform infrastructure level (database encryption, TLS termination at API gateway). RG does not implement its own encryption.
- **Data classification**: RG stores organizational hierarchy structure and opaque resource identifiers. Resource IDs may reference PII-containing entities in consuming gears, but RG treats them as opaque strings. See PRD section 6.6.
- **Threat model**:

| Threat | Attack Vector | Impact | Mitigation |
| ------ | ------------- | ------ | ---------- |
| Cycle injection | Concurrent move operations creating circular parent-child | Data corruption, infinite traversals | SERIALIZABLE isolation + cycle detection in same tx |
| Tenant boundary escape | Cross-tenant membership or parent-child link | Unauthorized data access via hierarchy traversal | Tenant compatibility validation on all writes |
| Privilege escalation via membership | Adding resource to higher-privilege group | Unauthorized access to resources | AuthZ evaluation via `PolicyEnforcer` on all JWT endpoints |
| Hierarchy DoS | Creating extremely deep/wide trees | Query latency degradation, resource exhaustion | `max_depth`/`max_width` query profile enforcement |
| Unauthorized subtree access | Accessing hierarchy outside caller tenant scope | Information disclosure | SecureORM `WHERE tenant_id IN (...)` on all reads |
| Unauthorized type manipulation | Modifying type rules in Types Registry to weaken hierarchy constraints | Weakened access control boundaries | Out of RG scope: Types Registry registration policy and AuthZ govern who may register or evolve types; compatible-only evolution inside a major version; RG applies the rules to new writes only |
- **Audit logging**: RG delegates to platform-level request audit logging. No RG-specific audit trail beyond standard Gears access logs and database-level `created_at`/`updated_at` timestamps. See PRD section 6.7.
- **Authorization permissions**: RG REST API endpoints are protected by `PolicyEnforcer` (standard Gears AuthZ flow). Permission mapping for RG operations is configured in the AuthZ policy, not hardcoded in RG. RG does not define its own permission matrix — it relies on the platform AuthZ policy configuration. The table below lists expected permissions (`resource.type` + `action.name` as per AuthZ PDP request model — see [AuthZ DESIGN §Scope Determination](../../../../docs/arch/authorization/DESIGN.md)) and the REST operations where `PolicyEnforcer` is invoked:

| Resource (`resource.type`) | Action (`action.name`) | REST Operation | Method | Path | Auth Mode |
| -------------------------- | ---------------------- | -------------- | ------ | ---- | --------- |
| `gts.cf.core.rg.group.v1~` | `list` | `listGroups` | GET | `/groups` | JWT |
| `gts.cf.core.rg.group.v1~` | `create` | `createGroup` | POST | `/groups` | JWT |
| `gts.cf.core.rg.group.v1~` | `read` | `getGroup` | GET | `/groups/{group_id}` | JWT |
| `gts.cf.core.rg.group.v1~` | `update` | `updateGroup` | PUT | `/groups/{group_id}` | JWT |
| `gts.cf.core.rg.group.v1~` | `delete` | `deleteGroup` | DELETE | `/groups/{group_id}` | JWT |
| `gts.cf.core.rg.group.v1~` | `read` | `listGroupHierarchy` | GET | `/groups/{group_id}/hierarchy` | JWT |
| _(AuthZ bypassed)_ | — | `listGroupHierarchy` | GET | `/groups/{group_id}/hierarchy` | MTLS |
| `gts.cf.core.rg.group_membership.v1~` | `list` | `listMemberships` | GET | `/memberships` | JWT |
| `gts.cf.core.rg.group_membership.v1~` | `create` | `addMembership` | POST | `/memberships/{group_id}/{resource_type}/{resource_id}` | JWT |
| `gts.cf.core.rg.group_membership.v1~` | `delete` | `deleteMembership` | DELETE | `/memberships/{group_id}/{resource_type}/{resource_id}` | JWT |

Notes:
  - `resource.type` and `action.name` are illustrative names following Gears AuthZ conventions. Actual GTS type paths and action names are configured in the AuthZ policy.
  - Standard action vocabulary: `list` (collection), `read` (single resource), `create`, `update`, `delete` — aligned with [AuthZ usage scenarios](../../../../docs/arch/authorization/AUTHZ_USAGE_SCENARIOS.md).
  - The AuthZ plugin reads hierarchy in-process via `ResourceGroupReadHierarchy` registered in `ClientHub` and **bypasses `PolicyEnforcer` invocation** on those reads (`AccessScope::allow_all()`); the plugin still produces AuthZ tenant/subtree constraints from the returned hierarchy — see [RG Authentication Modes: JWT vs MTLS](#rg-authentication-modes-jwt-vs-mtls).
  - MTLS-authenticated requests (AuthZ plugin only) **also** bypass `PolicyEnforcer` entirely — `p2`, **deferred / not implemented yet**, planned for the future microservice split — see [RG Authentication Modes: JWT vs MTLS](#rg-authentication-modes-jwt-vs-mtls).
  - `listGroupHierarchy` shares `resource_group` + `read` permission with `getGroup` — both are group read operations; the AuthZ policy may differentiate them if needed.

### Reliability Architecture

RG is a stateless service layer backed by a PostgreSQL database:

- **Fault tolerance**: HA, failover, and backup are handled at the platform database infrastructure level. RG does not implement its own circuit breakers or redundancy beyond transaction retry for serialization conflicts (see Concurrency Testing section).
- **Types Registry dependency**: group creation, group metadata validation and membership creation need a type lookup through the Types Registry SDK. If Types Registry is unavailable these writes fail with **503 Service Unavailable** and a retry hint, before any database transaction starts; reads of existing groups and memberships do not need type data and are unaffected. RG keeps no type cache until the Types Registry SDK provides one. A stored type reference that Types Registry cannot resolve is a data inconsistency and maps to 500.
- **AuthZ dependency resilience**: Circuit breaking for the AuthZ Resolver dependency (PolicyEnforcer calls on JWT path) is handled at the platform PolicyEnforcer/SDK level. If the AuthZ Resolver becomes unavailable, JWT-authenticated RG requests will fail with **503 Service Unavailable** errors. RG does not implement its own circuit breaker for this dependency.
- **Recovery**: RPO/RTO follow platform defaults for stateful services with PostgreSQL persistence. No gear-specific recovery architecture.

### Data Governance

- **Data ownership**: RG gear owns all data in the `resource_group`, `resource_group_membership`, and `resource_group_closure` tables. Group types are owned by Types Registry; RG stores only their Registry References.
- **Data lineage**: RG data is produced by RG API consumers (Instance/Tenant Admins, Apps) and consumed by AuthZ plugin via `ResourceGroupReadHierarchy` for tenant hierarchy resolution.
- **Data dictionary**: field definitions are documented in Database Schemas section (3.7).

### API Versioning

REST API uses path-based versioning (`/api/resource-group/v1/`). SDK trait contracts (`ResourceGroupClient`, `ResourceGroupReadHierarchy`) are stable interfaces — breaking changes require a new trait version and a migration path for consumers. Breaking change policy follows platform conventions.

### Observability

RG follows standard Gears observability patterns:

- **Logging**: structured request/response logging via platform middleware (request ID, tenant ID, operation, latency). Domain-level events (group created, group moved, membership added) logged at INFO level. Error paths logged at WARN/ERROR with deterministic error category.
- **Metrics**: standard HTTP endpoint metrics (request count, latency histogram, error rate) exposed via platform metrics infrastructure. No RG-specific custom metrics in v1.
- **Alerting**: follows platform alerting defaults for error rate and latency thresholds. No RG-specific alert rules in v1.
- **Health checks**: RG delegates health check and readiness/liveness probe endpoints to the platform infrastructure layer (`toolkit` runtime). The platform exposes standard health endpoints (e.g., `GET /health`, `GET /ready`) that include database connectivity checks. RG does not implement its own health check endpoint.
- **Distributed tracing**: RG participates in platform distributed tracing via OpenTelemetry trace propagation injected by platform middleware. Request spans include `request_id`, `tenant_id`, and operation context. All API handlers MUST use `#[tracing::instrument]` and enrich spans with business context fields (e.g., `type_code`, `group_id`, `app_id`) via `tracing::Span::current().record()` to enable effective production debugging and request correlation.

### Architecture Evolution: RG as Persistent Storage for Types Registry

Group types were first stored by RG itself (three local tables, a type CRUD API and an init-time bootstrap used by other gears) and exposed first under the RG URL namespace and then under the Types Registry URL namespace. That evolution is done, and it did not take the planned saga form: RG no longer stores types at all. Types are GTS Type Schemas in Types Registry derived from `gts.cf.core.rg.type.v1~`, and RG keeps only the Types Registry Registry Reference in its group and membership rows (see ADR-002).

What this means for the design:

- Types Registry is the single source of truth for group types; RG has no type endpoints, no type SDK methods, no type service, no seeding and no bootstrap, and gears that own types (Account Management) declare them statically instead of registering them at startup.
- RG depends on the Types Registry SDK for type lookups; the lookup happens before the database transaction and is not cached by RG.
- There is no cross-gear consistency protocol to run: type evolution is governed by Types Registry compatibility rules, and RG applies the current rules to new writes only.
- The migration recreated the RG tables without carrying data over, because the gears were not deployed to production.
- Temporary: until Types Registry delivers its own publication of the RG type inventory (its P0 task T31), the Types Registry gear seeds the `gts.cf.core.rg.*` inventory into its database and publishes the local contracts itself. This bridge is removed when that task lands.

### Known Technical Debt

| Item | Trigger for Resolution | Priority |
| ---- | ---------------------- | -------- |
| ETag-based optimistic concurrency for `PUT /groups/{id}` | Concurrent update conflicts observed in production | Low |
| `resource_group_membership` partitioning (455M rows projected) | Table size >50 GB or query latency degradation | Medium |
| Domain events for group/membership lifecycle | Consumer demand for real-time notifications or cache invalidation | Medium |
| Type lookup cache | Types Registry SDK offers a cache (planned upstream); RG has none today | Medium |
| Parallel seeding for memberships via `JoinSet` | Large seed configurations with many independent items | Low |

### Open Questions

See [PRD §13 Open Questions](./PRD.md#13-open-questions) for active design questions including:

- Delete behavior modes (`leaf-only` vs `subtree-cascade`) — resolve before DECOMPOSITION
- `resource_group_membership` partitioning strategy — resolve before production
- Type lookup cache — waits for the Types Registry SDK cache

- AuthN/AuthZ gear contracts remain unchanged.
- AuthZ can operate without RG — RG is an optional data source.
- AuthZ extensibility is implemented through plugin behavior that consumes RG read contracts.
- RG provider is swappable by configuration (built-in gear or vendor-specific provider) without changing consumer contracts.
- SQL conversion remains in existing PEP flow (`PolicyEnforcer` + compiler), consistent with approved architecture.

### 4.1 Database Size Analysis & Production Projections

#### Test Environment Baseline

Benchmark environment used PostgreSQL 17 (Docker). PostgreSQL is not a required dependency — any SQL-compatible database supported by SeaORM can be used (see `cpt-cf-resource-group-constraint-db-agnostic`). Row sizes and storage projections are representative for typical RDBMS engines.

Test dataset: 100K groups, 200K memberships, 359K closure rows:

| Table | Rows | Data | Indexes+TOAST | Total | Avg Row |
|---|---|---|---|---|---|
| `resource_group` | 100,000 | 10 MB | 20 MB | **30 MB** | 101 B |
| `resource_group_closure` | 359,400 | 23 MB | 36 MB | **60 MB** | 68 B |
| `resource_group_membership` | 200,000 | 14 MB | 30 MB | **44 MB** | 73 B |
| **Total** | — | **48 MB** | **86 MB** | **135 MB** | — |

#### Column Widths (avg bytes, measured via pg_stats in test environment)

> The measurements below predate the move of group types to Types Registry, when `gts_type_id` was a 2 B SMALLINT and the three local type tables existed. `gts_type_id` is now a 16 B UUID Registry Reference on `resource_group` and `resource_group_membership`, and `resource_group` gains a 1 B `is_tenant` column. As an estimate (not re-measured): about +15 B per `resource_group` row, and +14 B per membership row in the table and in each index that contains `gts_type_id` (the primary key and `(gts_type_id, resource_id)`), i.e. roughly +6 GB data and +13 GB indexes at 455M memberships. The type tables are gone.

**resource_group** (~100 B/row): `id` 16 B (UUID), `parent_id` 16 B (UUID nullable), `gts_type_id` 2 B (SMALLINT at measurement time; 16 B UUID now), `name` 14 B (TEXT avg), `metadata` variable (JSONB nullable, typically 20–60 B when present; includes `barrier` for applicable types), `tenant_id` 16 B (UUID), `created_at`/`updated_at` 8 B each, row overhead ~20 B. Note: `barrier` is not a separate column — it is stored inside `metadata` JSONB as `metadata.self_managed` (see `cpt-cf-resource-group-principle-barrier-as-data`).

**resource_group_closure** (68 B/row): `ancestor_id` 16 B, `descendant_id` 16 B, `depth` 4 B, row overhead ~32 B.

**resource_group_membership** (73 B/row): `group_id` 16 B, `gts_type_id` 6 B at measurement time (16 B UUID now), `resource_id` 10 B, `created_at` 8 B, row overhead ~33 B.

#### Production Extrapolation

Assumptions: **1.5M tenants**, **303.5M users** (1–2 memberships each → ~455M), **~5M total groups**, average hierarchy depth ~3 → ~18M closure rows (ratio 3.59× from test dataset).

| Table | Rows | Data | Indexes | Total | % |
|---|---|---|---|---|---|
| `resource_group_membership` | 455M | 33.2 GB | 68.3 GB | **101.5 GB** | 95.3% |
| `resource_group_closure` | 18M | 1.15 GB | 1.80 GB | **2.95 GB** | 2.8% |
| `resource_group` | 5M | 560 MB | 1.0 GB | **1.6 GB** | 1.5% |
| **Total** | — | **~35 GB** | **~71.1 GB** | **~106.1 GB** | — |

Index-to-data ratio: **2.03×** (reasonable for btree-only indexes with UUID keys; higher ratio reflects compact data rows relative to multi-column index entries).

#### Key Observations

1. **Membership table dominates** — 455M rows, ~101.5 GB (95.5% of total). Any optimization here has the biggest impact.
2. **Closure table is manageable** — ~3 GB total. Indexes turned 50–121 ms queries into <0.5 ms.
3. **Memory requirements** — minimum 24 GB RAM (shared_buffers 6 GB), recommended 48 GB RAM (shared_buffers 12 GB) to keep hot indexes in memory.
4. **Partitioning candidate** — `resource_group_membership`: 455M rows, ~101.5 GB. Tenant scope is derived via `group_id` FK (not stored directly), so tenant-based partitioning would require adding a denormalized `tenant_id` column or using hash partitioning by `group_id`. Strategy needs evaluation (see PRD open questions).

### 4.2 Testing Architecture

#### Testing Levels

| Level | Database | Network | What is real | What is mocked |
|---|---|---|---|---|
| **Unit** | No DB — in-memory trait mocks | No network | Domain services, invariant logic, error mapping | All repositories (trait-based `InMemory*` impls) |
| **Integration** | SQLite in-memory (`:memory:`, per-test schema) | No network — direct repo calls | Repositories, closure table SQL, SecureORM tenant scoping, constraints | PostgreSQL-dialect SQL, SERIALIZABLE semantics |
| **API** | SQLite in-memory | No real network — `Router::oneshot()` (in-process HTTP simulation) | REST handlers, OData parsing, domain services, DB | `PolicyEnforcer` / `AuthZResolverApi` (mock Allow/Deny) |
| **E2E** | Real PostgreSQL (Docker or hosted) | Real HTTP via `httpx` to running `cf-gears-server` | Everything: AuthZ, DB, network, auth modes | Nothing — full production-like stack |

#### Level 1: Unit Tests (Domain Layer)

Unit tests verify domain invariants and service logic in isolation from the database. All repository dependencies are mocked via trait implementations.

**Infrastructure**: none (in-process only).

**Test support gear** (`src/domain/test_support.rs`), following the pattern established by `oagw` and `credstore`:

| Mock | Purpose | Pattern |
|------|---------|---------|
| `MockTypesRegistry` | Types Registry SDK double that serves registered type schemas (tenant type plus test types) | returns / rejects / reports unavailability on demand |
| `InMemoryEntityRepository` | HashMap-backed store keyed by `id` | seed via `with_entities(vec![...])` |
| `InMemoryClosureRepository` | Vec-backed closure rows | seed via `with_closure(vec![...])` |
| `InMemoryMembershipRepository` | Vec-backed membership links | seed via `with_memberships(vec![...])` |

Test builder: `RgTestHarness::unit()` with fluent API — `.with_types(...)` (registers types in the mock Types Registry), `.with_entities(...)`, `.with_closure(...)`, `.with_query_profile(max_depth, max_width)` → `.build()` returns `(EntityService, HierarchyService, MembershipService)`. See `src/domain/test_support.rs`.

| What to test | What is mocked | Verification target |
|---|---|---|
| Unknown group type on create | `MockTypesRegistry` without the type | `TypeNotFound` (404) |
| Unknown membership resource type | `MockTypesRegistry` without the type | `Validation` (400) |
| Types Registry unavailable | `MockTypesRegistry` failing | `ServiceUnavailable` (503), no transaction opened |
| Stored type reference not resolvable | `InMemoryEntityRepository` with a group whose type UUID is unknown to `MockTypesRegistry` | `Internal` (500) |
| Metadata validation against type schema | `MockTypesRegistry` with a type schema | invalid `metadata` rejected with `Validation` |
| Second tenant root | `InMemoryEntityRepository` with an existing tenant root | `TenantRootAlreadyExists` |
| Cycle detection in entity create/move | `InMemoryClosureRepository` with ancestor chain | `CycleDetected` when target is descendant |
| Self-parent rejection | `InMemoryEntityRepository` | `CycleDetected` for `parent_id == id` |
| Parent type compatibility | `MockTypesRegistry` + `InMemoryEntityRepository` | `InvalidParentType` when parent type not in `allowed_parent_types` |
| Depth limit enforcement | `InMemoryClosureRepository` + profile `max_depth=3` | `LimitViolation` at depth 4 |
| Width limit enforcement | `InMemoryClosureRepository` + profile `max_width=N` | `LimitViolation` when exceeding |
| Membership add — group does not exist | `InMemoryEntityRepository` empty | `NotFound` |
| Membership add — duplicate composite key | `InMemoryMembershipRepository` pre-seeded | `Conflict` |
| Delete entity — active children | `InMemoryClosureRepository` with descendants | `ConflictActiveReferences` |
| Delete entity — active memberships | `InMemoryMembershipRepository` with links | `ConflictActiveReferences` |
| Error mapper — all domain→SDK error variants | No mocks | Direct `From` impl test per variant, 100% coverage |
| Reduced constraints — tightened profile, stored data exceeds | `InMemoryClosureRepository` with deep tree | Reads return full data; violating writes rejected |

#### Level 2: Integration Tests (Persistence Layer)

Integration tests verify SQL correctness, closure table integrity, transactional behavior, and SecureORM tenant isolation against a real database.

**Infrastructure**: SQLite in-memory (`:memory:`). Each test creates a fresh in-memory DB and applies migrations via `run_migrations_for_testing(db, migrations)` from `toolkit-db`. This gives sub-millisecond setup, no Docker dependency, and deterministic isolation without transaction rollback overhead.

**Rationale for SQLite**: functional domain logic (closure table operations, seeding idempotency, tenant scoping, OData filtering) is identical across SQL dialects. PostgreSQL-specific behaviors — FK enforcement order, SERIALIZABLE isolation semantics, `gts_type_path` DOMAIN validation, `gen_random_uuid()` — are covered by Level 4 E2E tests that run against real PostgreSQL.

**Schema setup**: gear migrations are applied against the in-memory SQLite DB. Migrations are defined in `src/infra/storage/migrations/mod.rs` using the `SQLITE_UP` constants; the `POSTGRES_UP` variants are used at Level 4.

**Isolation strategy**: each test function creates its own `:memory:` DB instance. No shared state across tests; no transaction rollback needed.

| What to test | Setup | Verification target |
|---|---|---|
| Tenant root uniqueness — second tenant root | Seed a tenant root group | Unique violation on `uq_rg_tenant_root` mapped to `TenantRootAlreadyExists`; non-tenant roots may coexist |
| Type reference storage | Empty DB | `gts_type_id` stored as the Registry Reference UUID with no foreign key; no type tables exist |
| Entity CRUD — insert with parent, read, update mutable fields | Types in mock Types Registry | Entity rows persisted with correct FK relations |
| Closure table correctness — create root | Empty DB + types in mock Types Registry | Self-row `(id, id, 0)` created |
| Closure table correctness — create child | Seed parent entity | Self-row + parent row `(parent, child, 1)` + transitive ancestor rows |
| Closure table correctness — create 5-level deep tree | Types in mock Types Registry | Verify `(N*(N+1))/2` closure rows with correct depths |
| Subtree move — closure rebuild | Seed tree `A→B→C`, move `B` under `D` | Old closure paths removed, new paths via `D` created, `C` ancestors updated |
| Subtree delete — cascade closure removal | Seed tree with subtree | All closure rows for removed nodes deleted |
| Cycle detection at DB level — concurrent moves | Seed `A→B→C`, concurrent move `A` under `C` + move `C` under `A` | At least one fails with serialization error or `CycleDetected` _(SERIALIZABLE semantics verified at E2E only — SQLite does not support SERIALIZABLE isolation)_ |
| SERIALIZABLE retry — concurrent entity create under same parent | Two parallel create operations | Both succeed (via retry) or one gets deterministic error _(SERIALIZABLE semantics verified at E2E only — see Level 4)_ |
| Membership CRUD — add, remove, query by group, query by resource | Seed entities | Composite key `(group_id, resource_type, resource_id)` enforced |
| Membership — duplicate rejection at DB level | Pre-seed membership | `UNIQUE` constraint violation mapped to `Conflict` |
| Tenant isolation — SecureORM | Seed data for tenant A, query with `SecurityContext` of tenant B | Empty result set |
| Tenant isolation — cross-tenant parent rejected | Seed entity in tenant A, create child with `tenant_id = B` | Tenant incompatibility error |
| OData `$filter` — groups, memberships | Seed diverse dataset | Filtered results match expected subset |
| Cursor pagination — traverse all pages | Seed 75 entities, `limit=25` | Three pages, no duplicates, no gaps, stable order |
| Cursor pagination — empty result set | Empty DB | Single empty page with no cursors |
| Query profile — depth limit on write, no truncation on read | Seed tree exceeding new limit | Reads return full data; new writes at violating depth rejected |
| Seeding idempotency — groups, memberships | Run seed twice | Second run produces no changes; DB state identical |
| Force delete — cascade subtree + memberships | Seed tree with memberships, delete root with `force=true` | All descendants and their memberships removed |

**Closure integrity checker** (test utility):

A helper function `verify_closure_integrity(tx)` that validates:
- every entity has a self-row `(id, id, 0)`
- for every `parent_id` edge, a closure row `(parent, child, 1)` exists
- closure is transitively complete (if `(A, B, d1)` and `(B, C, d2)` exist, then `(A, C, d1+d2)` exists)
- no orphan closure rows (both `ancestor_id` and `descendant_id` reference existing entities)

This function is called at the end of every integration test that mutates hierarchy data.

#### Level 3: API Tests (REST Layer)

API tests verify HTTP-level behavior: request/response shapes, status codes, OData query parsing, authentication mode routing, and RFC 9457 error format.

**Infrastructure**: `Router::oneshot()` (axum test pattern per `10_checklists_and_templates.md`) with SQLite in-memory database + real domain services. `PolicyEnforcer` is mocked to isolate RG REST layer from AuthZ.

**Mock boundaries**:

| Dependency | Mock | Why |
|---|---|---|
| `PolicyEnforcer` / `AuthZResolverApi` | `MockAuthZResolverClient` (always Allow) or `DenyingAuthZResolverClient` | Isolate from AuthZ; test RG's own auth mode logic |
| Database | SQLite in-memory | REST tests need real query execution for OData/pagination; PostgreSQL-dialect behavior covered by E2E |
| Domain services | Real (not mocked) | REST layer delegates to real services |

**Schema setup**: same pattern as Integration — SQLite `:memory:` + `run_migrations_for_testing()`. Test builder (`RgTestHarness::api()`) encapsulates all initialization: DB, migrations, domain services, routes, AuthZ mocks. Builder provides `.with_types(...)` (mock Types Registry), `.with_authz_client(...)`, `.build().await` → returns harness with `.router()` accessor.

| What to test | Method | Verification target |
|---|---|---|
| Create group — unknown type | `POST /groups` | 404, Problem JSON with type not found |
| Create group — Types Registry unavailable | `POST /groups` | 503 with retry hint |
| Create group — with parent | `POST /groups` | 201 Created, closure rows created |
| Create group — invalid parent type | `POST /groups` | 400/409, Problem JSON with `InvalidParentType` |
| Move group — cycle | `PUT /groups/{id}` (parent = descendant) | 409, `CycleDetected` |
| Delete group — has children, no force | `DELETE /groups/{id}` | 409, `ConflictActiveReferences` |
| Delete group — force cascade | `DELETE /groups/{id}?force=true` | 204 No Content, subtree + memberships removed |
| List group hierarchy — depth filter | `GET /groups/{id}/hierarchy?$filter=hierarchy/depth ge 0` | 200 OK, descendants with `depth` field |
| List group hierarchy — ancestors | `GET /groups/{id}/hierarchy?$filter=hierarchy/depth le 0` | 200 OK, ancestors with negative `depth` |
| Add membership | `POST /memberships/{gid}/{rtype}/{rid}` | 201 Created |
| Add membership — duplicate | `POST /memberships/{gid}/{rtype}/{rid}` again | 409 Conflict |
| Remove membership | `DELETE /memberships/{gid}/{rtype}/{rid}` | 204 No Content |
| List memberships — by group | `GET /memberships?$filter=group_id eq '...'` | 200 OK, filtered result |
| Cursor pagination — all list endpoints | `GET /groups?limit=2`, follow `next_cursor` | All items eventually returned |
| Invalid OData filter | `GET /groups?$filter=invalid` | 400 Bad Request |
| JWT auth — standard request | Request with bearer token | `PolicyEnforcer` called, response scoped by tenant |
| JWT auth — AuthZ denies | Request + `DenyingAuthZResolverClient` | 403 Forbidden |
| MTLS auth — hierarchy endpoint allowed | Simulated MTLS context, `GET /groups/{id}/hierarchy` | 200 OK, no PolicyEnforcer call |
| MTLS auth — non-hierarchy endpoint rejected | Simulated MTLS context, `POST /groups` | 403 Forbidden |
| All error categories — RFC 9457 format | Trigger each error category | Response has `type`, `title`, `status`, `detail` fields |

#### Level 4: E2E Tests (Python / pytest)

E2E tests verify the full stack running as `cf-gears-server` with real AuthZ, real DB, and real network requests.

**Infrastructure**: running cf-gears-server (Docker or local), `pytest` + `httpx`.

**Location**: `testing/e2e/suites/resource_group/`

```
testing/e2e/suites/resource_group/
├── conftest.py       # fixtures: base_url, tenant_id, seed data
├── helpers.py        # register_type() (via Types Registry), create_group(), add_membership(), etc.
├── test_groups.py    # group CRUD, hierarchy, move, delete
├── test_memberships.py  # membership add/remove/query
└── test_hierarchy.py    # depth traversal, ancestor/descendant queries
```

Fixtures (following `oagw` e2e pattern): session-scoped `rg_base_url` (from env `E2E_BASE_URL`), `tenant_id`, seed data fixtures. See `testing/e2e/suites/resource_group/conftest.py`.

| What to test | Marker | Verification target |
|---|---|---|
| Group of a type registered in Types Registry | `@pytest.mark.smoke` | Type registered through Types Registry, group create succeeds; unknown type returns 404 |
| Group hierarchy — create tree, query descendants | `@pytest.mark.smoke` | Descendants returned with correct `depth` values |
| Membership — add, query by group, query by resource, remove | `@pytest.mark.smoke` | Membership links appear/disappear as expected |
| Tenant isolation — two tenants, no cross-visibility | — | Tenant A data invisible to tenant B |
| Error scenarios — cycle, invalid parent, second tenant root | — | Correct HTTP status codes (400, 409) |
| OData filter — eq, ne, in across all list endpoints | — | Filtered results match expected |
| Pagination — large dataset traversal | — | All items returned across pages |

#### What Must NOT Be Mocked

| Component | Level | Why |
|---|---|---|
| Closure table logic (SQL correctness) | Integration (Level 2) | Correctness depends on real SQL execution — SQLite verifies functional behavior |
| Forest invariants | Integration (Level 2) | DB-level constraint enforcement tested on SQLite; PG-dialect FK order tested at E2E |
| SecureORM tenant scoping | Integration (Level 2) | Must verify real `WHERE tenant_id IN (...)` generation |
| PostgreSQL FK enforcement order | E2E (Level 4) | `ON DELETE RESTRICT` enforcement order differs from SQLite — only testable against real PG |
| SERIALIZABLE transaction semantics | E2E (Level 4) | Retry-under-concurrent-writes behavior is PostgreSQL-specific |

#### Concurrency Testing

Hierarchy mutations run with bounded retry, at an isolation level chosen per operation rather than fixed:

- `SERIALIZABLE` where a write depends on a predicate over rows it does not itself lock:
  - `create` — the depth/width limit check reads the parent's depth and its sibling count, and the insert that follows locks neither, so two concurrent creates under the same parent can each read a count that passes the check and both commit past the limit.
  - `move`, a parent-changing `update`, and a **force** delete — these rewrite closure rows across a subtree, exposed to the same phantom-read window between the cycle check and the closure/entity mutation.
- The backend default where there is no such predicate: a name/metadata-only `update`, which changes one row by primary key, and a **non-force** delete, which takes a row lock (`SELECT ... FOR UPDATE`) on its target so the children and membership checks it decides from stay true until it commits.

The second group does not *require* `SERIALIZABLE` — it does not stop being correct under it. What changes is where contention shows up: on PostgreSQL's default (`READ COMMITTED`) those operations wait on a row lock rather than aborting with `40001`, so retries there are for deadlocks, not for serialization failures. A deployment that raises the default to `SERIALIZABLE`, and SQLite, which is serializable regardless, keep the abort-and-retry behaviour; retry is in place either way. Concurrency tests verify correctness under parallel access.

**Serialization retry policy**:

- max retries: 3 (`toolkit_db::secure::DEFAULT_TX_RETRY_ATTEMPTS`, overridable per call)
- backoff: jittered exponential between attempts — base 2 ms, factor 5, capped at
  100 ms (`retry_backoff_delay`). The success path never waits. An earlier
  revision of this section said "none (immediate retry)"; that was true until
  the retry loop gained backoff, and an immediate retry is what made two
  colliding transactions recompute the same zero delay and collide again.
- on exhaustion: the **last** error is returned unchanged. There is no
  `ServiceUnavailable` variant to return and no retry-after hint; the caller
  surfaces whatever the database said, which today reaches HTTP as a 500.
  Whether an exhausted retry deserves its own status is an open contract
  question, tracked in `db-behavior-audit.md` under Deferred.
- transaction timeout: none is set. `TxConfig` carries `isolation` and
  `access_mode` and nothing else, so there is no per-call knob. On the server
  side `statement_timeout` bounds a single statement, not a transaction;
  `transaction_timeout` (PostgreSQL 17+) bounds the transaction, and on
  earlier versions there is no equivalent. The benchmark environment above is
  PostgreSQL 17, so the 5s bound this section used to claim is reachable — it
  is simply not configured. Nothing in this repository sets any of the three;
  see the query-cost note above for how a deployment can.

**Concurrency test pattern** (E2E test level — requires real PostgreSQL for SERIALIZABLE isolation):

Test pattern: seed hierarchy (e.g. `A → B → C → D`, `E → F`), spawn N concurrent tasks performing conflicting moves (move B under E, move F under A, etc.), synchronize start via barrier, then verify: `verify_closure_integrity(tx)`, no cycles in canonical `parent_id` chain, all moves either succeeded or returned deterministic error.

#### NFR Verification Mapping

| NFR | Test level | How verified |
|---|---|---|
| `cpt-cf-resource-group-nfr-hierarchy-query-latency` (p95 < 250ms) | Integration | Timed queries on seeded dataset (100K+ groups); assert < 250ms |
| `cpt-cf-resource-group-nfr-membership-query-latency` (p95 < 30ms) | Integration | Timed queries on seeded dataset (200K+ memberships); assert < 30ms |
| `cpt-cf-resource-group-nfr-transactional-consistency` | E2E | Concurrent writes on real PostgreSQL + `verify_closure_integrity()` after each |
| `cpt-cf-resource-group-nfr-deterministic-errors` | Unit | Test every `From<DomainError>` mapping; verify 100% variant coverage |
| `cpt-cf-resource-group-nfr-production-scale` | Deferred | Capacity planning validated via database size analysis (section 4.1) |

## 5. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **Auth Architecture Context**: [docs/arch/authorization/DESIGN.md](../../../../docs/arch/authorization/DESIGN.md)
- **RG Model Context**: [docs/arch/authorization/RESOURCE_GROUP_MODEL.md](../../../../docs/arch/authorization/RESOURCE_GROUP_MODEL.md)
