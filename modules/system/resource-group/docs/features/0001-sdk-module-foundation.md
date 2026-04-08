<!-- Created: 2026-04-07 by Constructor Tech -->

# Feature: SDK Contracts, Error Types & Module Foundation

- [x] `p1` - **ID**: `cpt-cf-resource-group-featstatus-sdk-module-foundation`

- [x] `p1` - `cpt-cf-resource-group-feature-sdk-module-foundation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [GTS Type Path Validation](#gts-type-path-validation)
  - [Domain Error to Problem Mapping](#domain-error-to-problem-mapping)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [SDK Models and Value Objects](#sdk-models-and-value-objects)
  - [SDK Trait Contracts](#sdk-trait-contracts)
  - [SDK Error Taxonomy](#sdk-error-taxonomy)
  - [Persistence Adapter and DB Migrations](#persistence-adapter-and-db-migrations)
  - [Module Scaffold and Initialization](#module-scaffold-and-initialization)
  - [REST and OData Infrastructure](#rest-and-odata-infrastructure)
  - [E2E Test Suite](#e2e-test-suite)
  - [SDK Value Object & Model Tests](#sdk-value-object--model-tests)
  - [OData Filter & DTO Tests](#odata-filter--dto-tests)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
  - [SDK Value Objects & Models](#sdk-value-objects--models)
  - [DTO Conversions & Serialization](#dto-conversions--serialization)
  - [OData Filter Field Mapping](#odata-filter-field-mapping)
- [8. E2E Test Plan](#8-e2e-test-plan)
  - [File Layout](#file-layout)
  - [Shared Helpers (conftest.py)](#shared-helpers-conftestpy)
  - [S1: `test_route_smoke_all_endpoints`](#s1-test_route_smoke_all_endpoints)
  - [S2: `test_dto_roundtrip_group_json_shape`](#s2-test_dto_roundtrip_group_json_shape)
  - [S8: `test_error_response_rfc9457`](#s8-test_error_response_rfc9457)
  - [S9: `test_pagination_cursor_roundtrip`](#s9-test_pagination_cursor_roundtrip)
  - [Acceptance Criteria (S1, S2, S8, S9)](#acceptance-criteria-s1-s2-s8-s9)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Establish the SDK crate with trait contracts, domain models, and error taxonomy; scaffold the RG module with ClientHub registration; implement persistence adapter with SeaORM entities and DB migrations for all 6 tables; wire cross-cutting infrastructure for REST/OData endpoints and deterministic error mapping.

### 1.2 Purpose

This feature provides the foundation that all subsequent RG features depend on. It defines the public API surface (SDK traits), the domain model types, the error taxonomy, and the infrastructure scaffolding (module wiring, persistence, REST framework) without implementing domain-specific business logic.

**Requirements**: `cpt-cf-resource-group-fr-rest-api`, `cpt-cf-resource-group-fr-odata-query`, `cpt-cf-resource-group-fr-deterministic-errors`, `cpt-cf-resource-group-fr-no-authz-and-sql-logic`, `cpt-cf-resource-group-nfr-deterministic-errors`, `cpt-cf-resource-group-nfr-compatibility`, `cpt-cf-resource-group-nfr-production-scale`, `cpt-cf-resource-group-nfr-transactional-consistency`, `cpt-cf-resource-group-interface-resource-group-client`, `cpt-cf-resource-group-interface-integration-read-hierarchy`

**Principles**: `cpt-cf-resource-group-principle-policy-agnostic`

**Constraints**: `cpt-cf-resource-group-constraint-no-authz-decision`, `cpt-cf-resource-group-constraint-no-sql-filter-generation`, `cpt-cf-resource-group-constraint-db-agnostic`, `cpt-cf-resource-group-constraint-surrogate-ids-internal`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-resource-group-actor-apps` | Programmatic consumer of SDK traits via ClientHub |
| `cpt-cf-resource-group-actor-instance-administrator` | Operates migrations and module deployment |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.1
- **Dependencies**: None (foundation feature)
- **Not applicable**: UX (backend API — no user interface); COMPL (internal platform module — no regulatory data handling); OPS observability and rollout are managed at the module infrastructure level (DESIGN §3.7 and platform runbooks); PERF targets are set at the system level in PRD.md NFR section.

