<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: Upload Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-upload-lifecycle`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-file-storage-feature-upload-lifecycle`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Initial Upload Flow](#initial-upload-flow)
  - [Variant-B Re-Upload Flow](#variant-b-re-upload-flow)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Initial Upload Algorithm (create_presigned_url)](#initial-upload-algorithm-create_presigned_url)
  - [Variant-B Re-Upload Algorithm (create_presigned_overwrite_url)](#variant-b-re-upload-algorithm-create_presigned_overwrite_url)
  - [Reconcile Algorithm](#reconcile-algorithm)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Upload Coordinator](#implement-upload-coordinator)
  - [Implement Idempotent Concurrent Reconcile](#implement-idempotent-concurrent-reconcile)
  - [Wire authz on Upload Entry-Points](#wire-authz-on-upload-entry-points)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Orchestrate the presign-first upload flow end-to-end. Three coordinator entry-points:

- `create_presigned_url` — initial upload. Validates the GTS file type and user-metadata budget, registers a `pending_upload` row with sentinel `etag_pinned` and `version_id = NULL`, picks the right adapter (default-private fallback when `backend_id` is omitted), and asks the adapter for a presigned PUT URL with `Content-Type` / `Content-Disposition` / `x-amz-meta-<k>` pinned (NOT `x-amz-meta-gts-file-type`).
- `create_presigned_overwrite_url` — variant-B re-upload. SELECTs the row, pins the row's CURRENT `name` / `mime_type` / `custom_metadata` into a fresh presigned PUT URL, MAX-merges `upload_expires_at`. Rejects requests carrying a `meta` argument (the server is the sole source of pinned metadata on this path).
- `reconcile` — explicit HEAD-and-pull primitive. HEADs the storage backend, captures the authoritative `(s3_etag, s3_version_id, content_type, content_disposition, content_length, x-amz-meta-*)`, writes them into the row in a single conditional UPDATE (with race detection on `(etag, updated_at[, xmin])` and a 3-attempt retry loop). `gts_file_type` is preserved from the DB; never pulled from S3.

### 1.2 Purpose

This feature is the heart of the chat-frontend → chat-backend → FileStorage → S3 → chat-backend lifecycle. The supersession transaction realises last-write-wins on `(tenant, backend, file_path)` when a fresh `file_id` is used; variant-B re-upload preserves the `file_id` and the backend object key, so cross-module handles holding the old `file_id` see the new bytes after the next reconcile (or via the lazy self-heal on `read_file`).

`reconcile` replaces the legacy `change_status` / `sync` SDK calls: FileStorage HEADs S3 itself and is the sole arbiter of the row's etag, version_id, and S3-mirrored metadata. Application backends never have to plumb a possibly-wrong S3 etag through their own status surfaces.

**Requirements**: `cpt-cf-file-storage-fr-upload-file`, `cpt-cf-file-storage-fr-file-ownership`, `cpt-cf-file-storage-fr-authorization`, `cpt-cf-file-storage-fr-file-type-classification`, `cpt-cf-file-storage-fr-data-classification`, `cpt-cf-file-storage-nfr-durability`, `cpt-cf-file-storage-nfr-url-availability`

**Principles**: `cpt-cf-file-storage-principle-presign-first`, `cpt-cf-file-storage-principle-self-healing`, `cpt-cf-file-storage-principle-tenant-owner`

### 1.3 Actors

- `cpt-cf-file-storage-actor-cf-modules` — the application backend (e.g. chat) calls `create_presigned_url` / `create_presigned_overwrite_url` and then `reconcile`
- `cpt-cf-file-storage-actor-platform-user` — the end-client whose browser PUTs bytes to the presigned URL between steps

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (§3.6 Presign-first upload sequence, §3.9 step 8 reconcile algorithm)
- **ADR**: [ADR-0002](../ADR/0002-cpt-cf-file-storage-adr-opaque-file-ids.md), [ADR-0003](../ADR/0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md), [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md), [ADR-0005](../ADR/0005-cpt-cf-file-storage-adr-versioning-and-aba.md)
- **Use cases**: `cpt-cf-file-storage-usecase-direct-upload`, `cpt-cf-file-storage-usecase-upload-share`
- **Decomposition**: [DECOMPOSITION.md §2.5](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-file-storage-feature-files-schema-and-repo`, `cpt-cf-file-storage-feature-s3-compatible-adapter`

## 2. Actor Flows (CDSL)

### Initial Upload Flow

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-upload-lifecycle-initial`

**Actor**: `cpt-cf-file-storage-actor-cf-modules` (application backend), with the end-client's browser performing the data-plane PUT

**Steps**:

1. [ ] - `p1` - Application backend invokes `create_presigned_url(ctx, backend_id?, owner, file_path, meta, params)` via the SDK - `inst-iflow-1`
2. [ ] - `p1` - SDK runs the initial upload algorithm - `inst-iflow-2`
3. [ ] - `p1` - End-client `PUT`s bytes to `upload_url` directly against S3; S3 returns 200 + ETag - `inst-iflow-3`
4. [ ] - `p1` - End-client signals completion to the application backend - `inst-iflow-4`
5. [ ] - `p1` - Application backend invokes `reconcile(ctx, file_id)` via the SDK - `inst-iflow-5`
6. [ ] - `p1` - SDK runs the reconcile algorithm - `inst-iflow-6`
7. [ ] - `p1` - **RETURN** `ReconcileResult { info, s3_etag, s3_version_id }` to the application backend - `inst-iflow-7`

### Variant-B Re-Upload Flow

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-upload-lifecycle-variant-b`

**Actor**: same as initial upload flow

**Steps**:

1. [ ] - `p1` - Application backend invokes `create_presigned_overwrite_url(ctx, file_id, params)` via the SDK — **no `meta`** argument - `inst-vb-1`
2. [ ] - `p1` - SDK runs the variant-B re-upload algorithm (server pins row's current meta into the new presigned PUT, MAX-merges `upload_expires_at`) - `inst-vb-2`
3. [ ] - `p1` - End-client `PUT`s new bytes to the same backend object key - `inst-vb-3`
4. [ ] - `p1` - Application backend invokes `reconcile(ctx, file_id)` to refresh the row's etag and version_id from S3 - `inst-vb-4`
5. [ ] - `p1` - **RETURN** updated `ReconcileResult` to the application backend - `inst-vb-5`

## 3. Processes / Business Logic (CDSL)

### Initial Upload Algorithm (create_presigned_url)

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-upload-lifecycle-create-presigned-url`

**Input**: `(SecurityContext ctx, Option<BackendId> backend_id, OwnerRef owner, &str file_path, FileMeta meta, UrlParams params)`

**Output**: `PresignedUploadHandle { file_id, upload_url, etag_pinned, expires_at }`

**Steps**:

1. [ ] - `p1` - **IF** `owner.tenant_id != ctx.tenant_id` → Err(AccessDenied) - `inst-cpu-1`
2. [ ] - `p1` - Validate `meta.gts_file_type` matches `gts.x.fstorage.file.type.v1~…`; invalid → Err(BadRequest) - `inst-cpu-2`
3. [ ] - `p1` - Validate aggregate user-metadata size ≤ 2 KB. The budget covers `Content-Type` + `Content-Disposition` + every `x-amz-meta-<k>=<v>`. **`gts_file_type` does NOT count** because it is not mirrored to S3. Oversize → Err(PayloadTooLarge { max_bytes: 2048 }) - `inst-cpu-3`
4. [ ] - `p1` - API: `authz.authorize(ctx, write, "gts.x.fstorage.file.type.v1~{gts_file_type}")`; denied → Err(AccessDenied) - `inst-cpu-4`
5. [ ] - `p1` - Resolve adapter via `cpt-cf-file-storage-algo-backend-router-and-roster-resolve(ctx, backend_id)`; default-private fallback when `None` - `inst-cpu-5`
6. [ ] - `p1` - **IF** the adapter does not declare `PresignedUrls` → Err(CapabilityUnavailable) - `inst-cpu-6`
7. [ ] - `p1` - Mint `file_id = Uuid::new_v7()` (server-side per `cpt-cf-file-storage-constraint-server-minted-file-id`) - `inst-cpu-7`
8. [ ] - `p1` - Derive backend object key from `file_id` at the adapter boundary (deterministic; not persisted as a column) - `inst-cpu-8`
9. [ ] - `p1` - Compute sentinel `etag_pinned` (placeholder — the row's authoritative `etag` is set later by `reconcile`) - `inst-cpu-9`
10. [ ] - `p1` - Compute `upload_expires_at = NOW() + min(params.expires_in_seconds, backend.max_signed_url_ttl)` - `inst-cpu-10`
11. [ ] - `p1` - Build `PinnedObjectHeaders`:
    - `content_type` = `meta.mime_type`
    - `content_disposition` = `attachment; filename="<URL-encoded meta.name>"`
    - `user_metadata` = `meta.custom_metadata` (key/value pairs)
    - **NO `x-amz-meta-gts-file-type` entry** - `inst-cpu-11`
12. [ ] - `p1` - DB: INSERT a `pending_upload` row with `(id, tenant_id, backend_id, file_path, owner_id, name, gts_file_type, mime_type, size_bytes=0, etag=etag_pinned, version_id=NULL, status='pending_upload', custom_metadata, upload_expires_at, created_at=NOW(), updated_at=NOW())` - `inst-cpu-12`
13. [ ] - `p1` - Adapter call: `issue_presigned_put(key, pinned, params)`. The adapter signs SigV4 PUT with the supplied SignedHeaders. P1 does not pin any conditional preconditions on the upload PUT - `inst-cpu-13`
14. [ ] - `p1` - **RETURN** `PresignedUploadHandle { file_id, upload_url, etag_pinned, expires_at }` - `inst-cpu-14`

### Variant-B Re-Upload Algorithm (create_presigned_overwrite_url)

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-upload-lifecycle-overwrite-presign`

**Input**: `(SecurityContext ctx, FileId file_id, UrlParams params)` — **no `meta` argument**

**Output**: `PresignedUploadHandle { file_id, upload_url, etag_pinned: row.etag (pre-overwrite), expires_at }`

**Steps**:

1. [ ] - `p1` - DB: SELECT row WHERE id = $file_id AND tenant_id = $ctx.tenant_id - `inst-vbup-1`
2. [ ] - `p1` - **IF** row missing → Err(NotFound) - `inst-vbup-2`
3. [ ] - `p1` - **IF** row.status != 'uploaded' (e.g. `pending_upload` — no committed bytes yet, or `deleting`) → Err(NotFound) for `pending_upload`, Err(DeleteInProgress) for `deleting` - `inst-vbup-3`
4. [ ] - `p1` - API: `authz.authorize(ctx, write, "gts.x.fstorage.file.type.v1~{row.gts_file_type}")`; denied → Err(AccessDenied) - `inst-vbup-4`
5. [ ] - `p1` - Resolve adapter from `row.backend_id`. **IF** the adapter does not declare `PresignedUrls` → Err(CapabilityUnavailable) - `inst-vbup-5`
6. [ ] - `p1` - Build `PinnedObjectHeaders` from the row's CURRENT meta:
    - `content_type` = `row.mime_type`
    - `content_disposition` = `attachment; filename="<URL-encoded row.name>"`
    - `user_metadata` = `row.custom_metadata`
    - **NO `x-amz-meta-gts-file-type` entry** - `inst-vbup-6`
7. [ ] - `p1` - Compute `new_expires_at = NOW() + min(params.expires_in_seconds, backend.max_signed_url_ttl)` - `inst-vbup-7`
8. [ ] - `p1` - Adapter call: `issue_presigned_put(derive(file_id), pinned, params)` returns `(upload_url, expires_at)` - `inst-vbup-8`
9. [ ] - `p1` - DB: `UPDATE files SET upload_expires_at = MAX(coalesce(upload_expires_at, '-infinity'::timestamp), $new_expires_at), updated_at = NOW() WHERE id = $file_id` - `inst-vbup-9`
10. [ ] - `p1` - **RETURN** `PresignedUploadHandle { file_id, upload_url, etag_pinned: row.etag, expires_at: new_expires_at }` - `inst-vbup-10`

The handler **MUST** reject any presign-batch upload item that carries `file_id` AND `meta` with per-item `400 bad_request` (`code = bad_request`, message: "meta is forbidden on re-upload"). The validation lives in the REST adapter layer; the SDK's `create_presigned_overwrite_url` does not declare a `meta` parameter at all (compile-time enforcement).

### Reconcile Algorithm

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-upload-lifecycle-reconcile`

**Input**: `(SecurityContext ctx, FileId file_id)` — note: no etag parameter; `reconcile` HEADs the backend itself. The REST endpoint rejects `If-Match` with `400`.

**Output**: `ReconcileResult { info: FileInfo, s3_etag: String, s3_version_id: Option<String> }` on success; `Conflict` / `DeleteInProgress` / `NotFound` / `UploadExpired` / `BackendFailure` otherwise

**Steps**:

```
loop up to 3 times:
  1. DB: SELECT row WHERE id = $file_id AND tenant_id = $ctx.tenant_id
     - row missing → Err(NotFound)
     - row.status == 'deleting' → Err(DeleteInProgress)
  2. API: authz.authorize(ctx, write, "gts.x.fstorage.file.type.v1~{row.gts_file_type}")
     - denied → Err(AccessDenied)
  3. IF row.status == 'pending_upload' AND row.upload_expires_at IS NOT NULL
        AND row.upload_expires_at < NOW():
       Err(UploadExpired)
  4. capture (etag_db, version_id_db, updated_at_db, status_db, meta_db) from the row
     [on Postgres also capture xmin_db]
  5. Adapter: head_object(derive(file_id))
     - 404 OR transport failure → Err(BackendFailure)
     - capture (s3_etag, s3_version_id, content_type, content_disposition,
                content_length, user_metadata)
  6. Build new_meta_from_s3:
     - name from Content-Disposition (URL-decoded; fallback to row.name if header malformed)
     - mime_type from Content-Type (fallback to row.mime_type if header missing)
     - custom_metadata from user_metadata (case-normalized keys, x-amz-meta- prefix stripped)
     - gts_file_type KEPT FROM DB (NOT pulled from S3)
     - size_bytes from Content-Length
  7. DB: UPDATE files
            SET status = 'uploaded',
                etag = $s3_etag,
                version_id = $s3_version_id,
                name = $new_meta_from_s3.name,
                mime_type = $new_meta_from_s3.mime_type,
                custom_metadata = $new_meta_from_s3.custom_metadata,
                size_bytes = $content_length,
                upload_expires_at = NULL,
                updated_at = NOW()
          WHERE id = $file_id
            AND etag = $etag_db
            AND updated_at = $updated_at_db
            [AND xmin = $xmin_db]
     - 1 row affected → break loop
     - 0 rows affected → race detected, retry
end loop:
  - 3 unsuccessful attempts → Err(Conflict { correlation_id })

Return ReconcileResult { info: refreshed_FileInfo, s3_etag, s3_version_id }
```

The retry loop bounds contention. Concurrent `reconcile` calls converge: the loser's UPDATE matches `0` rows, retries from a fresh SELECT, observes the row already converged, and either succeeds as a no-op-equivalent or retries until convergence within 3 attempts.

`Conflict` from `reconcile` only surfaces when contention from a different operation (e.g. `put_file_info` rotating the row in parallel) keeps the retry loop from converging. The application backend can re-fetch and retry.

## 4. States (CDSL)

This feature drives the `pending_upload → uploaded` transition described in `cpt-cf-file-storage-state-files-schema-and-repo-row`, plus the `uploaded → uploaded` drift correction. No new state machine is introduced here.

## 5. Definitions of Done

### Implement Upload Coordinator

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-upload-lifecycle-coordinator`

The system **MUST** implement the Upload Coordinator component that owns `create_presigned_url`, `create_presigned_overwrite_url`, and `reconcile`. The coordinator **MUST** validate the GTS file type and 2 KB user-metadata budget on initial upload, run authz on every entry-point, mint a fresh `file_id` for initial uploads, derive the backend object key from `file_id` at the adapter boundary, INSERT the `pending_upload` row, and call the resolved adapter to issue the presigned PUT URL. Variant-B re-upload **MUST** SELECT the row, pin the row's current `name` / `mime_type` / `custom_metadata` into the presigned PUT, and MAX-merge `upload_expires_at`. `reconcile` **MUST** HEAD the backend, pull all S3-mirrored fields except `gts_file_type` into the row in a conditional UPDATE with race detection on `(etag, updated_at[, xmin])` and a 3-attempt retry loop.

**Implements**:

- `cpt-cf-file-storage-flow-upload-lifecycle-initial`
- `cpt-cf-file-storage-flow-upload-lifecycle-variant-b`
- `cpt-cf-file-storage-algo-upload-lifecycle-create-presigned-url`
- `cpt-cf-file-storage-algo-upload-lifecycle-overwrite-presign`
- `cpt-cf-file-storage-algo-upload-lifecycle-reconcile`

**Constraints**: `cpt-cf-file-storage-constraint-server-minted-file-id`, `cpt-cf-file-storage-constraint-no-ambient-authn`, `cpt-cf-file-storage-constraint-meta-via-put-meta-only`, `cpt-cf-file-storage-constraint-meta-mirrored-via-put-meta`

**Touches**:

- API: SDK `create_presigned_url`, `create_presigned_overwrite_url`, `reconcile`; REST `POST /presign-batch`, `POST /files/{file_id}/meta/reconcile` (wired in `rest-api`)
- DB: `file_storage.files` (INSERT, UPDATE)

### Implement Idempotent Concurrent Reconcile

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-upload-lifecycle-idempotent-reconcile`

The system **MUST** implement `reconcile` such that concurrent calls converge on the same post-reconcile state without surfacing errors to either caller (under pure `reconcile ⇄ reconcile` racing). The retry loop **MUST** be bounded at 3 attempts; after that, surface `Conflict` (HTTP 409, `code = conflict`) with a `correlation_id` so operators can join logs.

**Implements**:

- `cpt-cf-file-storage-algo-upload-lifecycle-reconcile`

**Constraints**: (no new constraint registrations)

**Touches**:

- API: SDK `reconcile`; REST `POST /files/{file_id}/meta/reconcile` (wired in `rest-api`)
- DB: `file_storage.files` (UPDATE)

### Wire authz on Upload Entry-Points

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-upload-lifecycle-authz`

The system **MUST** call `AuthZResolverClient::authorize` on every entry into `create_presigned_url`, `create_presigned_overwrite_url`, and `reconcile`, passing `gts.x.fstorage.file.type.v1~{file.gts_file_type}` as the resource. FileStorage **MUST NOT** parse the bearer token itself — the `SecurityContext` arrives via ModKit middleware. A denied result **MUST** translate to `Err(AccessDenied)` without exposing whether the file exists (no enumeration oracle).

**Implements**:

- `cpt-cf-file-storage-algo-upload-lifecycle-create-presigned-url`
- `cpt-cf-file-storage-algo-upload-lifecycle-overwrite-presign`
- `cpt-cf-file-storage-algo-upload-lifecycle-reconcile`

**Constraints**: `cpt-cf-file-storage-constraint-no-ambient-authn`

**Touches**:

- SDK dependency: `AuthZResolverClient` resolved through `ClientHub`

## 6. Acceptance Criteria

- [ ] `create_presigned_url` returns `(file_id, upload_url, etag_pinned, expires_at)` with a freshly minted `file_id` (UUID v7) and a backend-bound `upload_url`. The PUT URL pins `Content-Type` (from `meta.mime_type`), `Content-Disposition` (from `meta.name`), and every `x-amz-meta-<k>` (from `meta.custom_metadata`); does NOT pin `x-amz-meta-gts-file-type`.
- [ ] `create_presigned_url` with aggregate user-metadata > 2 KB returns `Err(PayloadTooLarge { max_bytes: 2048 })` and inserts no row.
- [ ] An end-client given the `upload_url` can PUT bytes directly against the S3 endpoint; the resulting object carries the pinned headers exactly.
- [ ] After `reconcile(file_id)` returns `Ok(ReconcileResult)`, the row's `status = 'uploaded'`, `etag` equals the raw S3 ETag, `version_id` equals the raw S3 VersionId (or `NULL` on non-versioning backends), `name` / `mime_type` / `custom_metadata` are pulled from the HEAD response, and `gts_file_type` is unchanged from the row's pre-reconcile value. `upload_expires_at` is `NULL`.
- [ ] Two concurrent `reconcile(file_id)` calls both return `Ok(ReconcileResult)` with the same `info.etag` and `s3_etag` — neither sees `Conflict` (under pure reconcile-vs-reconcile racing).
- [ ] `reconcile` against a row already in `uploaded` whose backend ETag has not changed is a no-op-equivalent: returns `Ok(ReconcileResult)`, row's etag is unchanged.
- [ ] `reconcile` against a row already in `uploaded` whose backend ETag drifted (out-of-band PUT) rotates the row's etag (and version_id, mirrored metadata, size_bytes) to S3's current state. `gts_file_type` is preserved.
- [ ] `reconcile` against a row in `Deleting` returns `Err(DeleteInProgress)`.
- [ ] `reconcile` against a `pending_upload` row whose `upload_expires_at` has passed returns `Err(UploadExpired)`.
- [ ] An attempt to call `create_presigned_url` with `owner.tenant_id` ≠ `ctx.tenant_id` returns `Err(AccessDenied)`.
- [ ] An invalid GTS file type (missing prefix, wrong format) returns `Err(BadRequest)` and does NOT INSERT a row.
- [ ] If the resolved backend does not declare `PresignedUrls`, `create_presigned_url` returns `Err(CapabilityUnavailable)` with no row inserted.
- [ ] `create_presigned_overwrite_url(ctx, file_id, params)` succeeds for an `uploaded` row, returns `etag_pinned = row.etag`, pins the row's CURRENT meta into the presigned PUT, and updates `upload_expires_at` to `MAX(current, NOW + TTL)`.
- [ ] A `presign-batch` upload item with `file_id` set AND `meta` populated returns per-item `400 bad_request` (`code = bad_request`).
- [ ] `create_presigned_overwrite_url` against a row in `pending_upload` returns `Err(NotFound)`; against a row in `Deleting` returns `Err(DeleteInProgress)`.
