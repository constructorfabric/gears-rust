<!-- Created: 2026-04-07 by Constructor Tech -->
<!-- Updated: 2026-08-21 by Constructor Tech -->

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
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Membership Service](#membership-service)
  - [Membership REST Handlers](#membership-rest-handlers)
  - [Unit Test Coverage for Membership](#unit-test-coverage-for-membership)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
  - [Membership Test Cases](#membership-test-cases)
  - [Membership Table Assertions (`resource_group_membership`)](#membership-table-assertions-resource_group_membership)
- [8. E2E Test Plan](#8-e2e-test-plan)
  - [S10: `test_membership_filter_wiring`](#s10-test_membership_filter_wiring)
  - [Acceptance Criteria (S10)](#acceptance-criteria-s10)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Membership lifecycle (add, remove, list) with composite key semantics `(group_id, resource_type, resource_id)`, deterministic lookups by group and by resource, membership schema validation of the resource type, tenant compatibility enforcement.

### 1.2 Purpose

Memberships link resources (users, courses, documents, etc.) to groups in the hierarchy. This feature implements the membership service that manages these many-to-many links with composite key uniqueness, tenant scope derived from the referenced group, and integration with the entity delete lifecycle (membership references block deletion unless force-cascaded).

**Requirements**: `cpt-cf-resource-group-fr-manage-membership`, `cpt-cf-resource-group-fr-query-membership-relations`, `cpt-cf-resource-group-fr-seed-memberships`, `cpt-cf-resource-group-nfr-membership-query-latency`, `cpt-cf-resource-group-nfr-data-lifecycle`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-resource-group-actor-instance-administrator` | Manages memberships through the ordinary authorized APIs |
| `cpt-cf-resource-group-actor-tenant-administrator` | Manages memberships within tenant scope |
| `cpt-cf-resource-group-actor-apps` | Programmatic membership management via `ResourceGroupClient` SDK |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — sections 5.3, 8.3
- **Design**: [DESIGN.md](../DESIGN.md) — sections 3.2 (Membership Service), 3.3 (API), 3.7 (resource_group_membership)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.4
- **Dependencies**: Features 0001, 0003 — SDK foundation, group existence, tenant scope
- **Not applicable**: UX (backend API — no user interface); COMPL (internal platform gear — no regulatory data handling); OPS observability and rollout are managed at the gear infrastructure level (DESIGN §3.7 and platform runbooks); PERF targets are set at the system level in PRD.md NFR section.

## 2. Actor Flows (CDSL)

### Add Membership

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-membership-add`

**Actor**: `cpt-cf-resource-group-actor-tenant-administrator`

**Success Scenarios**:
- Membership link created between group and resource
- Multiple resource types can coexist in the same group

**Error Scenarios**:
- Group not found, or outside the caller's scope → NotFound
- Duplicate membership (same composite key) → Conflict
- resource_type rejected by the bound membership schema → Validation error
- resource_type GTS path not registered → Validation error
- resource_id is empty or exceeds 2600 UTF-8 bytes → Validation error
- Tenant-incompatible: resource already linked in incompatible tenant → TenantIncompatibility

**Steps**:
1. [x] - `p1` - Actor sends POST /api/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id} - `inst-add-memb-1`
2. [x] - `p1` - Validate resource_type is a valid GtsTypePath - `inst-add-memb-2`
3. [x] - `p1` - DB: SELECT id, gts_type_id, tenant_id FROM resource_group WHERE id = {group_id} AND {caller scope} — load target group visible to the caller - `inst-add-memb-3`
4. [x] - `p1` - **IF** group not found or outside the caller's scope → **RETURN** NotFound - `inst-add-memb-4`
5. [x] - `p1` - Read active group/member type snapshots through Types Registry with the caller context, before the write transaction - `inst-add-memb-5`
6. [x] - `p1` - **IF** resource_type not registered → **RETURN** Validation error - `inst-add-memb-6`
7. [x] - `p1` - Load the group's bound membership schema - `inst-add-memb-7`
8. [x] - `p1` - **IF** the membership schema rejects resource_type → **RETURN** Validation error from the membership schema - `inst-add-memb-8`
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
- Membership not found, or its group is outside the caller's scope → NotFound

**Steps**:
1. [x] - `p1` - Actor sends DELETE /api/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id} - `inst-remove-memb-1`
2. [x] - `p1` - Use the resource_type GTS identifier directly - `inst-remove-memb-2`
3. [x] - `p1` - DB: DELETE FROM resource_group_membership WHERE group_id = {group_id} AND gts_type_id = {type_id} AND resource_id = {resource_id} AND group_id IN (groups in the caller's scope) - `inst-remove-memb-3`
4. [x] - `p1` - **IF** no rows affected (missing membership, or group outside the caller's scope) → **RETURN** NotFound: membership does not exist - `inst-remove-memb-4`
5. [x] - `p1` - **RETURN** success (204 No Content) - `inst-remove-memb-5`

### List Memberships

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-membership-list`

**Actor**: `cpt-cf-resource-group-actor-apps`

**Success Scenarios**:
- Paginated list of memberships matching filter criteria returned

**Steps**:
1. [x] - `p1` - Actor sends GET /api/resource-group/v1/memberships?$filter={expr}&cursor={token}&limit={n} - `inst-list-memb-1`
2. [x] - `p1` - Parse OData $filter: supported fields `resource_id` (eq, ne, in), `resource_type` (eq, ne, in), `group_id` (eq, ne, in) - `inst-list-memb-2`
3. [x] - `p1` - Match GTS type predicates against the stored type key through a dictionary subquery (unknown identifiers match no row; no key is created) - `inst-list-memb-3`
4. [x] - `p1` - DB: SELECT group_id, gts_type_id, resource_id FROM resource_group_membership WHERE {filter} AND group_id IN (groups in the caller's scope) ORDER BY {stable} LIMIT {limit+1} - `inst-list-memb-4`
5. [x] - `p1` - Resolve the page's type keys to GTS type identifiers in one batched dictionary read and return them - `inst-list-memb-5`
6. [x] - `p1` - Build Page response with items and cursor tokens - `inst-list-memb-6`
7. [x] - `p1` - **RETURN** Page<ResourceGroupMembership> - `inst-list-memb-7`

## 3. Processes / Business Logic (CDSL)

### Tenant Compatibility Check for Membership

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-membership-check-tenant-compat`

**Input**: resource_type, resource_id, target group's tenant_id

**Output**: Pass or TenantIncompatibility (message names neither tenant, to avoid disclosing the other tenant's id to the caller)

**Steps**:

Updated: the check is one existence read, not a scan and not a count, and it shares a `SERIALIZABLE` transaction with the membership insert it gates (RG-01) — see `has_membership_in_other_tenant` in `membership_repo.rs`.

1. [x] - `p1` - DB: SELECT 1 FROM resource_group rg WHERE rg.id IN (SELECT group_id FROM resource_group_membership WHERE gts_type_id = {resource_type_id} AND resource_id = {resource_id}) AND rg.tenant_id <> {target_tenant_id} LIMIT 1 — does this resource already belong to another tenant - `inst-tenant-check-1`
2. [x] - `p1` - **IF** no row comes back → pass: either the resource has no memberships at all (first membership, any tenant allowed) or every one of them is already the target tenant's - `inst-tenant-check-2`
3. [x] - `p1` - The read and the insert run in one `SERIALIZABLE` transaction: the read is a predicate the insert writes into, so below that level two first memberships from different tenants both commit - `inst-tenant-check-3`
4. [x] - `p1` - **IF** pass → the caller proceeds to the membership insert in the same transaction - `inst-tenant-check-4`
5. [x] - `p1` - **RETURN** TenantIncompatibility: resource already linked to another tenant (the message names neither tenant) - `inst-tenant-check-5`

## 4. States (CDSL)

Not applicable. Memberships are stateless links — they exist or do not exist. There are no intermediate states or transitions. Lifecycle is governed by add/remove operations.

## 5. Definitions of Done

### Membership Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-membership-service`

The system **MUST** implement a Membership Service that provides add, remove, and list operations for membership links with composite key semantics and tenant compatibility enforcement.

**Required behavior**:
- Add: validate group existence, verify resource_type is active and matches the bound membership schema, check tenant compatibility, persist with unique constraint
- Remove: delete by composite key, return NotFound if absent
- List: paginated query with OData `$filter` on `resource_id`, `resource_type`, `group_id`; cursor-based pagination
- Tenant compatibility: derive tenant scope from group's tenant_id; reject if resource already linked in incompatible tenant
- GTS types persisted as keys of RG's identity dictionary, created on first use inside the write transaction just before the insert; registry snapshots provide type rules before writes, and a dictionary row admits nothing
- Add rejects resource identifiers over 2600 UTF-8 bytes before registry reads or database writes; both backends enforce the same constraint
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

### Unit Test Coverage for Membership

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-testing-membership`

All acceptance criteria from feature 0004 are covered by automated tests:
- Add/remove lifecycle with composite key semantics
- bound membership schema validation
- Tenant compatibility enforcement
- Duplicate detection

## 6. Acceptance Criteria

- [x] Adding membership `(G1, User, R1)` creates link and returns 201 with membership body
- [x] Adding membership to nonexistent group returns `NotFound` (404)
- [x] Adding duplicate membership `(G1, User, R1)` returns `Conflict` (409)
- [x] Adding membership with unregistered resource_type GTS path returns validation error (400)
- [x] Adding membership with resource_type rejected by the bound membership schema returns validation error (400)
- [x] Multiple resource types can coexist in the same group: `(G1, User, U1)` and `(G1, Document, D1)` both succeed
- [x] Adding membership for resource already linked in incompatible tenant returns `TenantIncompatibility` (400)
- [x] Removing existing membership returns 204 No Content
- [x] Removing nonexistent membership returns `NotFound` (404)
- [x] List memberships with `$filter=group_id eq 'G1'` returns all memberships for group G1
- [x] List memberships with `$filter=resource_type eq 'User' and resource_id eq 'R1'` returns all groups containing that resource
- [x] Membership responses do not include `tenant_id` — tenant scope derived from group
- [x] No SMALLINT surrogate IDs exposed in membership REST responses
- [x] Tenant deprovisioning cascade-deletes associated memberships

---

## 7. Unit Test Plan

> General testing philosophy, patterns, and infrastructure: [`docs/toolkit_unified_system/12_unit_testing.md`](../../../../../docs/toolkit_unified_system/12_unit_testing.md).

### Membership Test Cases

**File**: `membership_service_test.rs`

Test setup: SQLite in-memory, admitted registry definitions, GroupService and MembershipService.

#### TC-MBR-01: Add membership happy path [P1]
- **Covers**: G25, 0004-AC-1
- **Setup**: Publish a membership schema admitting member_type and bind it to a group type, create group. Add membership.
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

#### TC-MBR-05: Add membership with a resource type excluded by the schema [P1]
- **Covers**: G29, 0004-AC-5
- **Setup**: Create a group with a membership schema that excludes resource_type
- **Assert**: `DomainError::Validation` from the membership schema

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
- **Setup**: Membership schema admitting typeA and typeB. Create group. Add (group, typeA, R1) and (group, typeB, R2).
- **Assert**: Both succeed, list_memberships returns both

#### TC-MBR-10: Tenant compatibility - first membership always allowed [P2]
- **Covers**: 0004-algo-tenant-check-2
- **Setup**: First add for any resource should succeed regardless of tenant
- **Assert**: Success

#### TC-MBR-11: Add membership with empty resource_id [P2]
- No validation on resource_id in code — empty string will be inserted
- Verify it either fails at DB constraint or succeeds (document behavior)

#### TC-MBR-12: Remove membership after resource-type retirement [P2]
- **Setup**: Create a membership, then retire its resource type in Types Registry.
- **Assert**: Removal succeeds using the stored identifiers without a registry lookup.
- **Assert**: Removing an absent composite key returns `DomainError::MembershipNotFound`.

#### TC-MBR-13: Add membership to group whose schema excludes every type [P1]
- The group's membership schema rejects any resource_type
- **Assert**: `DomainError::Validation` from the membership schema

#### TC-MBR-14: Same resource linked in multiple groups of same tenant [P1]
- Resource (type, R1) added to Group A (tenant T), then to Group B (tenant T)
- `existing_tenants.contains(&tenant_id)` = true → pass
- **Assert**: Both succeed

#### TC-MBR-15: List memberships empty result [P3]
- No memberships exist, query returns empty Page
- **Assert**: `page.items.is_empty()`

### Membership Table Assertions (`resource_group_membership`)

| Operation | Required DB Assertions |
|-----------|----------------------|
| **Add membership** | Row exists with correct composite key `(group_id, gts_type_id, resource_id)`. `gts_type_id` is the resource GTS type identifier stored as text. |
| **Remove membership** | Row **gone**. Other memberships of same group **untouched**. |
| **Force delete group** | All membership rows for subtree groups → **0**. |
| **Same resource multiple groups** | Two rows with different `group_id`, same `(gts_type_id, resource_id)`. |

---

## 8. E2E Test Plan

> General E2E testing philosophy, patterns, and infrastructure: [`docs/toolkit_unified_system/13_e2e_testing.md`](../../../../../docs/toolkit_unified_system/13_e2e_testing.md).

### S10: `test_membership_filter_wiring`

**Seam**: OData `$filter` parsing → SQL WHERE clause for memberships.

**Why not in unit tests**: TC-ODATA-05 verifies that `MembershipFilterField` maps `group_id` to the correct column name and kind. The full chain — HTTP `$filter=group_id eq '{id}'` → OData parser → FilterField lookup → SQL `WHERE group_id = ?` — is never tested end-to-end. A mismatch between the parser's expected field name and the FilterField impl breaks filtering silently (returns all rows instead of filtered).

```
Publish group and membership schemas
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
