<!-- Created: 2026-04-07 by Constructor Tech -->

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
  - [Group Data Seeding](#group-data-seeding)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Entity Service](#entity-service)
  - [Hierarchy Engine](#hierarchy-engine)
  - [Group REST Handlers and Hierarchy Endpoint](#group-rest-handlers-and-hierarchy-endpoint)
  - [Group Data Seeding](#group-data-seeding-1)
  - [Unit Test Coverage for Entity Hierarchy](#unit-test-coverage-for-entity-hierarchy)
  - [REST API Test Coverage](#rest-api-test-coverage)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
  - [Module-Level Summary](#module-level-summary)
  - [Coverage Summary](#coverage-summary)
  - [Entity Hierarchy Test Cases](#entity-hierarchy-test-cases)
  - [Group `metadata` — Barrier as Data](#group-metadata--barrier-as-data)
  - [REST-level metadata serialization](#rest-level-metadata-serialization)
  - [Invalid / Non-GTS Input Tests](#invalid--non-gts-input-tests)
  - [ADR-001 Hierarchy Reproduction in RG Module](#adr-001-hierarchy-reproduction-in-rg-module)
  - [metadata Validation Against Type's metadata_schema](#metadata-validation-against-types-metadata_schema)
  - [REST API Layer](#rest-api-layer)
  - [Priority Matrix](#priority-matrix)
  - [Acceptance Criteria](#acceptance-criteria)
  - [Closure Table Assertions (`resource_group_closure`)](#closure-table-assertions-resource_group_closure)
  - [Entity State Assertions (`resource_group` table)](#entity-state-assertions-resource_group-table)
  - [Hierarchy Endpoint Response Shape](#hierarchy-endpoint-response-shape)
  - [Seeding DB Verification](#seeding-db-verification)
  - [Test Infrastructure](#test-infrastructure)
- [8. E2E Test Plan](#8-e2e-test-plan)
  - [S5: `test_hierarchy_closure_postgresql`](#s5-test_hierarchy_closure_postgresql)
  - [S6: `test_move_closure_rebuild_postgresql`](#s6-test_move_closure_rebuild_postgresql)
  - [S7: `test_force_delete_cascade_postgresql`](#s7-test_force_delete_cascade_postgresql)
  - [Acceptance Criteria (S5, S6, S7)](#acceptance-criteria-s5-s6-s7)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Group entity lifecycle (create, get, update, move, delete) with strict forest invariants (single parent, no cycles), closure-table-based hierarchy engine for efficient ancestor/descendant queries, query profile enforcement (`max_depth`/`max_width`), subtree move/delete operations, hierarchy depth endpoint with relative depth, force delete, and group data seeding.

### 1.2 Purpose

Groups are the core nodes of the resource group hierarchy. This feature implements the entity service that enforces structural invariants, the hierarchy engine that maintains the closure table projection for efficient graph queries, and the query profile guardrails that bound hierarchy depth and width.

**Requirements**: `cpt-cf-resource-group-fr-manage-entities`, `cpt-cf-resource-group-fr-enforce-forest-hierarchy`, `cpt-cf-resource-group-fr-validate-parent-type`, `cpt-cf-resource-group-fr-delete-entity-no-active-references`, `cpt-cf-resource-group-fr-seed-groups`, `cpt-cf-resource-group-fr-closure-table`, `cpt-cf-resource-group-fr-query-group-hierarchy`, `cpt-cf-resource-group-fr-subtree-operations`, `cpt-cf-resource-group-fr-query-profile`, `cpt-cf-resource-group-fr-profile-change-no-rewrite`, `cpt-cf-resource-group-fr-reduced-constraints-behavior`, `cpt-cf-resource-group-fr-list-groups-depth`, `cpt-cf-resource-group-fr-force-delete`, `cpt-cf-resource-group-nfr-hierarchy-query-latency`

**Principles**: `cpt-cf-resource-group-principle-strict-forest`, `cpt-cf-resource-group-principle-query-profile-guardrail`

**Constraints**: `cpt-cf-resource-group-constraint-profile-change-safety`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-resource-group-actor-instance-administrator` | Manages group hierarchy via REST API, operates group seeding |
| `cpt-cf-resource-group-actor-tenant-administrator` | Manages groups within tenant scope via REST API |
| `cpt-cf-resource-group-actor-apps` | Programmatic group management via `ResourceGroupClient` SDK |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — sections 5.2, 5.4, 5.5, 5.6, 8.2
- **Design**: [DESIGN.md](../DESIGN.md) — sections 3.1, 3.2 (Entity Service, Hierarchy Service), 3.6 (sequences), 3.7 (resource_group, resource_group_closure), 3.8 (Query Profile)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.3
- **Dependencies**: Feature 0002 — type validation for parent-child compatibility
- **Not applicable**: UX (backend API — no user interface); COMPL (internal platform module — no regulatory data handling); OPS observability and rollout are managed at the module infrastructure level (DESIGN §3.7 and platform runbooks); PERF targets are set at the system level in PRD.md NFR section.

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
- Depth or width limit exceeded → LimitViolation

**Steps**:
1. [x] - `p1` - Actor sends POST /api/resource-group/v1/groups with {type, name, metadata, hierarchy: {parent_id, tenant_id}} - `inst-create-group-1`
2. [x] - `p1` - DB: BEGIN transaction (SERIALIZABLE isolation) - `inst-create-group-2`
3. [x] - `p1` - Resolve type GTS path to surrogate ID; verify type exists - `inst-create-group-3`
4. [x] - `p1` - **IF** parent_id is provided - `inst-create-group-4`
   1. [x] - `p1` - DB: SELECT id, gts_type_id, tenant_id FROM resource_group WHERE id = {parent_id} — load parent in tx - `inst-create-group-4a`
   2. [x] - `p1` - **IF** parent not found → **RETURN** NotFound - `inst-create-group-4b`
   3. [x] - `p1` - Validate child type's allowed_parents includes parent's type - `inst-create-group-4c`
   4. [x] - `p1` - **IF** type incompatible → **RETURN** InvalidParentType - `inst-create-group-4d`
   5. [x] - `p1` - Invoke query profile enforcement: check depth limit - `inst-create-group-4e`
   6. [x] - `p1` - Invoke query profile enforcement: check width limit (sibling count under parent) - `inst-create-group-4f`
5. [x] - `p1` - **ELSE** (root group) - `inst-create-group-5`
   1. [x] - `p1` - **IF** type does not allow root placement (can_be_root=false) → **RETURN** Validation error - `inst-create-group-5a`
6. [x] - `p1` - **IF** metadata provided AND type has metadata_schema → validate metadata against the chained GTS type schema via `TypesRegistryClient` (types-registry-sdk, already in workspace). The `gts` crate (v0.8.4) validates metadata fields against the inline `metadata` sub-schema defined in the chained RG type (`additionalProperties: false`, field types, `maxLength`). **IF** invalid → **RETURN** Validation error with field-level details - `inst-create-group-5b`
7. [x] - `p1` - DB: INSERT INTO resource_group (id, parent_id, gts_type_id, name, metadata, tenant_id) - `inst-create-group-6`
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
- Group name, metadata, or type updated successfully
- On type change: children's types validated against new type

**Error Scenarios**:
- Group not found → NotFound
- New type's allowed_parents does not include current parent → InvalidParentType
- Children's types do not include new type in their allowed_parents → InvalidParentType

**Steps**:
1. [x] - `p1` - Actor sends PUT /api/resource-group/v1/groups/{group_id} with {name, type, metadata} - `inst-update-group-1`
2. [x] - `p1` - DB: SELECT FROM resource_group WHERE id = {group_id} — load existing group - `inst-update-group-2`
3. [x] - `p1` - **IF** group not found → **RETURN** NotFound - `inst-update-group-3`
4. [x] - `p1` - **IF** type is changed - `inst-update-group-4`
   1. [x] - `p1` - Validate new type's allowed_parents permits current parent's type (or new type allows root if no parent) - `inst-update-group-4a`
   2. [x] - `p1` - DB: SELECT gts_type_id FROM resource_group WHERE parent_id = {group_id} — load children types - `inst-update-group-4b`
   3. [x] - `p1` - **FOR EACH** child: verify child's type includes new type in allowed_parents - `inst-update-group-4c`
   4. [x] - `p1` - **IF** any child would become invalid → **RETURN** InvalidParentType with child details - `inst-update-group-4d`
5. [x] - `p1` - **IF** metadata provided AND type has metadata_schema → validate metadata against the chained GTS type schema via `TypesRegistryClient` / `gts` crate. **IF** invalid → **RETURN** Validation error - `inst-update-group-4e`
6. [x] - `p1` - DB: UPDATE resource_group SET name, gts_type_id, metadata, updated_at — apply changes - `inst-update-group-5`
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

### Group Data Seeding

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-entity-hier-seed-groups`

**Input**: List of group seed definitions with parent references and type assignments

**Output**: Seed result (groups created, updated, unchanged count)

**Steps**:
1. [x] - `p1` - Load seed definitions, order by dependency (parents before children) - `inst-seed-groups-1`
2. [x] - `p1` - **FOR EACH** seed_def in ordered definitions - `inst-seed-groups-2`
   1. [x] - `p1` - DB: SELECT FROM resource_group WHERE id = {seed_def.id} or name/type match - `inst-seed-groups-2a`
   2. [x] - `p1` - **IF** group exists AND definition matches → skip (unchanged) - `inst-seed-groups-2b`
   3. [x] - `p1` - **IF** group exists AND definition differs → update via update flow - `inst-seed-groups-2c`
   4. [x] - `p1` - **IF** group does not exist → create via create flow (validates type compat, builds closure) - `inst-seed-groups-2d`
3. [x] - `p1` - **RETURN** seed result: {created: N, updated: N, unchanged: N} - `inst-seed-groups-3`

## 4. States (CDSL)

Not applicable. Groups exist as hierarchy nodes without lifecycle states. A group is either present in the hierarchy or deleted. Structural integrity (parent-child relationships, closure projections) is managed by the entity service and hierarchy engine, not by state machine transitions.

## 5. Definitions of Done

### Entity Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-entity-hier-entity-service`

The system **MUST** implement an Entity Service that provides create, get, update, move, and delete operations for group entities with forest invariant enforcement.

**Required behavior**:
- Create: validate type, parent compatibility, profile limits; persist entity + closure rows in SERIALIZABLE tx
- Get: retrieve by UUID; return NotFound if absent
- Update: validate type change against parent and children compatibility; update mutable fields
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
- `GET /groups` — list groups with OData `$filter` (fields: `type`, `hierarchy/parent_id`, `id`, `name`; operators: `eq`, `ne`, `in`) and cursor-based pagination
- `POST /groups` — create group, return 201 Created
- `GET /groups/{group_id}` — get group by UUID, return 404 if not found
- `PUT /groups/{group_id}` — update group (name, type, metadata) or move group (hierarchy.parent_id), return 200 OK
- `DELETE /groups/{group_id}?force={true|false}` — delete group, return 204 No Content
- `GET /groups/{group_id}/hierarchy` — hierarchy depth traversal with OData `$filter` on `hierarchy/depth` and `type`, cursor-based pagination

**Implements**:
- `cpt-cf-resource-group-flow-entity-hier-create-group`
- `cpt-cf-resource-group-flow-entity-hier-update-group`
- `cpt-cf-resource-group-flow-entity-hier-move-group`
- `cpt-cf-resource-group-flow-entity-hier-delete-group`

**Touches**:
- API: `GET/POST /api/resource-group/v1/groups`, `GET/PUT/DELETE /api/resource-group/v1/groups/{group_id}`, `GET /api/resource-group/v1/groups/{group_id}/hierarchy`

### Group Data Seeding

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-entity-hier-seeding`

The system **MUST** provide an idempotent group seeding mechanism for deployment bootstrapping.

**Required behavior**:
- Accept ordered list of group seed definitions (parents before children)
- For each seed: create if missing, update if definition differs, skip if unchanged
- Validate parent-child links, type compatibility, and profile limits during seeding
- Seeding runs as a pre-deployment step with system SecurityContext
- Repeated runs produce the same result (idempotent)

**Implements**:
- `cpt-cf-resource-group-algo-entity-hier-seed-groups`

**Touches**:
- DB: `resource_group`, `resource_group_closure`

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
- PUT /types/{code} (update type)
- POST/DELETE /memberships/{group_id}/{type}/{resource_id}
- GET /groups/{id}/hierarchy
- DELETE /groups/{id}?force=true

## 6. Acceptance Criteria

- [x] Root group (can_be_root=true, no parent) is created with self-referencing closure row (depth=0)
- [x] Child group is created with closure rows linking to all ancestors at correct depths
- [x] Creating group with parent of incompatible type returns `InvalidParentType` (409)
- [x] Creating group with nonexistent parent returns `NotFound` (404)
- [x] Creating root group when type has can_be_root=false returns validation error (400)
- [x] Moving group to new parent rebuilds closure paths transactionally for entire subtree
- [x] Moving group under its own descendant returns `CycleDetected` (409)
- [x] Moving group under itself (self-parent) returns `CycleDetected` (409)
- [x] Moving group to incompatible parent type returns `InvalidParentType` (409)
- [x] Updating group type validates both parent and children compatibility
- [x] Deleting leaf group (no children, no memberships) succeeds (204) and removes closure rows
- [x] Deleting group with children without force returns `ConflictActiveReferences` (409) with response body listing blocking entities (children count/IDs and membership count) so the caller can display what prevents deletion
- [x] Force delete removes entire subtree including memberships and closure rows
- [x] Hierarchy endpoint returns ancestors (negative depth) and descendants (positive depth) with correct relative distances
- [x] OData `$filter` on `hierarchy/depth` supports eq, ne, gt, ge, lt, le operators
- [x] Write operations that exceed max_depth are rejected with `DepthLimitExceeded`
- [x] Write operations that exceed max_width are rejected with `WidthLimitExceeded`
- [x] Reads return full stored data even when profile was tightened (no truncation)
- [x] Concurrent hierarchy mutations use SERIALIZABLE isolation with bounded retry
- [x] Group seeding creates hierarchy with correct parent-child links and closure rows (idempotent)
- [x] Creating group with metadata that violates type's metadata_schema returns Validation error (400) — field type mismatch, maxLength exceeded, unknown field (additionalProperties:false)
- [x] Creating group with valid metadata matching type's metadata_schema succeeds
- [x] Updating group metadata validates against type's metadata_schema
- [x] Creating group when type has no metadata_schema accepts any metadata (no validation)
---

## 7. Unit Test Plan

> General testing philosophy, patterns, and infrastructure: [`docs/modkit_unified_system/12_unit_testing.md`](../../../../../docs/modkit_unified_system/12_unit_testing.md).

### Module-Level Summary

~325 tests (308 in `cf-resource-group` + 17 in `cf-resource-group-sdk`). Fast (< 5s total). Zero sleeps. Every test atomic.

Unit tests guard **deterministic domain logic** — the same logic that runs identically regardless of whether it's called via HTTP or directly in Rust. If a test needs a real PostgreSQL or a real HTTP connection, it belongs in Feature 0007 (E2E), not here.

This feature covers the unit and integration test plan for the `resource-group` module. The current test suite contains 318 tests (302 in `cf-resource-group` across 10 test files + inline `#[cfg(test)]` modules, 16 in `cf-resource-group-sdk`) totaling ~9,400 lines of test code. All tests pass with 0 failures.

The plan was originally based on a gap analysis against acceptance criteria defined in features 0001-0005 and ADR-001 (GTS Type System). The analysis incorporates:
- Acceptance criteria from features 0001-0005 and ADR-001
- Testing patterns from other project modules (`nodes-registry`, `types-registry`, `api-gateway`)
- ADR-001 metadata validation requirements (`additionalProperties: false`, field types, maxLength constraints)

**Scope**: Domain service tests with SQLite in-memory, in-source `#[cfg(test)]` for pure logic, metadata validation against `metadata_schema`.

**Out of scope**: E2E tests (Feature 0007), PostgreSQL-specific tests, MTLS, performance.

### Coverage Summary

#### What IS Covered

**cf-resource-group** (302 tests):

| File | Tests | Covers |
|------|-------|--------|
| In-source `#[cfg(test)]` (lib.rs) | 23 | Inline tests in `auth.rs` (MTLS/JWT mode routing, path matching), `dto.rs` (DTO serde attributes, `type` rename, camelCase), `odata_mapper.rs` (Type/Group/Hierarchy/Membership ODataMapper field→column) |
| `api_rest_test.rs` | 54 | Type CRUD REST (create 201, dup 409, invalid 400, list 200, get 200/404, delete 204), Group REST (create/list/get/update/delete/hierarchy), Membership REST (add/remove/list), RFC 9457 error format + Content-Type verification (TC-REST-10), deserialization errors, SMALLINT non-exposure, metadata in REST responses, route smoke all 14 endpoints (RG7) |
| `authz_integration_test.rs` | 9 | PolicyEnforcer tenant scoping, deny-all, allow-all, resource_id passing, all CRUD actions, full chain list_groups/deny |
| `domain_unit_test.rs` | 79 | `validate_type_code` (5 cases), `DomainError` construction (13 variants), `DomainError` → `ResourceGroupError` mapping, `DomainError` → `Problem` mapping, serialization failure detection, `EnforcerError` → `DomainError` conversions, `DbErr` → `DomainError`, ADR-001 hierarchy reproduction, GTS-specific logic, invalid/non-GTS input validation |
| `group_service_test.rs` | 55 | TC-GRP-01..38: child creation + closure rows, 3-level hierarchy, incompatible parent type, can_be_root enforcement, move with closure rebuild, cycle detection, self-parent, type change validation, leaf/force delete, hierarchy depth traversal, max_depth/max_width enforcement, name validation, cross-tenant parent, simultaneous type+parent change, detach to root, metadata barrier tests |
| `type_service_test.rs` | 45 | TC-TYP-01..16 + TC-META-01..11 + TC-META-ATK-01..11 + TC-GTS-01..15: create/update/delete types, placement invariant, hierarchy safety checks, metadata_schema round-trip (Object/Array/String/Number), `can_be_root` resolution via TypesRegistry, internal key stripping, security attack vectors (privilege escalation, DoS, SQL injection), resolve_id/resolve_ids, allowed_parents/memberships resolution |
| `membership_service_test.rs` | 15 | TC-MBR-01..15: add/remove membership, nonexistent group, duplicate, unregistered type, not in allowed_memberships, tenant incompatibility, multiple resource types, first-always-allowed tenant, empty resource_id |
| `seeding_test.rs` | 12 | TC-SEED-01..12: seed_types (create/skip/update/idempotent), seed_groups (create with closure/skip/wrong order), seed_memberships (create/duplicate skip/tenant-incompatible skip/nonexistent group), empty list |
| `tenant_filtering_db_test.rs` | 7 | Tenant isolation (list/get/hierarchy/update/delete cross-tenant), InGroup predicate, membership data storage |
| `tenant_scoping_test.rs` | 10 | AccessScope construction (for_tenant, for_tenants, allow_all, deny_all, tenant_only, for_resource) |

**cf-resource-group-sdk** (16 tests):

| File | Tests | Covers |
|------|-------|--------|
| `models.rs` (in-source) | 10 | TC-SDK-01..24: `GtsTypePath::new()` validation (valid/invalid/boundary), trim+lowercase normalization, serde round-trip (JSON serialize/deserialize, invalid rejection), `Display`+`Into<String>`, SDK model camelCase serialization, `type` field rename, optional field omission, `QueryProfile::default()` |
| `odata/groups.rs` (in-source) | 3 | TC-ODATA-01..03: `GroupFilterField` names, kinds, `FIELDS` completeness |
| `odata/hierarchy.rs` (in-source) | 1 | TC-ODATA-04: `HierarchyFilterField` names and kinds |
| `odata/memberships.rs` (in-source) | 1 | TC-ODATA-05: `MembershipFilterField` names and kinds |

**Total**: ~9,700 lines of test code, 325 tests (all passing), 0 failed

#### What IS NOT Covered (Gaps)

> **Status as of 2026-03-29**: Gap analysis updated after full test suite run (318 tests passing). Most original gaps (G1-G52) have been closed. Remaining open gaps and newly discovered gaps are listed below.

**Original gaps (G1-G52) — status:**

| # | Area | Gap | Status |
|---|------|-----|--------|
| G1-G7 | Type Management | CRUD, safety checks, placement invariant, delete | ✅ **CLOSED** — 45 tests in `type_service_test.rs` |
| G8-G24 | Group Hierarchy | Create/move/delete, closure, cycles, query profile, type compat | ✅ **CLOSED** — 55 tests in `group_service_test.rs` |
| G25-G33 | Membership | Add/remove, validation, tenant compat, duplicates | ✅ **CLOSED** — 15 tests in `membership_service_test.rs` |
| G34-G35 | REST Layer | Update type, membership endpoints | ✅ **CLOSED** — 53 tests in `api_rest_test.rs` (PUT type, POST/DELETE membership, hierarchy, force delete, metadata, SMALLINT non-exposure, deserialization errors, GTS tilde encoding, error Content-Type) |
| G36-G38 | SDK Value Object | `GtsTypePath` validation, serde, format matching | ✅ **CLOSED** — 10 tests in `models.rs` (in-source) |
| G39-G41 | DTO & Serde | `From` impls, `type` rename, camelCase, skip_serializing_if | ✅ **CLOSED** — inline tests in `dto.rs` |
| G42-G44 | OData Filter Fields | GroupFilterField, HierarchyFilterField, MembershipFilterField | ✅ **CLOSED** — 5 tests in SDK `odata/*.rs` (in-source) |
| G45 | OData Mapper | Field-to-column mapping for all mappers | ✅ **CLOSED** — 4 tests in `odata_mapper.rs`: Type (TC-ODATA-06), Group (TC-ODATA-07), Hierarchy (TC-ODATA-08a), Membership (TC-ODATA-08) |
| G46-G48 | Seeding | seed_types, seed_groups, seed_memberships | ✅ **CLOSED** — 12 tests in `seeding_test.rs` |
| G49-G51 | Error Chains | EnforcerError/DbErr → DomainError | ✅ **CLOSED** — covered in `domain_unit_test.rs` (79 tests) |
| G52 | QueryProfile | `QueryProfile::default()` values | ✅ **CLOSED** — tested in SDK `models.rs` |

**Remaining open gaps (RG):**

| # | Priority | Area | Gap | Impact |
|---|----------|------|-----|--------|
| ~~RG2~~ | ~~CRITICAL~~ | ~~OData Mappers~~ | ✅ **CLOSED** — TC-ODATA-07 (Group), TC-ODATA-08a (Hierarchy), TC-ODATA-08 (Membership) all implemented in `odata_mapper.rs` | |
| ~~RG3~~ | ~~CRITICAL~~ | ~~REST: Error Mapping~~ | ✅ **CLOSED** — TC-REST-10 added to `api_rest_test.rs`: verifies Content-Type `application/problem+json` and correct status codes (404, 409, 400) + no internal leak fields | |
| ~~RG4~~ | ~~HIGH~~ | ~~REST: Response DTO Serialization~~ | ✅ **CLOSED** — 7 inline DTO tests (TC-DTO-01..07) in `dto.rs` cover all `From` impls, serde attributes, and field conversion. 2 REST tests (`rest_group_response_omits_null_metadata`, `rest_type_response_omits_null_metadata_schema`) verify `skip_serializing_if` null omission at HTTP level. | |
| ~~RG5~~ | ~~HIGH~~ | ~~Infrastructure: Repositories~~ | **ACCEPTED** — repository logic is thoroughly covered indirect via 55 group service, 45 type service, 15 membership service, and 12 seeding tests that exercise every repo method through real SQLite DB. Direct repo unit tests would duplicate this coverage without additional value. | |
| ~~RG6~~ | ~~MEDIUM~~ | ~~Module Init~~ | **ACCEPTED** — `module.rs` initialization is integration-level wiring (OnceLock, ClientHub, capability registration) that cannot be meaningfully tested without a full server bootstrap. Covered by E2E tests (0007 S1-S10) which exercise the real initialized module. | |
| ~~RG7~~ | ~~MEDIUM~~ | ~~REST: Route Registration~~ | ✅ **CLOSED** — `rest_route_smoke_all_endpoints_registered` test verifies all 14 endpoints (5 type + 6 group + 3 membership) respond non-405 via `Router::oneshot`. | |

### Entity Hierarchy Test Cases

**File**: `group_service_test.rs`

Test setup: SQLite in-memory + TypeService + GroupService with configurable QueryProfile.

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

#### TC-GRP-10: Update group type - validates parent compatibility [P1]
- **Covers**: G16, 0003-AC-10
- **Setup**: Type A (root), Type B (allowed_parents=[A]), Type C (root, no allowed_parents). Create group of type B under group of type A. Change group type to C (which doesn't allow parent A).
- **Assert**: `DomainError::InvalidParentType` ("does not allow current parent type")

#### TC-GRP-11: Update group type - validates children compatibility [P1]
- **Covers**: G16, 0003-AC-10
- **Setup**: Type P (root), Type C (allowed_parents=[P]), Type P2 (root). Create P group with C child. Change P group to type P2.
- **Assert**: `DomainError::InvalidParentType` ("child group... does not allow... as parent type")

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
- Create with `metadata: Some(json!({"barrier": true}))`, verify stored and returned

#### TC-GRP-25: Multiple root groups of same type [P2]
- Create 2 root groups of same can_be_root=true type, both succeed

#### TC-GRP-26: Update group - simultaneous type change AND parent change [P1]
- Both `type_changed` and `parent_changed` are true → type validation + move logic both run
- Verify the combined operation succeeds or fails atomically

#### TC-GRP-27: Update root group type to non-root type (no parent) [P1]
- Root group (parent_id=None), change type to can_be_root=false type
- Hits `else if !rg_type.can_be_root` branch (line 508-512)
- **Assert**: `DomainError::InvalidParentType("cannot be a root group")`

#### TC-GRP-28: Update group with nonexistent new type_path [P2]
- **Assert**: `DomainError::TypeNotFound`

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
- Create group with `metadata: Some(json!({"barrier": true}))`
- Get group → `metadata.barrier == true`
- **Covers**: PRD 3.4, Feature 0005-AC "barrier as data"
- **DB assert**: `resource_group.metadata` JSONB column contains `{"barrier": true}`

#### TC-META-13: Group with rich metadata — multiple fields [P1]
- `metadata: Some(json!({"barrier": true, "label": "Partner", "category": "premium"}))`
- **Assert**: all fields preserved in round-trip

#### TC-META-14: Group metadata update replaces entirely (not merge) [P1]
- Create group with `metadata: {"a": 1, "b": 2}`, update with `metadata: {"c": 3}`
- **Assert**: `metadata == {"c": 3}`, old keys gone
- **DB assert**: confirm in `resource_group` table

#### TC-META-15: Group metadata None → update with metadata → get returns metadata [P2]
- Create with None, update with `{"barrier": false}`, get → `{"barrier": false}`

#### TC-META-16: Group metadata set → update with None → metadata gone [P2]
- Create with `{"x": 1}`, update with `metadata: None`
- **Assert**: get returns metadata = None, JSON response has no `metadata` key

#### TC-META-17: Barrier group visible in hierarchy (RG does NOT filter) [P1]
- Create parent → child with `metadata: {"barrier": true}` → grandchild
- `list_group_hierarchy(parent)` → returns ALL 3 including barrier child
- **Covers**: PRD "RG does not filter based on barrier", Feature 0005-AC
- **Assert**: barrier group present in results, depth correct

#### TC-META-18: Group metadata in hierarchy endpoint response [P1]
- Create groups with various metadata
- `list_group_hierarchy` → each `GroupWithDepthDto` includes `metadata` field
- **Assert**: metadata preserved in hierarchy response (Feature 0005 requirement)

### REST-level metadata serialization

**File**: `api_rest_test.rs`

#### TC-META-19: REST create type with metadataSchema (camelCase in JSON) [P1]
- POST body: `{"code": "...", "canBeRoot": true, "metadataSchema": {"type": "object"}}`
- **Assert**: 201, response body has `"metadataSchema"` key (camelCase)

#### TC-META-20: REST create group with metadata in body [P1]
- POST body: `{"type": "...", "name": "X", "metadata": {"barrier": true}}`
- **Assert**: 201, response body has `"metadata": {"barrier": true}`

#### TC-META-21: REST response omits metadata when null [P2]
- Create group without metadata
- **Assert**: JSON response does NOT contain `"metadata"` key (via `skip_serializing_if`)

#### TC-META-22: REST response omits metadataSchema when null [P2]
- Create type without metadata_schema
- **Assert**: JSON response does NOT contain `"metadataSchema"` key

### Invalid / Non-GTS Input Tests

**ONE existing test** checks invalid type code: `create_type_invalid_code_returns_400` with `"code": "invalid"`. Nothing else.

**File**: `api_rest_test.rs` (REST-level for deserialization), `type_service_test.rs` / `group_service_test.rs` (service-level for domain validation)

#### Type code / type_path — wrong GTS format:

#### TC-NOGTS-01: Create type with valid GTS path but NOT RG prefix [P1]
- `code: "gts.cf.core.user.v1~"` — valid GTS format, but missing `system.rg.type.v1~` prefix
- **Assert**: 400 Validation ("must start with prefix")

#### TC-NOGTS-02: Create type with empty code [P1]
- `code: ""`
- **Assert**: 400 ("must not be empty")

#### TC-NOGTS-03: Create type with completely garbage code [P2]
- `code: "'; DROP TABLE gts_type; --"` (SQL injection attempt)
- **Assert**: 400 Validation (wrong prefix) — no SQL injection

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

#### TC-DESER-01: Create type with `code: 123` (number not string) [P1]
- **Assert**: 400/422 — Axum JSON deserialization error before handler

#### TC-DESER-02: Create type with `can_be_root: "yes"` (string not bool) [P1]
- **Assert**: 400/422

#### TC-DESER-03: Create type with `can_be_root` missing [P1]
- Body: `{"code": "gts.cf.core.rg.type.v1~test.v1~"}`
- **Assert**: 400/422 (required field missing — no `#[serde(default)]` on `can_be_root`)

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

### ADR-001 Hierarchy Reproduction in RG Module

Reproduce the full ADR example hierarchy (T1→D2→B3, T7→D8, T9) with correct types, parents, and metadata — entirely through RG service layer.

#### TC-ADR-01: Full ADR hierarchy with types, groups, and memberships [P1]
- **Setup**: Create all RG types (tenant, department, branch + user/course as membership types) via TypeService. Create groups T1, D2, B3, T7, D8, T9 via GroupService with correct parent-child and metadata. Add memberships (user in T1, user in D2, course in B3).
- **Assert per group**:
  - T1: root tenant, `parent_id=None`, `metadata: None`
  - D2: dept under T1, `metadata: {category: "finance", short_description: "Mega Department"}`
  - B3: branch under D2, `metadata: {location: "Building A, Floor 3"}`
  - T7: barrier tenant under T1, `metadata: {barrier: true}`
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

### metadata Validation Against Type's metadata_schema

Per ADR-001, each chained RG type defines a `metadata_schema` with `additionalProperties: false`, field types, and length constraints. **RG MUST validate group metadata against the type's metadata_schema on create/update.**

GTS-level validation (33 tests in `rg_gts_type_system_tests.rs`) validates at schema registration time. These unit tests verify the **runtime** validation path: when a caller creates/updates a group, RG checks the `metadata` payload against the stored `metadata_schema` for the group's type.

> **Implementation**: `validate_metadata_via_gts()` in `validation.rs` resolves the type's schema through `TypesRegistryClient` (types-registry-sdk, same pattern as `credstore` module) and validates metadata via `jsonschema` against the resolved schema. The GTS layer handles `$ref` resolution, `allOf` composition, and `x-gts-traits` — the resolved schema from `TypesRegistryClient::get()` already includes these. No fallback — `TypesRegistryClient` is always available at runtime (application does not start without types-registry).

##### Tenant metadata (`barrier: boolean`, `custom_domain: hostname`)

#### TC-ADR-09: Tenant — valid metadata.barrier=true accepted [P1]
- Create tenant group with `metadata: {"barrier": true}`
- **Assert**: 201 success

#### TC-ADR-10: Tenant — barrier wrong type (string) rejected [P1]
- Create tenant group with `metadata: {"barrier": "yes"}`
- **Assert**: 400 Validation error — `barrier` must be boolean

#### TC-ADR-11: Tenant — barrier wrong type (number) rejected [P1]
- `metadata: {"barrier": 42}`
- **Assert**: 400

#### TC-ADR-12: Tenant — unknown metadata field rejected [P1]
- `metadata: {"barrier": true, "foo": "bar"}`
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
- Create department group with `metadata: {"barrier": true}`
- Department schema does NOT have `barrier` → `additionalProperties: false` rejects
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

#### TC-ADR-30: Type without metadata_schema — any metadata accepted [P2]
- Create type with `metadata_schema: None`
- Create group with `metadata: {"anything": "goes", "x": 42}`
- **Assert**: 201 — no schema means no validation

#### TC-ADR-31: Update type metadata_schema — existing groups NOT retroactively validated [P2]
- Create type with permissive metadata_schema, create group with metadata
- Update type with stricter metadata_schema (adds maxLength)
- **Assert**: existing group still readable. New groups validated against new schema.

#### TC-ADR-15: metadata_schema round-trip with ADR tenant schema [P1]
- Create tenant RG type with metadata_schema from ADR (barrier: boolean, custom_domain: hostname)
- Get type → metadata_schema returned correctly (no internal `__` keys in response)
- **Assert**: metadata_schema matches input

#### TC-ADR-16: Chained type path format in RG [P1]
- ADR uses: `gts.cf.core.rg.type.v1~y.core.tn.tenant.v1~` (multi-segment)
- Code validates prefix: `gts.cf.core.rg.type.v1~` (different namespace!)
- **Assert**: Verify which prefix the code actually requires. If `system` not `core` → document discrepancy with ADR.

#### TC-ADR-17: Type response contains no SMALLINT IDs [P1]
- Create type, GET → response JSON
- **Assert**: no `gts_type_id`, `type_id`, `parent_type_id` numeric fields. `code`, `allowed_parents`, `allowed_memberships` are all strings.

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

#### TC-REST-01: Update type PUT returns 200 [P2]
- **Covers**: G34, 0002-AC-7
- **Setup**: Create type via service, PUT with updated body
- **Assert**: 200 OK, body contains updated fields

#### TC-REST-02: Update type not found returns 404 [P2]
- **Covers**: G34

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

#### P1 - Critical (must have, business invariants) — 64 tests

These test domain invariants that prevent data corruption or violate core business rules:

| ID | Test Case | Risk if Missing |
|----|-----------|-----------------|
| **TC-TYP-02** | Create type - nonexistent allowed_parents | Dangling type references |
| **TC-TYP-04** | Placement invariant violation | Orphan types that can't be placed |
| **TC-TYP-06** | Update type - remove parent in use | Breaks existing group hierarchy |
| **TC-TYP-07** | Update type - can_be_root=false with roots | Orphans existing root groups |
| **TC-TYP-09** | Delete type with groups | Cascading data loss |
| **TC-GRP-01** | Child group + closure rows | Hierarchy queries broken |
| **TC-GRP-02** | 3-level closure completeness | Ancestor/descendant queries wrong |
| **TC-GRP-03** | Incompatible parent type | Type system bypassed |
| **TC-GRP-04** | Root group when can_be_root=false | Type system bypassed |
| **TC-GRP-05** | Move with closure rebuild | Hierarchy corrupt after move |
| **TC-GRP-06** | Move under descendant -> cycle | Infinite loops in hierarchy |
| **TC-GRP-07** | Self-parent -> cycle | Infinite loops in hierarchy |
| **TC-GRP-08** | Move to incompatible type | Type system bypassed |
| **TC-GRP-10** | Type change vs parent compat | Type constraints violated |
| **TC-GRP-11** | Type change vs children compat | Children become orphans |
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
| **TC-GRP-26** | Simultaneous type + parent change | Atomicity of combined update |
| **TC-GRP-27** | Root group → non-root type change | Root groups orphaned |
| **TC-GRP-29** | Move child to root (detach) | Closure corruption on detach |
| **TC-GRP-30** | Move to root when can_be_root=false | Unauthorized root creation |
| **TC-GRP-37** | Depth exact boundary (>=) | Off-by-one in guardrail |
| **TC-GRP-38** | Width exact boundary (>=) | Off-by-one in guardrail |
| **TC-SDK-24** | validate_type_code vs GtsTypePath mismatch | Silent validation inconsistency |
| **TC-SDK-01..06,08** | GtsTypePath value object validation | Invalid types accepted into system |
| **TC-SDK-11,12** | GtsTypePath serde round-trip | API deserialization breaks silently |
| **TC-SDK-14,15** | SDK model camelCase + `type` rename | Wire format mismatch |
| **TC-DTO-05** | CreateGroupDto `type` JSON rename | Request deserialization fails |
| **TC-ODATA-01,02** | GroupFilterField names + kinds | OData $filter silently broken |
| **TC-ODATA-04,05** | Hierarchy + Membership FilterField | OData $filter silently broken |
| **TC-SEED-01..03** | seed_types create/update/skip | Bootstrap data corruption |
| **TC-SEED-05,06** | seed_groups create + skip | Hierarchy bootstrap broken |
| **TC-SEED-07,08** | seed_memberships create + skip | Membership bootstrap broken |
| **TC-ERR-01** | EnforcerError::Denied -> AccessDenied | Auth errors mishandled |
| **TC-REST-10** | Error response HTTP mapping — status codes + Content-Type | Incorrect HTTP status codes or missing `application/problem+json` Content-Type |

#### P2 - Important (error paths, REST layer, edges) — 53 tests

| ID | Area |
|----|------|
| TC-TYP-03, TC-TYP-05, TC-TYP-08, TC-TYP-10..15 | Type management edges + metadata + memberships |
| TC-GRP-09, TC-GRP-19..21, TC-GRP-24..25, TC-GRP-28, TC-GRP-31..36 | Group update/validation/error paths |
| TC-MBR-07..12 | Membership edges + empty resource_id + unregistered type |
| TC-REST-01..08 | REST layer coverage |
| TC-SDK-07, TC-SDK-09, TC-SDK-10, TC-SDK-16..23 | SDK edge cases + boundary |
| TC-DTO-01..04, TC-DTO-06, TC-DTO-07 | DTO conversion |
| TC-ODATA-03, TC-ODATA-06..08 | OData mapper correctness |
| TC-SEED-04, TC-SEED-09, TC-SEED-11, TC-SEED-12 | Seeding edge cases |
| TC-ERR-02..05 | Error conversion chains |
| TC-GRP-33 | max_width on move |

#### P3 - Nice to have (boundary, cosmetic) — 4 tests

| ID | Test Case |
|----|-----------|
| TC-SDK-13 | GtsTypePath Display + Into<String> |
| TC-TYP-16 | Hierarchy check skips deleted parent type |
| TC-MBR-15 | List memberships empty result |
| TC-SEED-10 | seed_types empty list |

### Acceptance Criteria

- [x] All ~325 unit tests pass (`cargo test -p cf-resource-group -p cf-resource-group-sdk`) — 325 tests, 0 failed (as of 2026-03-29)
- [x] Full suite completes in < 5 seconds
- [x] Zero `sleep`, `timeout`, or `tokio::time` usage in tests
- [x] Every domain invariant from features 0001-0005 is covered by at least one test
- [x] `make fmt && make lint && make test` passes with zero errors

#### Remaining Gaps (see section 2.2 — RG2..RG7)

- [x] ~~RG2~~: All OData mapper field→column tests added (4 tests)
- [x] ~~RG3~~: Error HTTP status + Content-Type tests added (TC-REST-10)
- [x] ~~RG4~~: DTO serialization covered by 7 inline tests + 2 REST null-omission tests
- [x] ~~RG5~~: Accepted — indirect coverage via 127 service-level tests is sufficient
- [x] ~~RG6~~: Accepted — module init is integration-level, covered by E2E (feature 0005, S3)
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
| **Update type** | `gts_type_id` changed. `parent_id`, `name` **unchanged**. |

### Hierarchy Endpoint Response Shape

`list_group_hierarchy(B)` for tree A → B → C **MUST** return:
- Self-node B: `hierarchy.depth == 0`
- Ancestor A: `hierarchy.depth < 0` (e.g., -1)
- Descendant C: `hierarchy.depth > 0` (e.g., 1)
- **All nodes present** (no missing nodes)
- Each node has `hierarchy.tenant_id`, `hierarchy.parent_id`

### Seeding DB Verification

| Operation | Required DB Assertions |
|-----------|----------------------|
| **seed_types creates** | Type **physically exists** in `gts_type`. Junction rows present. |
| **seed_types unchanged** | `updated_at` **not modified** on re-run. |
| **seed_groups creates** | Groups exist in `resource_group`. Closure rows correct. `parent_id` FK valid. |
| **seed_memberships creates** | Membership rows exist with correct composite key. |

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
pub async fn create_root_type(svc: &TypeService, suffix: &str) -> ResourceGroupType { ... }

/// Create child type referencing parent codes. One DB round-trip.
pub async fn create_child_type(
    svc: &TypeService, suffix: &str, parents: &[&str], memberships: &[&str]
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
  type_service_test.rs           # ✅ DONE (45 tests) TC-TYP + TC-META + TC-GTS
  group_service_test.rs          # ✅ DONE (55 tests) TC-GRP-01..38
  membership_service_test.rs     # ✅ DONE (15 tests) TC-MBR-01..15
  seeding_test.rs                # ✅ DONE (12 tests) TC-SEED-01..12
```

---

## 8. E2E Test Plan

> General E2E testing philosophy, patterns, and infrastructure: [`docs/modkit_unified_system/13_e2e_testing.md`](../../../../../docs/modkit_unified_system/13_e2e_testing.md).

Tests S5, S6, S7 verify PostgreSQL-specific behavior that unit tests cannot catch because 0006 runs on SQLite where FK enforcement requires `PRAGMA foreign_keys = ON` and closure SQL may silently produce wrong results due to looser type coercion.

### S5: `test_hierarchy_closure_postgresql`

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

### S6: `test_move_closure_rebuild_postgresql`

**Seam**: Closure table DELETE + re-INSERT under PostgreSQL SERIALIZABLE transaction.

**Why not in unit tests**: TC-GRP-05 covers move logic on SQLite. On PostgreSQL, `DELETE FROM resource_group_closure` followed by `INSERT INTO ... SELECT` new paths runs under SERIALIZABLE isolation — incorrect transaction handling can cause inconsistent closure state visible to concurrent reads.

```
POST root_A → child
POST root_B

PUT /groups/{child.id} {parent_id: root_B.id}    → 200

GET /groups/{root_B.id}/hierarchy    → 200
  assert child in items with depth == 1            (closure rebuilt on PG)
```

### S7: `test_force_delete_cascade_postgresql`

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

### GTS Metadata Validation Test Requirements

#### Unit tests (MUST)

Unit tests MUST fully validate GTS metadata schema integration:

- **Schema resolution**: `validate_metadata_via_gts()` resolves the type's schema through `TypesRegistryClient` and validates metadata against the resolved schema (including `allOf` composition, `$ref` resolution, `x-gts-traits`).
- **Positive cases**: Valid metadata accepted for each type that defines a `metadata_schema` (tenant with `barrier: true`, department with `display_name`, etc.).
- **Negative cases**: Invalid metadata rejected with field-level errors — wrong type (`barrier: "yes"` → 400), unknown fields (`additionalProperties: false`), missing required fields, length violations (`maxLength`).
- **Round-trip**: `metadata_schema` stored in DB → loaded → used for validation produces the same result as the original schema. Non-object schemas (stored under `__user_schema`) round-trip correctly.

Test files: `group_service_test.rs` (TC-ADR-09..TC-ADR-22), `type_service_test.rs` (TC-META-01..TC-META-11).

Unit tests use a real `TypesRegistryClient` backed by `InMemoryGtsRepository` (same as `rg_gts_type_system_tests.rs`). RG type schemas are registered in the in-memory store before running group creation tests. No mocks needed — the types registry runs in-process with SQLite.

#### E2E tests (MUST)

E2E tests MUST verify the positive path of GTS metadata validation against a running server with real `TypesRegistryClient`:

- Create a type with a `metadata_schema` that defines at least one typed field (e.g. `{"type": "object", "properties": {"label": {"type": "string"}}, "additionalProperties": false}`).
- Create a group of that type with valid metadata matching the schema (e.g. `{"label": "test"}`).
- **Assert**: 201 success, response contains the metadata as provided.

E2E tests do NOT need to exhaustively test negative validation cases (wrong types, unknown fields, etc.) — those are covered by unit tests. The E2E test verifies the end-to-end wiring: type creation → schema storage → group creation → metadata validation → success.

Modify existing E2E group creation tests (`test_integration_seams.py`) to use a type with `metadata_schema` instead of `metadata_schema: null`, and pass valid metadata in the group creation request.
