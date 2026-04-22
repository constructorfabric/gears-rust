<!-- Created: 2026-04-07 by Constructor Tech -->
<!-- Updated: 2026-04-20 by Constructor Tech -->

# Feature: Membership Management

- [x] `p1` - **ID**: `cpt-cf-resource-group-featstatus-membership`

- [x] `p1` - `cpt-cf-resource-group-feature-membership`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Add Membership](#add-membership)
  - [Remove Membership](#remove-membership)
  - [List Memberships](#list-memberships)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Tenant Compatibility Check for Membership](#tenant-compatibility-check-for-membership)
  - [Membership Data Seeding](#membership-data-seeding)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Membership Service](#membership-service)
  - [Membership REST Handlers](#membership-rest-handlers)
  - [Membership Data Seeding](#membership-data-seeding-1)
  - [Unit Test Coverage for Membership](#unit-test-coverage-for-membership)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
  - [Membership Test Cases](#membership-test-cases)
  - [Seeding — Memberships](#seeding--memberships)
  - [Membership Table Assertions (`resource_group_membership`)](#membership-table-assertions-resource_group_membership)
- [8. E2E Test Plan](#8-e2e-test-plan)
  - [S10: `test_membership_filter_wiring`](#s10-test_membership_filter_wiring)
  - [Acceptance Criteria (S10)](#acceptance-criteria-s10)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Membership lifecycle (add, remove, list) with composite key semantics `(group_id, resource_type, resource_id)`, deterministic lookups by group and by resource, allowed_membership_types type validation, tenant compatibility enforcement, and idempotent membership seeding.

### 1.2 Purpose

Memberships link resources (users, courses, documents, etc.) to groups in the hierarchy. This feature implements the membership service that manages these many-to-many links with composite key uniqueness, tenant scope derived from the referenced group, and integration with the entity delete lifecycle (membership references block deletion unless force-cascaded).

**Requirements**: `cpt-cf-resource-group-fr-manage-membership`, `cpt-cf-resource-group-fr-query-membership-relations`, `cpt-cf-resource-group-fr-seed-memberships`, `cpt-cf-resource-group-nfr-membership-query-latency`, `cpt-cf-resource-group-nfr-data-lifecycle`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-resource-group-actor-instance-administrator` | Manages memberships across tenants, operates seeding |
| `cpt-cf-resource-group-actor-tenant-administrator` | Manages memberships within tenant scope |
| `cpt-cf-resource-group-actor-apps` | Programmatic membership management via `ResourceGroupClient` SDK |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — sections 5.3, 8.3
- **Design**: [DESIGN.md](../DESIGN.md) — sections 3.2 (Membership Service), 3.3 (API), 3.7 (resource_group_membership)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.4
- **Dependencies**: Features 0001, 0003 — SDK foundation, group existence, tenant scope
- **Not applicable**: UX (backend API — no user interface); COMPL (internal platform module — no regulatory data handling); OPS observability and rollout are managed at the module infrastructure level (DESIGN §3.7 and platform runbooks); PERF targets are set at the system level in PRD.md NFR section.

## 2. Actor Flows (CDSL)

### Add Membership

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-membership-add`

**Actor**: `cpt-cf-resource-group-actor-tenant-administrator`

**Success Scenarios**:
- Membership link created between group and resource
- Multiple resource types can coexist in the same group

**Error Scenarios**:
- Group not found → NotFound
- Duplicate membership (same composite key) → Conflict
- resource_type not in group type's allowed_membership_types → Validation error
- resource_type GTS path not registered → Validation error
- Tenant-incompatible: resource already linked in incompatible tenant → TenantIncompatibility

**Steps**:
1. [x] - `p1` - Actor sends POST /api/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id} - `inst-add-memb-1`
2. [x] - `p1` - Validate resource_type is a valid GtsTypePath - `inst-add-memb-2`
3. [x] - `p1` - DB: SELECT id, gts_type_id, tenant_id FROM resource_group WHERE id = {group_id} — load target group - `inst-add-memb-3`
4. [x] - `p1` - **IF** group not found → **RETURN** NotFound - `inst-add-memb-4`
5. [x] - `p1` - Resolve resource_type GTS path to surrogate ID; verify type exists in gts_type - `inst-add-memb-5`
6. [x] - `p1` - **IF** resource_type not registered → **RETURN** Validation error - `inst-add-memb-6`
7. [x] - `p1` - Load group type's allowed_membership_types from gts_type_allowed_membership junction - `inst-add-memb-7`
8. [x] - `p1` - **IF** resource_type not in allowed_membership_types → **RETURN** Validation error: "resource_type not permitted for this group type" - `inst-add-memb-8`
9. [x] - `p1` - Invoke tenant compatibility check for resource across existing memberships - `inst-add-memb-9`
10. [x] - `p1` - **IF** tenant incompatible → **RETURN** TenantIncompatibility - `inst-add-memb-10`
11. [x] - `p1` - DB: INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id, created_at) with UNIQUE constraint - `inst-add-memb-11`
12. [x] - `p1` - **IF** unique constraint violation → **RETURN** Conflict: membership already exists - `inst-add-memb-12`
13. [x] - `p1` - **RETURN** created ResourceGroupMembership with group_id, resource_type, resource_id - `inst-add-memb-13`

### Remove Membership

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-membership-remove`

**Actor**: `cpt-cf-resource-group-actor-tenant-administrator`

**Success Scenarios**:
- Membership link removed

**Error Scenarios**:
- Membership not found → NotFound

**Steps**:
1. [x] - `p1` - Actor sends DELETE /api/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id} - `inst-remove-memb-1`
2. [x] - `p1` - Resolve resource_type GTS path to surrogate ID - `inst-remove-memb-2`
3. [x] - `p1` - DB: DELETE FROM resource_group_membership WHERE group_id = {group_id} AND gts_type_id = {type_id} AND resource_id = {resource_id} - `inst-remove-memb-3`
4. [x] - `p1` - **IF** no rows affected → **RETURN** NotFound: membership does not exist - `inst-remove-memb-4`
5. [x] - `p1` - **RETURN** success (204 No Content) - `inst-remove-memb-5`

### List Memberships

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-membership-list`

**Actor**: `cpt-cf-resource-group-actor-apps`

**Success Scenarios**:
- Paginated list of memberships matching filter criteria returned

**Steps**:
1. [x] - `p1` - Actor sends GET /api/resource-group/v1/memberships?$filter={expr}&cursor={token}&limit={n} - `inst-list-memb-1`
2. [x] - `p1` - Parse OData $filter: supported fields `resource_id` (eq, ne, in), `resource_type` (eq, ne, in), `group_id` (eq, ne, in) - `inst-list-memb-2`
3. [x] - `p1` - Resolve any GTS type paths in filter values to surrogate IDs at persistence boundary - `inst-list-memb-3`
4. [x] - `p1` - DB: SELECT group_id, gts_type_id, resource_id FROM resource_group_membership WHERE {filter} ORDER BY {stable} LIMIT {limit+1} - `inst-list-memb-4`
5. [x] - `p1` - Resolve surrogate IDs back to GTS type paths for response - `inst-list-memb-5`
6. [x] - `p1` - Build Page response with items and cursor tokens - `inst-list-memb-6`
7. [x] - `p1` - **RETURN** Page<ResourceGroupMembership> - `inst-list-memb-7`

## 3. Processes / Business Logic (CDSL)

### Tenant Compatibility Check for Membership

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-membership-check-tenant-compat`

**Input**: resource_type, resource_id, target group's tenant_id

**Output**: Pass or TenantIncompatibility with conflicting tenant details

**Steps**:
1. [x] - `p1` - DB: SELECT rgm.group_id, rg.tenant_id FROM resource_group_membership rgm JOIN resource_group rg ON rgm.group_id = rg.id WHERE rgm.gts_type_id = {resource_type_id} AND rgm.resource_id = {resource_id} — find existing memberships for this resource - `inst-tenant-check-1`
2. [x] - `p1` - **IF** no existing memberships → **RETURN** pass (first membership, any tenant allowed) - `inst-tenant-check-2`
3. [x] - `p1` - Collect distinct tenant_ids from existing memberships - `inst-tenant-check-3`
4. [x] - `p1` - **IF** target group's tenant_id is in the same tenant scope as existing memberships (same tenant or related via tenant hierarchy) → **RETURN** pass - `inst-tenant-check-4`
5. [x] - `p1` - **RETURN** TenantIncompatibility: resource already linked in tenant {existing_tenant}, cannot add to tenant {target_tenant} - `inst-tenant-check-5`

### Membership Data Seeding

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-membership-seed`

**Input**: List of membership seed definitions (group_id, resource_type, resource_id)

**Output**: Seed result (memberships created, skipped, failed count)

**Steps**:
1. [x] - `p1` - Load seed definitions - `inst-seed-memb-1`
2. [x] - `p1` - **FOR EACH** seed_def in definitions - `inst-seed-memb-2`
   1. [x] - `p1` - Verify group exists: DB: SELECT id, tenant_id FROM resource_group WHERE id = {seed_def.group_id} - `inst-seed-memb-2a`
   2. [x] - `p1` - **IF** group not found → log warning, skip - `inst-seed-memb-2b`
   3. [x] - `p1` - Invoke tenant compatibility check - `inst-seed-memb-2c`
   4. [x] - `p1` - **IF** incompatible → log warning, skip - `inst-seed-memb-2d`
   5. [x] - `p1` - DB: INSERT INTO resource_group_membership ON CONFLICT DO NOTHING — idempotent upsert - `inst-seed-memb-2e`
3. [x] - `p1` - **RETURN** seed result: {created: N, skipped: N, failed: N} - `inst-seed-memb-3`

## 4. States (CDSL)

Not applicable. Memberships are stateless links — they exist or do not exist. There are no intermediate states or transitions. Lifecycle is governed by add/remove operations.

## 5. Definitions of Done

### Membership Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-membership-service`

The system **MUST** implement a Membership Service that provides add, remove, and list operations for membership links with composite key semantics and tenant compatibility enforcement.

**Required behavior**:
- Add: validate group existence, validate resource_type exists and is in allowed_membership_types, check tenant compatibility, persist with unique constraint
- Remove: delete by composite key, return NotFound if absent
- List: paginated query with OData `$filter` on `resource_id`, `resource_type`, `group_id`; cursor-based pagination
- Tenant compatibility: derive tenant scope from group's tenant_id; reject if resource already linked in incompatible tenant
- GTS type path resolution for resource_type at persistence boundary (no surrogate IDs in API)
- Active reference integration: membership count checked by entity delete in feature 3

**Implements**:
- `cpt-cf-resource-group-flow-membership-add`
- `cpt-cf-resource-group-flow-membership-remove`
- `cpt-cf-resource-group-flow-membership-list`
- `cpt-cf-resource-group-algo-membership-check-tenant-compat`

**Touches**:
- DB: `resource_group_membership`, `resource_group` (JOIN for tenant_id)
- Entities: `ResourceGroupMembership`

### Membership REST Handlers

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-membership-rest-handlers`

The system **MUST** implement REST endpoint handlers for membership management under `/api/resource-group/v1/memberships`.

**Required endpoints**:
- `GET /memberships` — list memberships with OData `$filter` (fields: `resource_id`, `resource_type`, `group_id`; operators: `eq`, `ne`, `in`) and cursor-based pagination. No `tenant_id` in response.
- `POST /memberships/{group_id}/{resource_type}/{resource_id}` — add membership, return 201 Created
- `DELETE /memberships/{group_id}/{resource_type}/{resource_id}` — remove membership, return 204 No Content

**Implements**:
- `cpt-cf-resource-group-flow-membership-add`
- `cpt-cf-resource-group-flow-membership-remove`
- `cpt-cf-resource-group-flow-membership-list`

**Touches**:
- API: `GET /api/resource-group/v1/memberships`, `POST/DELETE /api/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id}`

### Membership Data Seeding

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-membership-seeding`

The system **MUST** provide an idempotent membership seeding mechanism for deployment bootstrapping.

**Required behavior**:
- Accept list of membership seed definitions
- Validate group existence and tenant compatibility for each seed
- Idempotent: duplicate inserts silently skipped (ON CONFLICT DO NOTHING)
- Seeding runs as a pre-deployment step with system SecurityContext

**Implements**:
- `cpt-cf-resource-group-algo-membership-seed`

**Touches**:
- DB: `resource_group_membership`

### Unit Test Coverage for Membership

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-testing-membership`

All acceptance criteria from feature 0004 are covered by automated tests:
- Add/remove lifecycle with composite key semantics
- allowed_membership_types validation
- Tenant compatibility enforcement
- Duplicate detection

## 6. Acceptance Criteria

- [x] Adding membership `(G1, User, R1)` creates link and returns 201 with membership body
- [x] Adding membership to nonexistent group returns `NotFound` (404)
- [x] Adding duplicate membership `(G1, User, R1)` returns `Conflict` (409)
- [x] Adding membership with unregistered resource_type GTS path returns validation error (400)
- [x] Adding membership with resource_type not in group type's allowed_membership_types returns validation error (400)
- [x] Multiple resource types can coexist in the same group: `(G1, User, U1)` and `(G1, Document, D1)` both succeed
- [x] Adding membership for resource already linked in incompatible tenant returns `TenantIncompatibility` (409)
- [x] Removing existing membership returns 204 No Content
- [x] Removing nonexistent membership returns `NotFound` (404)
- [x] List memberships with `$filter=group_id eq 'G1'` returns all memberships for group G1
- [x] List memberships with `$filter=resource_type eq 'User' and resource_id eq 'R1'` returns all groups containing that resource
- [x] Membership responses do not include `tenant_id` — tenant scope derived from group
- [x] No SMALLINT surrogate IDs exposed in membership REST responses
- [x] Membership seeding creates links, skips duplicates, validates tenant compatibility (idempotent)
- [x] Tenant deprovisioning cascade-deletes associated memberships

---

## 7. Unit Test Plan

> General testing philosophy, patterns, and infrastructure: [`docs/modkit_unified_system/12_unit_testing.md`](../../../../../docs/modkit_unified_system/12_unit_testing.md).

### Membership Test Cases

**File**: `membership_service_test.rs`

Test setup: SQLite in-memory + TypeService + GroupService + MembershipService.

#### TC-MBR-01: Add membership happy path [P1]
- **Covers**: G25, 0004-AC-1
- **Setup**: Create type with allowed_membership_types=[member_type], create group. Add membership.
- **Assert**: Membership returned with group_id, resource_type, resource_id

#### TC-MBR-02: Add membership to nonexistent group [P1]
- **Covers**: G26, 0004-AC-2
- **Assert**: `DomainError::GroupNotFound`

#### TC-MBR-03: Add duplicate membership [P1]
- **Covers**: G27, 0004-AC-3
- **Setup**: Add membership, then add same (group_id, resource_type, resource_id) again
- **Assert**: `DomainError::Conflict`

#### TC-MBR-04: Add membership with unregistered resource_type [P1]
- **Covers**: G28, 0004-AC-4
- **Assert**: `DomainError::Validation` with "Unknown resource type"

#### TC-MBR-05: Add membership with resource_type not in allowed_membership_types [P1]
- **Covers**: G29, 0004-AC-5
- **Setup**: Create group of type that does NOT include resource_type in allowed_membership_types
- **Assert**: `DomainError::Validation` with "not in allowed_membership_types"

#### TC-MBR-06: Tenant compatibility violation [P1]
- **Covers**: G30, 0004-AC-7
- **Setup**: Create group in tenant A, add membership (type, resource-1). Create group in tenant B. Try to add same resource (type, resource-1) to group in tenant B.
- **Assert**: `DomainError::TenantIncompatibility`

#### TC-MBR-07: Remove existing membership [P1]
- **Covers**: G31, 0004-AC-8
- **Setup**: Add membership, then remove it
- **Assert**: Success (no error)

#### TC-MBR-08: Remove nonexistent membership [P1]
- **Covers**: G32, 0004-AC-9
- **Assert**: `DomainError::MembershipNotFound`

#### TC-MBR-09: Multiple resource types in same group [P2]
- **Covers**: 0004-AC-6
- **Setup**: Type with allowed_membership_types=[typeA, typeB]. Create group. Add (group, typeA, R1) and (group, typeB, R2).
- **Assert**: Both succeed, list_memberships returns both

#### TC-MBR-10: Tenant compatibility - first membership always allowed [P2]
- **Covers**: 0004-algo-tenant-check-2
- **Setup**: First add for any resource should succeed regardless of tenant
- **Assert**: Success

#### TC-MBR-11: Add membership with empty resource_id [P2]
- No validation on resource_id in code — empty string will be inserted
- Verify it either fails at DB constraint or succeeds (document behavior)

#### TC-MBR-12: Remove membership with unregistered resource_type [P2]
- resolve_id returns None → `DomainError::Validation("Unknown resource type")`

#### TC-MBR-13: Add membership to group with empty allowed_membership_types [P1]
- Group type has `allowed_membership_types: []` — any resource_type should be rejected
- **Assert**: `DomainError::Validation("not in allowed_membership_types")`

#### TC-MBR-14: Same resource linked in multiple groups of same tenant [P1]
- Resource (type, R1) added to Group A (tenant T), then to Group B (tenant T)
- `existing_tenants.contains(&tenant_id)` = true → pass
- **Assert**: Both succeed

#### TC-MBR-15: List memberships empty result [P3]
- No memberships exist, query returns empty Page
- **Assert**: `page.items.is_empty()`

### Seeding — Memberships

**File**: `seeding_test.rs` (integration tests with SQLite)

#### TC-SEED-07: seed_memberships creates links [P1]
- **Covers**: G48, 0004-AC-14
- **Setup**: Create group + type, seed membership definitions.
- **Assert**: `result.created == N`

#### TC-SEED-08: seed_memberships skips duplicates (Conflict -> skip) [P1]
- **Covers**: G48
- **Setup**: Seed membership, seed again.
- **Assert**: `result.unchanged == 1`

#### TC-SEED-09: seed_memberships skips tenant-incompatible (TenantIncompatibility -> skip) [P2]
- **Covers**: G48
- **Setup**: Seed membership in tenant A, then seed same resource to tenant B group.
- **Assert**: `result.skipped == 1`

#### TC-SEED-12: seed_memberships with nonexistent group [P2]
- group_id not in DB → error NOT caught by Conflict/TenantIncompatibility → early return Err
- **Assert**: Error propagated (not silently skipped)

### Membership Table Assertions (`resource_group_membership`)

| Operation | Required DB Assertions |
|-----------|----------------------|
| **Add membership** | Row exists with correct composite key `(group_id, gts_type_id, resource_id)`. `gts_type_id` is SMALLINT (resolved). |
| **Remove membership** | Row **gone**. Other memberships of same group **untouched**. |
| **Force delete group** | All membership rows for subtree groups → **0**. |
| **Same resource multiple groups** | Two rows with different `group_id`, same `(gts_type_id, resource_id)`. |

---

## 8. E2E Test Plan

> General E2E testing philosophy, patterns, and infrastructure: [`docs/modkit_unified_system/13_e2e_testing.md`](../../../../../docs/modkit_unified_system/13_e2e_testing.md).

### S10: `test_membership_filter_wiring`

**Seam**: OData `$filter` parsing → SQL WHERE clause for memberships.

**Why not in unit tests**: TC-ODATA-05 verifies that `MembershipFilterField` maps `group_id` to the correct column name and kind. The full chain — HTTP `$filter=group_id eq '{id}'` → OData parser → FilterField lookup → SQL `WHERE group_id = ?` — is never tested end-to-end. A mismatch between the parser's expected field name and the FilterField impl breaks filtering silently (returns all rows instead of filtered).

```
POST type (with allowed_membership_types)
POST group_a, group_b

PUT /groups/{a.id}/memberships/{type}/res-1     → 201
PUT /groups/{b.id}/memberships/{type}/res-2     → 201

GET /memberships?$filter=group_id eq '{a.id}'   → 200
  assert all items have group_id == a.id
  assert res-2 NOT in items                      (filter actually applied)

GET /memberships?$filter=group_id eq '{b.id}'   → 200
  assert all items have group_id == b.id
```

### Acceptance Criteria (S10)

- [x] S10 verifies the full `$filter` parse → SQL → subset chain — not just that the request is accepted
- [x] Test creates data in two groups and verifies each filter returns only the correct subset
