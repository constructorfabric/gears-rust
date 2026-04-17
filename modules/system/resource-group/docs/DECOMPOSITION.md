<!-- Created: 2026-04-07 by Constructor Tech -->

# Decomposition: Resource Group (RG)

**Overall implementation status:**
- [x] `p1` - **ID**: `cpt-cf-resource-group-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 SDK Contracts, Error Types & Module Foundation &mdash; HIGH](#21-sdk-contracts-error-types--module-foundation-mdash-high)
  - [2.2 GTS Type Management &mdash; HIGH](#22-gts-type-management-mdash-high)
  - [2.3 Group Entity & Hierarchy Engine &mdash; HIGH](#23-group-entity--hierarchy-engine-mdash-high)
  - [2.4 Membership Management &mdash; MEDIUM](#24-membership-management-mdash-medium)
  - [2.5 Integration Read Port & Dual Authentication Modes &mdash; MEDIUM](#25-integration-read-port--dual-authentication-modes-mdash-medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

The Resource Group DESIGN is decomposed into seven features organized around the module's core domain boundaries: type management, group entity lifecycle with hierarchy, membership management, external integration with authentication modes, and test plans.

**Decomposition Strategy**:
- Features grouped by functional domain cohesion (one domain service per feature, foundation first)
- Dependencies follow a strict layering: SDK/module foundation → type management → entity/hierarchy → membership → integration
- Each feature covers specific components, sequences, and requirement subsets from DESIGN
- 100% coverage of all DESIGN elements verified (7 components, 8 sequences, 6 principles, 5 constraints)
- 100% coverage of all PRD requirements verified (29 FR, 7 NFR, 2 interfaces)
- Cross-cutting concerns (REST infrastructure, OData, error mapping, persistence) consolidated in the foundation feature

## 2. Entries

### 2.1 SDK Contracts, Error Types & Module Foundation &mdash; HIGH

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-sdk-module-foundation`

- **Purpose**: Establish the SDK crate with trait contracts, domain models, and error taxonomy; scaffold the module with ClientHub registration, persistence adapter, DB migrations, REST/OData infrastructure, and the cross-cutting error mapping layer that all subsequent features depend on.

- **Depends On**: None

- **Scope**:
  - SDK crate (`resource-group-sdk`): `models.rs` (ResourceGroupType, ResourceGroup, ResourceGroupWithDepth, ResourceGroupMembership, Page, PageInfo, GtsTypePath), `api.rs` (ResourceGroupClient, ResourceGroupReadHierarchy, ResourceGroupReadPluginClient traits), `error.rs` (ResourceGroupError taxonomy)
  - Module scaffold: `#[modkit::module]` annotated module, ClientHub registration for `dyn ResourceGroupClient` and `dyn ResourceGroupReadHierarchy`
  - Persistence adapter: SeaORM entity definitions for all 6 tables (gts_type, gts_type_allowed_parent, gts_type_allowed_membership, resource_group, resource_group_membership, resource_group_closure), DB migration scripts
  - Error mapping: DomainError to Problem (RFC-9457) mapping for all deterministic error categories (Validation, NotFound, TypeAlreadyExists, InvalidParentType, CycleDetected, ConflictActiveReferences, LimitViolation, TenantIncompatibility, ServiceUnavailable, Internal)
  - REST infrastructure: OperationBuilder wiring, OData `$filter` parsing, cursor-based pagination helpers
  - GtsTypePath value object with format validation (`^gts\.[a-z_]...~$`)
  - Module initialization phasing (SystemCapability → ready) for circular dependency resolution with AuthZ

- **Out of scope**:
  - Domain service logic (features 2-5)
  - Specific REST endpoint handlers (features 2-5)
  - Integration read service logic (feature 5)
  - AuthZ policy evaluation (external module)

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
  - ResourceGroupType
  - ResourceGroup (ResourceGroupEntity)
  - ResourceGroupMembership
  - ResourceGroupClosure
  - ResourceGroupError
  - GtsTypePath (value object)
  - Page, PageInfo (pagination)

