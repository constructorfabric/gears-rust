<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: S3-Compatible Backend Adapter

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-s3-compatible-adapter`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-file-storage-feature-s3-compatible-adapter`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Issue Presigned PUT URL](#issue-presigned-put-url)
  - [Issue CopyObject Self-Copy (PUT /meta DB+S3 sync)](#issue-copyobject-self-copy-put-meta-dbs3-sync)
  - [Issue Batch of Presigned GET URLs](#issue-batch-of-presigned-get-urls)
  - [Open Read Stream](#open-read-stream)
  - [Head Object](#head-object)
  - [Delete Object](#delete-object)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement S3 Adapter Skeleton](#implement-s3-adapter-skeleton)
  - [Implement SigV4 PUT and GET URL Issuance](#implement-sigv4-put-and-get-url-issuance)
  - [Implement Streaming Read Path](#implement-streaming-read-path)
  - [Implement Metadata Mirror Pinning (Initial Upload + Variant-B + CopyObject)](#implement-metadata-mirror-pinning-initial-upload--variant-b--copyobject)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Implement the only P1 backend adapter — `s3-compatible`. PUT/GET/HEAD/DELETE/CopyObject against the configured bucket using `aws-sdk-s3` (or compatible), and generate SigV4 PUT/GET presigned URLs (per ADR-0003). For backends with the `PublicReadUrls` capability, also generate bare-HTTPS download URLs without signing. P1 ships PUT-SigV4 without any backend-side preconditions on the upload presign path — correctness is upheld by FileStorage's own primitives plus self-healing (ADR-0004). The adapter does NOT see `gts_file_type` (DB-only field); that filtering happens one layer up.

### 1.2 Purpose

This adapter handles every P1 deployment: AWS S3, MinIO, Ceph RGW, Wasabi, GCS S3-compat, and the local-disk recipe via `s3s-fs` running side-by-side. ADR-0001 sets the SQL-as-authoritative contract; ADR-0003 sets PUT-SigV4 over POST-Policy; ADR-0004 makes presigned-first overwrite reuse the existing key with self-healing as the correctness mechanism (eager via `sync`, lazy via `read_file`). This feature implements all three together.

**Requirements**: `cpt-cf-file-storage-fr-direct-transfer`, `cpt-cf-file-storage-fr-signed-urls`

**Principles**: `cpt-cf-file-storage-principle-presign-first`, `cpt-cf-file-storage-principle-stream-by-default`, `cpt-cf-file-storage-principle-batch-presigned-urls`, `cpt-cf-file-storage-principle-self-healing`

### 1.3 Actors

- `cpt-cf-file-storage-actor-cf-modules` — the SDK consumer asks for presigned URLs through this adapter (transitively)
- `cpt-cf-file-storage-actor-platform-user` — the end-client who PUTs / GETs bytes against the issued URLs

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (§3.2 S3 Backend Adapter, §3.6 sequences)
- **ADR**: [ADR-0001](../ADR/0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md), [ADR-0003](../ADR/0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md), [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md)
- **Use cases**: `cpt-cf-file-storage-usecase-direct-upload`, `cpt-cf-file-storage-usecase-signed-url`, `cpt-cf-file-storage-usecase-fetch-media`
- **Decomposition**: [DECOMPOSITION.md §2.4](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-file-storage-feature-backend-router-and-roster`

## 2. Actor Flows (CDSL)

This feature implements the storage-backend boundary; user-facing flows belong to higher-level features (`upload-lifecycle`, `read-and-update`, `batch-presign-downloads`, `rest-api`). The S3 adapter is invoked transitively by all of those.

## 3. Processes / Business Logic (CDSL)

### Issue Presigned PUT URL

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-s3-compatible-adapter-presigned-put`

**Input**: `(BackendObjectKey key, PinnedObjectHeaders pinned, UrlParams params)` — P1 has no conditional-PUT mode parameter; the `pinned` struct carries `content_type`, `content_disposition`, and the `user_metadata` map (the FileStorage core has already excluded `gts_file_type` from `user_metadata`)

**Output**: `(upload_url, expires_at)` — the FileStorage core wraps it in a `PresignedUploadHandle` together with the row's `file_id` and the sentinel / current `etag_pinned`

**Steps**:

1. [ ] - `p1` - Build the SignedHeaders set: `host`, `content-type` (= `pinned.content_type`), `content-disposition` (= `pinned.content_disposition`), and `x-amz-meta-<k>` for every entry in `pinned.user_metadata`. **Do NOT include `x-amz-meta-gts-file-type`** — it is not in `user_metadata` and must not be added by the adapter - `inst-pput-1`
2. [ ] - `p1` - Compute `expires_at = NOW() + min(params.expires_in_seconds, backend.max_signed_url_ttl)` - `inst-pput-2`
3. [ ] - `p1` - Generate SigV4 PUT URL via `aws-sdk-s3 generate_presigned_url(PutObject, …)` - `inst-pput-3`
4. [ ] - `p1` - **RETURN** `(upload_url, expires_at)` - `inst-pput-4`

### Issue CopyObject Self-Copy (PUT /meta DB+S3 sync)

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-s3-compatible-adapter-copy-object-self`

**Input**: `(BackendObjectKey key, PinnedObjectHeaders new_pinned, Option<Etag> if_match)`

**Output**: `(new_s3_etag, new_s3_version_id)` — the etag and (versioning-on backends) VersionId returned by S3

**Steps**:

1. [ ] - `p1` - Build the CopyObject request:
    - Bucket: backend's bucket
    - Key: `key`
    - CopySource: `<bucket>/<key>` (self-copy)
    - MetadataDirective: `REPLACE`
    - ContentType: `new_pinned.content_type`
    - ContentDisposition: `new_pinned.content_disposition`
    - Metadata: `new_pinned.user_metadata` (which does NOT contain `gts_file_type`) - `inst-cosc-1`
2. [ ] - `p1` - **IF** `if_match.is_some()` → set `CopySourceIfMatch: <if_match>` header - `inst-cosc-2`
3. [ ] - `p1` - Issue the request via `aws-sdk-s3 CopyObject` - `inst-cosc-3`
4. [ ] - `p1` - **IF** S3 returns 412 (precondition) → return Err(EtagMismatch) - `inst-cosc-4`
5. [ ] - `p1` - **IF** S3 returns 5xx or transport error → return Err(BackendFailure) - `inst-cosc-5`
6. [ ] - `p1` - Extract `new_etag` (strip surrounding quotes) and `new_version_id` (or `None` on non-versioning buckets) from the response - `inst-cosc-6`
7. [ ] - `p1` - **RETURN** Ok((new_etag, new_version_id)) - `inst-cosc-7`

### Issue Batch of Presigned GET URLs

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-s3-compatible-adapter-presigned-get-batch`

**Input**: `Vec<(BackendObjectKey key, UrlParams params, MimeHint, NameHint)>`

**Output**: `Vec<PresignedDownload { url, expires_at }>` — one per input entry, in the same order

**Steps**:

1. [ ] - `p1` - **FOR EACH** input entry - `inst-pget-1`
   1. [ ] - `p1` - **IF** backend declares `PublicReadUrls` AND `default_public = true` AND `item.version_id` is `None` → build a bare-HTTPS URL (no signing) of the form `https://<bucket-host>/<key>`; `expires_at` is a far-future sentinel; `is_public = true` - `inst-pget-1a`
   2. [ ] - `p1` - **ELSE** (SigV4 GET URL) - `inst-pget-1b`
      1. [ ] - `p1` - Compute `expires_at = NOW() + min(params.expires_in_seconds, backend.max_signed_url_ttl)` - `inst-pget-1b1`
      2. [ ] - `p1` - Build response-overrides query params: `response-content-disposition` = `attachment; filename="<URL-encoded display_name_hint>"`, `response-content-type` = `<mime_type_hint>` - `inst-pget-1b2`
      3. [ ] - `p1` - **IF** `item.version_id.is_some()` AND backend has `versioning = true` → also set `versionId=<vid>` query param - `inst-pget-1b3`
      4. [ ] - `p1` - Generate SigV4 GET URL via `aws-sdk-s3 generate_presigned_url(GetObject, …)` - `inst-pget-1b4`
      5. [ ] - `p1` - `is_public = false` - `inst-pget-1b5`
   3. [ ] - `p1` - Append `PresignedDownload { url, expires_at, is_public }` to the result - `inst-pget-1c`
2. [ ] - `p1` - **RETURN** `Vec<PresignedDownload>` (preserving input order) - `inst-pget-2`

### Open Read Stream

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-s3-compatible-adapter-open-read`

**Input**: `BackendObjectKey key`

**Output**: `(FileByteStream, BackendObjectMetadata { s3_etag, s3_version_id, size_bytes, content_type, content_disposition, user_metadata })`

**Steps**:

1. [ ] - `p1` - API: GetObject(bucket=$bucket, key=$key) - `inst-oread-1`
2. [ ] - `p1` - **IF** S3 returns 404 - `inst-oread-2`
   1. [ ] - `p1` - **RETURN** Err(NotFound) - `inst-oread-2a`
3. [ ] - `p1` - **IF** S3 returns 5xx - `inst-oread-3`
   1. [ ] - `p1` - **RETURN** Err(BackendFailure) - `inst-oread-3a`
4. [ ] - `p1` - Capture `s3_etag` (strip surrounding quotes), `s3_version_id` (from `x-amz-version-id` header; `None` on non-versioning buckets), `size_bytes` (Content-Length), `content_type`, `content_disposition`, and every `x-amz-meta-<k>` header (case-normalized, prefix stripped — placed in `user_metadata`) from response headers - `inst-oread-4`
5. [ ] - `p1` - Wrap the `aws-sdk-s3` body in a `Stream<Item = Result<Bytes, FileStorageError>>` adapter - `inst-oread-5`
6. [ ] - `p1` - **RETURN** `(stream, metadata)` to the caller (the SDK facade then runs the lazy self-healing check) - `inst-oread-6`

### Head Object

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-s3-compatible-adapter-head-object`

**Input**: `BackendObjectKey key`

**Output**: `BackendObjectMetadata { s3_etag, s3_version_id, size_bytes, content_type, content_disposition, user_metadata }`

**Steps**:

1. [ ] - `p1` - API: HeadObject(bucket=$bucket, key=$key) - `inst-head-1`
2. [ ] - `p1` - **IF** S3 returns 404 - `inst-head-2`
   1. [ ] - `p1` - **RETURN** Err(BackendFailure) (caller — typically `reconcile` — surfaces this as HTTP 502; the row is left intact for the operator) - `inst-head-2a`
3. [ ] - `p1` - **IF** S3 returns 5xx or transport error - `inst-head-3`
   1. [ ] - `p1` - **RETURN** Err(BackendFailure) - `inst-head-3a`
4. [ ] - `p1` - Capture `s3_etag` (strip surrounding quotes), `s3_version_id` (from `x-amz-version-id` header), `size_bytes` (Content-Length), `content_type`, `content_disposition`, and every `x-amz-meta-<k>` header (case-normalized, prefix stripped) from the response - `inst-head-4`
5. [ ] - `p1` - **RETURN** Ok(BackendObjectMetadata { … }) - `inst-head-5`

`head_object` is required by:
- the upload coordinator's `reconcile` flow (see [feature-upload-lifecycle](./feature-upload-lifecycle.md) §3 "Reconcile Algorithm");
- `put_file_info`'s strong-CAS variant when `If-Match` is supplied (HEAD verifies S3.etag and S3.version_id before issuing CopyObject).

### Delete Object

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-s3-compatible-adapter-delete`

**Input**: `BackendObjectKey key`

**Output**: `Result<(), FileStorageError>` — best-effort, idempotent

**Steps**:

1. [ ] - `p1` - API: DeleteObject(bucket=$bucket, key=$key) - `inst-del-1`
2. [ ] - `p1` - Treat `NotFound` as success (idempotent) - `inst-del-2`
3. [ ] - `p1` - Treat 5xx as `BackendFailure`; the orphan-delete queue retries later - `inst-del-3`
4. [ ] - `p1` - **RETURN** Ok(()) on 2xx or 404 - `inst-del-4`

## 4. States (CDSL)

The adapter is stateless; no per-key lifecycle is tracked here. The `pending_upload → uploaded` row state lives in the Files Repo, not in the adapter.

## 5. Definitions of Done

### Implement S3 Adapter Skeleton

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-s3-compatible-adapter-skeleton`

The system **MUST** expose an `S3Backend` struct that implements the internal `StorageBackend` trait (6 methods: `open_read`, `head_object`, `delete_object`, `issue_presigned_put`, `copy_object_self`, `issue_presigned_gets`), holds a single `aws-sdk-s3` client per backend instance, and is registered with the Backend Router under its configured `id`.

**Implements**:

- `cpt-cf-file-storage-algo-s3-compatible-adapter-open-read`
- `cpt-cf-file-storage-algo-s3-compatible-adapter-head-object`
- `cpt-cf-file-storage-algo-s3-compatible-adapter-delete`
- `cpt-cf-file-storage-algo-s3-compatible-adapter-copy-object-self`

**Constraints**: `cpt-cf-file-storage-constraint-opaque-content`

**Touches**:

- Crate: `file-storage` (component: S3 Backend Adapter)
- Trait: `StorageBackend`

### Implement SigV4 PUT and GET URL Issuance

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-s3-compatible-adapter-sigv4`

The system **MUST** produce SigV4-signed PUT URLs (with the pinned `Content-Type`, `Content-Disposition`, and `x-amz-meta-<k>` headers from `PinnedObjectHeaders`; never `x-amz-meta-gts-file-type`) and SigV4-signed GET URLs (with response-overrides for `Content-Disposition` and `Content-Type` always set from the DB row). For backends with `PublicReadUrls` capability and `default_public = true`, the GET-URL path **MUST** also support emitting bare-HTTPS URLs without signing (`is_public = true`, no expiry). P1 does NOT pin any conditional preconditions on the upload PUT path. URL TTLs **MUST** be capped by the backend's configured `max_signed_url_ttl`.

**Implements**:

- `cpt-cf-file-storage-algo-s3-compatible-adapter-presigned-put`
- `cpt-cf-file-storage-algo-s3-compatible-adapter-presigned-get-batch`

**Constraints**: `cpt-cf-file-storage-constraint-opaque-content`

**Touches**:

- Crate: `file-storage` (component: S3 Backend Adapter)

### Implement Streaming Read Path

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-s3-compatible-adapter-streaming-read`

The system **MUST** expose `open_read(key) -> (Stream<Bytes>, BackendObjectMetadata)`; the stream **MUST** chunk through the `aws-sdk-s3 GetObject` response body without buffering the full object in memory. The metadata struct **MUST** include the S3 `ETag` header value so the SDK facade can run self-healing reconciliation.

**Implements**:

- `cpt-cf-file-storage-algo-s3-compatible-adapter-open-read`

**Constraints**: (no new constraint registrations — covered by stream-by-default and opaque-content above)

**Touches**:

- Crate: `file-storage` (component: S3 Backend Adapter)

### Implement Metadata Mirror Pinning (Initial Upload + Variant-B + CopyObject)

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-s3-compatible-adapter-metadata-mirror`

On every presigned PUT (initial upload OR variant-B re-upload) and on every `CopyObject` self-copy, the system **MUST** pin the user-visible mirrored metadata fields per the supplied `PinnedObjectHeaders`: `Content-Type`, `Content-Disposition`, and every `x-amz-meta-<k>` entry. The adapter **MUST NOT** add `x-amz-meta-gts-file-type` — that field is DB-only (`cpt-cf-file-storage-constraint-meta-mirrored-via-put-meta`). On read paths, the adapter returns the captured user-metadata as part of `BackendObjectMetadata`; whether to adopt it is the upper layer's decision (`reconcile` does, `put_file_info` does not — it WRITES new metadata via `CopyObject`).

**Implements**:

- `cpt-cf-file-storage-algo-s3-compatible-adapter-presigned-put`
- `cpt-cf-file-storage-algo-s3-compatible-adapter-copy-object-self`

**Constraints**: `cpt-cf-file-storage-constraint-meta-mirrored-via-put-meta`

**Touches**:

- Crate: `file-storage` (component: S3 Backend Adapter)
- S3 user-metadata: `Content-Type`, `Content-Disposition`, `x-amz-meta-<k>` (from `meta.custom_metadata`); never `x-amz-meta-gts-file-type`

## 6. Acceptance Criteria

- [ ] An end-client given a presigned PUT URL can write bytes directly to the bucket and gets `200 OK` from S3.
- [ ] A presigned GET URL produced by the adapter, when followed by a browser, downloads the file with the `response-content-disposition` (filename) and `response-content-type` query-param-driven headers — independent of any drift in S3 user-metadata.
- [ ] `open_read(key)` streams the object body chunk-by-chunk; no full-object buffering observable in memory profiles. The returned `BackendObjectMetadata` contains `s3_etag`, `s3_version_id`, `size_bytes`, `content_type`, `content_disposition`, and `user_metadata`.
- [ ] `head_object(key)` returns the same `BackendObjectMetadata` shape; on missing key it returns `BackendFailure`. On a versioning-on bucket, `s3_version_id` is `Some(_)`; on a versioning-off bucket it is `None`.
- [ ] `delete_object(key)` is idempotent: a second call on an already-deleted key returns `Ok` without error.
- [ ] `copy_object_self(key, new_pinned, if_match)` on an existing object replaces user-metadata with the new pinned set (verifiable via a follow-up HEAD); returns the new `s3_etag` and `s3_version_id`. With a stale `if_match`, S3 returns 412 and the adapter surfaces `Err(EtagMismatch)`.
- [ ] On every successful presigned PUT, the resulting S3 object carries the pinned `Content-Type`, `Content-Disposition`, and `x-amz-meta-<k>` headers exactly as supplied; the object does NOT carry `x-amz-meta-gts-file-type`.
- [ ] On every successful `CopyObject` self-copy, the resulting S3 object's user-metadata is REPLACED to exactly the new pinned set; absent entries from the new set are dropped from the object.
- [ ] An attempt to read those mirror fields via the FileStorage `get_file_info` REST surface returns the **DB row** values, not the S3 user-metadata values (proves SQL is authoritative for reads).
- [ ] On a `PublicReadUrls`-capable backend with `default_public = true`, `issue_presigned_gets` returns a bare-HTTPS URL (no SigV4 signature query params, no expiry) and `is_public = true`.