## 2. Actor Flows (CDSL)

Not applicable. This feature provides SDK contracts and module infrastructure without user-facing interactions. Actor flows that exercise these contracts are defined in features 2-5 (type management, entity/hierarchy, membership, integration) which implement domain operations on top of this foundation.

## 3. Processes / Business Logic (CDSL)

### GTS Type Path Validation

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path`

**Input**: Raw string candidate for a GTS type path

**Output**: Validated `GtsTypePath` value object or validation error

**Steps**:
1. [x] - `p1` - Receive raw string input - `inst-gts-val-1`
2. [x] - `p1` - Trim whitespace and normalize to lowercase - `inst-gts-val-2`
3. [x] - `p1` - **IF** string is empty - `inst-gts-val-3`
   1. [x] - `p1` - **RETURN** Validation error: "GTS type path must not be empty" - `inst-gts-val-3a`
4. [x] - `p1` - **IF** string does not match pattern `^gts\.[a-z0-9_.]+~([a-z0-9_.]+~)*$` - `inst-gts-val-4`
   1. [x] - `p1` - **RETURN** Validation error: "Invalid GTS type path format" - `inst-gts-val-4a`
5. [x] - `p1` - **IF** string length exceeds maximum (255 chars) - `inst-gts-val-5`
   1. [x] - `p1` - **RETURN** Validation error: "GTS type path exceeds maximum length" - `inst-gts-val-5a`
6. [x] - `p1` - Construct `GtsTypePath` value object wrapping the validated string - `inst-gts-val-6`
7. [x] - `p1` - **RETURN** validated `GtsTypePath` - `inst-gts-val-7`

### Domain Error to Problem Mapping

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-sdk-foundation-map-domain-error`

**Input**: `ResourceGroupError` domain error variant

**Output**: RFC-9457 Problem response with HTTP status, type URI, title, and detail

**Steps**:
1. [x] - `p1` - Receive `ResourceGroupError` variant - `inst-err-map-1`
2. [x] - `p1` - Match error variant to HTTP status and Problem fields - `inst-err-map-2`
   1. [x] - `p1` - `Validation` -> 400 Bad Request, type "validation", field-level details - `inst-err-map-2a`
   2. [x] - `p1` - `NotFound` -> 404 Not Found, type "not-found", entity identifier in detail - `inst-err-map-2b`
   3. [x] - `p1` - `TypeAlreadyExists` -> 409 Conflict, type "type-already-exists", conflicting code in detail - `inst-err-map-2c`
   4. [x] - `p1` - `InvalidParentType` -> 409 Conflict, type "invalid-parent-type", type mismatch in detail - `inst-err-map-2d`
   5. [x] - `p1` - `AllowedParentsViolation` -> 409 Conflict, type "allowed-parents-violation", violating groups in detail - `inst-err-map-2e`
   6. [x] - `p1` - `CycleDetected` -> 409 Conflict, type "cycle-detected", involved node IDs in detail - `inst-err-map-2f`
   7. [x] - `p1` - `ConflictActiveReferences` -> 409 Conflict, type "active-references", reference count in detail - `inst-err-map-2g`
   8. [x] - `p1` - `LimitViolation` -> 409 Conflict, type "limit-violation", limit name and values in detail - `inst-err-map-2h`
   9. [x] - `p1` - `TenantIncompatibility` -> 409 Conflict, type "tenant-incompatibility", tenant IDs in detail - `inst-err-map-2i`
   10. [x] - `p1` - `ServiceUnavailable` -> 503 Service Unavailable, type "service-unavailable" - `inst-err-map-2j`
   11. [x] - `p1` - `Internal` -> 500 Internal Server Error, type "internal", no internal details exposed - `inst-err-map-2k`
3. [x] - `p1` - Construct Problem response with `type`, `title`, `status`, `detail` fields - `inst-err-map-3`
4. [x] - `p1` - **RETURN** Problem response - `inst-err-map-4`

## 4. States (CDSL)

Not applicable. This feature defines SDK contracts, module scaffold, and persistence infrastructure. No entity lifecycle state machines are introduced here. Entity state management is covered by features 2-4 (type/group/membership lifecycle).

## 5. Definitions of Done