- **Design Components**:

  - [x] `p1` - `cpt-cf-resource-group-component-module`
  - [x] `p1` - `cpt-cf-resource-group-component-persistence-adapter`

- **API**:
  - Module initialization and ClientHub registration (no domain REST endpoints in this feature)

- **Sequences**:

  - `cpt-cf-resource-group-seq-init-order`

### 2.2 GTS Type Management &mdash; HIGH

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-type-management`

- **Purpose**: Implement the full type lifecycle (create, list, get, update, delete) with code format validation, uniqueness enforcement, hierarchy-safe updates, delete-if-unused policy, and idempotent type seeding for deployment bootstrapping.

- **Depends On**: `cpt-cf-resource-group-feature-sdk-module-foundation`

- **Scope**:
  - Type service: create/list/get/update/delete type operations with domain validation
  - Type code validation: length 1..63, no whitespace, case-insensitive uniqueness via `schema_id` unique constraint
  - Duplicate rejection: deterministic `TypeAlreadyExists` on conflict
  - Hierarchy-safe updates: reject removal of `allowed_parents` or `can_be_root` changes when existing groups would violate new rules (`AllowedParentsViolation`)
  - `is_tenant` trait (from `x-gts-traits`, default `false`): tenant types create self-referencing `tenant_id = group.id` scope; non-tenant types inherit tenant scope
  - Delete safety: reject type deletion when entities of that type exist
  - Type REST endpoints: CRUD under `/api/types-registry/v1/types` with OData `$filter` on `code` field
  - allowed_parents and allowed_memberships junction table management (gts_type_allowed_parent, gts_type_allowed_membership)
  - Type data seeding: idempotent seed operation for bootstrapping (missing types created, existing types updated)
  - GTS type path ↔ SMALLINT surrogate ID resolution at persistence boundary
  - Placement invariant enforcement: `can_be_root OR len(allowed_parents) >= 1`

- **Out of scope**:
  - Group entity operations (feature 3)
  - GTS schema catalog integration with Types Registry module (Phase 3 architecture evolution)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-resource-group-fr-manage-types`
  - [x] `p1` - `cpt-cf-resource-group-fr-validate-type-code`
  - [x] `p1` - `cpt-cf-resource-group-fr-reject-duplicate-type`
  - [x] `p1` - `cpt-cf-resource-group-fr-seed-types`
  - [x] `p1` - `cpt-cf-resource-group-fr-validate-type-update-hierarchy`
  - [x] `p1` - `cpt-cf-resource-group-fr-delete-type-only-if-empty`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-resource-group-principle-dynamic-types`

- **Design Constraints Covered**:

  None (type management relies on foundation constraints from feature 1)

- **Domain Model Entities**:
  - ResourceGroupType
  - GtsTypePath

- **Design Components**:

  - [x] `p1` - `cpt-cf-resource-group-component-type-service`

- **API**:
  - GET /api/types-registry/v1/types
  - POST /api/types-registry/v1/types
  - GET /api/types-registry/v1/types/{code}
  - PUT /api/types-registry/v1/types/{code}
  - DELETE /api/types-registry/v1/types/{code}

- **Sequences**:

  None (type operations are straightforward CRUD without complex multi-component interactions)

### 2.3 Group Entity & Hierarchy Engine &mdash; HIGH

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-entity-hierarchy`

- **Purpose**: Implement group entity lifecycle (create, get, update, move, delete) with strict forest invariants, closure-table-based hierarchy engine for efficient ancestor/descendant queries, query profile enforcement, subtree operations, and the hierarchy depth endpoint with relative depth computation.

- **Depends On**: `cpt-cf-resource-group-feature-type-management`

