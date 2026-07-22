# Feature: Record Persistence & Lifecycle

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Ingest with Idempotency Dedup](#ingest-with-idempotency-dedup)
  - [Batch Ingest with Per-Record Results](#batch-ingest-with-per-record-results)
  - [Deactivation Cascade (Depth-1, Atomic)](#deactivation-cascade-depth-1-atomic)
  - [Fetch Record by Id](#fetch-record-by-id)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Canonical-Field Equality on Conflict](#canonical-field-equality-on-conflict)
  - [Compensation Persistence](#compensation-persistence)
- [4. States (CDSL)](#4-states-cdsl)
  - [UsageRecord Status](#usagerecord-status)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement single insert with 4-tuple dedup](#implement-single-insert-with-4-tuple-dedup)
  - [Implement batch insert with per-record results](#implement-batch-insert-with-per-record-results)
  - [Implement compensation persistence](#implement-compensation-persistence)
  - [Implement the depth-1 atomic deactivation cascade](#implement-the-depth-1-atomic-deactivation-cascade)
  - [Implement point read by id](#implement-point-read-by-id)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-featstatus-record-persistence-implemented`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p1` - `cpt-cf-uc-plugin-feature-record-persistence`

## 1. Feature Context

### 1.1 Overview

The backend write plane — the sole path into `usage_records` — providing single and batch insert with 4-tuple backend deduplication, verbatim append-only compensation rows, and a depth-1 atomic `active → inactive` deactivation cascade.

### 1.2 Purpose

Realizes faithful, order-preserving record persistence with the `usage_records` `UNIQUE (tenant_id, gts_id, idempotency_key, created_at)` constraint as the atomic serialization authority. It anchors idempotency at the storage boundary (silent absorb on identical canonical fields, `IdempotencyConflict` on a mismatch), persists compensation rows through the same insert path with no netting, and flips a record plus its depth-1 active compensations to inactive in one transaction. It is the allocation target for the bulk-ingestion throughput NFR.

**Requirements**: `cpt-cf-uc-plugin-fr-record-persistence`, `cpt-cf-uc-plugin-fr-idempotent-dedup`, `cpt-cf-uc-plugin-fr-compensation-persistence`, `cpt-cf-uc-plugin-fr-deactivation`, `cpt-cf-uc-plugin-nfr-ingestion-throughput`

**Principles**: `cpt-cf-uc-plugin-principle-pure-persistence` (realized; owned by Feature 1)

**Constraints**: `cpt-cf-uc-plugin-constraint-dedup-key-preservation`

### 1.3 Actors

| Actor                                | Role in Feature                                                                                                                               |
| ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-uc-plugin-actor-plugin-host` | Calls the record SPI methods with already-authorized, structurally-validated records carrying the gateway-derived `id` and `idempotency_key`. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Decomposition**: `cpt-cf-uc-plugin-feature-record-persistence`
- **Design elements**: `cpt-cf-uc-plugin-component-record-store`, `cpt-cf-uc-plugin-dbtable-usage-records`
- **Sequences**: `cpt-cf-uc-plugin-seq-ingest-dedup`, `cpt-cf-uc-plugin-seq-ingest-batch`, `cpt-cf-uc-plugin-seq-deactivate-cascade`
- **Use cases**: `cpt-cf-uc-plugin-usecase-ingest-dedup`
- **Dependencies**: `cpt-cf-uc-plugin-feature-foundation`

### 1.5 Out of Scope

- Reading records back for aggregation or keyset list — Feature 3 (`cpt-cf-uc-plugin-feature-query-aggregation`).
- Schema DDL for `usage_records` (hypertable, dedup UNIQUE, FK, indexes) — created by Feature 1's Schema Migrations; this feature is the row-writer.
- Chunk expiry / retention of stored rows — Feature 5 (`cpt-cf-uc-plugin-feature-retention`).

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-uc-plugin-usecase-ingest-dedup`

### Ingest with Idempotency Dedup

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-flow-record-ingest`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- A fresh 4-tuple is inserted with `status = Active` and returned.
- A same-4-tuple retry with identical canonical fields returns the stored row (silent absorb).

**Error Scenarios**:

- A same-4-tuple submission with differing canonical fields returns `IdempotencyConflict`.
- The near-impossible retention race between the conflicting insert and the read-back returns a retryable `Transient`.

**Steps**:

1. [ ] - `p1` - Host calls `create_usage_record` with an authorized, structurally-valid record - `inst-ing-1`
2. [ ] - `p1` - DB: INSERT into `usage_records` ON CONFLICT (tenant_id, gts_id, idempotency_key, created_at) DO NOTHING RETURNING - `inst-ing-2`
3. [ ] - `p1` - **IF** a row is returned (no conflict) - `inst-ing-3`
   1. [ ] - `p1` - **RETURN** the inserted `UsageRecord` (status Active) - `inst-ing-3a`
4. [ ] - `p1` - **ELSE** the 4-tuple already exists; read the stored row and compare canonical fields (`value`, `resource_ref`, `subject_ref`, `corrects_id`, `metadata`) via `cpt-cf-uc-plugin-algo-record-canonical-equal` - `inst-ing-4`
   1. [ ] - `p1` - **IF** all canonical fields equal - `inst-ing-4a`
      1. [ ] - `p1` - **RETURN** the stored `UsageRecord` (silent absorb) - `inst-ing-4a1`
   2. [ ] - `p1` - **ELSE** - `inst-ing-4b`
      1. [ ] - `p1` - **RETURN** `IdempotencyConflict` - `inst-ing-4b1`
5. [ ] - `p1` - **IF** the conflicting row's chunk was dropped by retention before read-back - `inst-ing-5`
   1. [ ] - `p1` - **RETURN** a retryable `Transient` - `inst-ing-5a`

> A same `idempotency_key` with a different `created_at` is a distinct 4-tuple and therefore a fresh insert — two distinct records with distinct ids (ADR-0014); `created_at` is part of the dedup key, not a compared canonical field.

### Batch Ingest with Per-Record Results

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-flow-record-batch-ingest`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- One outcome per input record is returned, positionally aligned to input order.

**Error Scenarios**:

- A conflict or rejection on one record does not fail the others.

**Steps**:

1. [ ] - `p1` - Host calls `create_usage_records` with a list of records - `inst-bat-1`
2. [ ] - `p1` - DB: Multi-row INSERT ON CONFLICT (tenant_id, gts_id, idempotency_key, created_at) DO NOTHING in a single write - `inst-bat-2`
3. [ ] - `p1` - **FOR EACH** input record - `inst-bat-3`
   1. [ ] - `p1` - Resolve its per-row outcome (inserted / absorbed / conflict) preserving input position - `inst-bat-3a`
4. [ ] - `p1` - **RETURN** the per-record results positionally aligned to the input - `inst-bat-4`

### Deactivation Cascade (Depth-1, Atomic)

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-flow-record-deactivate`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- The active target and its depth-1 active compensations flip to inactive in one transaction; no other field changes.

**Error Scenarios**:

- A missing target returns `UsageRecordNotFound`.
- An already-inactive target returns `UsageRecordAlreadyInactive`.

**Steps**:

1. [ ] - `p1` - Host calls `deactivate_usage_record(id)` - `inst-dea-1`
2. [ ] - `p1` - DB: BEGIN - `inst-dea-2`
3. [ ] - `p1` - DB: UPDATE the active target row plus its depth-1 active compensations to `status = inactive` - `inst-dea-3`
4. [ ] - `p1` - **IF** rows were flipped - `inst-dea-4`
   1. [ ] - `p1` - DB: COMMIT and **RETURN** Ok - `inst-dea-4a`
5. [ ] - `p1` - **IF** no matching row exists - `inst-dea-5`
   1. [ ] - `p1` - DB: ROLLBACK and **RETURN** `UsageRecordNotFound` - `inst-dea-5a`
6. [ ] - `p1` - **IF** the target exists but is already inactive - `inst-dea-6`
   1. [ ] - `p1` - DB: ROLLBACK and **RETURN** `UsageRecordAlreadyInactive` - `inst-dea-6a`

### Fetch Record by Id

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-flow-record-point-read`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- `get_usage_record(id)` resolves and returns the single matching row.

**Error Scenarios**:

- An absent `id` returns `UsageRecordNotFound`.

**Steps**:

1. [ ] - `p2` - Host calls `get_usage_record(id)` - `inst-get-1`
2. [ ] - `p2` - DB: SELECT the row by the gateway-derived `id` (the leading column of the `(id, created_at)` PK; uniqueness holds because `id = UUIDv5` of the full 4-tuple, ADR-0013/ADR-0014) - `inst-get-2`
3. [ ] - `p2` - **IF** no row matches - `inst-get-3`
   1. [ ] - `p2` - **RETURN** `UsageRecordNotFound` - `inst-get-3a`
4. [ ] - `p2` - **ELSE RETURN** the `UsageRecord` - `inst-get-4`

## 3. Processes / Business Logic (CDSL)

### Canonical-Field Equality on Conflict

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-algo-record-canonical-equal`

**Input**: The incoming record and the stored row that share the 4-tuple dedup key.

**Output**: A dedup verdict — silent absorb vs `IdempotencyConflict`.

**Steps**:

1. [ ] - `p1` - Compare `value` - `inst-can-1`
2. [ ] - `p1` - Compare `resource_ref` (`resource_id` / `resource_type`) - `inst-can-2`
3. [ ] - `p1` - Compare `subject_ref` (`subject_id` / `subject_type`) - `inst-can-3`
4. [ ] - `p1` - Compare `corrects_id` - `inst-can-4`
5. [ ] - `p1` - Compare `metadata` (byte-for-byte) - `inst-can-5`
6. [ ] - `p1` - Compare `id` as a defensive tautology (equal by construction for a well-formed request; guards a corrupted stored row) - `inst-can-6`
7. [ ] - `p1` - **IF** all compared fields equal - `inst-can-7`
   1. [ ] - `p1` - **RETURN** silent absorb (return the stored row) - `inst-can-7a`
8. [ ] - `p1` - **ELSE** - `inst-can-8`
   1. [ ] - `p1` - **RETURN** `IdempotencyConflict` - `inst-can-8a`

> `created_at`, the dedup-key fields, and the server-managed `status` / `ingested_at` are excluded from the comparison (`created_at` and the key are already matched by the conflict; `status` / `ingested_at` are server-owned).

### Compensation Persistence

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-algo-record-compensation`

**Input**: A caller-supplied record carrying a signed `value` and an optional `corrects_id`.

**Output**: A persisted compensation row, stored verbatim.

**Steps**:

1. [ ] - `p1` - Accept the record on the ordinary insert path (`cpt-cf-uc-plugin-flow-record-ingest`), with no dedicated compensate operation - `inst-comp-1`
2. [ ] - `p1` - Persist the signed `value` and `corrects_id` verbatim - `inst-comp-2`
3. [ ] - `p1` - Compute and validate no netting - `inst-comp-3`
4. [ ] - `p1` - **RETURN** the stored compensation `UsageRecord` - `inst-comp-4`

## 4. States (CDSL)

### UsageRecord Status

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-state-record-status`

**States**: active, inactive

**Initial State**: active (set on first accept)

**Transitions**:

1. [ ] - `p1` - **FROM** active **TO** inactive **WHEN** `deactivate_usage_record` targets the row (or its depth-1 parent is deactivated) - `inst-rst-1`

> The transition is one-way (ADR-0005): there is no `inactive → active` reactivation and no other field is mutated by the transition.

## 5. Definitions of Done

### Implement single insert with 4-tuple dedup

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-record-single-insert`

The system **MUST** persist one record via `INSERT … ON CONFLICT (tenant_id, gts_id, idempotency_key, created_at) DO NOTHING`, setting `status = Active` on first accept, storing `metadata` byte-for-byte, and resolving a conflict by canonical-field equality into silent absorb or `IdempotencyConflict`.

**Implements**:

- `cpt-cf-uc-plugin-flow-record-ingest`
- `cpt-cf-uc-plugin-algo-record-canonical-equal`

**Constraints**: `cpt-cf-uc-plugin-constraint-dedup-key-preservation`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`
- Entities: `UsageRecord`

### Implement batch insert with per-record results

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-record-batch-insert`

The system **MUST** persist a batch as a single multi-row write returning one outcome per input record in input order, where a conflict on one record does not fail the others. The batch write path **MUST** sustain the parent ingestion envelope (≥ 10,000 records/sec) without breaching the parent ingestion-latency budget.

**Implements**:

- `cpt-cf-uc-plugin-flow-record-batch-ingest`

**Constraints**: `cpt-cf-uc-plugin-nfr-ingestion-throughput`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`

### Implement compensation persistence

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-record-compensation`

The system **MUST** persist a compensation entry (signed `value` plus optional `corrects_id`) verbatim through the ordinary insert path, with no dedicated compensate operation and no netting computed or validated.

**Implements**:

- `cpt-cf-uc-plugin-algo-record-compensation`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`
- Entities: `UsageRecord`

### Implement the depth-1 atomic deactivation cascade

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-record-deactivation`

The system **MUST** flip an active target row and its depth-1 active compensations to inactive in a single transaction, mutating no other field, returning `UsageRecordNotFound` on a missing target and `UsageRecordAlreadyInactive` on an already-inactive target.

**Implements**:

- `cpt-cf-uc-plugin-flow-record-deactivate`
- `cpt-cf-uc-plugin-state-record-status`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`

### Implement point read by id

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-record-point-read`

The system **MUST** implement `get_usage_record` resolving a single row by the gateway-derived `id` and returning `UsageRecordNotFound` when absent.

**Implements**:

- `cpt-cf-uc-plugin-flow-record-point-read`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`

## 6. Acceptance Criteria

- [ ] A record is persisted and retrievable by `id`; a second submission with the same dedup key and identical canonical fields yields a single stored record (silent absorb).
- [ ] A submission with the same dedup key but differing canonical fields is rejected with `IdempotencyConflict`.
- [ ] A submission with the same `idempotency_key` but a different `created_at` is stored as a distinct record (4-tuple identity, ADR-0014).
- [ ] Batch ingestion returns one outcome per input record in input order; a conflict on one record does not fail the others.
- [ ] A compensation entry (signed value plus reference to the corrected record) is persisted verbatim through the ingestion path without mutating the corrected record.
- [ ] Deactivating an active record flips it and its depth-1 active compensations to inactive in a single transaction with no other field changed; a missing target returns not-found and an already-inactive target returns already-inactive.
- [ ] `metadata` is persisted byte-for-byte; the plugin computes no business delta or netting.
- [ ] The batch write path sustains ≥ 10,000 records/sec within the parent throughput-profile envelope.

## 7. Non-Applicable Concerns

- **Security — Authentication, Authorization, Input Validation (SEC-FDESIGN-001..003)**: Not applicable — records arrive already authorized and structurally validated; the plugin enforces the value-sign matrix only as a structural precondition and re-validates nothing (`cpt-cf-uc-plugin-principle-pure-persistence`).
- **Security — Audit Trail (SEC-FDESIGN-005)**: Not applicable — no auditable user actions are produced at the storage tier; attribution is gear-owned.
- **Query / Read patterns (DATA-FDESIGN-001 read side)**: Not applicable here — aggregation and keyset raw reads are Feature 3; this feature owns only the write plane and the by-`id` point read.
- **Data Retention (DATA-FDESIGN-004)**: Addressed by Feature 5 (`cpt-cf-uc-plugin-feature-retention`); this feature is only coupled to it via retention-bounded dedup-key preservation.
- **Usability (UX) / Compliance (COMPL)**: Not applicable — no user interface and no plugin-level regulatory obligation; inherited from Feature 1's dispositions.
- **Observability (OPS-FDESIGN-001)**: Instrumented cross-cuttingly by Feature 6 (insert-duration histogram, dedup-absorbed/stale counters, batch-rows histogram, compensations counter).