### SDK Models and Value Objects

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-sdk-foundation-sdk-models`

The system **MUST** define SDK model types in `resource-group-sdk/src/models.rs` that represent the public API surface for all RG domain entities and query constructs.

**Required types**:
- `ResourceGroupType` — type definition with `schema_id` (GtsTypePath), `allowed_parents` (Vec), `allowed_memberships` (Vec), `can_be_root` (bool), `metadata_schema` (Option)
- `ResourceGroup` — group entity with `id` (Uuid), `type` (GtsTypePath), `name` (String), `metadata` (Option), `hierarchy` (ResourceGroupHierarchy with `parent_id`, `tenant_id`)
- `ResourceGroupWithDepth` — extends ResourceGroup with `hierarchy.depth` (i32, relative distance)
- `ResourceGroupMembership` — membership link with `group_id` (Uuid), `resource_type` (GtsTypePath), `resource_id` (String)
- `GtsTypePath` — validated value object wrapping GTS type path string, with format validation
- `Page<T>` — cursor-based pagination wrapper with `items` (Vec), `page_info` (PageInfo)
- `PageInfo` — pagination metadata with `next_cursor`, `prev_cursor`, `limit`
- `ListQuery` — OData filter + pagination parameters

**Implements**:
- `cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path`

**Constraints**: `cpt-cf-resource-group-constraint-surrogate-ids-internal`

**Touches**:
- Entities: `ResourceGroupType`, `ResourceGroup`, `ResourceGroupWithDepth`, `ResourceGroupMembership`, `GtsTypePath`, `Page`, `PageInfo`, `ListQuery`

### SDK Trait Contracts

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-sdk-foundation-sdk-traits`

The system **MUST** define SDK trait contracts in `resource-group-sdk/src/api.rs` that represent the stable public interface for all RG operations.

**Required traits**:
- `ResourceGroupClient` — full CRUD trait: type management (`create_type`, `get_type`, `list_types`, `update_type`, `delete_type`), group management (`create_group`, `get_group`, `list_groups`, `update_group`, `delete_group`, `list_group_depth`), membership management (`add_membership`, `remove_membership`, `list_memberships`). All methods accept `SecurityContext` as first argument.
- `ResourceGroupReadHierarchy` — narrow hierarchy-only read trait: `list_group_depth(ctx, group_id, query)` returning `Page<ResourceGroupWithDepth>`. Used exclusively by AuthZ plugin.
- `ResourceGroupReadPluginClient` — extends `ResourceGroupReadHierarchy` with `list_memberships`. Used for vendor-specific plugin gateway routing.

**Constraints**: `cpt-cf-resource-group-constraint-no-authz-decision`, `cpt-cf-resource-group-constraint-no-sql-filter-generation`

**Touches**:
- Entities: `ResourceGroupClient`, `ResourceGroupReadHierarchy`, `ResourceGroupReadPluginClient`

### SDK Error Taxonomy

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-sdk-foundation-sdk-errors`

The system **MUST** define `ResourceGroupError` enum in `resource-group-sdk/src/error.rs` covering all deterministic failure categories.

**Required variants**: `Validation`, `NotFound`, `TypeAlreadyExists`, `InvalidParentType`, `AllowedParentsViolation`, `CycleDetected`, `ConflictActiveReferences`, `LimitViolation`, `TenantIncompatibility`, `ServiceUnavailable`, `Internal`.

Each variant **MUST** carry structured context (field details for Validation, entity identifier for NotFound, conflicting code for TypeAlreadyExists, etc.) sufficient for the error mapping algorithm to produce informative Problem responses.

**Implements**:
- `cpt-cf-resource-group-algo-sdk-foundation-map-domain-error`

**Touches**:
- Entities: `ResourceGroupError`

### Persistence Adapter and DB Migrations

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-sdk-foundation-persistence`

The system **MUST** define SeaORM entity models and DB migration scripts for all 6 RG tables.

Each persistence adapter (type, group, closure, membership) **MUST** be defined as a trait first (e.g., `TypeRepositoryTrait`, `GroupRepositoryTrait`, `ClosureRepositoryTrait`, `MembershipRepositoryTrait`) and injected into domain services as `Arc<dyn Trait>`. This enables unit testing with in-memory trait implementations (`InMemoryTypeRepository`, etc.) without a database, and ensures a clean contract boundary between domain and infrastructure layers.