- **Scope**:
  - Entity service: create/get/update (PUT full replace)/move/delete group operations with domain validation
  - Forest integrity: cycle detection and single-parent validation inside SERIALIZABLE write transactions
  - Parent type compatibility: validate parent-child type rules on create, move, and type change (including validation that children's types still permit the new type in their `allowed_parents`)
  - Entity delete safety: reject when active references (children, memberships) prevent removal per configured deletion policy
  - Closure table engine: self-row creation on entity insert, ancestor-descendant row computation on parent assignment, subtree move with full path rebuild (delete old paths + insert new paths), cascade closure row removal on entity delete
  - Hierarchy queries: ancestors/descendants ordered by depth via indexed closure table lookups
  - Descendants endpoint: `GET /groups/{group_id}/descendants` and ancestors endpoint: `GET /groups/{group_id}/ancestors` returning `ResourceGroupWithDepth` with relative depth (positive = descendants, negative = ancestors, 0 = self) and OData filtering on `hierarchy/depth`
  - Query profile enforcement: configurable `max_depth`/`max_width` on writes, no truncation on reads for already-existing data, deterministic `DepthLimitExceeded`/`WidthLimitExceeded` errors
  - Group REST endpoints: CRUD under `/api/resource-group/v1/groups` with OData `$filter` on `type`, `hierarchy/parent_id`, `id`, `name`
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
  - GET /api/resource-group/v1/groups/{group_id}/descendants
  - GET /api/resource-group/v1/groups/{group_id}/ancestors

- **Sequences**:

  - `cpt-cf-resource-group-seq-create-entity-with-parent`
  - `cpt-cf-resource-group-seq-move-subtree`

### 2.4 Membership Management &mdash; MEDIUM

- [x] `p1` - **ID**: `cpt-cf-resource-group-feature-membership`

- **Purpose**: Implement membership lifecycle (add, remove, list) with composite key semantics, deterministic lookups by group and by resource, tenant compatibility validation derived from the referenced group, and idempotent membership seeding.

- **Depends On**: `cpt-cf-resource-group-feature-sdk-module-foundation`, `cpt-cf-resource-group-feature-entity-hierarchy`

- **Scope**:
  - Membership service: add/remove membership links with composite key `(group_id, resource_type, resource_id)`
  - Membership queries: by group (`group_id`), by resource (`resource_type` + `resource_id`), by group and resource type (`group_id` + `resource_type`)
  - Membership REST endpoints: list/add/remove under `/api/resource-group/v1/memberships` with OData `$filter` on `resource_id`, `resource_type`, `group_id`
  - Tenant compatibility: tenant scope derived from group's `tenant_id` via JOIN, reject tenant-incompatible membership writes when resource already linked in incompatible tenant
  - GTS type path resolution for `resource_type` via surrogate ID at persistence boundary
  - allowed_memberships validation: reject if `resource_type` is not in the group type's allowed_memberships
  - Membership data seeding: idempotent seed with group existence and tenant compatibility validation
  - Active reference integration: membership references block entity deletion in feature 3 (unless force delete)
  - Data lifecycle: cascade-delete memberships on tenant deprovisioning

- **Out of scope**:
  - Group hierarchy operations (feature 3)
  - AuthZ integration and plugin routing (feature 5)
  - GTS validation for `resource_type` against external type registries (known technical debt)

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

- **Purpose**: Expose the integration read service for external consumers (AuthZ plugin via `ResourceGroupReadHierarchy`), implement dual authentication modes (JWT with full AuthZ evaluation, MTLS with hierarchy-only bypass), enforce tenant scope for ownership-graph profile writes, and configure plugin gateway routing for vendor-specific provider support.

- **Depends On**: `cpt-cf-resource-group-feature-entity-hierarchy`, `cpt-cf-resource-group-feature-membership`

- **Scope**:
  - Integration read service: expose `ResourceGroupReadHierarchy` via ClientHub for AuthZ plugin consumption, returning hierarchy data without policy or SQL semantics
  - Plugin gateway routing: built-in provider (local persistence path) vs vendor-specific provider (resolve plugin instance by configured vendor, delegate to `ResourceGroupReadPluginClient`) with SecurityContext passthrough
  - JWT authentication: standard AuthZ evaluation via `PolicyEnforcer.access_scope()` on all REST endpoints, `AccessScope` applied via SecureORM for tenant-scoped queries
  - MTLS authentication: client certificate verification against trusted CA bundle, endpoint allowlist (`GET /groups/{group_id}/descendants` and `GET /groups/{group_id}/ancestors`), AuthZ bypass for trusted system principals, system SecurityContext creation
  - MTLS configuration: `ca_cert`, `allowed_clients` (by certificate CN), `allowed_endpoints` (method + path pairs)
  - Tenant scope enforcement for ownership-graph profile: parent-child edges and membership writes validated for tenant-hierarchy compatibility, platform-admin provisioning exception for cross-tenant management, tenant-scoped reads via `SecurityContext.subject_tenant_id`
  - Barrier as data: `metadata.barrier` stored in group metadata JSONB without enforcement by RG, returned in API responses within `metadata` object for consumption by Tenant Resolver and AuthZ
  - In-process vs out-of-process: ClientHub direct call (monolith, no MTLS needed) vs MTLS-authenticated remote call (microservices)
  - SecurityContext propagation: `ctx` passed through gateway to selected provider without policy interpretation

- **Out of scope**:
  - AuthZ policy evaluation logic (owned by AuthZ module)
  - SQL filter generation and AccessScope compilation (owned by PEP/compiler)
  - Tenant Resolver barrier enforcement (owned by TR module)
  - Custom barrier semantics (vendor-replaceable via TR/AuthZ plugins)

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-resource-group-fr-integration-read-port`
  - [x] `p1` - `cpt-cf-resource-group-fr-dual-auth-modes`
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
  - GET /api/resource-group/v1/groups/{group_id}/descendants (JWT + MTLS)
  - GET /api/resource-group/v1/groups/{group_id}/ancestors (JWT + MTLS)
  - All other endpoints (JWT only, MTLS returns 403)

- **Sequences**:

  - `cpt-cf-resource-group-seq-authz-rg-sql-split`
  - `cpt-cf-resource-group-seq-e2e-authz-flow`
  - `cpt-cf-resource-group-seq-auth-modes`
  - `cpt-cf-resource-group-seq-mtls-authz-read`
  - `cpt-cf-resource-group-seq-jwt-rg-request`

---

## 3. Feature Dependencies

```text
cpt-cf-resource-group-feature-sdk-module-foundation
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

- `cpt-cf-resource-group-feature-type-management` requires `cpt-cf-resource-group-feature-sdk-module-foundation`: type service depends on SDK trait contracts, persistence adapter, and error mapping infrastructure
- `cpt-cf-resource-group-feature-entity-hierarchy` requires `cpt-cf-resource-group-feature-type-management`: entity create/move validates parent-child type compatibility against registered types; closure table engine depends on entity persistence
- `cpt-cf-resource-group-feature-membership` requires `cpt-cf-resource-group-feature-sdk-module-foundation` and `cpt-cf-resource-group-feature-entity-hierarchy`: membership links reference existing group entities and use SDK contracts; membership tenant scope is derived from group data
- `cpt-cf-resource-group-feature-integration-auth` requires `cpt-cf-resource-group-feature-entity-hierarchy` and `cpt-cf-resource-group-feature-membership`: integration read service exposes hierarchy and membership data to external consumers; dual auth modes route requests through existing entity/membership services
- `cpt-cf-resource-group-feature-type-management` and `cpt-cf-resource-group-feature-entity-hierarchy` form a strict sequence (types before entities); `cpt-cf-resource-group-feature-membership` can start in parallel with `cpt-cf-resource-group-feature-integration-auth` once entity-hierarchy is complete, but integration-auth depends on both
