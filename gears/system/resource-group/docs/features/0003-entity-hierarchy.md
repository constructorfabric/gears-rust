<!-- Created: 2026-04-07 by Constructor Tech -->
<!-- Updated: 2026-04-20 by Constructor Tech -->

# Feature: Group Entity & Hierarchy Engine

- [x] `p1` - **ID**: `cpt-cf-resource-group-featstatus-entity-hierarchy`

- [x] `p1` - `cpt-cf-resource-group-feature-entity-hierarchy`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Create Group](#create-group)
  - [Update Group](#update-group)
  - [Move Group (Subtree)](#move-group-subtree)
  - [Delete Group](#delete-group)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Cycle Detection](#cycle-detection)
  - [Closure Table Rebuild for Subtree Move](#closure-table-rebuild-for-subtree-move)
  - [Query Profile Enforcement](#query-profile-enforcement)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Entity Service](#entity-service)
  - [Hierarchy Engine](#hierarchy-engine)
  - [Group REST Handlers and Hierarchy Endpoint](#group-rest-handlers-and-hierarchy-endpoint)
  - [Unit Test Coverage for Entity Hierarchy](#unit-test-coverage-for-entity-hierarchy)
  - [REST API Test Coverage](#rest-api-test-coverage)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
  - [Current Verification Suites](#current-verification-suites)
  - [Entity Hierarchy Test Cases](#entity-hierarchy-test-cases)
  - [Group `metadata` — Barrier as Data](#group-metadata--barrier-as-data)
  - [REST-level metadata serialization](#rest-level-metadata-serialization)
  - [Invalid / Non-GTS Input Tests](#invalid--non-gts-input-tests)
  - [ADR-001 Hierarchy Reproduction in RG Gear](#adr-001-hierarchy-reproduction-in-rg-gear)
  - [Metadata Validation Against the Materialized Registry Schema](#metadata-validation-against-the-materialized-registry-schema)
  - [REST API Layer](#rest-api-layer)
  - [Priority Matrix](#priority-matrix)
  - [Acceptance Criteria](#acceptance-criteria)
  - [Closure Table Assertions (`resource_group_closure`)](#closure-table-assertions-resource_group_closure)
  - [Entity State Assertions (`resource_group` table)](#entity-state-assertions-resource_group-table)
  - [Hierarchy Endpoint Response Shape](#hierarchy-endpoint-response-shape)
  - [Test Infrastructure](#test-infrastructure)
- [8. E2E Test Plan](#8-e2e-test-plan)
  - [S5: `test_hierarchy_closure`](#s5-test_hierarchy_closure)
  - [S6: `test_move_closure_rebuild`](#s6-test_move_closure_rebuild)
  - [S7: `test_force_delete_cascade`](#s7-test_force_delete_cascade)
  - [Acceptance Criteria (S5, S6, S7)](#acceptance-criteria-s5-s6-s7)
  - [Registry Migration Verification](#registry-migration-verification)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Group entity lifecycle (create, get, update, move, delete) with strict forest invariants (single parent, no cycles), closure-table-based hierarchy engine for efficient ancestor/descendant queries, query profile enforcement (`max_depth`/`max_width`), subtree move/delete operations, hierarchy depth endpoint with relative depth, force delete.

### 1.2 Purpose

Groups are the core nodes of the resource group hierarchy. This feature implements the entity service that enforces structural invariants, the hierarchy engine that maintains the closure table projection for efficient graph queries, and the query profile guardrails that bound hierarchy depth and width.

**Requirements**: `cpt-cf-resource-group-fr-manage-entities`, `cpt-cf-resource-group-fr-enforce-forest-hierarchy`, `cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`, `cpt-cf-resource-group-fr-validate-parent-type`, `cpt-cf-resource-group-fr-delete-entity-no-active-references`, `cpt-cf-resource-group-fr-seed-groups`, `cpt-cf-resource-group-fr-closure-table`, `cpt-cf-resource-group-fr-query-group-hierarchy`, `cpt-cf-resource-group-fr-subtree-operations`, `cpt-cf-resource-group-fr-query-profile`, `cpt-cf-resource-group-fr-profile-change-no-rewrite`, `cpt-cf-resource-group-fr-reduced-constraints-behavior`, `cpt-cf-resource-group-fr-list-groups-depth`, `cpt-cf-resource-group-fr-force-delete`, `cpt-cf-resource-group-nfr-hierarchy-query-latency`

**Principles**: `cpt-cf-resource-group-principle-strict-forest`, `cpt-cf-resource-group-principle-tenant-root-uniqueness`, `cpt-cf-resource-group-principle-query-profile-guardrail`

**Constraints**: `cpt-cf-resource-group-constraint-profile-change-safety`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-resource-group-actor-instance-administrator` | Manages group hierarchy via REST API; consumers own deployment provisioning |
| `cpt-cf-resource-group-actor-tenant-administrator` | Manages groups within tenant scope via REST API |
| `cpt-cf-resource-group-actor-apps` | Programmatic group management via `ResourceGroupClient` SDK |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — sections 5.2, 5.4, 5.5, 5.6, 8.2
- **Design**: [DESIGN.md](../DESIGN.md) — sections 3.1, 3.2 (Entity Service, Hierarchy Service), 3.6 (sequences), 3.7 (resource_group, resource_group_closure), 3.8 (Query Profile)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.3
- **Dependencies**: Feature 0002 — type validation for parent-child compatibility
- **Not applicable**: UX (backend API — no user interface); COMPL (internal platform gear — no regulatory data handling); OPS observability and rollout are managed at the gear infrastructure level (DESIGN §3.7 and platform runbooks); PERF targets are set at the system level in PRD.md NFR section.

## 2. Actor Flows (CDSL)

### Create Group

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-entity-hier-create-group`

**Actor**: `cpt-cf-resource-group-actor-tenant-administrator`

**Success Scenarios**:
- Root group created (no parent) with self-referencing closure row
- Child group created under existing parent with full closure path rows

**Error Scenarios**:
- Invalid type → Validation error
- Parent not found → NotFound
- Parent type not in allowed_parents → InvalidParentType
- Type does not allow root placement (can_be_root=false) and no parent → Validation error
- Creating a tenant-type group as root while another tenant-type root already exists → `TenantRootAlreadyExists` (409 Conflict)
- Depth or width limit exceeded → LimitViolation

The active registry snapshot and payload validation are completed before the
transaction opens. Type and tenant identifiers are immutable after creation.

**Steps**:
1. [x] - `p1` - Actor sends POST /api/resource-group/v1/groups with {type, name, metadata, hierarchy: {parent_id, tenant_id}} - `inst-create-group-1`
2. [x] - `p1` - DB: BEGIN transaction (SERIALIZABLE isolation) - `inst-create-group-2`
3. [x] - `p1` - Use the active registry snapshot resolved before the transaction; persist its GTS identifier - `inst-create-group-3`
4. [x] - `p1` - **IF** parent_id is provided - `inst-create-group-4`
   1. [x] - `p1` - DB: SELECT id, gts_type_id, tenant_id FROM resource_group WHERE id = {parent_id} — load parent in tx - `inst-create-group-4a`
   2. [x] - `p1` - **IF** parent not found → **RETURN** NotFound - `inst-create-group-4b`
   3. [x] - `p1` - Validate child type's allowed_parents includes parent's type - `inst-create-group-4c`
   4. [x] - `p1` - **IF** type incompatible → **RETURN** InvalidParentType - `inst-create-group-4d`
   5. [x] - `p1` - Invoke query profile enforcement: check depth limit - `inst-create-group-4e`
   6. [x] - `p1` - Invoke query profile enforcement: check width limit (sibling count under parent) - `inst-create-group-4f`
5. [x] - `p1` - **ELSE** (root group) - `inst-create-group-5`
   1. [x] - `p1` - **IF** type does not allow root placement (can_be_root=false) → **RETURN** Validation error - `inst-create-group-5a`
   2. [x] - `p1` - **IF** type code starts with `TENANT_RG_TYPE_PATH` (tenant type) AND there already exists any other root group whose type is a tenant type → **RETURN** `TenantRootAlreadyExists` (409 Conflict) - `inst-create-group-5c`
6. [x] - `p1` - Before the write transaction, validate the complete metadata payload through the SDK validator compiled from the materialized registry schema; return Validation on refusal - `inst-create-group-5b`
7. [x] - `p1` - After every rejecting check (a caller-supplied id already taken is rejected first): get or create the type's dictionary key in this transaction, then DB: INSERT INTO resource_group (id, parent_id, gts_type_id, name, metadata, tenant_id) - `inst-create-group-6`
7. [x] - `p1` - DB: INSERT INTO resource_group_closure (ancestor_id=id, descendant_id=id, depth=0) — self-row - `inst-create-group-7`
8. [x] - `p1` - **IF** parent_id is provided - `inst-create-group-8`
   1. [x] - `p1` - DB: INSERT INTO resource_group_closure — ancestor rows from parent's ancestors with depth+1 - `inst-create-group-8a`
9. [x] - `p1` - DB: COMMIT - `inst-create-group-9`
10. [x] - `p1` - **IF** serialization conflict → rollback and retry (bounded retry policy) - `inst-create-group-10`
11. [x] - `p1` - **RETURN** created ResourceGroup with id, type, name, metadata, hierarchy - `inst-create-group-11`

### Update Group

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-entity-hier-update-group`

**Actor**: `cpt-cf-resource-group-actor-tenant-administrator`

**Success Scenarios**:
- Group name, metadata, or parent updated successfully
- Group type and tenant ownership remain immutable; parent changes validate the moved subtree placement

**Error Scenarios**:
- Group not found → NotFound
- Parent change violates the stored type's placement rules → InvalidParentType
- Non-null replacement metadata violates the type schema → Validation

**Steps**:
1. [x] - `p1` - Actor sends PUT /api/resource-group/v1/groups/{group_id} with {name, parent_id, metadata}; nullable replacement fields must be explicit - `inst-update-group-1`
2. [x] - `p1` - DB: SELECT FROM resource_group WHERE id = {group_id} — load existing group - `inst-update-group-2`
3. [x] - `p1` - **IF** group not found → **RETURN** NotFound - `inst-update-group-3`
4. [x] - `p1` - Preserve the stored type identifier and refuse a mismatched pre-read type snapshot - `inst-update-group-4`
   1. [x] - `p1` - Require the preloaded type snapshot when the parent changes - `inst-update-group-4a`
   2. [x] - `p1` - Determine whether the current row requires a structural parent change - `inst-update-group-4b`
   3. [x] - `p1` - Escalate an unexpected parent change to a serializable retry before any write - `inst-update-group-4c`
   4. [x] - `p1` - Validate the move with the current parent snapshot, placement traits and closure rules - `inst-update-group-4d`
5. [x] - `p1` - Validate non-null replacement metadata through the SDK snapshot before the transaction; use the same immutable type inside the write - `inst-update-group-4e`
6. [x] - `p1` - DB: UPDATE resource_group SET name, parent_id, metadata, updated_at — preserve type and tenant identifiers - `inst-update-group-5`
7. [x] - `p1` - **RETURN** updated ResourceGroup - `inst-update-group-6`

### Move Group (Subtree)

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-entity-hier-move-group`

**Actor**: `cpt-cf-resource-group-actor-tenant-administrator`

**Success Scenarios**:
- Group and its entire subtree moved to new parent with closure paths rebuilt transactionally

**Error Scenarios**:
- Group not found → NotFound
- New parent not found → NotFound
- New parent is a descendant of group → CycleDetected
- Self-parent attempt → CycleDetected
- Parent type incompatible → InvalidParentType
- Depth or width limit exceeded at new position → LimitViolation

**Steps**:
1. [x] - `p1` - Actor sends PUT /api/resource-group/v1/groups/{group_id} with new hierarchy.parent_id - `inst-move-group-1`
2. [x] - `p1` - DB: BEGIN transaction (SERIALIZABLE isolation) - `inst-move-group-2`
3. [x] - `p1` - Load group and new parent in transaction - `inst-move-group-3`
4. [x] - `p1` - **IF** new_parent_id == group_id → **RETURN** CycleDetected (self-parent) - `inst-move-group-4`
5. [x] - `p1` - Invoke cycle detection: check new parent is NOT in subtree of group - `inst-move-group-5`
6. [x] - `p1` - **IF** cycle detected → **RETURN** CycleDetected with involved node IDs - `inst-move-group-6`
7. [x] - `p1` - Validate parent type compatibility for group's type against new parent's type - `inst-move-group-7`
8. [x] - `p1` - Invoke query profile enforcement: check depth at new position - `inst-move-group-8`
9. [x] - `p1` - Invoke closure rebuild algorithm for subtree under group - `inst-move-group-9`
10. [x] - `p1` - DB: UPDATE resource_group SET parent_id = {new_parent_id}, updated_at = now() - `inst-move-group-10`
11. [x] - `p1` - DB: COMMIT - `inst-move-group-11`
12. [x] - `p1` - **IF** serialization conflict → rollback and retry (bounded retry policy) - `inst-move-group-12`
13. [x] - `p1` - **RETURN** updated ResourceGroup - `inst-move-group-13`

### Delete Group

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-entity-hier-delete-group`

**Actor**: `cpt-cf-resource-group-actor-tenant-administrator`

**Success Scenarios**:
- Leaf group (no children, no memberships) deleted with closure rows removed
- Force delete: group and entire subtree deleted with all memberships cascaded

**Error Scenarios**:
- Group not found → NotFound
- Has children or memberships (without force) → ConflictActiveReferences

**Steps**:
1. [x] - `p1` - Actor sends DELETE /api/resource-group/v1/groups/{group_id}?force={true|false} - `inst-delete-group-1`
2. [x] - `p1` - DB: SELECT FROM resource_group WHERE id = {group_id} - `inst-delete-group-2`
3. [x] - `p1` - **IF** group not found → **RETURN** NotFound - `inst-delete-group-3`
4. [x] - `p1` - **IF** force = false - `inst-delete-group-4`
   1. [x] - `p1` - DB: SELECT COUNT(*) FROM resource_group WHERE parent_id = {group_id} — check children - `inst-delete-group-4a`
   2. [x] - `p1` - DB: SELECT COUNT(*) FROM resource_group_membership WHERE group_id = {group_id} — check memberships - `inst-delete-group-4b`
   3. [x] - `p1` - **IF** children > 0 OR memberships > 0 → **RETURN** ConflictActiveReferences - `inst-delete-group-4c`
5. [x] - `p1` - **IF** force = true - `inst-delete-group-5`
   1. [x] - `p1` - Collect entire subtree: DB: SELECT descendant_id FROM resource_group_closure WHERE ancestor_id = {group_id} - `inst-delete-group-5a`
   2. [x] - `p1` - DB: DELETE FROM resource_group_membership WHERE group_id IN (subtree IDs) — cascade memberships - `inst-delete-group-5b`
   3. [x] - `p1` - DB: DELETE FROM resource_group_closure WHERE ancestor_id IN (subtree IDs) OR descendant_id IN (subtree IDs) — cascade closure - `inst-delete-group-5c`
   4. [x] - `p1` - DB: DELETE FROM resource_group WHERE id IN (subtree IDs) — delete groups bottom-up - `inst-delete-group-5d`
6. [x] - `p1` - **ELSE** (leaf delete without force) - `inst-delete-group-6`
   1. [x] - `p1` - DB: DELETE FROM resource_group_closure WHERE descendant_id = {group_id} — remove closure rows - `inst-delete-group-6a`
   2. [x] - `p1` - DB: DELETE FROM resource_group WHERE id = {group_id} - `inst-delete-group-6b`
7. [x] - `p1` - **RETURN** success (204 No Content) - `inst-delete-group-7`

## 3. Processes / Business Logic (CDSL)

### Cycle Detection

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-entity-hier-cycle-detect`

**Input**: Group ID being moved, proposed new parent ID

**Output**: Pass or CycleDetected with involved node IDs

**Steps**:
1. [x] - `p1` - **IF** new_parent_id == group_id → **RETURN** CycleDetected (self-parent) - `inst-cycle-1`
2. [x] - `p1` - DB: SELECT descendant_id FROM resource_group_closure WHERE ancestor_id = {group_id} — get all descendants of the moving group - `inst-cycle-2`
3. [x] - `p1` - **IF** new_parent_id IN descendants → **RETURN** CycleDetected: new parent is a descendant of the moving group - `inst-cycle-3`
4. [x] - `p1` - **RETURN** pass - `inst-cycle-4`

### Closure Table Rebuild for Subtree Move

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-entity-hier-closure-rebuild`

**Input**: Group ID being moved, old parent ID, new parent ID

**Output**: Updated closure table rows (within active transaction)

**Steps**:
1. [x] - `p1` - Collect subtree: DB: SELECT descendant_id FROM resource_group_closure WHERE ancestor_id = {group_id} — includes group itself - `inst-closure-rebuild-1`
2. [x] - `p1` - Delete affected paths: DB: DELETE FROM resource_group_closure WHERE descendant_id IN (subtree) AND ancestor_id NOT IN (subtree) — remove old ancestor paths above the moving group - `inst-closure-rebuild-2`
3. [x] - `p1` - Compute new ancestor paths from new parent: DB: SELECT ancestor_id, depth FROM resource_group_closure WHERE descendant_id = {new_parent_id} — get new parent's ancestors - `inst-closure-rebuild-3`
4. [x] - `p1` - **FOR EACH** new_ancestor in new parent's ancestors (including new parent) - `inst-closure-rebuild-4`
   1. [x] - `p1` - **FOR EACH** subtree_node in subtree - `inst-closure-rebuild-4a`
      1. [x] - `p1` - DB: INSERT INTO resource_group_closure (ancestor_id = new_ancestor, descendant_id = subtree_node, depth = new_ancestor_depth + subtree_node_relative_depth + 1) - `inst-closure-rebuild-4a1`
5. [x] - `p1` - **RETURN** (closure rows updated within transaction — commit handled by caller) - `inst-closure-rebuild-5`

### Query Profile Enforcement

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-entity-hier-enforce-query-profile`

**Input**: Operation context (create/move), current group position, profile config (max_depth, max_width)

**Output**: Pass or LimitViolation (DepthLimitExceeded / WidthLimitExceeded)

**Steps**:
1. [x] - `p1` - Load profile config: max_depth (optional), max_width (optional) - `inst-profile-1`
2. [x] - `p1` - **IF** max_depth is enabled (not null) - `inst-profile-2`
   1. [x] - `p1` - Compute resulting depth: depth of new parent + 1 + max descendant depth in subtree (for move) or 0 (for create) - `inst-profile-2a`
   2. [x] - `p1` - **IF** resulting depth > max_depth → **RETURN** LimitViolation: DepthLimitExceeded with current depth and limit - `inst-profile-2b`
3. [x] - `p1` - **IF** max_width is enabled (not null) - `inst-profile-3`
   1. [x] - `p1` - DB: SELECT COUNT(*) FROM resource_group WHERE parent_id = {parent_id} — current sibling count - `inst-profile-3a`
   2. [x] - `p1` - **IF** sibling_count + 1 > max_width → **RETURN** LimitViolation: WidthLimitExceeded with current width and limit - `inst-profile-3b`
4. [x] - `p1` - **RETURN** pass - `inst-profile-4`

## 4. States (CDSL)

Not applicable. Groups exist as hierarchy nodes without lifecycle states. A group is either present in the hierarchy or deleted. Structural integrity (parent-child relationships, closure projections) is managed by the entity service and hierarchy engine, not by state machine transitions.

## 5. Definitions of Done

### Entity Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-entity-hier-entity-service`

The system **MUST** implement an Entity Service that provides create, get, update, move, and delete operations for group entities with forest invariant enforcement.

**Required behavior**:
- Create: validate type, parent compatibility, profile limits; persist entity + closure rows in SERIALIZABLE tx
- Get: retrieve by UUID; return NotFound if absent
- Update: preserve the group type, validate supplied metadata and any parent change; update mutable fields
- Move: cycle detection, parent type validation, profile limits; rebuild closure paths in SERIALIZABLE tx with bounded retry
- Delete: reference check (children + memberships); reject or force-cascade; remove closure rows
- All hierarchy-mutating writes (create/move/delete) use SERIALIZABLE isolation with bounded retry for serialization conflicts

**Implements**:
- `cpt-cf-resource-group-flow-entity-hier-create-group`
- `cpt-cf-resource-group-flow-entity-hier-update-group`
- `cpt-cf-resource-group-flow-entity-hier-move-group`
- `cpt-cf-resource-group-flow-entity-hier-delete-group`
- `cpt-cf-resource-group-algo-entity-hier-cycle-detect`
- `cpt-cf-resource-group-algo-entity-hier-enforce-query-profile`

**Touches**:
- DB: `resource_group`, `resource_group_closure`
- Entities: `ResourceGroupEntity`, `ResourceGroupClosure`

### Hierarchy Engine

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-entity-hier-hierarchy-engine`

The system **MUST** implement a Hierarchy Service that maintains the closure table and serves ancestor/descendant queries.

**Required behavior**:
- Closure table maintenance: self-row on insert, ancestor rows from parent chain, full path rebuild on subtree move, cascade removal on delete
- Ancestor queries: return all ancestors of a group ordered by depth (ascending)
- Descendant queries: return all descendants of a group ordered by depth (ascending)
- Hierarchy depth endpoint: `GET /groups/{group_id}/hierarchy` returning `Page<ResourceGroupWithDepth>` with `hierarchy.depth` (relative: 0=self, positive=descendants, negative=ancestors)
- OData filtering on `hierarchy/depth` (eq, ne, gt, ge, lt, le) and `type` (eq, ne, in)
- Query profile enforcement: `max_depth`/`max_width` checked on writes only; reads return full stored data even if profile was tightened; no data rewrite on profile change

**Implements**:
- `cpt-cf-resource-group-algo-entity-hier-closure-rebuild`
- `cpt-cf-resource-group-algo-entity-hier-enforce-query-profile`

**Constraints**: `cpt-cf-resource-group-constraint-profile-change-safety`

**Touches**:
- DB: `resource_group_closure`, `resource_group`
- Entities: `ResourceGroupClosure`, `ResourceGroupWithDepth`

### Group REST Handlers and Hierarchy Endpoint

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-entity-hier-rest-handlers`

The system **MUST** implement REST endpoint handlers for group management under `/api/resource-group/v1/groups` and the hierarchy depth endpoint.

**Required endpoints**:
- `GET /groups` — list groups with OData `$filter` (fields: `type`, `hierarchy/parent_id`, `tenant_id`, `id`, `name`; operators: `eq`, `ne`, `in`) and cursor-based pagination
- `POST /groups` — create group, return 201 Created
- `GET /groups/{group_id}` — get group by UUID, return 404 if not found
- `PUT /groups/{group_id}` — replace mutable fields (name, parent_id, metadata); type and tenant ownership remain immutable, return 200 OK
- `DELETE /groups/{group_id}?force={true|false}` — delete group, return 204 No Content
- `GET /groups/{group_id}/hierarchy` — hierarchy depth traversal with OData `$filter` on `hierarchy/depth` and `type`, cursor-based pagination

**Implements**:
- `cpt-cf-resource-group-flow-entity-hier-create-group`
- `cpt-cf-resource-group-flow-entity-hier-update-group`
- `cpt-cf-resource-group-flow-entity-hier-move-group`
- `cpt-cf-resource-group-flow-entity-hier-delete-group`

**Touches**:
- API: `GET/POST /api/resource-group/v1/groups`, `GET/PUT/DELETE /api/resource-group/v1/groups/{group_id}`, `GET /api/resource-group/v1/groups/{group_id}/hierarchy`

### Unit Test Coverage for Entity Hierarchy

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-testing-entity-hierarchy`

All acceptance criteria from feature 0003 are covered by automated tests:
- Child group creation with closure table verification
- Move operations with cycle detection and closure rebuild
- Type compatibility on create/move/update
- Query profile enforcement (max_depth, max_width)
- Delete with reference checks and force cascade
- Hierarchy depth endpoint with correct relative depths

### REST API Test Coverage

- [x] `p2` - **ID**: `cpt-cf-resource-group-dod-testing-rest-api`

REST-level tests for endpoints not covered by existing `api_rest_test.rs`:
- POST/DELETE /memberships/{group_id}/{type}/{resource_id}
- GET /groups/{id}/hierarchy
- DELETE /groups/{id}?force=true

## 6. Acceptance Criteria

- [x] Root group (can_be_root=true, no parent) is created with self-referencing closure row (depth=0)
- [x] Child group is created with closure rows linking to all ancestors at correct depths
- [x] Creating group with parent of incompatible type returns `InvalidParentType` (400)
- [x] Creating group with nonexistent parent returns `NotFound` (404)
- [x] Creating root group when type has can_be_root=false returns validation error (400)
- [x] Moving group to new parent rebuilds closure paths transactionally for entire subtree
- [x] Moving group under its own descendant returns `CycleDetected` (400)
- [x] Moving group under itself (self-parent) returns `CycleDetected` (400)
- [x] Moving group to incompatible parent type returns `InvalidParentType` (400)
- [x] Group type is immutable; reparenting validates placement against the admitted type snapshot
- [x] Deleting leaf group (no children, no memberships) succeeds (204) and removes closure rows
- [x] Deleting group with children without force returns `ConflictActiveReferences` (400) with response body listing blocking entities (children count/IDs and membership count) so the caller can display what prevents deletion
- [x] Force delete removes entire subtree including memberships and closure rows
- [x] Hierarchy endpoint returns ancestors (negative depth) and descendants (positive depth) with correct relative distances
- [x] OData `$filter` on `hierarchy/depth` supports eq, ne, gt, ge, lt, le operators
- [x] Write operations that exceed max_depth are rejected with `DepthLimitExceeded`
- [x] Write operations that exceed max_width are rejected with `WidthLimitExceeded`
- [x] Reads return full stored data even when profile was tightened (no truncation)
- [x] Concurrent hierarchy mutations use SERIALIZABLE isolation with bounded retry
- [x] Consumers provision group hierarchies through the ordinary authorized API after publishing their types
- [x] Creating group with metadata that violates materialized payload schema returns Validation error (400) — field type mismatch, maxLength exceeded, unknown field (additionalProperties:false)
- [x] Creating group with valid metadata matching materialized payload schema succeeds
- [x] Updating group metadata validates against materialized payload schema
- [x] Creating a group is checked against the admitted full payload schema, including inherited constraints
---

## 7. Unit Test Plan

> General testing philosophy, patterns, and infrastructure: [`docs/toolkit_unified_system/12_unit_testing.md`](../../../../../docs/toolkit_unified_system/12_unit_testing.md).

### Current Verification Suites

Tests provision admitted registry definitions before exercising ordinary group
operations. Test counts are determined by the current suite; the detailed cases
below retain the supported hierarchy, tenant, metadata and REST behavior plan.

| Suite | Contract |
| --- | --- |
| `group_service_test.rs` | Root/child placement, name and payload validation, immutable type, move/cycle rules, closure maintenance and profile limits |
| `registry_types_test.rs` | Registry lifecycle behavior and schema/trait consumption for writes |
| `tenant_filtering_db_test.rs` and `create_group_target_tenant_test.rs` | Caller scope, target-tenant authorization and row visibility |
| `db_behavior_audit_test.rs` and `query_recorder_test.rs` | SQL scope enforcement, set-based hierarchy operations and query behavior |
| `api_rest_test.rs` | Group/hierarchy REST contracts, error classification and absence of retired type routes |
| SQLite/PostgreSQL type-identifier migration suites | Preserved identifiers, groups, memberships, closure rows, indexes and constraints |
| `pg_concurrency_test.rs` and `pg_smoke_test.rs` | PostgreSQL transaction/concurrency behavior and backend SQL |

### Entity Hierarchy Test Cases

**File**: `group_service_test.rs`

Test setup: SQLite in-memory, admitted registry definitions and GroupService with configurable QueryProfile.

#### TC-GRP-01: Create child group with parent - closure rows correct [P1]
- **Covers**: G8, 0003-AC-1,2
- **Setup**: Create parent type (can_be_root=true), child type (allowed_parents=[parent_type]). Create root group, then child group under it.
- **Assert**: Child group returned with `hierarchy.parent_id`, closure table has self-row (depth=0) and ancestor row (depth=1)

#### TC-GRP-02: Create 3-level hierarchy - closure table completeness [P1]
- **Covers**: G8, 0003-AC-2
- **Setup**: Grandparent -> Parent -> Child groups
- **Assert**: Child has closure rows to grandparent (depth=2), parent (depth=1), self (depth=0). Parent has rows to grandparent (depth=1), self (depth=0).

#### TC-GRP-03: Create group with incompatible parent type [P1]
- **Covers**: G9, 0003-AC-3
- **Setup**: Create type A (can_be_root=true), type B (can_be_root=false, allowed_parents=[]... wait, that violates placement invariant). Create type A (root), type B (allowed_parents=[A]), type C (allowed_parents=[B]). Create root group of type A. Try to create child of type C under group A.
- **Assert**: `DomainError::InvalidParentType`

#### TC-GRP-04: Create root group when can_be_root=false [P1]
- **Covers**: G10, 0003-AC-5
- **Setup**: Create type with can_be_root=false, allowed_parents=[some_parent_type]. Try to create root group (no parent).
- **Assert**: `DomainError::InvalidParentType` with "cannot be a root group"

#### TC-GRP-05: Move group - happy path with closure rebuild [P1]
- **Covers**: G11, 0003-AC-6
- **Setup**: Create tree: Root1 -> Child -> Grandchild. Create Root2. Move Child (with subtree) under Root2.
- **Assert**: Child.parent_id == Root2.id. Closure table rebuilt: Grandchild has path to Root2 (depth=2), Child (depth=1), self (depth=0). Old paths to Root1 removed.

#### TC-GRP-06: Move group under its descendant -> CycleDetected [P1]
- **Covers**: G12, 0003-AC-7
- **Setup**: Create Root -> Parent -> Child. Try to move Root under Child.
- **Assert**: `DomainError::CycleDetected`

#### TC-GRP-07: Self-parent -> CycleDetected [P1]
- **Covers**: G13, 0003-AC-8
- **Setup**: Create group. Update with parent_id = own id.
- **Assert**: `DomainError::CycleDetected`

#### TC-GRP-08: Move group to incompatible parent type [P1]
- **Covers**: G14, 0003-AC-9
- **Setup**: Type A (root), Type B (allowed_parents=[A]). Create group of type B under group of type A. Create group of type A (root). Try to move group B under another group B.
- **Assert**: `DomainError::InvalidParentType`

#### TC-GRP-09: Update group name and metadata [P2]
- **Covers**: G15
- **Setup**: Create group, update with new name and metadata
- **Assert**: Updated group returned with new name/metadata

#### TC-GRP-12: Delete leaf group (no children, no memberships) [P1]
- **Covers**: G17, 0003-AC-11
- **Setup**: Create group, delete without force
- **Assert**: Success, group no longer found, closure rows removed

#### TC-GRP-13: Delete group with children without force [P1]
- **Covers**: G18, 0003-AC-12
- **Setup**: Create parent -> child. Delete parent without force.
- **Assert**: `DomainError::ConflictActiveReferences` with "child group(s)"; error detail MUST include blocking entity count (children count) so the caller can display what prevents deletion

#### TC-GRP-14: Delete group with memberships without force [P1]
- **Covers**: G19, 0003-AC-12
- **Setup**: Create group, add membership. Delete group without force.
- **Assert**: `DomainError::ConflictActiveReferences` with "memberships"; error detail MUST include blocking membership count

#### TC-GRP-15: Force delete subtree [P1]
- **Covers**: G20, 0003-AC-13
- **Setup**: Create Root -> Parent -> Child, with memberships on each. Force delete Root.
- **Assert**: All 3 groups gone, all memberships gone, all closure rows gone

#### TC-GRP-16: Hierarchy endpoint - ancestors and descendants [P1]
- **Covers**: G21, 0003-AC-14
- **Setup**: Create 3-level tree (A -> B -> C). Call list_group_hierarchy(B).
- **Assert**: Returns A (depth=-1), B (depth=0), C (depth=1)

#### TC-GRP-17: max_depth enforcement on create [P1]
- **Covers**: G22, 0003-AC-16
- **Setup**: QueryProfile { max_depth: Some(2), max_width: None }. Create Root (depth=0), Child (depth=1). Try to create Grandchild (depth=2).
- **Assert**: `DomainError::LimitViolation` with "Depth limit exceeded"

#### TC-GRP-18: max_width enforcement on create [P1]
- **Covers**: G23, 0003-AC-17
- **Setup**: QueryProfile { max_depth: None, max_width: Some(2) }. Create Root, add Child1, Child2 under Root. Try to add Child3.
- **Assert**: `DomainError::LimitViolation` with "Width limit exceeded"

#### TC-GRP-19: max_depth enforcement on move [P2]
- **Covers**: G24, 0003-AC-16
- **Setup**: QueryProfile { max_depth: Some(3) }. Deep tree. Move subtree to position that would exceed max_depth.
- **Assert**: `DomainError::LimitViolation`

#### TC-GRP-20: Group name validation - empty [P2]
- **Covers**: G33
- **Assert**: `DomainError::Validation` with "between 1 and 255"

#### TC-GRP-21: Group name validation - too long (>255) [P2]
- **Covers**: G33
- **Assert**: `DomainError::Validation` with "between 1 and 255"

#### TC-GRP-22: Create group with nonexistent type_path [P1]
- **Assert**: `DomainError::TypeNotFound`

#### TC-GRP-23: Create child group with parent from different tenant [P1]
- Parent.tenant_id != child tenant_id → `DomainError::Validation("must match parent tenant_id")`
- This is NOT InvalidParentType — it's a separate Validation branch (line 379-384)

#### TC-GRP-24: Create group with metadata (JSONB) [P2]
- Create with `metadata: Some(json!({"self_managed": true}))`, verify stored and returned

#### TC-GRP-25: Multiple root groups of same type [P2]
- Create 2 root groups of same can_be_root=true type, both succeed

#### TC-GRP-29: Move child to root (detach from parent) - happy path [P1]
- Child under parent, move with new_parent_id=None, type allows can_be_root=true
- **Assert**: Success, parent_id=None, closure rebuilt (old ancestor rows removed, self-row only)

#### TC-GRP-30: Move child to root when can_be_root=false [P1]
- **Assert**: `DomainError::InvalidParentType("cannot be a root group")`

#### TC-GRP-31: Move nonexistent group [P2]
- **Assert**: `DomainError::GroupNotFound`

#### TC-GRP-32: Move to nonexistent parent [P2]
- **Assert**: `DomainError::GroupNotFound` for the new parent

#### TC-GRP-33: max_width enforcement on move [P2]
- Move group under parent that already has max_width children
- **Assert**: `DomainError::LimitViolation("Width limit exceeded")`

#### TC-GRP-34: Delete nonexistent group [P2]
- **Assert**: `DomainError::GroupNotFound`

#### TC-GRP-35: Force delete leaf node (no descendants) [P2]
- Group with no children, force=true — descendant_ids is empty, still works
- **Assert**: Success

#### TC-GRP-36: list_group_hierarchy nonexistent group [P2]
- **Assert**: `DomainError::GroupNotFound`

#### TC-GRP-37: Depth limit exact boundary (parent_depth+1 == max_depth) [P1]
- Comparison is `>=` not `>`: at exact limit, reject
- max_depth=3, parent at depth=2, try add child at depth=3 → `LimitViolation`

#### TC-GRP-38: Width limit exact boundary (sibling_count == max_width) [P1]
- max_width=2, parent has exactly 2 children, try add 3rd → `LimitViolation`

### Group `metadata` — Barrier as Data

**File**: `group_service_test.rs` (service-level), `api_rest_test.rs` (REST-level)

#### TC-META-12: Group with metadata barrier stored and returned [P1]
- Create group with `metadata: Some(json!({"self_managed": true}))`
- Get group → `metadata.self_managed == true`
- **Covers**: PRD 3.4, Feature 0005-AC "barrier as data"
- **DB assert**: `resource_group.metadata` JSONB column contains `{"self_managed": true}`

#### TC-META-13: Group with rich metadata — multiple fields [P1]
- `metadata: Some(json!({"self_managed": true, "label": "Partner", "category": "premium"}))`
- **Assert**: all fields preserved in round-trip

#### TC-META-14: Group metadata update replaces entirely (not merge) [P1]
- Create group with `metadata: {"a": 1, "b": 2}`, update with `metadata: {"c": 3}`
- **Assert**: `metadata == {"c": 3}`, old keys gone
- **DB assert**: confirm in `resource_group` table

#### TC-META-15: Group metadata None → update with metadata → get returns metadata [P2]
- Create with None, update with `{"self_managed": false}`, get → `{"self_managed": false}`

#### TC-META-16: Group metadata set → update with None → metadata gone [P2]
- Create with `{"x": 1}`, update with `metadata: None`
- **Assert**: get returns metadata = None, JSON response has no `metadata` key

#### TC-META-17: Barrier group visible in hierarchy (RG does NOT filter) [P1]
- Create parent → child with `metadata: {"self_managed": true}` → grandchild
- `list_group_hierarchy(parent)` → returns ALL 3 including barrier child
- **Covers**: PRD "RG does not filter based on barrier", Feature 0005-AC
- **Assert**: barrier group present in results, depth correct

#### TC-META-18: Group metadata in hierarchy endpoint response [P1]
- Create groups with various metadata
- `list_group_hierarchy` → each `GroupWithDepthDto` includes `metadata` field
- **Assert**: metadata preserved in hierarchy response (Feature 0005 requirement)

### REST-level metadata serialization

**File**: `api_rest_test.rs`

#### TC-META-20: REST create group with metadata in body [P1]
- POST body: `{"type": "...", "name": "X", "metadata": {"self_managed": true}}`
- **Assert**: 201, response body has `"metadata": {"self_managed": true}`

#### TC-META-21: REST response omits metadata when null [P2]
- Create group without metadata
- **Assert**: JSON response does NOT contain `"metadata"` key (via `skip_serializing_if`)

### Invalid / Non-GTS Input Tests

**ONE existing test** checks invalid type code: `create_type_invalid_code_returns_400` with `"code": "invalid"`. Nothing else.


#### Type code / type_path — wrong GTS format:

#### TC-NOGTS-04: Create group with non-RG type_path [P1]
- `type: "gts.cf.core.user.v1~"` — valid GTS but not RG type
- **Assert**: 400 Validation ("must start with prefix")

#### TC-NOGTS-05: Create group with empty type_path [P1]
- `type: ""`
- **Assert**: 400 ("must not be empty")

#### TC-NOGTS-06: Membership with non-GTS resource_type [P1]
- POST `/memberships/{group_id}/not.a.gts.path/res-1`
- **Assert**: 400 Validation ("Unknown resource type") — resolve_id returns None

#### TC-NOGTS-07: Membership with empty resource_type [P2]
- POST `/memberships/{group_id}//res-1` or empty segment
- **Assert**: 404 (route mismatch) or 400

#### REST deserialization errors — wrong JSON types:

#### TC-DESER-04: Create group with `type` field missing [P1]
- Body: `{"name": "X"}`
- **Assert**: 400/422

#### TC-DESER-05: Create group with `parent_id: "not-a-uuid"` [P2]
- **Assert**: 400/422 (UUID parse failure)

#### TC-DESER-06: Malformed JSON body [P1]
- Body: `{bad json}`
- **Assert**: 400

#### TC-DESER-07: Empty body when body expected [P1]
- POST /types with empty body
- **Assert**: 400/422

#### TC-DESER-08: Create group with `name: ""` (empty string) [P1]
- Deserialization succeeds, but `validate_name` catches it
- **Assert**: 400 Validation ("between 1 and 255 characters")

#### TC-DESER-09: Group path `group_id` not a UUID [P2]
- GET `/groups/not-a-uuid`
- **Assert**: 400 (Path parameter parse failure)

#### TC-DESER-10: Membership path `group_id` not a UUID [P2]
- POST `/memberships/not-a-uuid/type/res`
- **Assert**: 400

#### TC-DESER-11: Extra unknown fields in body [P3]
- `{"code": "...", "can_be_root": true, "unknown_field": 42}`
- **Assert**: Verify behavior — serde default is ignore (200) or reject?

### ADR-001 Hierarchy Reproduction in RG Gear

Reproduce the full ADR example hierarchy (T1→D2→B3, T7→D8, T9) with correct types, parents, and metadata — entirely through RG service layer.

#### TC-ADR-01: Full ADR hierarchy with types, groups, and memberships [P1]
- **Setup**: Create all RG types (tenant, department, branch + user/course as membership types) through the registry test fixture. Create groups T1, D2, B3, T7, D8, T9 via GroupService with correct parent-child and metadata. Add memberships (user in T1, user in D2, course in B3).
- **Assert per group**:
  - T1: root tenant, `parent_id=None`, `metadata: None`
  - D2: dept under T1, `metadata: {category: "finance", short_description: "Mega Department"}`
  - B3: branch under D2, `metadata: {location: "Building A, Floor 3"}`
  - T7: self-managed tenant under T1, `metadata: {self_managed: true}`
  - D8: dept under T7, `metadata: {category: "hr"}`
  - T9: root tenant, `metadata: {custom_domain: "t9.example.com"}`
- **Closure assert**: full hierarchy depths correct
- **Membership assert**: each membership group_id + resource_type correct

#### TC-ADR-02: Tenant type allows self-nesting (T7 under T1) [P1]
- Tenant type: `allowed_parents: [tenant_type_code]` — self-referential
- Create T1 (root), create T7 under T1 (both tenant type)
- **Assert**: Success, T7 parent_id = T1.id

#### TC-ADR-03: Department cannot be root [P1]
- Department type: `can_be_root: false, allowed_parents: [tenant_type_code]`
- Try to create department with parent_id=None
- **Assert**: `DomainError::InvalidParentType("cannot be a root group")`

#### TC-ADR-04: Branch only under department (not under tenant) [P1]
- Branch type: `allowed_parents: [department_type_code]`
- Try to create branch directly under tenant
- **Assert**: `DomainError::InvalidParentType`

#### TC-ADR-05: Branch allows users AND courses as members [P1]
- Branch type: `allowed_memberships: [user_type, course_type]`
- Add user membership to branch → success
- Add course membership to branch → success
- **Assert**: both memberships exist

#### TC-ADR-06: Tenant allows only users as members (not courses) [P1]
- Tenant type: `allowed_memberships: [user_type]` (no course)
- Add user to tenant → success
- Add course to tenant → **DomainError::Validation("not in allowed_memberships")**

#### TC-ADR-07: Same resource (user) in multiple groups with different group_ids [P1]
- Per ADR: "R8 appears twice: as user in D8 and T7"
- Add user R8 to D8, add user R8 to T7 (same tenant)
- **Assert**: both memberships succeed

#### TC-ADR-08: Same resource (R4) with different types in different groups [P1]
- Per ADR: "R4 as course in B3 and as user in T1"
- Add R4 with type=course to B3, add R4 with type=user to T1
- **Assert**: both succeed (different gts_type_id, same resource_id)

### Metadata Validation Against the Materialized Registry Schema

Each chained RG type may constrain the group payload, including metadata, with field types, length limits and `additionalProperties`. **RG MUST validate the full payload through the SDK using the unmodified materialized registry schema on create and non-null metadata replacement.**

Registry admission validates the authored schemas. These cases verify the runtime payload validation path through the registry SDK using the materialized full schema and the group metadata payload.

`RegistryTypeCatalog` reads content and resolved schema through `TypesRegistryApi`.
`RgType` compiles the public SDK payload validator and services validate the full
payload before opening the write transaction. The cases below exercise inherited
constraints and metadata rules through that implemented path.

##### Tenant metadata (`self_managed: boolean`, `custom_domain: hostname`)

#### TC-ADR-09: Tenant — valid metadata.self_managed=true accepted [P1]
- Create tenant group with `metadata: {"self_managed": true}`
- **Assert**: 201 success

#### TC-ADR-10: Tenant — self_managed wrong type (string) rejected [P1]
- Create tenant group with `metadata: {"self_managed": "yes"}`
- **Assert**: 400 Validation error — `self_managed` must be boolean

#### TC-ADR-11: Tenant — self_managed wrong type (number) rejected [P1]
- `metadata: {"self_managed": 42}`
- **Assert**: 400

#### TC-ADR-12: Tenant — unknown metadata field rejected [P1]
- `metadata: {"self_managed": true, "foo": "bar"}`
- **Assert**: 400 — `additionalProperties: false` rejects unknown fields

#### TC-ADR-13: Tenant — valid custom_domain accepted [P1]
- `metadata: {"custom_domain": "t9.example.com"}`
- **Assert**: 201

#### TC-ADR-14: Tenant — custom_domain wrong type (number) rejected [P2]
- `metadata: {"custom_domain": 123}`
- **Assert**: 400

#### TC-ADR-15: Tenant — empty metadata accepted (all fields optional) [P1]
- `metadata: {}`
- **Assert**: 201 — no required fields in tenant metadata schema

#### TC-ADR-16: Tenant — metadata=null accepted [P2]
- `metadata: null` or field absent
- **Assert**: 201 — metadata is optional

##### Department metadata (`category: maxLength 100`, `short_description: maxLength 500`)

#### TC-ADR-17: Department — category within limit accepted [P1]
- `metadata: {"category": "finance"}` (7 chars, ≤ 100)
- **Assert**: 201

#### TC-ADR-18: Department — category at boundary (100 chars) accepted [P1]
- `metadata: {"category": "X".repeat(100)}`
- **Assert**: 201

#### TC-ADR-19: Department — category over limit (101 chars) rejected [P1]
- `metadata: {"category": "X".repeat(101)}`
- **Assert**: 400 — maxLength: 100 violated

#### TC-ADR-20: Department — short_description within limit (500 chars) accepted [P1]
- `metadata: {"short_description": "X".repeat(500)}`
- **Assert**: 201

#### TC-ADR-21: Department — short_description over limit (501 chars) rejected [P1]
- `metadata: {"short_description": "X".repeat(501)}`
- **Assert**: 400 — maxLength: 500 violated

#### TC-ADR-22: Department — unknown field rejected [P1]
- `metadata: {"category": "hr", "short_description2": "typo"}`
- **Assert**: 400 — `short_description2` not in schema, `additionalProperties: false`

#### TC-ADR-23: Department — wrong value type for category (bool not string) [P1]
- `metadata: {"category": false}`
- **Assert**: 400

##### Branch metadata (`location: string`, no maxLength)

#### TC-ADR-24: Branch — valid location accepted [P1]
- `metadata: {"location": "Building A, Floor 3"}`
- **Assert**: 201

#### TC-ADR-25: Branch — unknown field rejected [P1]
- `metadata: {"location": "ok", "unknown_field": true}`
- **Assert**: 400

#### TC-ADR-26: Branch — location wrong type (number) rejected [P2]
- `metadata: {"location": 42}`
- **Assert**: 400

##### Cross-type metadata isolation

#### TC-ADR-27: Tenant metadata fields on department → rejected [P1]
- Create department group with `metadata: {"self_managed": true}`
- Department schema does NOT have `self_managed` → `additionalProperties: false` rejects
- **Assert**: 400

#### TC-ADR-28: Department metadata fields on tenant → rejected [P1]
- Create tenant group with `metadata: {"category": "finance"}`
- Tenant schema does NOT have `category` → rejected
- **Assert**: 400

##### Update metadata validation

#### TC-ADR-29: Update group metadata — same validation rules apply [P1]
- Create department with valid metadata `{"category": "hr"}`
- Update with `metadata: {"category": "X".repeat(101)}` → 400 (over limit)
- Update with `metadata: {"category": "finance"}` → 200 (valid)

#### TC-ADR-30: Type without additional metadata restrictions [P2]
- Publish a derived type that adds no restrictions to the base metadata payload
- Create group with `metadata: {"anything": "goes", "x": 42}`
- **Assert**: 201 — the admitted full schema permits this metadata object; validation still runs

#### TC-ADR-16: Chained type path format in RG [P1]
- ADR uses: `gts.cf.core.rg.type.v1~y.core.tn.tenant.v1~` (multi-segment)
- Code validates the RG family prefix: `gts.cf.core.rg.type.v1~`
- **Assert**: An admitted chained schema in that family is accepted; a foreign family is rejected.

#### TC-ADR-18: Group response contains no SMALLINT IDs [P1]
- Create group, GET → response JSON
- **Assert**: `type` field is string GTS path. No `gts_type_id`.

#### TC-ADR-19: Membership response contains no SMALLINT IDs [P1]
- Add membership, response JSON
- **Assert**: `resource_type` is string GTS path. No `gts_type_id`.

#### TC-ADR-20: Hierarchy response contains no SMALLINT IDs [P1]
- list_group_hierarchy → response items
- **Assert**: each item `type` is string, no numeric type IDs

### REST API Layer

**File**: `api_rest_test.rs` (extend existing)

#### TC-REST-03: Add membership POST returns 201 [P2]
- **Covers**: G35, 0004-AC-1
- **Setup**: Create type+group via service, POST membership
- **Assert**: 201 Created

#### TC-REST-04: Remove membership DELETE returns 204 [P2]
- **Covers**: G35, 0004-AC-8

#### TC-REST-05: List memberships GET returns 200 [P2]
- **Covers**: G35, 0004-AC-10

#### TC-REST-06: Create group with parent via REST [P2]
- **Covers**: G8
- POST with parent_id in body, verify 201 + hierarchy fields

#### TC-REST-07: Delete group with force=true via REST [P2]
- **Covers**: G20
- DELETE /groups/{id}?force=true, verify 204

#### TC-REST-08: Hierarchy endpoint via REST [P2]
- **Covers**: G21
- GET /groups/{id}/hierarchy, verify 200 + depth fields

#### TC-REST-10: Error response HTTP mapping — status codes and Content-Type [P1]
- **Covers**: RG3
- **Why**: `error.rs` maps `DomainError` variants to HTTP status + `Content-Type: application/problem+json`. No direct unit tests verify this mapping. `error_response_has_problem_fields` (existing) checks one 404 response, but does not verify all status codes or Content-Type header explicitly.
- **Setup**: Trigger multiple error paths via `Router::oneshot`:
  - GET /groups/{random-uuid} → 404 Not Found
  - POST /types {duplicate code} → 409 Conflict
  - POST /types {invalid body} → 400 Bad Request
  - POST /groups {type "gts.cf.core.rg.type.v1~nonexistent.v1~"} → 404 (TypeNotFound)
- **Assert per response**:
  - HTTP status code matches expected
  - `Content-Type` header contains `application/problem+json`
  - Response body has `status`, `title`, `detail` fields (RFC 9457)
  - No `stack`, `trace`, `backtrace` fields in response body (no internal leaks)

### Priority Matrix

#### P1 - Critical (must have, business invariants)

These test domain invariants that prevent data corruption or violate core business rules:

| ID | Test Case | Risk if Missing |
|----|-----------|-----------------|
| **TC-GRP-01** | Child group + closure rows | Hierarchy queries broken |
| **TC-GRP-02** | 3-level closure completeness | Ancestor/descendant queries wrong |
| **TC-GRP-03** | Incompatible parent type | Type system bypassed |
| **TC-GRP-04** | Root group when can_be_root=false | Type system bypassed |
| **TC-GRP-05** | Move with closure rebuild | Hierarchy corrupt after move |
| **TC-GRP-06** | Move under descendant -> cycle | Infinite loops in hierarchy |
| **TC-GRP-07** | Self-parent -> cycle | Infinite loops in hierarchy |
| **TC-GRP-08** | Move to incompatible type | Type system bypassed |
| **TC-GRP-12** | Leaf delete | Data cleanup |
| **TC-GRP-13** | Delete with children no force | Accidental data loss |
| **TC-GRP-14** | Delete with memberships no force | Accidental data loss |
| **TC-GRP-15** | Force delete subtree | Cascade completeness |
| **TC-GRP-16** | Hierarchy depth traversal | Core read feature |
| **TC-GRP-17** | max_depth on create | Profile guardrail |
| **TC-GRP-18** | max_width on create | Profile guardrail |
| **TC-MBR-01** | Add membership happy path | Core write feature |
| **TC-MBR-02** | Add to nonexistent group | Dangling membership |
| **TC-MBR-03** | Duplicate membership | Data integrity |
| **TC-MBR-05** | Not in allowed_memberships | Type system bypassed |
| **TC-MBR-06** | Tenant incompatibility | Cross-tenant data leak |
| **TC-MBR-13** | Empty allowed_memberships rejects all | Type system bypassed |
| **TC-MBR-14** | Same resource in multiple groups same tenant | Multi-group membership broken |
| **TC-GRP-22** | Create group nonexistent type | Group with invalid type |
| **TC-GRP-23** | Child group cross-tenant parent | Tenant isolation broken |
| **TC-GRP-29** | Move child to root (detach) | Closure corruption on detach |
| **TC-GRP-30** | Move to root when can_be_root=false | Unauthorized root creation |
| **TC-GRP-37** | Depth exact boundary (>=) | Off-by-one in guardrail |
| **TC-GRP-38** | Width exact boundary (>=) | Off-by-one in guardrail |
| **TC-SDK-24** | validate_type_code vs GtsTypePath mismatch | Silent validation inconsistency |
| **TC-SDK-01..06,08** | GtsTypePath value object validation | Invalid types accepted into system |
| **TC-SDK-11,12** | GtsTypePath serde round-trip | API deserialization breaks silently |
| **TC-SDK-15** | SDK group model `type` rename | Wire format mismatch |
| **TC-DTO-05** | CreateGroupDto `type` JSON rename | Request deserialization fails |
| **TC-ODATA-01,02** | GroupFilterField names + kinds | OData $filter silently broken |
| **TC-ODATA-04,05** | Hierarchy + Membership FilterField | OData $filter silently broken |
| **TC-ERR-01** | EnforcerError::AccessDenied -> AccessDenied | Auth errors mishandled |
| **TC-REST-10** | Error response HTTP mapping — status codes + Content-Type | Incorrect HTTP status codes or missing `application/problem+json` Content-Type |

#### P2 - Important (error paths, REST layer, edges)

| ID | Area |
|----|------|
| TC-GRP-09, TC-GRP-19..21, TC-GRP-24..25, TC-GRP-31..36 | Group update/validation/error paths |
| TC-MBR-07..12 | Membership edges + empty resource_id + unregistered type |
| TC-REST-03..08 | REST layer coverage |
| TC-SDK-07, TC-SDK-09, TC-SDK-10, TC-SDK-16..23 | SDK edge cases + boundary |
| TC-DTO-03..05, TC-DTO-07 | DTO conversion |
| TC-ODATA-03, TC-ODATA-07..08 | OData mapper correctness |
| TC-ERR-02..05 | Error conversion chains |
| TC-GRP-33 | max_width on move |

#### P3 - Nice to have (boundary, cosmetic)

| ID | Test Case |
|----|-----------|
| TC-SDK-13 | GtsTypePath Display + Into<String> |
| TC-MBR-15 | List memberships empty result |

### Acceptance Criteria

- [x] The current unit suites pass (`cargo test -p cf-gears-resource-group -p cf-gears-resource-group-sdk`); no legacy test count is assumed after the registry migration
- [x] Full suite completes in < 5 seconds
- [x] Zero `sleep`, `timeout`, or `tokio::time` usage in tests
- [x] Every domain invariant from features 0001-0005 is covered by at least one test
- [x] `make fmt && make lint && make test` passes with zero errors

#### Remaining Gaps (see section 2.2 — RG2..RG7)

- [x] ~~RG2~~: All OData mapper field→column tests added (4 tests)
- [x] ~~RG3~~: Error HTTP status + Content-Type tests added (TC-REST-10)
- [x] ~~RG4~~: DTO serialization covered by 7 inline tests + 2 REST null-omission tests
- [x] ~~RG5~~: Accepted — indirect coverage via 127 service-level tests is sufficient
- [x] ~~RG6~~: Accepted — gear init is integration-level, covered by E2E (feature 0005, S3)
- [x] ~~RG7~~: Route smoke test added — all 14 endpoints verified non-405

### Closure Table Assertions (`resource_group_closure`)

Every test that creates, moves, or deletes groups **MUST** query the closure table directly and verify:

| Operation | Required DB Assertions |
|-----------|----------------------|
| **Create root group** | Self-row exists `(ancestor=id, descendant=id, depth=0)`. Total closure rows for this group = **1**. |
| **Create child group** | Self-row `(depth=0)` + ancestor rows for every ancestor with correct depth. Total = `depth_in_tree + 1`. |
| **Create 3-level tree** | Total closure rows = **6** (1+2+3). Each depth value verified. |
| **Move subtree** | Old ancestor paths **deleted** (COUNT=0 for old_root→moved_nodes). New ancestor paths created with correct depths. **Internal subtree paths preserved** (child→grandchild depth unchanged). Self-rows untouched. Nodes outside moved subtree **unaffected**. |
| **Move child to root** | All ancestor rows removed. Only self-row remains (COUNT=1). |
| **Delete leaf** | All closure rows WHERE `descendant_id=id OR ancestor_id=id` → **0 rows**. Parent's closure rows **untouched**. |
| **Force delete subtree** | All closure rows for all nodes in subtree → **0**. Nodes outside subtree **unaffected**. |

**Helper function**: `assert_closure_rows(conn, group_id, expected: &[(Uuid, i32)])` — verifies exact set of (ancestor_id, depth) pairs for a given descendant.

### Entity State Assertions (`resource_group` table)

| Operation | Required DB Assertions |
|-----------|----------------------|
| **Create group** | `parent_id`, `gts_type_id`, `tenant_id`, `name`, `metadata` match request. |
| **Update name/metadata** | `name` and `metadata` changed. `parent_id`, `gts_type_id` **unchanged**. |
| **Move group** | `parent_id` updated. `gts_type_id`, `name`, `tenant_id` **unchanged**. |

### Hierarchy Endpoint Response Shape

`list_group_hierarchy(B)` for tree A → B → C **MUST** return:
- Self-node B: `hierarchy.depth == 0`
- Ancestor A: `hierarchy.depth < 0` (e.g., -1)
- Descendant C: `hierarchy.depth > 0` (e.g., 1)
- **All nodes present** (no missing nodes)
- Each node has `hierarchy.tenant_id`, `hierarchy.parent_id`

### Test Infrastructure

#### Shared Test Helpers (`tests/common/mod.rs`)

Extract duplicated setup code from existing tests:

```rust
// tests/common/mod.rs

/// SQLite in-memory DB with migrations. ~1ms per call.
pub async fn test_db() -> Arc<DBProvider<DbError>> { ... }

/// SecurityContext for given tenant.
pub fn make_ctx(tenant_id: Uuid) -> SecurityContext { ... }

/// AllowAll PolicyEnforcer (returns tenant-scoped AccessScope).
pub fn make_enforcer() -> PolicyEnforcer { ... }

/// Create root type with unique code suffix. One DB round-trip.
pub async fn create_root_type(svc: &TestTypes, suffix: &str) -> ResourceGroupType { ... }

/// Create child type referencing parent codes. One DB round-trip.
pub async fn create_child_type(
    svc: &TestTypes, suffix: &str, parents: &[&str], memberships: &[&str]
) -> ResourceGroupType { ... }

/// Create root group. Returns ResourceGroup.
pub async fn create_root_group(
    svc: &GroupService, ctx: &SecurityContext, type_code: &str, name: &str, tenant_id: Uuid
) -> ResourceGroup { ... }

/// Create child group under parent.
pub async fn create_child_group(
    svc: &GroupService, ctx: &SecurityContext, type_code: &str, parent_id: Uuid, name: &str, tenant_id: Uuid
) -> ResourceGroup { ... }

/// Assert exact closure rows for a descendant. Panics with diff on mismatch.
pub async fn assert_closure_rows(
    conn: &impl DBRunner, descendant_id: Uuid, expected: &[(Uuid, i32)]  // (ancestor_id, depth)
) { ... }

/// Assert total closure row count for a set of groups (no extra rows left).
pub async fn assert_closure_count(conn: &impl DBRunner, group_ids: &[Uuid], expected_total: usize) { ... }

/// Assert junction table rows for a type.
pub async fn assert_allowed_parents(conn: &impl DBRunner, type_id: i16, expected_parent_ids: &[i16]) { ... }

/// Assert no SMALLINT IDs in JSON value (recursive check).
pub fn assert_no_surrogate_ids(json: &serde_json::Value) { ... }
```

#### Test File Organization

```
# In-source unit tests (pure logic, no DB, #[test] only — instant)
resource-group-sdk/src/models.rs              # ✅ DONE (10 tests) TC-SDK-01..24
resource-group-sdk/src/odata/groups.rs        # ✅ DONE (3 tests) TC-ODATA-01..03
resource-group-sdk/src/odata/hierarchy.rs     # ✅ DONE (1 test) TC-ODATA-04
resource-group-sdk/src/odata/memberships.rs   # ✅ DONE (1 test) TC-ODATA-05
resource-group/src/api/rest/auth.rs           # ✅ DONE (existing inline tests)
resource-group/src/api/rest/dto.rs            # ✅ DONE (inline tests)
resource-group/src/infra/storage/odata_mapper.rs  # ✅ DONE (4 tests: Type/Group/Hierarchy/Membership mappers)

# Integration tests (SQLite :memory: DB, #[tokio::test])
tests/
  common/mod.rs                  # ✅ DONE — shared helpers + assertion helpers
  domain_unit_test.rs            # ✅ DONE (79 tests) — includes TC-ERR-01..05, TC-ADR-01..08, TC-NOGTS-*, TC-GTS-*
  api_rest_test.rs               # ✅ DONE (54 tests) — includes error Content-Type (TC-REST-10), route smoke (RG7)
  authz_integration_test.rs      # ✅ DONE (9 tests)
  tenant_filtering_db_test.rs    # ✅ DONE (7 tests)
  tenant_scoping_test.rs         # ✅ DONE (10 tests)
  group_service_test.rs          # ✅ DONE (55 tests) retained TC-GRP cases
  membership_service_test.rs     # ✅ DONE (15 tests) TC-MBR-01..15
```

---

## 8. E2E Test Plan

> General E2E testing philosophy, patterns, and infrastructure: [`docs/toolkit_unified_system/13_e2e_testing.md`](../../../../../docs/toolkit_unified_system/13_e2e_testing.md).

Tests S5, S6, S7 verify PostgreSQL-specific behavior that unit tests cannot catch because 0006 runs on SQLite where FK enforcement requires `PRAGMA foreign_keys = ON` and closure SQL may silently produce wrong results due to looser type coercion.

### S5: `test_hierarchy_closure`

**Seam**: Closure table INSERT SQL under PostgreSQL.

**Why not in unit tests**: TC-GRP-01/02 verify closure rows on SQLite. PostgreSQL uses `INSERT INTO resource_group_closure SELECT ...` with joins against existing closure rows — column order or join condition errors produce wrong depth values silently on SQLite but fail or corrupt data on PostgreSQL.

```
POST parent_type (can_be_root), child_type (allowed_parents: [parent])
POST root → child → grandchild

GET /groups/{root.id}/hierarchy     → 200
  assert len(items) == 3
  assert root       depth == 0
  assert child      depth == 1
  assert grandchild depth == 2

GET /groups/{child.id}/hierarchy    → 200
  assert len(items) == 2
  assert child      depth == 0     (relative to query root)
  assert grandchild depth == 1
```

### S6: `test_move_closure_rebuild`

**Seam**: Closure table DELETE + re-INSERT under PostgreSQL SERIALIZABLE transaction.

**Why not in unit tests**: TC-GRP-05 covers move logic on SQLite. On PostgreSQL, `DELETE FROM resource_group_closure` followed by `INSERT INTO ... SELECT` new paths runs under SERIALIZABLE isolation — incorrect transaction handling can cause inconsistent closure state visible to concurrent reads.

```
POST root_A → child
POST root_B

PUT /groups/{child.id} {parent_id: root_B.id}    → 200

GET /groups/{root_B.id}/hierarchy    → 200
  assert child in items with depth == 1            (closure rebuilt on PG)
```

### S7: `test_force_delete_cascade`

**Seam**: FK `ON DELETE RESTRICT` + service-level cascade on PostgreSQL.

**Why not in unit tests**: TC-GRP-15 covers force delete on SQLite where FK enforcement is off by default. PostgreSQL enforces `ON DELETE RESTRICT` on `resource_group.parent_id` and `resource_group_membership.group_id` — wrong deletion order passes SQLite but raises a constraint violation on PostgreSQL.

```
POST root → child

DELETE /groups/{root.id}?force=true              → 204

GET /groups/{root.id}                            → 404
```

### Acceptance Criteria (S5, S6, S7)

- [x] S5 verifies `depth` values on real PostgreSQL — not just "hierarchy returns items"
- [x] S6 verifies child appears under new parent with correct depth after PG SERIALIZABLE move
- [x] S7 verifies 204 + target returns 404 — PG FK cascade succeeds in correct deletion order

### Registry Migration Verification

The cases above use admitted type declarations and immutable GTS identifiers.
The removed local type CRUD and seed-service cases are replaced by the managed
admission, consumer provisioning and migration coverage in feature 0002. The
local SQLite E2E harness does not discharge PostgreSQL-specific claims; dedicated
backend suites verify those behaviors.
