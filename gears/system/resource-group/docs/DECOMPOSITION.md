<!-- Created: 2026-04-07 by Constructor Tech -->
<!-- Updated: 2026-04-20 by Constructor Tech -->

# Decomposition: Resource Group (RG)

**Overall implementation status:**
- [x] `p1` - **ID**: `cpt-cf-resource-group-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 SDK Contracts, Error Types & Gear Foundation &mdash; HIGH](#21-sdk-contracts-error-types--gear-foundation-mdash-high)
  - [2.2 Type Resolution from Types Registry &mdash; HIGH](#22-type-resolution-from-types-registry-mdash-high)
  - [2.3 Group Entity & Hierarchy Engine &mdash; HIGH](#23-group-entity--hierarchy-engine-mdash-high)
  - [2.4 Membership Management &mdash; MEDIUM](#24-membership-management-mdash-medium)
  - [2.5 Integration Read Port & Dual Authentication Modes &mdash; MEDIUM](#25-integration-read-port--dual-authentication-modes-mdash-medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

The Resource Group DESIGN is decomposed into seven features organized around the gear's core domain boundaries: type resolution from Types Registry, group entity lifecycle with hierarchy, membership management, external integration with authentication modes, and test plans.

**Decomposition Strategy**:
- Features grouped by functional domain cohesion (one domain service per feature, foundation first)
- Dependencies follow a strict layering: SDK/gear foundation → type resolution → entity/hierarchy → membership → integration
- Each feature covers specific components, sequences, and requirement subsets from DESIGN
- 100% coverage of all DESIGN elements verified (7 components, 8 sequences, 6 principles, 5 constraints)
- 100% coverage of all PRD requirements verified (29 FR, 7 NFR, 2 interfaces)
- Cross-cutting concerns (REST infrastructure, OData, error mapping, persistence) consolidated in the foundation feature

## 2. Entries

### 2.1 SDK Contracts, Error Types & Gear Foundation &mdash; HIGH

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-sdk-gear-foundation`

- **Purpose**: Establish the SDK crate with trait contracts, domain models, and error taxonomy; scaffold the gear with ClientHub registration, persistence adapter, DB migrations, REST/OData infrastructure, and the cross-cutting error mapping layer that all subsequent features depend on.

- **Depends On**: None

- **Scope**:
  - SDK crate (`resource-group-sdk`): `models.rs` (ResourceGroup, ResourceGroupWithDepth, ResourceGroupMembership, Page, PageInfo, GtsTypePath), `api.rs` (ResourceGroupClient, ResourceGroupReadHierarchy, ResourceGroupReadPluginClient traits), `error.rs` (ResourceGroupError taxonomy)
  - Gear scaffold: `#[toolkit::gear]` annotated gear, ClientHub registration for `dyn ResourceGroupClient` and `dyn ResourceGroupReadHierarchy`
  - Persistence adapter: SeaORM entity definitions for the 3 tables (resource_group, resource_group_membership, resource_group_closure; types are not stored, rows hold Types Registry Registry References), DB migration scripts
  - Error mapping: DomainError to Problem (RFC-9457) mapping for all deterministic error categories (Validation, NotFound, InvalidParentType, CycleDetected, ConflictActiveReferences, LimitViolation, TenantIncompatibility, ServiceUnavailable, Internal)
  - REST infrastructure: OperationBuilder wiring, OData `$filter` parsing, cursor-based pagination helpers
  - GtsTypePath value object with format validation (`^gts\.[a-z_]...~$`)
  - gear initialization phasing (SystemCapability → ready) for circular dependency resolution with AuthZ