**Required tables** (per DESIGN 3.7):
- `gts_type` — SMALLINT PK (identity), `schema_id` (unique TEXT), `metadata_schema` (JSONB nullable), timestamps
- `gts_type_allowed_parent` — composite PK `(type_id, parent_type_id)` with CASCADE FK
- `gts_type_allowed_membership` — composite PK `(type_id, membership_type_id)` with CASCADE FK
- `resource_group` — UUID PK, `parent_id` FK (self-referential), `gts_type_id` FK, `name`, `metadata` (JSONB nullable), `tenant_id`, timestamps. Indexes: `(parent_id)`, `(name)`, `(gts_type_id, id)`, `(tenant_id)`
- `resource_group_membership` — unique `(group_id, gts_type_id, resource_id)`, FK group_id → resource_group, FK gts_type_id → gts_type, `created_at`. Index: `(gts_type_id, resource_id)`
- `resource_group_closure` — composite PK `(ancestor_id, descendant_id)`, `depth` INTEGER, FK both → resource_group. Indexes: `(descendant_id)`, `(ancestor_id, depth)`

**Constraints**: `cpt-cf-resource-group-constraint-db-agnostic`, `cpt-cf-resource-group-constraint-surrogate-ids-internal`

**Touches**:
- DB: `gts_type`, `gts_type_allowed_parent`, `gts_type_allowed_membership`, `resource_group`, `resource_group_membership`, `resource_group_closure`

### Module Scaffold and Initialization

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-sdk-foundation-module-scaffold`

The system **MUST** provide an RG module annotated with `#[modkit::module]` that registers SDK clients in ClientHub and establishes the phased initialization order for circular dependency resolution with AuthZ.

**Required behavior**:
- Phase 1 (SystemCapability): register `dyn ResourceGroupClient` and `dyn ResourceGroupReadHierarchy` in ClientHub. REST/gRPC endpoints NOT yet accepting traffic.
- Phase 2 (ready): start accepting REST/gRPC traffic. Write operations can now call `PolicyEnforcer` → `AuthZResolverClient` (available since AuthZ init in Phase 1).
- ClientHub registration: single `RgService` implementation registered as both `dyn ResourceGroupClient` and `dyn ResourceGroupReadHierarchy`.
- Query profile configuration loaded from module config (`max_depth`, `max_width`).

**Implements**:
- Sequence `cpt-cf-resource-group-seq-init-order`

**Constraints**: `cpt-cf-resource-group-constraint-no-authz-decision`

**Touches**:
- Entities: `RgModule`, `RgService`

### REST and OData Infrastructure

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-sdk-foundation-rest-odata`

The system **MUST** wire OperationBuilder-based REST API routing with OData `$filter` parsing and cursor-based pagination for all list endpoints.

**Required infrastructure**:
- OperationBuilder endpoint registration for types (under `/api/types-registry/v1/`), groups and memberships (under `/api/resource-group/v1/`)
- OData `$filter` parser supporting field-specific operators: `eq`, `ne`, `in` for string/UUID fields; `eq`, `ne`, `gt`, `ge`, `lt`, `le` for integer fields; nested path syntax (`hierarchy/parent_id`, `hierarchy/depth`)
- Cursor-based pagination: `limit` (1..200, default 25), `cursor` (opaque token). Ordering is undefined but consistent — no `$orderby` support.
- DomainError → Problem (RFC-9457) error response mapping wired into all endpoint handlers via OperationBuilder error hooks
- Path-based API versioning: `/api/resource-group/v1/` for groups and memberships, `/api/types-registry/v1/` for types

**Implements**:
- `cpt-cf-resource-group-algo-sdk-foundation-map-domain-error`

**Touches**:
- API: `GET/POST/PUT/DELETE /api/types-registry/v1/types*`, `GET/POST/PUT/DELETE /api/resource-group/v1/groups*`, `GET/POST/DELETE /api/resource-group/v1/memberships*`

### E2E Test Suite

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-e2e-test-suite`

The system **MUST** have 10 E2E tests in a single file `test_integration_seams.py` covering integration seams across features 0001–0005. Suite runtime < 15 seconds. Zero flakes on 10 consecutive runs.

