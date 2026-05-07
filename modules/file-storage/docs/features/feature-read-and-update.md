<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: Read and Update

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-read-and-update`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-file-storage-feature-read-and-update`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [In-Process Streaming Read Flow](#in-process-streaming-read-flow)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Read File with Self-Healing](#read-file-with-self-healing)
  - [Get File Info](#get-file-info)
  - [Put File Info (DB+S3 atomic sync)](#put-file-info-dbs3-atomic-sync)
  - [Delete File (2-Phase)](#delete-file-2-phase)
  - [List Files](#list-files)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement read_file with Self-Healing](#implement-read_file-with-self-healing)
  - [Implement Metadata Get and PUT](#implement-metadata-get-and-put)
  - [Implement 2-Phase Delete](#implement-2-phase-delete)
  - [Implement Owner-Scoped list_files](#implement-owner-scoped-list_files)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Implement every read- and update-path SDK operation:

- `get_file_info` — DB-only authoritative metadata view.
- `read_file` — in-process streaming read with lazy self-healing (per ADR-0004).
- `put_file_info` — atomic DB+S3 metadata sync via `CopyObject` self-copy with `MetadataDirective: REPLACE`. `If-Match` is **optional**; when supplied it becomes a strong CAS over both stores.
- `delete_file` — 2-phase hard delete via the transient `Deleting` status. `If-Match` is **optional**.
- `list_files` — owner-scoped paginated listing.

### 1.2 Purpose

These are the operations every consumer of the SDK runs after the initial upload. `read_file` is the in-process streaming entry-point used by antivirus, llm-gateway, and file-parser; it carries the lazy self-healing trigger that converges the brief `(content, etag, S3-mirrored metadata)` desync window in the variant-B re-upload flow. `put_file_info` is the only metadata-replace path; it keeps DB.meta and S3 user-metadata in sync atomically through a `CopyObject` self-copy. `delete_file` is the 2-phase hard delete: Phase 1 conditional UPDATE flips the row to `Deleting`, Phase 2 deletes the backend object with inline retries (and may leave the row stuck if the backend is persistently unavailable), Phase 3 hard-DELETEs the row.

The eager equivalent of `read_file`'s self-healing — `reconcile(file_id)` — lives in [feature-upload-lifecycle](./feature-upload-lifecycle.md); that algorithm is the canonical specification and this feature only references it.

**Requirements**: `cpt-cf-file-storage-fr-download-file`, `cpt-cf-file-storage-fr-delete-file`, `cpt-cf-file-storage-fr-get-metadata`, `cpt-cf-file-storage-fr-list-files`, `cpt-cf-file-storage-fr-update-metadata`, `cpt-cf-file-storage-fr-retention-indefinite`, `cpt-cf-file-storage-nfr-transfer-latency`, `cpt-cf-file-storage-nfr-scalability`

**Principles**: `cpt-cf-file-storage-principle-self-healing`, `cpt-cf-file-storage-principle-stream-by-default`, `cpt-cf-file-storage-principle-optimistic-concurrency`, `cpt-cf-file-storage-principle-atomic-metadata`

### 1.3 Actors

- `cpt-cf-file-storage-actor-cf-modules` — antivirus / llm-gateway / file-parser / chat backend (read consumers and metadata mutators)

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (§3.6 streaming in-process read sequence, §3.9 self-healing trigger points, §3.9 step 11 — 2-phase delete)
- **ADR**: [ADR-0002](../ADR/0002-cpt-cf-file-storage-adr-opaque-file-ids.md), [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md), [ADR-0005](../ADR/0005-cpt-cf-file-storage-adr-versioning-and-aba.md)
- **Use cases**: `cpt-cf-file-storage-usecase-fetch-media`, `cpt-cf-file-storage-usecase-get-metadata`, `cpt-cf-file-storage-usecase-delete-file`
- **Decomposition**: [DECOMPOSITION.md §2.6](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-file-storage-feature-files-schema-and-repo`, `cpt-cf-file-storage-feature-s3-compatible-adapter`

## 2. Actor Flows (CDSL)

### In-Process Streaming Read Flow

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-read-and-update-streaming-read`

**Actor**: `cpt-cf-file-storage-actor-cf-modules` (antivirus / llm-gateway / file-parser, etc.)

**Success Scenarios**:

- Caller pinned the etag they observed earlier and the row is unchanged → stream proceeds, end-to-end consistent
- Caller did not pin etag (`None`) → stream proceeds with whatever is current
- Pinned etag matches the row but a zombie upload landed at the backend → SDK detects via `s3_etag != row.etag`, repairs the DB, returns `EtagMismatch{ current: s3_etag }` (caller retries once and sees the new bytes)

**Error Scenarios**:

- Row missing for caller's tenant or row is in `Deleting` → `NotFound`
- authz denies read on the file's GTS type → `AccessDenied`
- Backend returned 404 (S3 lost the object) → `NotFound`; the inverse-sweep GC will flag the row as `lost`

**Steps**:

1. [ ] - `p1` - Caller invokes `read_file(ctx, file_id, etag?)` via the SDK - `inst-rflow-1`
2. [ ] - `p1` - SDK runs `cpt-cf-file-storage-algo-read-and-update-read-file` - `inst-rflow-2`
3. [ ] - `p1` - **IF** Ok(handle), caller iterates `handle.bytes` (Stream<Result<Bytes>>) until exhausted - `inst-rflow-3`

## 3. Processes / Business Logic (CDSL)

### Read File with Self-Healing

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-read-and-update-read-file`

**Input**: `(SecurityContext ctx, FileId file_id, Option<Etag> pinned_etag)`

**Output**: `FileReadHandle { info, bytes }` on success; `EtagMismatch` / `NotFound` / `AccessDenied` / `BackendFailure` on failure

**Steps**:

1. [ ] - `p1` - DB: SELECT row WHERE id = $file_id AND tenant_id = $ctx.tenant_id AND status = 'uploaded' - `inst-read-1`
2. [ ] - `p1` - **IF** row missing (absent OR in `pending_upload` OR in `deleting`) → Err(NotFound) - `inst-read-2`
3. [ ] - `p1` - **IF** `pinned_etag.is_some() && pinned_etag != row.etag` → Err(EtagMismatch{ current: row.etag }) (legitimate version drift, no self-heal needed) - `inst-read-3`
4. [ ] - `p1` - API: `authz.authorize(ctx, read, "gts.x.fstorage.file.type.v1~{row.gts_file_type}")`; denied → Err(AccessDenied) - `inst-read-4`
5. [ ] - `p1` - Adapter: `open_read(derive(file_id))` returns `(stream, BackendObjectMetadata { s3_etag, s3_version_id, size_bytes, content_type, content_disposition, user_metadata })` - `inst-read-5`
6. [ ] - `p1` - **IF** `s3_etag != row.etag` → desync detected. Run `cpt-cf-file-storage-algo-files-schema-and-repo-self-heal-repair(file_id, s3_etag, s3_version_id, size_bytes, content_type, content_disposition, user_metadata)` (system-context, no SecurityContext) - `inst-read-6`
   1. [ ] - `p1` - **IF** `pinned_etag.is_some()` → return `Err(EtagMismatch{ current: s3_etag })` (caller's pin is now known stale) - `inst-read-6a`
   2. [ ] - `p1` - **ELSE** re-fetch row to pick up the repaired state → return Ok(FileReadHandle { info: refreshed, bytes: stream }) - `inst-read-6b`
7. [ ] - `p1` - **ELSE RETURN** Ok(FileReadHandle { info: row, bytes: stream }) - `inst-read-7`

### Get File Info

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-read-and-update-get-info`

**Input**: `(SecurityContext ctx, FileId file_id, Option<Etag> pinned_etag)`

**Output**: `FileInfo`

**Steps**:

1. [ ] - `p1` - DB: SELECT row WHERE id = $file_id AND tenant_id = $ctx.tenant_id AND status = 'uploaded' - `inst-gi-1`
2. [ ] - `p1` - **IF** row missing → Err(NotFound) - `inst-gi-2`
3. [ ] - `p1` - **IF** `pinned_etag.is_some() && pinned_etag != row.etag` → Err(EtagMismatch{ current: row.etag }) - `inst-gi-3`
4. [ ] - `p1` - API: `authz.authorize(ctx, read, "gts.x.fstorage.file.type.v1~{row.gts_file_type}")`; denied → Err(AccessDenied) - `inst-gi-4`
5. [ ] - `p1` - **RETURN** Ok(FileInfo from row) - `inst-gi-5`

### Put File Info (DB+S3 atomic sync)

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-read-and-update-put-info`

**Input**: `(SecurityContext ctx, FileId file_id, FileMetaUpdate update, Option<Etag> if_match)` where `update` declares `name?`, `mime_type?`, `custom_metadata?` only (`gts_file_type` is structurally absent — type-system enforced)

**Output**: `FileInfo` with rotated etag and version_id (the values returned by the S3 `CopyObject` response)

**Steps**:

```
loop up to 3 times:
  1. DB: SELECT row WHERE id = $file_id AND tenant_id = $ctx.tenant_id;
     capture (etag_db, version_id_db, updated_at_db[, xmin_db], status_db, meta_db).
     - row missing → Err(NotFound)
     - status_db == 'deleting' → Err(DeleteInProgress)
  2. API: authz.authorize(ctx, write, "gts.x.fstorage.file.type.v1~{row.gts_file_type}")
     - denied → Err(AccessDenied)
  3. IF if_match.is_some() (strong CAS path):
       a. IF etag_db != if_match → Err(EtagMismatch { current: etag_db })
       b. Adapter: head_object(derive(file_id)) → (s3_etag_live, s3_version_id_live)
       c. IF s3_etag_live != if_match → Err(EtagMismatch { current: s3_etag_live })
       d. IF backend.versioning AND s3_version_id_live != version_id_db → Err(EtagMismatch)
  4. Compute new_meta = merge(meta_db, update). Preserve gts_file_type from meta_db.
  5. Validate aggregate user-metadata budget ≤ 2 KB (Content-Type + Content-Disposition + every
     x-amz-meta-<k>=<v>; gts_file_type does NOT count).
     - oversize → Err(PayloadTooLarge { max_bytes: 2048 })
  6. Build PinnedObjectHeaders from new_meta:
     - content_type = new_meta.mime_type
     - content_disposition = attachment; filename="<URL-encoded new_meta.name>"
     - user_metadata = new_meta.custom_metadata
     - NO x-amz-meta-gts-file-type
  7. Adapter: copy_object_self(derive(file_id), pinned, if_match)
     - returns (new_etag_from_s3, new_version_id_from_s3)
     - 412 (precondition) → Err(EtagMismatch)
     - other failure → Err(BackendFailure)
  8. DB: UPDATE files
            SET name = $new_meta.name,
                mime_type = $new_meta.mime_type,
                custom_metadata = $new_meta.custom_metadata,
                etag = $new_etag_from_s3,
                version_id = $new_version_id_from_s3,
                updated_at = NOW()
          WHERE id = $file_id
            AND etag = $etag_db
            AND updated_at = $updated_at_db
            [AND xmin = $xmin_db]
     - 1 row → break loop
     - 0 rows → race detected, retry from step 1
end loop:
  - 3 unsuccessful attempts → Err(Conflict)

Return Ok(refreshed FileInfo)
```

The CopyObject step is what makes this an atomic DB+S3 sync: S3's user-metadata is rewritten in place with `MetadataDirective: REPLACE`, and the response carries the new ETag and VersionId that the row then adopts. There is no observable window where DB.meta and S3.meta disagree for any reader that follows the standard `PUT /meta` happy path.

### Delete File (2-Phase)

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-read-and-update-delete-file`

**Input**: `(SecurityContext ctx, FileId file_id, Option<Etag> if_match)`

**Output**: `()` on success; `EtagMismatch` / `NotFound` / `AccessDenied` / `DeleteInProgress` / `BackendFailure` on failure

**Phase 1 — claim the row**:

1. [ ] - `p1` - DB: SELECT row WHERE id = $file_id AND tenant_id = $ctx.tenant_id - `inst-df-1`
2. [ ] - `p1` - **IF** row missing → Err(NotFound) - `inst-df-2`
3. [ ] - `p1` - **IF** row.status == 'deleting' → Err(DeleteInProgress) - `inst-df-3`
4. [ ] - `p1` - API: `authz.authorize(ctx, delete, "gts.x.fstorage.file.type.v1~{row.gts_file_type}")`; denied → Err(AccessDenied) - `inst-df-4`
5. [ ] - `p1` - DB: `UPDATE files SET status='deleting', updated_at=NOW() WHERE id=$file_id AND status='uploaded' [AND etag=$if_match]` - `inst-df-5`
6. [ ] - `p1` - **IF** affected_rows == 0 → re-SELECT to disambiguate; **RETURN** Err(EtagMismatch) (if `if_match` mismatched) OR Err(DeleteInProgress) (if status flipped) OR Err(NotFound) - `inst-df-6`

**Phase 2 — backend cleanup**:

7. [ ] - `p1` - Adapter: `delete_object(derive(file_id))` (S3 `DeleteObject` is idempotent) - `inst-df-7`
8. [ ] - `p1` - **IF** transient failure (5xx, network, throttle) → inline retry up to 3 attempts with exponential backoff (e.g. 100 ms, 500 ms, 2 s) - `inst-df-8`
9. [ ] - `p1` - **IF** still failing → leave the row in `deleting`; **RETURN** Err(BackendFailure). The P2 GC sweep retries; subsequent reads on the row return NotFound - `inst-df-9`

**Phase 3 — purge the row**:

10. [ ] - `p1` - DB: `DELETE FROM files WHERE id=$file_id AND status='deleting'` (no etag check — Phase 1 owns the row) - `inst-df-10`
11. [ ] - `p1` - **RETURN** Ok(()) - `inst-df-11`

### List Files

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-read-and-update-list-files`

**Input**: `(SecurityContext ctx, ListFilesQuery query)` where `query = { owner_id: Option<Uuid>, cursor: Option<String>, limit: Option<u32> }`

**Output**: `FileList { items, next_cursor }`

**Steps**:

1. [ ] - `p1` - **IF** `query.owner_id` is `Some(uuid)` → Build WHERE: `tenant_id = $ctx.tenant_id AND owner_id = $uuid AND status = 'uploaded'` - `inst-lf-1`
2. [ ] - `p1` - **ELSE** → Build WHERE: `tenant_id = $ctx.tenant_id AND owner_id = $ctx.subject_id AND status = 'uploaded'` (default safe scope: caller's own files) - `inst-lf-2`
3. [ ] - `p1` - Apply cursor: decode `query.cursor` as `(created_at, id)` opaque pair if present - `inst-lf-3`
4. [ ] - `p1` - DB: SELECT rows ORDER BY created_at DESC, id ASC LIMIT $limit + 1 (using the `files_created_idx` index) - `inst-lf-4`
5. [ ] - `p1` - **IF** result has > $limit rows → truncate to $limit; encode the (limit+1)-th row's `(created_at, id)` as `next_cursor` - `inst-lf-5`
6. [ ] - `p1` - **RETURN** Ok(FileList { items, next_cursor }) - `inst-lf-6`

P1 does NOT expose `mime_type`, `gts_file_type`, `backend_id`, `created_after`, `created_before` filters; those land in P2 — see DESIGN §4 Future deltas.

## 4. States (CDSL)

This feature operates over rows in the `uploaded` state on the read paths, drives the `uploaded → uploaded` transition on the `put_file_info` path (etag/version_id rotated by `CopyObject`), and drives the `uploaded → deleting → (purged)` transition on the delete path. State definitions live in `cpt-cf-file-storage-state-files-schema-and-repo-row`.

## 5. Definitions of Done

### Implement read_file with Self-Healing

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-read-and-update-read-file`

The system **MUST** implement `read_file(ctx, file_id, etag?) -> FileReadHandle` per `cpt-cf-file-storage-algo-read-and-update-read-file`. Lazy in-process self-healing **MUST** fire on every backend GET whose `s3_etag` differs from the row's `etag`, BEFORE returning the handle to the caller. The repair UPDATE **MUST** run in system-context (no `SecurityContext`); concurrent self-heals on the same row **MUST** converge to one winner without surfacing an error. The eager equivalent of this trigger is the `reconcile` REST endpoint (lives in `feature-upload-lifecycle`).

**Implements**:

- `cpt-cf-file-storage-flow-read-and-update-streaming-read`
- `cpt-cf-file-storage-algo-read-and-update-read-file`

**Constraints**: `cpt-cf-file-storage-constraint-system-context-maintenance`, `cpt-cf-file-storage-constraint-no-ambient-authn`

**Touches**:

- API: SDK `read_file` only — **no REST surface in P1**
- DB: `file_storage.files` (SELECT, system-context UPDATE for self-heal)

### Implement Metadata Get and PUT

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-read-and-update-meta-get-put`

The system **MUST** implement `get_file_info(ctx, file_id, etag?) -> FileInfo` and `put_file_info(ctx, file_id, FileMetaUpdate, etag?) -> FileInfo`. `put_file_info` **MUST** use null-keeps-existing semantics (no PATCH endpoint anywhere) and **MUST** issue `CopyObject` self-copy with `MetadataDirective: REPLACE` to atomically rotate S3 user-metadata, then write the new etag and version_id (from the CopyObject response) into the row in a conditional UPDATE with race detection. `gts_file_type` is structurally absent from `FileMetaUpdate` — type-system enforced. `If-Match` is OPTIONAL; when supplied, the strong-CAS path verifies DB.etag, S3.etag (via HEAD), and (on versioning-on backends) S3.version_id, then issues `CopyObject` with `x-amz-copy-source-if-match`. Both endpoints **MUST** treat rows in `Deleting` as `NotFound` (reads) or `DeleteInProgress` (writes).

**Implements**:

- `cpt-cf-file-storage-algo-read-and-update-get-info`
- `cpt-cf-file-storage-algo-read-and-update-put-info`

**Constraints**: `cpt-cf-file-storage-constraint-no-ambient-authn`, `cpt-cf-file-storage-constraint-meta-mirrored-via-put-meta`, `cpt-cf-file-storage-constraint-no-meta-cas`, `cpt-cf-file-storage-constraint-versioning-aware-cas`

**Touches**:

- API: SDK `get_file_info`, `put_file_info`; REST `GET /files/{file_id}/meta`, `PUT /files/{file_id}/meta` (wired in `rest-api`)
- DB: `file_storage.files`
- Backend: adapter `head_object`, `copy_object_self`

### Implement 2-Phase Delete

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-read-and-update-delete`

The system **MUST** implement `delete_file(ctx, file_id, etag?) -> ()` as a 3-step flow per `cpt-cf-file-storage-algo-read-and-update-delete-file`: Phase 1 conditional UPDATE flips the row to `Deleting` (with optional `If-Match`); Phase 2 issues `delete_object` against the adapter with up to 3 inline retries on transient failure; Phase 3 hard-DELETEs the row. On persistent backend failure during Phase 2 the row is left in `Deleting` and the call returns `BackendFailure` (HTTP 502); subsequent reads on the row return `NotFound`. The orphan-delete grace period **MUST** be ≥ `max_signed_url_ttl + signed_url_clock_skew_margin` (verified at config-load) so already-issued presigned download URLs remain valid for their full TTL (invariant I6).

**Implements**:

- `cpt-cf-file-storage-algo-read-and-update-delete-file`

**Constraints**: `cpt-cf-file-storage-constraint-no-soft-delete`

**Touches**:

- API: SDK `delete_file`; REST `DELETE /files/{file_id}` (wired in `rest-api`)
- DB: `file_storage.files` (Phase 1 UPDATE, Phase 3 DELETE)
- Backend: adapter `delete_object` (Phase 2)

### Implement Owner-Scoped list_files

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-read-and-update-list`

The system **MUST** implement `list_files(ctx, ListFilesQuery) -> FileList` with mandatory owner scoping (defaults to caller's own files when `owner_id` is omitted), cursor-based pagination, and ordering by `created_at DESC, id ASC` using the `files_created_idx` index. Cursor values **MUST** be opaque to clients. P1 exposes only `owner_id`, `cursor`, `limit`; other filters are deferred to P2.

**Implements**:

- `cpt-cf-file-storage-algo-read-and-update-list-files`

**Constraints**: (no new constraint registrations)

**Touches**:

- API: SDK `list_files`; REST `GET /files?owner_id=…&cursor=…&limit=…` (wired in `rest-api`)
- DB: `file_storage.files` (indexed listing)

## 6. Acceptance Criteria

- [ ] `read_file(file_id, Some(etag))` against an unchanged row streams bytes end-to-end without firing self-healing.
- [ ] `read_file(file_id, Some(etag))` against a row whose backend object was overwritten out-of-band returns `EtagMismatch{ current: s3_etag }` AND the DB row is updated to reflect S3 (etag, version_id, size_bytes, mirrored metadata) before the return — verifiable on a follow-up `get_file_info`. `gts_file_type` is preserved.
- [ ] `read_file(file_id, None)` against a desynced row transparently repairs the row and returns the consistent `(info, bytes)` pair — caller sees no error.
- [ ] `read_file` against a row in `Deleting` returns `NotFound`.
- [ ] `put_file_info` with `name = Some("…"), mime_type = None, custom_metadata = None` updates the row's name, issues a `CopyObject` self-copy with the new `Content-Disposition`, and keeps every other field unchanged. The S3 object's user-metadata reflects the new name on a follow-up HEAD; the row's `etag` rotates to the new ETag returned by `CopyObject`.
- [ ] `put_file_info` with a body containing `gts_file_type` is rejected by the SDK at the type system level (compile error in callers); the REST endpoint rejects it with `400 bad_request`.
- [ ] `put_file_info` with `If-Match: <stale>` returns `412 etag_mismatch` (DB or S3 or version_id check failed). `put_file_info` without `If-Match` is best-effort last-write-wins; concurrent calls eventually all succeed under the 3-attempt retry loop or the last one returns `Conflict`.
- [ ] `put_file_info` against a row in `Deleting` returns `Err(DeleteInProgress)`.
- [ ] `put_file_info` with aggregate user-metadata > 2 KB returns `Err(PayloadTooLarge { max_bytes: 2048 })` and does NOT issue `CopyObject`.
- [ ] `delete_file(file_id, Some(etag))` returns `Ok` and the row is gone after Phase 3; the captured backend object remains in the backend until the orphan-grace period elapses, so a presigned download URL issued just before the delete still resolves until its TTL expires.
- [ ] `delete_file(file_id, None)` succeeds for an `uploaded` row even without `If-Match` (best-effort).
- [ ] `delete_file` against a row in `Deleting` returns `Err(DeleteInProgress)`.
- [ ] `delete_file` against a row whose backend `delete_object` persistently fails leaves the row in `Deleting` and returns `Err(BackendFailure)`; subsequent `get_file_info` / `read_file` return `NotFound`.
- [ ] `list_files` with `owner_id = X` returns only files owned by principal X within the caller's tenant; the partial unique index ensures no row appears twice.
- [ ] `list_files` cursor pagination is stable: `next_cursor` of page N decoded into the next request returns page N+1 without overlap or gaps; two rows with the same `created_at` are deterministically ordered by their `id`.
