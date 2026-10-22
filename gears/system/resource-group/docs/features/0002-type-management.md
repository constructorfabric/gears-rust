<!-- Created: 2026-04-07 by Constructor Tech -->
<!-- Updated: 2026-04-20 by Constructor Tech -->

# Feature: Resource Group Types from Types Registry

- [x] `p1` - **ID**: `cpt-cf-resource-group-featstatus-type-management`

- [x] `p1` - `cpt-cf-resource-group-feature-type-management`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Resolve Group Type on Create](#resolve-group-type-on-create)
  - [Resolve Types on Add Membership](#resolve-types-on-add-membership)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Type Code Validation](#type-code-validation)
  - [Type Catalog Lookup](#type-catalog-lookup)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Type Catalog Port and Adapter](#type-catalog-port-and-adapter)
  - [No Type Surface in RG](#no-type-surface-in-rg)
  - [Error Conversion Chain Tests](#error-conversion-chain-tests)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature originally provided full type CRUD, hierarchy-safe updates and type seeding inside RG. Resource group types now live in Types Registry (ADR-002), so the feature is reduced to reading them: a type catalog port with an adapter over the Types Registry SDK, code validation, and the error mapping for unknown or unreachable types. RG has no type endpoints, SDK methods, type service, seeding or bootstrap.

### 1.2 Purpose

Types define the structural rules for the resource group hierarchy: which parent-child relationships are allowed, which resource types can be members, whether a type permits root placement, and the schema of group metadata. These rules are GTS Type Schema trait values (`can_be_root`, `allowed_parent_types`, `allowed_membership_types`) owned by Types Registry. This feature makes them available to the group and membership services without any local copy.

**Requirements**: `cpt-cf-resource-group-fr-types-in-types-registry`, `cpt-cf-resource-group-fr-resolve-types`, `cpt-cf-resource-group-fr-store-type-reference`

**Principles**: `cpt-cf-resource-group-principle-dynamic-types`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-resource-group-actor-instance-administrator` | Registers types in Types Registry (outside RG); RG applies them |
| `cpt-cf-resource-group-actor-apps` | Declares types statically or registers them in Types Registry; creates groups of those types through RG |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — sections 5.1, 8.1
- **Design**: [DESIGN.md](../DESIGN.md) — sections 3.1, 3.2 (Type Catalog), 3.5, 3.7
- **ADR**: [ADR-002](../ADR/ADR-002-types-in-types-registry.md)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.2
- **Dependencies**: Feature 0001 — SDK traits, persistence adapter, error mapping; the Types Registry gear
- **Not applicable**: UX (backend API — no user interface); COMPL (internal platform gear — no regulatory data handling); OPS observability and rollout are managed at the gear infrastructure level (DESIGN §3.7 and platform runbooks); PERF targets are set at the system level in PRD.md NFR section.

## 2. Actor Flows (CDSL)

### Resolve Group Type on Create

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-type-mgmt-resolve-group-type`

**Actor**: `cpt-cf-resource-group-actor-apps`

**Success Scenarios**:
- The type is found in Types Registry and its traits allow the requested placement
- Group `metadata` is valid against the type schema

**Error Scenarios**:
- Type code malformed or without the RG type prefix → Validation error
- Type unknown to Types Registry → type not found (404)
- Types Registry unavailable → ServiceUnavailable (503, retry hint)
- Placement not allowed by the traits → InvalidParentType
- `metadata` invalid against the type schema → Validation error

**Steps**:
1. [x] - `p1` - Validate the type code (see Type Code Validation) before any I/O - `inst-resolve-group-type-1`
2. [x] - `p1` - Resolve the code through the Type Catalog, outside any database transaction - `inst-resolve-group-type-2`
3. [x] - `p1` - **IF** the type is absent from the result → **RETURN** type not found - `inst-resolve-group-type-3`
4. [x] - `p1` - Validate `metadata` against the resolved type schema - `inst-resolve-group-type-4`
5. [x] - `p1` - Derive `is_tenant` from the type code prefix (`TENANT_RG_TYPE_PATH`) - `inst-resolve-group-type-5`
6. [x] - `p1` - Inside the transaction, check root placement against `can_be_root` or the parent type against `allowed_parent_types` - `inst-resolve-group-type-6`
7. [x] - `p1` - Store the type's Registry Reference (UUID) in `resource_group.gts_type_id` - `inst-resolve-group-type-7`

### Resolve Types on Add Membership

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-type-mgmt-resolve-membership-types`

**Actor**: `cpt-cf-resource-group-actor-apps`

**Error Scenarios**:
- Resource type unknown to Types Registry → Validation error (400)
- Group's stored type reference not resolvable → Internal (500)
- Resource type not in the group type's `allowed_membership_types` → Validation error
- Types Registry unavailable → ServiceUnavailable (503)

**Steps**:
1. [x] - `p1` - Load the group row to obtain its stored type reference - `inst-resolve-membership-types-1`
2. [x] - `p1` - Resolve the resource type code and the group's Registry Reference through the Type Catalog, before the transaction starts - `inst-resolve-membership-types-2`
3. [x] - `p1` - **IF** the resource type is absent → **RETURN** Validation error (unknown resource type) - `inst-resolve-membership-types-3`
4. [x] - `p1` - **IF** the group's type is absent → **RETURN** Internal error (data inconsistency) - `inst-resolve-membership-types-4`
5. [x] - `p1` - **IF** the resource type is not in `allowed_membership_types` of the group's type → **RETURN** Validation error - `inst-resolve-membership-types-5`
6. [x] - `p1` - Store the resource type's Registry Reference in `resource_group_membership.gts_type_id` - `inst-resolve-membership-types-6`

## 3. Processes / Business Logic (CDSL)

### Type Code Validation

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-type-mgmt-validate-type-input`

**Input**: A group type code from a create request.

**Output**: Validated code or validation error

**Steps**:
1. [x] - `p1` - **IF** the code is empty → **RETURN** Validation error - `inst-val-input-1`
2. [x] - `p1` - **IF** the code does not have the RG type prefix `gts.cf.core.rg.type.v1~` - `inst-val-input-2`
   1. [x] - `p1` - **RETURN** Validation error: "Type code must start with prefix" - `inst-val-input-2a`
3. [x] - `p1` - **IF** the code exceeds 1024 characters → **RETURN** Validation error - `inst-val-input-3`
4. [x] - `p1` - **RETURN** success - `inst-val-input-4`

### Type Catalog Lookup

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-type-mgmt-catalog-lookup`

**Input**: Lists of GTS id strings and Registry References, resolved together in one batched request (one request per 100 distinct keys).

**Output**: A map from the requested key to a type definition (Registry Reference, GTS id, `can_be_root`, `allowed_parent_types`, `allowed_membership_types`, resolved schema); keys unknown to Types Registry are absent

**Steps**:
1. [x] - `p1` - **IF** the input is empty → **RETURN** an empty map without calling Types Registry - `inst-catalog-lookup-1`
2. [x] - `p1` - Call the Types Registry SDK (platform contract, in-process client) with one fixed projection: effective traits plus resolved schema - `inst-catalog-lookup-2`
3. [x] - `p1` - **IF** the call fails → **RETURN** TypesRegistryUnavailable (maps to 503) - `inst-catalog-lookup-3`
4. [x] - `p1` - **RETURN** the definitions found; RG keeps no cache. A list page reuses the definitions already returned for its `$filter` and asks only for the types it does not have - `inst-catalog-lookup-4`

## 4. States (CDSL)

Not applicable. RG does not own a type lifecycle: type states and revisions belong to Types Registry, and a group's type never changes after creation.

## 5. Definitions of Done

### Type Catalog Port and Adapter

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-type-mgmt-type-catalog`

The system **MUST** read group and membership types only through a type catalog port whose adapter calls the Types Registry SDK, and **MUST** perform every lookup before the database transaction starts.

**Implements**:
- `cpt-cf-resource-group-flow-type-mgmt-resolve-group-type`
- `cpt-cf-resource-group-flow-type-mgmt-resolve-membership-types`
- `cpt-cf-resource-group-algo-type-mgmt-validate-type-input`
- `cpt-cf-resource-group-algo-type-mgmt-catalog-lookup`

**Touches**:
- Types Registry SDK (platform contract)
- Entities: `ResourceGroupType` (read model), `GtsTypePath`

### No Type Surface in RG

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-type-mgmt-no-type-surface`

The system **MUST NOT** expose type endpoints in REST or type methods in the SDK, **MUST NOT** store type definitions, and **MUST NOT** seed or register types at startup. Types are declared statically (tenant type in the RG SDK; user and user-group types in the Account Management SDK) or registered through the Types Registry API.

### Error Conversion Chain Tests

- [x] `p2` - **ID**: `cpt-cf-resource-group-dod-testing-error-conversions`

Extend `domain_unit_test.rs` with FROM-direction error conversions:
- `EnforcerError` (Denied, EvaluationFailed, CompileFailed) -> `DomainError::AccessDenied`
- `sea_orm::DbErr` -> `DomainError::Database`
- `toolkit_db::DbError` -> `DomainError::Database`

## 6. Acceptance Criteria

- [x] Creating a group of a type registered in Types Registry stores the type's Registry Reference (UUID) in `gts_type_id`
- [x] Creating a group of an unknown type returns type not found (404)
- [x] A type code without the `gts.cf.core.rg.type.v1~` prefix, empty, or longer than 1024 characters returns a validation error (400)
- [x] Adding a membership with an unknown resource type returns a validation error (400)
- [x] Adding a membership whose resource type is not in the group type's `allowed_membership_types` is rejected
- [x] Types Registry unavailable on create or add-membership returns 503 with a retry hint and no transaction was opened
- [x] A stored type reference that Types Registry cannot resolve returns 500
- [x] Group `metadata` is validated against the schema of the group's type; an unknown type is an error, not a skipped validation
- [x] REST responses and SDK types use GTS type paths; Registry References are never exposed
- [x] RG exposes no `/types` endpoints and the SDK has no type methods

## 7. Unit Test Plan

Types are supplied by a mock Types Registry in unit, integration and API tests (see DESIGN §4.2). Representative cases:

- Unknown group type on create → type not found
- Unknown membership resource type → validation error
- Types Registry failure → ServiceUnavailable and no transaction opened
- Group whose stored type UUID the mock cannot resolve → Internal
- Parent type not in `allowed_parent_types`, and root placement with `can_be_root = false` → InvalidParentType
- Metadata valid and invalid against the type schema
- Catalog lookup with empty input makes no registry call
- Type code validation: empty, wrong prefix, over 1024 characters
- Error conversions (see the error conversion DoD above)