**Implements**:
- S1, S2, S8, S9 (this feature), S3/S4 (feature 0005), S5/S6/S7 (feature 0003), S10 (feature 0004)

### SDK Value Object & Model Tests

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-testing-sdk-models`

In-source `#[cfg(test)]` tests in `resource-group-sdk/src/models.rs`:
- `GtsTypePath::new()` validation (empty, too long, invalid format, whitespace/case normalization, multi-segment paths)
- `GtsTypePath` serde round-trip (JSON serialize/deserialize, invalid input rejection via TryFrom)
- SDK model serialization shape (camelCase, `type` rename, optional field omission)
- `QueryProfile::default()` values

### OData Filter & DTO Tests

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-testing-odata-dto`

In-source `#[cfg(test)]` tests for filter field definitions and DTO conversions:
- `GroupFilterField`, `HierarchyFilterField`, `MembershipFilterField` name() and kind() correctness
- OData mapper field-to-column mapping (Type/Group/Membership mappers)
- DTO `From` conversions (domain model -> DTO and DTO -> request)
- DTO serde attributes (`type` rename, `default` vectors, optional field omission)

## 6. Acceptance Criteria

- [x] SDK crate (`resource-group-sdk`) compiles with all model types, trait contracts, and error types defined
- [x] `GtsTypePath::new("gts.cf.core.rg.type.v1~")` succeeds; `GtsTypePath::new("invalid")` returns validation error
- [x] All 6 DB tables are created by migration scripts with correct constraints and indexes
- [x] SeaORM entities compile and map to the DB schema without runtime errors
- [x] Module registers `dyn ResourceGroupClient` and `dyn ResourceGroupReadHierarchy` in ClientHub during Phase 1 init
- [x] All `ResourceGroupError` variants map to correct HTTP status codes and RFC-9457 Problem responses
- [x] OData `$filter` parser handles `eq`, `ne`, `in` operators and nested path syntax (`hierarchy/parent_id`)
- [x] Cursor-based pagination returns correct `PageInfo` with cursor tokens
- [x] No SMALLINT surrogate IDs appear in any SDK type, REST response schema, or trait method signature
- [x] Module does not contain any AuthZ decision logic, SQL filter generation, or policy evaluation code

---

## 7. Unit Test Plan

> General testing philosophy, patterns, and infrastructure: [`docs/modkit_unified_system/12_unit_testing.md`](../../../../../docs/modkit_unified_system/12_unit_testing.md).

### SDK Value Objects & Models

**File**: `resource-group-sdk/src/models.rs` (in-source `#[cfg(test)]` block — following `auth.rs` pattern)

Other modules (`nodes-registry`, `types-registry`) place pure-logic tests directly in source files. `GtsTypePath` has 110 lines of validation logic with zero tests.

#### TC-SDK-01: GtsTypePath::new() valid path [P1]
- **Covers**: G36, 0001-AC-2
- **Input**: `"gts.cf.core.rg.type.v1~"`
- **Assert**: `Ok(GtsTypePath)`, `as_str()` returns lowercase

#### TC-SDK-02: GtsTypePath::new() empty string [P1]
- **Covers**: G36
- **Assert**: `Err("must not be empty")`

#### TC-SDK-03: GtsTypePath::new() exceeds 255 chars [P1]
- **Covers**: G36
- **Assert**: `Err("exceeds maximum length")`

#### TC-SDK-04: GtsTypePath::new() invalid format - no gts prefix [P1]
- **Covers**: G38
- **Input**: `"invalid.path~"`
- **Assert**: `Err("Invalid GTS type path format")`

#### TC-SDK-05: GtsTypePath::new() invalid format - no trailing tilde [P1]
- **Covers**: G38
- **Input**: a valid GTS path prefix without trailing tilde (e.g. `"<gts-prefix>.v1"` — missing `~`)
- **Assert**: `Err`

#### TC-SDK-06: GtsTypePath::new() invalid format - uppercase chars [P1]
- **Covers**: G38
- **Input**: `"GTS.X.CORE.RG.TYPE.V1~"` with uppercase -> trimmed/lowercased

#### TC-SDK-07: GtsTypePath::new() trims whitespace and lowercases [P2]
- **Covers**: G36
- **Input**: `"  GTS.X.CORE.RG.TYPE.V1~  "`
- **Assert**: `Ok`, `as_str() == "gts.cf.core.rg.type.v1~"`