- **Out of scope**:
  - Domain service logic (features 2-5)
  - Specific REST endpoint handlers (features 2-5)
  - Integration read service logic (feature 5)
  - AuthZ policy evaluation (external gear)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-resource-group-fr-rest-api`
  - [x] `p1` - `cpt-cf-resource-group-fr-odata-query`
  - [x] `p1` - `cpt-cf-resource-group-fr-deterministic-errors`
  - [x] `p1` - `cpt-cf-resource-group-fr-no-authz-and-sql-logic`
  - [x] `p1` - `cpt-cf-resource-group-nfr-deterministic-errors`
  - [x] `p1` - `cpt-cf-resource-group-nfr-compatibility`
  - [x] `p1` - `cpt-cf-resource-group-nfr-production-scale`
  - [x] `p1` - `cpt-cf-resource-group-nfr-transactional-consistency`
  - [x] `p1` - `cpt-cf-resource-group-interface-resource-group-client`
  - [x] `p1` - `cpt-cf-resource-group-interface-integration-read-hierarchy`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-resource-group-principle-policy-agnostic`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-resource-group-constraint-no-authz-decision`
  - [x] `p1` - `cpt-cf-resource-group-constraint-no-sql-filter-generation`
  - [x] `p1` - `cpt-cf-resource-group-constraint-db-agnostic`
  - [x] `p1` - `cpt-cf-resource-group-constraint-surrogate-ids-internal`

- **Domain Model Entities**:
  - ResourceGroup (ResourceGroupEntity)
  - ResourceGroupMembership
  - ResourceGroupClosure
  - ResourceGroupError
  - GtsTypePath (value object)
  - Page, PageInfo (pagination)

- **Design Components**:

  - [x] `p1` - `cpt-cf-resource-group-component-gear`
  - [x] `p1` - `cpt-cf-resource-group-component-persistence-adapter`

- **API**:
  - gear initialization and ClientHub registration (no domain REST endpoints in this feature)

- **Sequences**:

  - `cpt-cf-resource-group-seq-init-order`

### 2.2 Type Resolution from Types Registry &mdash; HIGH

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-type-management`

- **Purpose**: Resolve group and membership types from Types Registry (GTS Type Schemas derived from `gts.cf.core.rg.type.v1~`) through a type catalog port, so RG keeps no type storage, CRUD, seeding or bootstrap (ADR-002).

- **Depends On**: `cpt-cf-resource-group-feature-sdk-gear-foundation`

- **Scope**:
  - Type catalog port and adapter over the Types Registry SDK (platform contract, in-process local client): GTS id and Registry Reference lookups in one fixed projection (effective traits plus resolved schema), always before the DB transaction, no RG-side cache
  - Type code validation: non-empty, RG type prefix, length limit
  - Trait-driven rules consumed by features 3 and 4: `can_be_root`, `allowed_parent_types`, `allowed_membership_types`, and the group metadata schema
  - Error mapping: unknown group type → 404, unknown membership resource type → 400, Types Registry unavailable → 503, unresolvable stored type reference → 500
  - Static type declarations: tenant type in the RG SDK; user and user-group types in the Account Management SDK

- **Out of scope**:
  - Type CRUD, type seeding, hierarchy-safety checks on type update (removed; owned by Types Registry)
  - Group entity operations (feature 3)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-resource-group-fr-types-in-types-registry`
  - [x] `p1` - `cpt-cf-resource-group-fr-resolve-types`
  - [x] `p1` - `cpt-cf-resource-group-fr-store-type-reference`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-resource-group-principle-dynamic-types`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-resource-group-constraint-surrogate-ids-internal`

- **Domain Model Entities**:
  - ResourceGroupType (read model, not stored)
  - GtsTypePath

- **Design Components**:

  - [x] `p1` - `cpt-cf-resource-group-component-type-catalog`

- **API**:
  None (RG exposes no type endpoints; types are managed through the Types Registry API)

- **Sequences**:

  None

### 2.3 Group Entity & Hierarchy Engine &mdash; HIGH

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-entity-hierarchy`

- **Purpose**: Implement group entity lifecycle (create, get, update, move, delete) with strict forest invariants, closure-table-based hierarchy engine for efficient ancestor/descendant queries, query profile enforcement, subtree operations, and the hierarchy depth endpoint with relative depth computation.

- **Depends On**: `cpt-cf-resource-group-feature-type-management`

