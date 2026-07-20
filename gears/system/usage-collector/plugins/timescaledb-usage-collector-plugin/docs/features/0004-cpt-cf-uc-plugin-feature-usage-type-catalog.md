# Feature: Usage-Type Catalog & Referential Integrity

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Create Usage Type](#create-usage-type)
  - [Delete Usage Type](#delete-usage-type)
  - [Get & List Usage Types](#get--list-usage-types)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement catalog create](#implement-catalog-create)
  - [Implement catalog point read](#implement-catalog-point-read)
  - [Implement catalog keyset list](#implement-catalog-keyset-list)
  - [Implement catalog delete with in-database FK rejection](#implement-catalog-delete-with-in-database-fk-rejection)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-featstatus-usage-type-catalog-implemented`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p1` - `cpt-cf-uc-plugin-feature-usage-type-catalog`

## 1. Feature Context

### 1.1 Overview

The sole store for the usage-type catalog and the in-database referential integrity between records and types: create / get / list / delete over `usage_type_catalog`, backed by the `ON DELETE RESTRICT` foreign key that makes orphaned records structurally impossible.

### 1.2 Purpose

Owns `create_usage_type` (insert keyed on the `gts_id` primary key; collision → `UsageTypeAlreadyExists`), `get_usage_type` (row or `UsageTypeNotFound`), `list_usage_types` (keyset-paginated, ordered by `gts_id`), and `delete_usage_type` (attempts the delete and lifts the `usage_records.gts_id → usage_type_catalog.gts_id` FK violation to `UsageTypeReferenced`). Because both tables live in the same database, the FK rejection is atomic with the delete attempt. `kind` and `metadata_fields` are stored verbatim; the plugin derives and validates no semantics.

**Requirements**: `cpt-cf-uc-plugin-fr-usage-type-catalog`, `cpt-cf-uc-plugin-fr-referential-integrity`

**Principles**: `cpt-cf-uc-plugin-principle-pure-persistence` (realized; owned by Feature 1)

**Constraints**: `cpt-cf-uc-plugin-constraint-fk-referential-integrity`

### 1.3 Actors

| Actor                                | Role in Feature                                                                    |
| ------------------------------------ | ---------------------------------------------------------------------------------- |
| `cpt-cf-uc-plugin-actor-plugin-host` | Calls the catalog SPI methods with authorized usage-type payloads and identifiers. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Decomposition**: `cpt-cf-uc-plugin-feature-usage-type-catalog`
- **Design elements**: `cpt-cf-uc-plugin-component-catalog-store`, `cpt-cf-uc-plugin-dbtable-usage-type-catalog`
- **Sequences**: `cpt-cf-uc-plugin-seq-create-type`, `cpt-cf-uc-plugin-seq-delete-type-fk`
- **Use cases**: `cpt-cf-uc-plugin-usecase-delete-referenced-type`
- **Dependencies**: `cpt-cf-uc-plugin-feature-foundation`

### 1.5 Out of Scope

- Validation of metadata-key well-formedness or membership, and counter/gauge derivation — the inherited pure-persistence posture owned by Feature 1 and enforced upstream by the gear core.
- The `usage_records` FK column definition (DDL) — created by Feature 1's Schema Migrations; this feature owns the catalog-side delete behavior the FK backs.
- The keyset look-ahead / cursor-encoding mechanism itself — the design-level keyset sequence `cpt-cf-uc-plugin-seq-list-keyset` (realized for records by Feature 3); this feature reuses that pattern against the Catalog Store, keyed on `gts_id`.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-uc-plugin-usecase-delete-referenced-type`

### Create Usage Type

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-flow-catalog-create`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- A catalog row is inserted keyed on `gts_id`; `kind` and `metadata_fields` are stored verbatim.

**Error Scenarios**:

- A `gts_id` primary-key collision returns `UsageTypeAlreadyExists`.

**Steps**:

1. [ ] - `p1` - Host calls `create_usage_type(usage_type)` - `inst-ct-1`
2. [ ] - `p1` - DB: INSERT into `usage_type_catalog` (gts_id PK, kind, metadata_fields) - `inst-ct-2`
3. [ ] - `p1` - **IF** the `gts_id` is free - `inst-ct-3`
   1. [ ] - `p1` - **RETURN** the inserted `UsageType` - `inst-ct-3a`
4. [ ] - `p1` - **ELSE** a primary-key collision - `inst-ct-4`
   1. [ ] - `p1` - **RETURN** `UsageTypeAlreadyExists` - `inst-ct-4a`

### Delete Usage Type

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-flow-catalog-delete`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- An unreferenced type is deleted; its identifier becomes available for re-registration.

**Error Scenarios**:

- A type referenced by any usage record is rejected with `UsageTypeReferenced`, atomically with the delete attempt.

**Steps**:

1. [ ] - `p1` - Host calls `delete_usage_type(gts_id)` - `inst-dt-1`
2. [ ] - `p1` - DB: DELETE the catalog row for the given `gts_id` - `inst-dt-2`
3. [ ] - `p1` - **IF** no usage record references the type - `inst-dt-3`
   1. [ ] - `p1` - **RETURN** Ok (deleted) - `inst-dt-3a`
4. [ ] - `p1` - **ELSE** the `ON DELETE RESTRICT` FK fires - `inst-dt-4`
   1. [ ] - `p1` - Lift the FK violation and **RETURN** `UsageTypeReferenced` - `inst-dt-4a`

### Get & List Usage Types

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-flow-catalog-read`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- `get_usage_type` returns the row by `gts_id`; `list_usage_types` returns a keyset-paginated page ordered by `gts_id`.

**Error Scenarios**:

- `get_usage_type` on an absent identifier returns `UsageTypeNotFound`.

**Steps**:

1. [ ] - `p1` - Host calls `get_usage_type(gts_id)` - `inst-gt-1`
2. [ ] - `p1` - DB: SELECT the catalog row by primary key `gts_id` - `inst-gt-2`
3. [ ] - `p1` - **IF** the row is absent - `inst-gt-3`
   1. [ ] - `p1` - **RETURN** `UsageTypeNotFound` - `inst-gt-3a`
4. [ ] - `p1` - **ELSE RETURN** the `UsageType` - `inst-gt-4`
5. [ ] - `p1` - For `list_usage_types`, apply the design-level keyset list pattern (`cpt-cf-uc-plugin-seq-list-keyset`) ordered by `gts_id` and **RETURN** the `Page` - `inst-gt-5`

## 3. Processes / Business Logic (CDSL)

Not applicable — catalog operations are direct primary-key inserts, reads, and a delete whose referential-integrity decision is made by the database FK, not by plugin-side business logic. The keyset list pattern is the design-level sequence `cpt-cf-uc-plugin-seq-list-keyset`, reused here against the Catalog Store keyed on `gts_id`; this feature introduces no additional standalone process.

## 4. States (CDSL)

Not applicable — a usage-type catalog row has no lifecycle state machine; it exists (created) or is removed (deleted). Deletion is gated by the FK, not by a status transition.

## 5. Definitions of Done

### Implement catalog create

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-catalog-create`

The system **MUST** insert a catalog row keyed on the `gts_id` primary key, storing `kind` and `metadata_fields` verbatim, and **MUST** return `UsageTypeAlreadyExists` on a primary-key collision.

**Implements**:

- `cpt-cf-uc-plugin-flow-catalog-create`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-catalog-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-type-catalog`
- Entities: `UsageType`

### Implement catalog point read

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-catalog-read`

The system **MUST** implement `get_usage_type` returning the row by `gts_id` or `UsageTypeNotFound` when absent.

**Implements**:

- `cpt-cf-uc-plugin-flow-catalog-read`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-catalog-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-type-catalog`
- Entities: `UsageType`

### Implement catalog keyset list

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-catalog-list`

The system **MUST** implement `list_usage_types` as a keyset-paginated list ordered by `gts_id`, reusing the raw-list keyset pattern against the Catalog Store.

**Implements**:

- `cpt-cf-uc-plugin-flow-catalog-read`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-catalog-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-type-catalog`
- Entities: `Page`, `CursorV1`

### Implement catalog delete with in-database FK rejection

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-catalog-delete-fk`

The system **MUST** attempt the delete and lift the `usage_records.gts_id → usage_type_catalog.gts_id` `ON DELETE RESTRICT` FK violation to `UsageTypeReferenced`, atomically with the delete attempt, so orphaned records are structurally impossible. The catalog is reference data and **MUST NOT** be retention-bounded.

**Implements**:

- `cpt-cf-uc-plugin-flow-catalog-delete`

**Constraints**: `cpt-cf-uc-plugin-constraint-fk-referential-integrity`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-catalog-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-type-catalog`

## 6. Acceptance Criteria

- [ ] Creating a usage type whose `gts_id` already exists returns `UsageTypeAlreadyExists`; `kind` and `metadata_fields` are stored verbatim.
- [ ] `get_usage_type` returns the row by `gts_id`, or `UsageTypeNotFound` when absent.
- [ ] `list_usage_types` returns keyset-paginated pages ordered by `gts_id`, honoring the supplied cursor.
- [ ] Deleting a usage type referenced by any usage record is rejected atomically with a `UsageTypeReferenced` error and the type remains.
- [ ] Deleting an unreferenced type succeeds and frees its identifier for re-registration.
- [ ] Orphaned usage records are structurally impossible, enforced by the in-database FK rather than by a gear-side pre-read.

## 7. Non-Applicable Concerns

- **Security — Authentication, Authorization, Input Validation (SEC-FDESIGN-001..003)**: Not applicable — catalog calls arrive already authorized and structurally validated; the plugin stores `kind` and `metadata_fields` verbatim and validates no semantics (`cpt-cf-uc-plugin-principle-pure-persistence`).
- **Performance — Hot Paths (PERF-FDESIGN-001)**: Not applicable — catalog operations are low-frequency reference-data CRUD, not on the ingestion or aggregation hot path; the catalog is small and primary-key addressed.
- **Data Retention (DATA-FDESIGN-004)**: Explicitly excluded — the catalog is reference data and is not retention-bounded (see Feature 5, which retains only `usage_records`).
- **State Management (ARCH-FDESIGN-005)**: Not applicable — a catalog row has no lifecycle state machine (see §4).
- **Usability (UX) / Compliance (COMPL)**: Not applicable — no user interface and no plugin-level regulatory obligation; inherited from Feature 1's dispositions.
- **Observability (OPS-FDESIGN-001)**: Instrumented cross-cuttingly by Feature 6 (`uc_timescaledb_usage_type_catalog_size` gauge and `uc_timescaledb_usage_type_referenced_total` counter).