#### TC-SDK-08: GtsTypePath::new() chained path (multi-segment) [P1]
- **Covers**: G38
- **Input**: `"gts.cf.core.rg.type.v1~x.test.v1~"`
- **Assert**: `Ok`

#### TC-SDK-09: GtsTypePath::new() double tilde (empty segment) [P2]
- **Covers**: G38
- **Input**: `"gts.cf.core.rg.type.v1~~"`
- **Assert**: `Err` (empty segment between tildes)

#### TC-SDK-10: GtsTypePath::new() special chars in segment [P2]
- **Covers**: G38
- **Input**: `"gts.cf.core.rg.type.v1~hello-world~"` (hyphen not allowed)
- **Assert**: `Err`

#### TC-SDK-11: GtsTypePath serde round-trip (JSON) [P1]
- **Covers**: G37
- **Setup**: Serialize `GtsTypePath` to JSON string, deserialize back
- **Assert**: `serde_json::to_string(&path)` produces `"gts.cf.core.rg.type.v1~"`, deserialize back equals original

#### TC-SDK-12: GtsTypePath serde invalid JSON string [P1]
- **Covers**: G37
- **Setup**: `serde_json::from_str::<GtsTypePath>("\"invalid\"")`
- **Assert**: `Err` (validation runs via TryFrom)

#### TC-SDK-13: GtsTypePath Display + Into<String> [P3]
- **Covers**: G36
- **Assert**: `.to_string()` and `String::from(path)` produce same result

#### TC-SDK-18: GtsTypePath "gts.~" (minimal rest, empty segment) [P2]
- rest = "~", segments = ["", ""], first segment empty → Err

#### TC-SDK-19: GtsTypePath numeric segments "gts.123~456~" [P2]
- Digits are allowed chars → Ok

#### TC-SDK-20: GtsTypePath underscores + dots "gts.a_b.c_d~" [P2]
- Valid chars → Ok

#### TC-SDK-21: GtsTypePath whitespace-only input "   " [P2]
- trim → empty → Err("must not be empty")

#### TC-SDK-22: GtsTypePath exactly 255 chars [P2]
- Boundary → Ok

#### TC-SDK-23: GtsTypePath exactly 256 chars [P2]
- Boundary → Err("exceeds maximum length")

#### TC-SDK-24: validate_type_code vs GtsTypePath normalization mismatch [P1]
- `validate_type_code("  GTS.X.SYSTEM.RG.TYPE.V1~  ")` → fails (no trim/lowercase)
- `GtsTypePath::new("  GTS.X.SYSTEM.RG.TYPE.V1~  ")` → succeeds (trims + lowercases)
- Document this inconsistency and verify behavior

#### TC-SDK-14: SDK model camelCase serialization [P1]
- **Covers**: G41
- **Setup**: Serialize `ResourceGroupType` to JSON
- **Assert**: Keys are camelCase (`canBeRoot`, `allowedParents`, `allowedMemberships`, `metadataSchema`)

#### TC-SDK-15: SDK model `type` field rename [P1]
- **Covers**: G41
- **Setup**: Serialize `ResourceGroup` to JSON
- **Assert**: Field is `"type"`, not `"type_path"`

#### TC-SDK-16: SDK model optional fields omitted when None [P2]
- **Covers**: G41
- **Setup**: Serialize `ResourceGroup { metadata: None, .. }` to JSON
- **Assert**: `"metadata"` key absent from JSON

#### TC-SDK-17: QueryProfile default values [P2]
- **Covers**: G52
- **Assert**: `QueryProfile::default().max_depth == Some(10)`, `.max_width == None`

### DTO Conversions & Serialization

**File**: `api/rest/dto.rs` (in-source `#[cfg(test)]` block)

Other modules test DTO conversion correctness. `dto.rs` has 9 `From` impls and serde attributes with zero tests.

#### TC-DTO-01: ResourceGroupType -> TypeDto preserves all fields [P2]
- **Covers**: G39
- **Assert**: code, can_be_root, allowed_parents, allowed_memberships, metadata_schema all match

#### TC-DTO-02: CreateTypeDto -> CreateTypeRequest conversion [P2]
- **Covers**: G39
- **Assert**: All fields transferred