- **Scope**:
  - Entity service: create/get/update (PUT full replace)/move/delete group operations with domain validation
  - Forest integrity: cycle detection and single-parent validation inside SERIALIZABLE write transactions
  - Parent type compatibility: validate parent-child type rules on create and move against the type's `allowed_parent_types` / `can_be_root`; a group's type is immutable after creation, so there is no type change to re-validate; `is_tenant` is derived at creation and the `uq_rg_tenant_root` partial unique index backs tenant root uniqueness
  - Entity delete safety: reject when active references (children, memberships) prevent removal per configured deletion policy
  - Closure table engine: self-row creation on entity insert, ancestor-descendant row computation on parent assignment, subtree move with full path rebuild (delete old paths + insert new paths), cascade closure row removal on entity delete
  - Hierarchy queries: ancestors/descendants ordered by depth via indexed closure table lookups
  - Hierarchy depth endpoint: `GET /groups/{group_id}/hierarchy` returning `ResourceGroupWithDepth` with relative depth (positive = descendants, negative = ancestors, 0 = self) and OData filtering on `hierarchy/depth`
  - Query profile enforcement: configurable `max_depth`/`max_width` on writes, no truncation on reads for already-existing data, deterministic `DepthLimitExceeded`/`WidthLimitExceeded` errors
  - Group REST endpoints: CRUD under `/api/resource-group/v1/groups` with OData `$filter` on `type`, `hierarchy/parent_id`, `tenant_id`, `id`, `name`
  - Force delete: optional `?force=true` for cascade deletion of subtree and associated memberships
  - Group data seeding: idempotent seed with parent-child link and type compatibility validation
  - Write concurrency: SERIALIZABLE isolation with bounded retry for serialization conflicts

- **Out of scope**:
  - Membership operations (feature 4)
  - Integration read service and plugin gateway routing (feature 5)
  - MTLS and JWT authentication mode routing (feature 5)
  - Tenant scope ownership-graph enforcement (feature 5)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-resource-group-fr-manage-entities`
  - [x] `p1` - `cpt-cf-resource-group-fr-enforce-forest-hierarchy`
  - [x] `p1` - `cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`
  - [x] `p1` - `cpt-cf-resource-group-fr-validate-parent-type`
  - [x] `p1` - `cpt-cf-resource-group-fr-delete-entity-no-active-references`
  - [x] `p1` - `cpt-cf-resource-group-fr-seed-groups`
  - [x] `p1` - `cpt-cf-resource-group-fr-closure-table`
  - [x] `p1` - `cpt-cf-resource-group-fr-query-group-hierarchy`
  - [x] `p1` - `cpt-cf-resource-group-fr-subtree-operations`
  - [x] `p1` - `cpt-cf-resource-group-fr-query-profile`
  - [x] `p1` - `cpt-cf-resource-group-fr-profile-change-no-rewrite`
  - [x] `p1` - `cpt-cf-resource-group-fr-reduced-constraints-behavior`
  - [x] `p1` - `cpt-cf-resource-group-fr-list-groups-depth`
  - [x] `p2` - `cpt-cf-resource-group-fr-force-delete`
  - [x] `p1` - `cpt-cf-resource-group-nfr-hierarchy-query-latency`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-resource-group-principle-strict-forest`
  - [x] `p1` - `cpt-cf-resource-group-principle-query-profile-guardrail`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-resource-group-constraint-profile-change-safety`

- **Domain Model Entities**:
  - ResourceGroupEntity
  - ResourceGroupClosure
  - ResourceGroupWithDepth

- **Design Components**:

  - [x] `p1` - `cpt-cf-resource-group-component-entity-service`
  - [x] `p1` - `cpt-cf-resource-group-component-hierarchy-service`

- **API**:
  - GET /api/resource-group/v1/groups
  - POST /api/resource-group/v1/groups
  - GET /api/resource-group/v1/groups/{group_id}
  - PUT /api/resource-group/v1/groups/{group_id}
  - DELETE /api/resource-group/v1/groups/{group_id}
  - GET /api/resource-group/v1/groups/{group_id}/hierarchy

- **Sequences**:

  - `cpt-cf-resource-group-seq-create-entity-with-parent`
  - `cpt-cf-resource-group-seq-move-subtree`

### 2.4 Membership Management &mdash; MEDIUM

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-membership`

- **Purpose**: Implement membership lifecycle (add, remove, list) with composite key semantics, deterministic lookups by group and by resource, tenant compatibility validation derived from the referenced group, and idempotent membership seeding.

