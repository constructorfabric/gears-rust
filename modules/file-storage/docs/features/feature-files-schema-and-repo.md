<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: Files Schema and Repo

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-files-schema-and-repo`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-file-storage-feature-files-schema-and-repo`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Race-Detection Conditional Mutation](#race-detection-conditional-mutation)
  - [Supersession Transaction](#supersession-transaction)
  - [System-Context Self-Heal Repair](#system-context-self-heal-repair)
  - [MAX-Merge Upload Expires At](#max-merge-upload-expires-at)
- [4. States (CDSL)](#4-states-cdsl)
  - [Files Row Lifecycle](#files-row-lifecycle)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Apply Schema Migration](#apply-schema-migration)
  - [Implement Files Repo Trait](#implement-files-repo-trait)
  - [Implement System-Context Repair Path](#implement-system-context-repair-path)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Own the persistent metadata layer — the `file_storage.files` table and the Files Repo component that wraps it. This feature lands the DDL, the conditional UPDATE primitive with `(etag, updated_at[, xmin])` race detection, the partial unique index for last-write-wins on logical paths, the system-context repair API used by lazy self-healing in `read_file`, and the `Deleting` transient status that supports the 2-phase delete flow.

The schema reflects the P1 finalized design:

- `etag` is the **raw S3 ETag** (`cpt-cf-file-storage-constraint-etag-content-only`) — content fingerprint only.
- `version_id` is a new column that mirrors S3 VersionId for backends with `versioning = true` (NULL otherwise; ADR-0005).
- `gts_file_type` is DB-only (`cpt-cf-file-storage-constraint-meta-mirrored-via-put-meta`).
- `updated_at` is a column refreshed to `NOW()` on every successful UPDATE; used together with `etag` (and optional `xmin` on Postgres) for race detection in conditional UPDATE WHERE clauses, and exposed in `FileInfo.updated_at` as the user-visible "last modified" timestamp.
- `created_at` and `updated_at` are **DB-managed** timestamps. They track DB-row events (INSERT / UPDATE) — never S3 events. S3's `Last-Modified` header (and any other S3-side timestamp) is **never** captured into either column. `reconcile` pulls `etag`, `version_id`, content-type, content-disposition, content-length, and `x-amz-meta-*` from the HEAD response — but the row's `created_at` stays put and `updated_at` is set to `NOW()` by the UPDATE itself, not derived from any S3 header.
- The `Deleting` status is a transient operational state, not a soft-delete tombstone (`cpt-cf-file-storage-constraint-no-soft-delete`).

### 1.2 Purpose

Every other write in the module — initial upload INSERT, variant-B re-upload UPDATE for `upload_expires_at`, `reconcile`, `put_file_info`, `delete_file` (Phase 1 + Phase 3) — is a conditional UPDATE / DELETE through this repo. The supersession transaction implements last-write-wins on `(tenant, backend, file_path)` for fresh-`file_id` overwrites.

**Requirements**: `cpt-cf-file-storage-fr-metadata-storage`, `cpt-cf-file-storage-fr-conditional-requests`, `cpt-cf-file-storage-nfr-metadata-latency`

**Principles**: `cpt-cf-file-storage-principle-optimistic-concurrency`, `cpt-cf-file-storage-principle-atomic-metadata`, `cpt-cf-file-storage-principle-file-id-address`

### 1.3 Actors

- `cpt-cf-file-storage-actor-cf-modules` — every consumer that reads or mutates file metadata goes through this repo (transitively, via the SDK)

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (§3.7 Database schemas, §2.1 Optimistic Concurrency)
- **ADR**: [ADR-0001](../ADR/0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md), [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md), [ADR-0005](../ADR/0005-cpt-cf-file-storage-adr-versioning-and-aba.md)
- **Migration**: [migration.sql](../migration.sql)
- **Decomposition**: [DECOMPOSITION.md §2.2](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-file-storage-feature-module-foundation`

## 2. Actor Flows (CDSL)

This feature has no user-facing actor flows. It is a persistence component consumed by every other feature.

## 3. Processes / Business Logic (CDSL)

### Race-Detection Conditional Mutation

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-files-schema-and-repo-conditional-mutation`

**Input**: `(file_id, captured_etag, captured_updated_at, [captured_xmin], expected_status, mutation_payload)`

**Output**: `Ok(updated FileInfo)` on success; `Err(NotFound)` / `Err(InvalidStatusTransition)` / `0-rows` (caller-driven retry signal) otherwise

**Steps**:

1. [ ] - `p1` - DB: UPDATE files SET <fields>, updated_at = NOW() WHERE id = $file_id AND etag = $captured_etag AND updated_at = $captured_updated_at [AND xmin = $captured_xmin] [AND status = $expected_status] - `inst-cmut-1`
2. [ ] - `p1` - **IF** affected_rows == 1 → Ok(refreshed FileInfo) - `inst-cmut-2`
3. [ ] - `p1` - **ELSE** signal "race detected" to the caller; the caller (e.g. `reconcile`, `put_file_info`) decides whether to retry from a fresh SELECT or surface `Conflict` after exhausting attempts - `inst-cmut-3`

For DB engines that do not expose a transaction-id system column (e.g. SQLite), the `xmin` clause is omitted; `(etag, updated_at)` alone provides race detection. The narrow window is documented in `cpt-cf-file-storage-constraint-no-meta-cas` for metadata-only mutations under contention.

### Supersession Transaction

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-files-schema-and-repo-supersession`

**Input**: `(winner_file_id, owner_address = (tenant_id, backend_id, file_path))`

**Output**: Committed `FileInfo` for the winner; previously-uploaded sibling rows for the same address marked / queued for orphan delete

**Steps**:

1. [ ] - `p1` - DB: BEGIN transaction - `inst-super-1`
2. [ ] - `p1` - DB: conditional UPDATE on `file_storage.files` flipping the winner row from `pending_upload` to `uploaded` (this is part of the `reconcile` flow) - `inst-super-2`
3. [ ] - `p1` - **IF** affected_rows != 1 → ROLLBACK; signal race per the conditional-mutation contract - `inst-super-3`
4. [ ] - `p1` - DB: DELETE other uploaded rows for the same `(tenant_id, backend_id, file_path)`, returning their `id` and `backend_id` for orphan-delete bookkeeping - `inst-super-4`
5. [ ] - `p1` - **FOR EACH** loser row returned, enqueue `(loser.backend_id, loser.id, eligible_at = NOW() + orphan_grace)` for asynchronous orphan-delete - `inst-super-5`
6. [ ] - `p1` - **TRY** COMMIT - `inst-super-6`
7. [ ] - `p1` - **CATCH** unique_violation on `files_tenant_backend_path_uploaded_uq` → ROLLBACK; **RETURN** Err(Conflict) - `inst-super-7`
8. [ ] - `p1` - **RETURN** Ok(refreshed FileInfo) - `inst-super-8`

Variant-B re-upload (`create_presigned_overwrite_url` + `reconcile`) does NOT trigger supersession — the `file_id` is preserved, and there is no sibling to delete.

### System-Context Self-Heal Repair

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-files-schema-and-repo-self-heal-repair`

**Input**: `(file_id, s3_etag, s3_version_id, observed_size_bytes, content_type, content_disposition, custom_metadata_from_s3)`

**Output**: Updated `FileInfo` on successful repair, or no-op (someone else repaired first)

**Steps**:

1. [ ] - `p1` - DB: SELECT row by `id` AND status = 'uploaded'; capture `(etag_db, version_id_db, updated_at_db, gts_file_type_db)` — note `gts_file_type` is preserved verbatim - `inst-heal-1`
2. [ ] - `p1` - **IF** row missing → return Err(NotFound) - `inst-heal-2`
3. [ ] - `p1` - Build new mirrored values from S3: name from Content-Disposition (URL-decoded), mime_type from Content-Type, custom_metadata from `custom_metadata_from_s3`. Preserve `gts_file_type_db`. - `inst-heal-3`
4. [ ] - `p1` - DB: UPDATE files SET etag = $s3_etag, version_id = $s3_version_id, name = $new_name, mime_type = $new_mime_type, custom_metadata = $new_custom_metadata, size_bytes = $observed_size_bytes, updated_at = NOW() WHERE id = $file_id AND etag = $etag_db AND updated_at = $updated_at_db - `inst-heal-4`
5. [ ] - `p1` - **IF** affected_rows == 0 → return Ok(no-op — concurrent repair won) - `inst-heal-5`
6. [ ] - `p1` - **RETURN** Ok(refreshed FileInfo) - `inst-heal-6`

### MAX-Merge Upload Expires At

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-files-schema-and-repo-max-merge-expires`

**Input**: `(file_id, new_expires_at)` from `create_presigned_overwrite_url`

**Output**: `Ok(())` after the row's `upload_expires_at` is set to `MAX(current, new)`

**Steps**:

1. [ ] - `p1` - DB: `UPDATE files SET upload_expires_at = (CASE WHEN upload_expires_at IS NULL OR upload_expires_at < $new_expires_at THEN $new_expires_at ELSE upload_expires_at END), updated_at = NOW() WHERE id = $file_id` - `inst-max-1`
2. [ ] - `p1` - **RETURN** Ok(()) - `inst-max-2`

This guarantees that issuing a new variant-B re-upload presign never shortens an outstanding window for an in-flight URL.

## 4. States (CDSL)

### Files Row Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-file-storage-state-files-schema-and-repo-row`

**States**: `pending_upload`, `uploaded`, `deleting`

**Initial State**: `pending_upload` (set by `INSERT` in `create_presigned_url` for initial uploads only; variant-B re-upload preserves the row's existing `uploaded` status)

**Transitions**:

1. [ ] - `p1` - **FROM** `pending_upload` **TO** `uploaded` **WHEN** `reconcile(file_id)` HEADs the backend and runs the conditional UPDATE (with supersession when a previously-uploaded sibling exists for the same logical address) - `inst-state-1`
2. [ ] - `p1` - **FROM** `pending_upload` **TO** (deleted row) **WHEN** P2 GC sweep finds `upload_expires_at < NOW()` - `inst-state-2`
3. [ ] - `p1` - **FROM** `uploaded` **TO** `deleting` **WHEN** `delete_file(file_id, etag?)` Phase 1 conditional UPDATE succeeds - `inst-state-3`
4. [ ] - `p1` - **FROM** `deleting` **TO** (deleted row) **WHEN** `delete_file` Phase 3 hard-DELETE runs after a successful Phase 2 backend cleanup - `inst-state-4`
5. [ ] - `p1` - **FROM** `uploaded` **TO** `uploaded` **WHEN** `reconcile(file_id)` against a row whose backend state drifted runs the drift-correction UPDATE (etag, version_id, S3-mirrored metadata rotated; `gts_file_type` preserved) - `inst-state-5`
6. [ ] - `p1` - **FROM** `uploaded` **TO** `uploaded` **WHEN** `put_file_info(file_id, update, etag?)` issues `CopyObject` self-copy and rotates etag, version_id, and mirrored metadata atomically - `inst-state-6`

A row stuck in `deleting` after a persistent backend failure during `delete_file` Phase 2 is reaped by a future P2 GC sweep; P1 leaves it visible to ops without auto-recovery. `Deleting` is a transient operational state, NOT a soft-delete tombstone (`cpt-cf-file-storage-constraint-no-soft-delete`).

## 5. Definitions of Done

### Apply Schema Migration

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-files-schema-and-repo-migration`

The system **MUST** apply the DDL described in [migration.sql](../migration.sql): the `file_storage.files` table with the P1 columns (including `version_id` nullable and `updated_at` for race detection) and the three P1 indexes (`files_tenant_backend_path_uploaded_uq` partial unique, `files_owner_lookup_idx`, `files_created_idx` with the `id` tiebreaker for stable cursor pagination).

**Implements**:

- `cpt-cf-file-storage-state-files-schema-and-repo-row`

**Constraints**: `cpt-cf-file-storage-constraint-static-config-p1`, `cpt-cf-file-storage-constraint-etag-content-only`, `cpt-cf-file-storage-constraint-no-soft-delete`

**Touches**:

- DB: `file_storage.files`
- Migration script: P1 baseline

### Implement Files Repo Trait

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-files-schema-and-repo-trait`

The system **MUST** expose a `FilesRepo` trait with: `insert_pending` (initial upload), `get_by_id`, `select_for_reconcile` (captures `(etag, version_id, updated_at[, xmin])`), `sync_meta` (the reconcile UPDATE that pulls etag, version_id, and S3-mirrored fields from the HEAD response), `merge_meta_after_copy` (the UPDATE that pairs with `CopyObject` self-copy on `PUT /meta`), `mark_deleting` (Phase 1 of `delete_file`, with optional etag clause), `purge_deleting` (Phase 3 of `delete_file`), `max_merge_upload_expires_at` (variant-B re-upload), `list_paginated`. Every conditional UPDATE **MUST** carry the `(etag, updated_at[, xmin])` race-detection clause.

**Implements**:

- `cpt-cf-file-storage-algo-files-schema-and-repo-conditional-mutation`
- `cpt-cf-file-storage-algo-files-schema-and-repo-supersession`
- `cpt-cf-file-storage-algo-files-schema-and-repo-max-merge-expires`

**Constraints**: `cpt-cf-file-storage-constraint-server-minted-file-id`

**Touches**:

- DB: `file_storage.files`
- Crate: `file-storage` (component: Files Repo)

### Implement System-Context Repair Path

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-files-schema-and-repo-repair`

The system **MUST** expose a system-context `repair_meta` method on the repo that runs without a `SecurityContext` and applies the `algo-self-heal-repair` algorithm. This method **MUST** be visible only to the SDK facade (in-crate; specifically the lazy in-process trigger inside `read_file`) and the future P2 GC sweep — never exposed through REST or any in-process consumer of `dyn FileStorageClient`. The eager `reconcile` REST endpoint runs under the caller's `SecurityContext` and authz; it is implemented through `sync_meta` (an authz-checked entry-point), not through `repair_meta`.

**Implements**:

- `cpt-cf-file-storage-algo-files-schema-and-repo-self-heal-repair`

**Constraints**: `cpt-cf-file-storage-constraint-system-context-maintenance`

**Touches**:

- DB: `file_storage.files`
- Crate: `file-storage` (component: Files Repo)

## 6. Acceptance Criteria

- [ ] DDL from [migration.sql](../migration.sql) applies cleanly on a fresh DB and is idempotent on re-run.
- [ ] The `version_id` column accepts `NULL` (for non-versioning backends) and `VARCHAR(1024)` strings (S3 VersionId values; AWS User Guide caps VersionId at 1,024 bytes — see [ADR-0005](../ADR/0005-cpt-cf-file-storage-adr-versioning-and-aba.md) §"VersionId length and shape").
- [ ] The `updated_at` column is refreshed to `NOW()` on every UPDATE; race-detection clauses on conditional UPDATEs include both `etag` and `updated_at` (and `xmin` on Postgres).
- [ ] `created_at` is set to `NOW()` exactly once at INSERT and is never modified by any subsequent statement; `updated_at` is set to `NOW()` by every successful UPDATE and is never derived from S3 response headers (e.g. `Last-Modified`). Reconcile pulls `etag`, `version_id`, and S3-mirrored metadata from the HEAD response — but never any S3-side timestamp into `created_at` or `updated_at`.
- [ ] The conditional UPDATE primitive returns `1` when the row is found at the supplied `(etag, updated_at[, xmin])` (and expected status if specified); returns `0` when the row moved underneath the caller. The caller's retry / surface-error decision is feature-specific (reconcile retries up to 3; put_file_info retries up to 3; delete_file does not retry on race).
- [ ] The supersession transaction commits the winner and queues the loser's `(backend_id, id)` for orphan delete in one TX; if the partial unique index rejects the commit, the entire TX rolls back.
- [ ] `repair_meta` no-ops when the row's `(etag, updated_at)` no longer match the caller's captured pair, and updates the row exactly once when called concurrently from two paths (the second sees `0` rows affected and treats it as success).
- [ ] All three P1 indexes exist and are used by the corresponding query patterns (`EXPLAIN`-verified for the partial unique index and the listing index, including the `id` tiebreaker on `files_created_idx`).
- [ ] A row in `Deleting` is invisible to readers (`get_by_id` filtering on `status = 'uploaded'` returns `NotFound`); concurrent mutations against it return `DeleteInProgress`.
- [ ] `max_merge_upload_expires_at` never shortens the existing `upload_expires_at` window: if the row's current value is in the future and the new computed value is earlier, the column stays at the existing value.