#### TC-DTO-03: ResourceGroup -> GroupDto preserves hierarchy fields [P2]
- **Covers**: G39
- **Assert**: hierarchy.parent_id, hierarchy.tenant_id match

#### TC-DTO-04: ResourceGroupWithDepth -> GroupWithDepthDto includes depth [P2]
- **Covers**: G39
- **Assert**: hierarchy.depth transferred

#### TC-DTO-05: CreateGroupDto JSON with `type` rename [P1]
- **Covers**: G40
- **Setup**: Deserialize `{"type": "gts...", "name": "X"}` into CreateGroupDto
- **Assert**: `dto.type_path` populated correctly from `"type"` JSON key

#### TC-DTO-06: CreateTypeDto default vectors [P2]
- **Covers**: G40
- **Setup**: Deserialize `{"code":"...", "can_be_root": true}` (no allowed_parents/memberships)
- **Assert**: `allowed_parents == []`, `allowed_memberships == []` (via `#[serde(default)]`)

#### TC-DTO-07: MembershipDto has no tenant_id field [P2]
- **Covers**: G40, 0004-AC-12
- **Setup**: Serialize MembershipDto to JSON
- **Assert**: No `tenant_id` key in output

### OData Filter Field Mapping

**File**: `resource-group-sdk/src/odata/groups.rs` + `hierarchy.rs` + `memberships.rs` (in-source `#[cfg(test)]` blocks)

OData filter fields use manual `FilterField` trait implementations with string field names and `FieldKind` enum. Incorrect mapping silently breaks filtering.

#### TC-ODATA-01: GroupFilterField names [P1]
- **Covers**: G42
- **Assert**: `Type.name() == "type"`, `HierarchyParentId.name() == "hierarchy/parent_id"`, `Id.name() == "id"`, `Name.name() == "name"`

#### TC-ODATA-02: GroupFilterField kinds [P1]
- **Covers**: G42
- **Assert**: `Type -> I64`, `HierarchyParentId -> Uuid`, `Id -> Uuid`, `Name -> String`

#### TC-ODATA-03: GroupFilterField FIELDS constant completeness [P2]
- **Covers**: G42
- **Assert**: `FIELDS.len() == 4`, contains all variants

#### TC-ODATA-04: HierarchyFilterField names and kinds [P1]
- **Covers**: G43
- **Assert**: `HierarchyDepth.name() == "hierarchy/depth"`, `Type.name() == "type"`, both `I64`

#### TC-ODATA-05: MembershipFilterField names and kinds [P1]
- **Covers**: G44
- **Assert**: `GroupId -> ("group_id", Uuid)`, `ResourceType -> ("resource_type", String)`, `ResourceId -> ("resource_id", String)`

#### TC-ODATA-06: TypeODataMapper field-to-column mapping [P2]
- **Covers**: G45
- **Assert**: `TypeFilterField::Code` maps to `TypeColumn::SchemaId`

#### TC-ODATA-07: GroupODataMapper field-to-column mapping [P2]
- **Covers**: G45
- **Assert**: `Type -> GtsTypeId`, `HierarchyParentId -> ParentId`, `Id -> Id`, `Name -> Name`

#### TC-ODATA-08: MembershipODataMapper field-to-column mapping [P2]
- **Covers**: G45
- **Assert**: `GroupId -> GroupId`, `ResourceType -> GtsTypeId`, `ResourceId -> ResourceId`

---

## 8. E2E Test Plan

> General E2E testing philosophy, patterns, and infrastructure: [`docs/modkit_unified_system/13_e2e_testing.md`](../../../../../docs/modkit_unified_system/13_e2e_testing.md).

Tests S1, S2, S8, S9 verify integration seams that unit tests (TC-DTO-*, TC-SDK-14/15, TC-REST-10) cannot cover because they run on `Router::oneshot` with SQLite, not a real running server.

### File Layout

```
testing/e2e/modules/resource_group/
├── conftest.py                          ← helpers, timeout config
├── test_authz_tenant_scoping.py         ← existing (9 tests) — keep as-is
├── test_mtls_auth.py                    ← existing (4 tests) — keep as-is
├── test_integration_seams.py            ← 10 integration seam tests
```