- **Depends On**: `cpt-cf-resource-group-feature-sdk-gear-foundation`, `cpt-cf-resource-group-feature-entity-hierarchy`

- **Scope**:
  - Membership service: add/remove membership links with composite key `(group_id, resource_type, resource_id)`
  - Membership queries: by group (`group_id`), by resource (`resource_type` + `resource_id`), by group and resource type (`group_id` + `resource_type`)
  - Membership REST endpoints: list/add/remove under `/api/resource-group/v1/memberships` with OData `$filter` on `resource_id`, `resource_type`, `group_id`
  - Tenant compatibility: tenant scope derived from group's `tenant_id` via JOIN, reject tenant-incompatible membership writes when resource already linked in incompatible tenant
  - GTS type path resolution for `resource_type` through the Types Registry SDK (Registry Reference stored in `gts_type_id`)
  - allowed_membership_types validation: reject if `resource_type` is unknown to Types Registry or not in the group type's `allowed_membership_types`
  - Membership data seeding: idempotent seed with group existence and tenant compatibility validation
  - Active reference integration: membership references block entity deletion in feature 3 (unless force delete)
  - Data lifecycle: cascade-delete memberships on tenant deprovisioning

- **Out of scope**:
  - Group hierarchy operations (feature 3)
  - AuthZ integration and plugin routing (feature 5)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-resource-group-fr-manage-membership`
  - [x] `p1` - `cpt-cf-resource-group-fr-query-membership-relations`
  - [x] `p1` - `cpt-cf-resource-group-fr-seed-memberships`
  - [x] `p1` - `cpt-cf-resource-group-nfr-membership-query-latency`
  - [x] `p1` - `cpt-cf-resource-group-nfr-data-lifecycle`

- **Design Principles Covered**:

  None (membership management operates within principles established by features 1 and 3)

- **Design Constraints Covered**:

  None (membership management relies on foundation constraints from feature 1)

- **Domain Model Entities**:
  - ResourceGroupMembership

- **Design Components**:

  - [x] `p1` - `cpt-cf-resource-group-component-membership-service`

- **API**:
  - GET /api/resource-group/v1/memberships
  - POST /api/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id}
  - DELETE /api/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id}

- **Sequences**:

  None (membership operations are direct service calls without complex multi-component interactions)

### 2.5 Integration Read Port & Dual Authentication Modes &mdash; MEDIUM

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-integration-auth`

- **Purpose**: Expose the integration read service for external consumers (AuthZ plugin via `ResourceGroupReadHierarchy`), implement JWT authentication with full AuthZ evaluation (and, deferred to `p2`, the future MTLS hierarchy-only bypass for the microservice split), enforce tenant scope for ownership-graph profile writes, and configure plugin gateway routing for vendor-specific provider support.

- **Depends On**: `cpt-cf-resource-group-feature-entity-hierarchy`, `cpt-cf-resource-group-feature-membership`

- **Scope**:
  - Integration read service: expose `ResourceGroupReadHierarchy` via ClientHub for AuthZ plugin consumption, returning hierarchy data without policy or SQL semantics
  - Plugin gateway routing: built-in provider (local persistence path) vs vendor-specific provider (resolve plugin instance by configured vendor, delegate to `ResourceGroupReadPluginClient`) with SecurityContext passthrough
  - JWT authentication: standard AuthZ evaluation via `PolicyEnforcer.access_scope()` on all REST endpoints, `AccessScope` applied via SecureORM for tenant-scoped queries
  - MTLS authentication _(p2 — deferred, not implemented yet)_: client certificate verification against trusted CA bundle, endpoint allowlist (only `GET /groups/{group_id}/hierarchy`), AuthZ bypass for trusted system principals, system SecurityContext creation
  - MTLS configuration _(p2 — deferred, not implemented yet)_: `ca_cert`, `allowed_clients` (by certificate CN), `allowed_endpoints` (method + path pairs)
  - Tenant scope enforcement for ownership-graph profile: parent-child edges and membership writes validated for tenant-hierarchy compatibility, platform-admin provisioning exception for cross-tenant management, tenant-scoped reads via `SecurityContext.subject_tenant_id`
  - Barrier as data: `metadata.self_managed` stored in group metadata JSONB without enforcement by RG, returned in API responses within `metadata` object for consumption by Tenant Resolver and AuthZ
  - In-process vs out-of-process: ClientHub direct call (monolith, no MTLS needed) — current `p1`; MTLS-authenticated remote call (microservices) — `p2`, deferred / not implemented yet
  - SecurityContext propagation: `ctx` passed through gateway to selected provider without policy interpretation