### Shared Helpers (conftest.py)

`REQUEST_TIMEOUT = 5.0` — applied to every httpx call.

Helper `assert_group_shape(data)` verifies the JSON wire shape of a group response: `id` and `tenant_id` are valid UUIDs; `name` is a string; `created_at` is present and parseable as ISO 8601; when `parent_id` is not None it is a valid UUID; when `metadata` is not None it is a dict.

### S1: `test_route_smoke_all_endpoints`

**Seam**: Route registration — handlers mounted on correct method + path on a real running server.

**Why not in unit tests**: TC-REST-* call handlers via `Router::oneshot` in-process. If a handler is not registered in `module.rs`, or mounted on the wrong path, all unit tests pass but the API is broken. The real `module.rs` wiring is only exercised on a live server.

```
HEAD /cf/resource-group/v1/groups               → not 404/405
HEAD /cf/resource-group/v1/groups/{uuid}        → not 405 (404 ok — group doesn't exist)
HEAD /cf/resource-group/v1/groups/{uuid}/hierarchy  → not 405
HEAD /cf/resource-group/v1/memberships          → not 405
POST /cf/types-registry/v1/types (empty body)   → not 404/405 (400 ok — validation)

Verify: each returns a status code, meaning the route exists and the handler runs.
No data setup needed. Fastest possible test.
```

### S2: `test_dto_roundtrip_group_json_shape`

**Seam**: DTO serialization — JSON field names, types, and presence match OpenAPI contract over HTTP.

**Why not in unit tests**: TC-DTO-01–07 and TC-SDK-14/15 test `From<Group> for GroupDto` (Rust struct conversion). They do NOT test the JSON wire format: `#[serde(rename = "type")]`, `#[serde(skip_serializing_if = "Option::is_none")]`, camelCase conventions, timestamp format. A serde attribute typo passes unit tests but breaks clients.

```
POST /types → create type
POST /groups → create group with metadata: {"barrier": true}
GET  /groups/{id} → 200

Assert JSON keys:
  "id"         — string, UUID format
  "name"       — string
  "type"       — string (NOT "type_path", NOT "gts_type_id")
  "tenant_id"  — string, UUID format
  "parent_id"  — null (root group)
  "depth"      — integer, == 0
  "metadata"   — {"barrier": true} (JSONB roundtrip)
  "created_at" — string, ISO 8601
  "updated_at" — null or absent (fresh create)

Assert NO unexpected keys leaking (like "gts_type_id" internal SMALLINT).
```

### S8: `test_error_response_rfc9457`

**Seam**: Error middleware — DomainError → HTTP status + `application/problem+json` Content-Type + no internal leaks.

**Why not in unit tests**: TC-REST-10 covers multiple error codes via `Router::oneshot` with mocked AuthZ and SQLite. E2E verifies the real server process serializes errors correctly with the real middleware chain.

```
GET /groups/{random-uuid}                        → 404
  assert "content-type" header contains "application/problem+json"
  assert body has "status": 404
  assert "stack" not in body and "trace" not in body
```

### S9: `test_pagination_cursor_roundtrip`

**Seam**: Cursor encode/decode across HTTP — base64 token survives URL encoding, pagination offset doesn't drift.

**Why not in unit tests**: Unit tests test `Page<T>` construction and `PageInfo` fields. The cursor codec (base64 encode/decode, URL-safe encoding) only runs in the handler layer and is never exercised by `Router::oneshot` tests.

```
Create 5 groups of same type

all_ids = []
cursor = None
while True:
    GET /groups?$top=2&$skiptoken={cursor}     → 200
    all_ids.extend(page item IDs)
    if page_info.next_cursor is None:
        break
    cursor = page_info.next_cursor

assert len(all_ids) == len(set(all_ids))         (no duplicates)
assert all 5 created IDs present                 (no missing)
```

### Acceptance Criteria (S1, S2, S8, S9)

- [x] S1 (route smoke) requires no data setup — fastest possible, all endpoints respond non-405
- [x] S2 (DTO roundtrip) verifies exact JSON key names — not just "response is 200"
- [x] S8 (error format) checks `Content-Type: application/problem+json` header and no internal field leaks
- [x] S9 (pagination) asserts no duplicates AND no missing items across all pages