- **Out of scope**:
  - AuthZ policy evaluation logic (owned by AuthZ gear)
  - SQL filter generation and AccessScope compilation (owned by PEP/compiler)
  - Tenant Resolver barrier enforcement (owned by TR gear)
  - Custom barrier semantics (vendor-replaceable via TR/AuthZ plugins)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-resource-group-fr-integration-read-port`
  - [x] `p1` - `cpt-cf-resource-group-fr-jwt-auth`
  - [ ] `p2` - `cpt-cf-resource-group-fr-dual-auth-modes` _(deferred, not implemented yet)_
  - [x] `p1` - `cpt-cf-resource-group-fr-tenant-scope-ownership-graph`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-resource-group-principle-tenant-scope-ownership-graph`
  - [x] `p1` - `cpt-cf-resource-group-principle-barrier-as-data`

- **Design Constraints Covered**:

  None (integration feature relies on foundation constraints from feature 1)

- **Domain Model Entities**:
  - ResourceGroupWithDepth (integration read response)
  - ResourceGroupMembership (integration read response)

- **Design Components**:

  - [x] `p1` - `cpt-cf-resource-group-component-integration-read-service`

- **API**:
  - GET /api/resource-group/v1/groups/{group_id}/hierarchy (JWT; additionally over MTLS — `p2`, deferred / not implemented yet)
  - All other endpoints (JWT only; MTLS path returns 403 — `p2`, deferred / not implemented yet)

- **Sequences**:

  - `cpt-cf-resource-group-seq-authz-rg-sql-split`
  - `cpt-cf-resource-group-seq-e2e-authz-flow`
  - `cpt-cf-resource-group-seq-auth-modes`
  - `cpt-cf-resource-group-seq-mtls-authz-read` _(p2 — deferred, not implemented yet)_
  - `cpt-cf-resource-group-seq-jwt-rg-request`

---

## 3. Feature Dependencies

```text
cpt-cf-resource-group-feature-sdk-gear-foundation
    |
    +---> cpt-cf-resource-group-feature-type-management
    |         |
    |         +---> cpt-cf-resource-group-feature-entity-hierarchy
    |                   |
    |                   +---> cpt-cf-resource-group-feature-membership
    |                   |         |
    |                   |         v
    |                   +---> cpt-cf-resource-group-feature-integration-auth
    |                                 ^
    +--- (also depends) -------------+
```

**Dependency Rationale**:

- `cpt-cf-resource-group-feature-type-management` requires `cpt-cf-resource-group-feature-sdk-gear-foundation`: the type catalog depends on SDK trait contracts and error mapping infrastructure, and on the Types Registry SDK
- `cpt-cf-resource-group-feature-entity-hierarchy` requires `cpt-cf-resource-group-feature-type-management`: entity create/move validates parent-child type compatibility against types resolved from Types Registry; closure table engine depends on entity persistence
- `cpt-cf-resource-group-feature-membership` requires `cpt-cf-resource-group-feature-sdk-gear-foundation` and `cpt-cf-resource-group-feature-entity-hierarchy`: membership links reference existing group entities and use SDK contracts; membership tenant scope is derived from group data
- `cpt-cf-resource-group-feature-integration-auth` requires `cpt-cf-resource-group-feature-entity-hierarchy` and `cpt-cf-resource-group-feature-membership`: integration read service exposes hierarchy and membership data to external consumers; dual auth modes route requests through existing entity/membership services
- `cpt-cf-resource-group-feature-type-management` and `cpt-cf-resource-group-feature-entity-hierarchy` form a strict sequence (types before entities); `cpt-cf-resource-group-feature-membership` can start in parallel with `cpt-cf-resource-group-feature-integration-auth` once entity-hierarchy is complete, but integration-auth depends on both
