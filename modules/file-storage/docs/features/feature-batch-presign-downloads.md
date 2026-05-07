<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: Batch Presigned Downloads

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-batch-presign-downloads`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-file-storage-feature-batch-presign-downloads`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Application-Backend Hands Browser N URLs](#application-backend-hands-browser-n-urls)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Presign URLs Batch Algorithm](#presign-urls-batch-algorithm)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement presign_urls Batch Method](#implement-presign_urls-batch-method)
  - [Enforce Orphan-Delete Grace Invariant](#enforce-orphan-delete-grace-invariant)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Implement `presign_urls` — the batch entry-point for browser-bound download URLs. Per item, optionally fail-fast on stale `etag`, optionally request a historical generation via `version_id` (only on backends with `versioning = true`), then issue either a SigV4-signed GET URL (with `response-content-type` and `response-content-disposition` overridden from the DB row) or a bare-HTTPS URL for files in backends with the `PublicReadUrls` capability and `default_public = true`.

Every download URL FileStorage issues sets `response-content-type` and `response-content-disposition` query params from the DB row's metadata (`cpt-cf-file-storage-constraint-presigned-download-headers-from-db`). This decouples the user-visible download from any S3-stored metadata that may have drifted, and makes file renames immediately reflected on every newly-issued URL without re-uploading bytes.

The batch shape is what keeps the future P3-remote topology at one RTT regardless of N (DESIGN §2.1, batch-first principle).

### 1.2 Purpose

Application backends that render file lists for end-users (chat UI, document picker, attachment grid) need to hand the browser N URLs at once. A singleton SDK call would force one round-trip per URL in the eventual P3-remote topology; the batch method amortises N URLs to one network hop. Per-item etag fail-fast lets callers pin the specific version they intend the user to see, surfacing `EtagMismatch` per item without rolling back the whole batch.

Rows in `Deleting` are excluded from the candidate set just like `pending_upload` rows: a presign request against such a row returns per-item `NotFound` (no enumeration leak between "absent" and "being deleted").

**Requirements**: `cpt-cf-file-storage-fr-signed-urls`

**Principles**: `cpt-cf-file-storage-principle-batch-presigned-urls`

### 1.3 Actors

- `cpt-cf-file-storage-actor-cf-modules` — application backends calling `presign_urls` to hand the browser a precomputed list of download URLs

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (§3.6 batched presigned downloads sequence, §3.9 step 10 invariant I6)
- **ADR**: [ADR-0003](../ADR/0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md)
- **Use cases**: `cpt-cf-file-storage-usecase-signed-url`, `cpt-cf-file-storage-usecase-fetch-media`
- **Decomposition**: [DECOMPOSITION.md §2.7](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-file-storage-feature-s3-compatible-adapter`, `cpt-cf-file-storage-feature-files-schema-and-repo`

## 2. Actor Flows (CDSL)

### Application-Backend Hands Browser N URLs

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-batch-presign-downloads-hand-urls`

**Actor**: `cpt-cf-file-storage-actor-cf-modules`

**Success Scenarios**:

- All N items succeed → caller receives a `Vec<PresignedDownload>` of length N
- Some items fail (authz, NotFound, EtagMismatch) → caller receives a per-item outcome vector with `Err` slots; the rest contain valid URLs

**Error Scenarios**:

- Whole-batch DB failure → outer `Err`
- Empty input vector → `Err(BadRequest)`

**Steps**:

1. [ ] - `p1` - Application backend assembles `Vec<PresignDownloadItem>` (one per file the user is about to see) - `inst-bflow-1`
2. [ ] - `p1` - Application backend invokes `presign_urls(ctx, items)` via the SDK - `inst-bflow-2`
3. [ ] - `p1` - SDK runs `cpt-cf-file-storage-algo-batch-presign-downloads-presign-batch` - `inst-bflow-3`
4. [ ] - `p1` - Application backend receives the per-item outcomes; merges into the response payload returned to its frontend - `inst-bflow-4`
5. [ ] - `p1` - Browser fetches each URL directly against the storage backend - `inst-bflow-5`

## 3. Processes / Business Logic (CDSL)

### Presign URLs Batch Algorithm

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-batch-presign-downloads-presign-batch`

**Input**: `(SecurityContext ctx, Vec<PresignDownloadItem> items)` where each item carries `(file_id, params, Option<Etag>, Option<String> version_id)`

**Output**: `Vec<PresignDownloadOutcome>` of the same length, in the same order

**Steps**:

1. [ ] - `p1` - **IF** items.is_empty() OR items.len() > 100 - `inst-pb-1`
   1. [ ] - `p1` - **RETURN** Err(BadRequest) - `inst-pb-1a`
2. [ ] - `p1` - DB: SELECT the rows from `file_storage.files` matching `(id IN $items.file_id, tenant_id = $ctx.tenant_id, status = 'uploaded')` — fields needed: `id`, `backend_id`, `etag`, `gts_file_type`, `name`, `mime_type`. Backend-side fields (`versioning`) are looked up from the in-memory roster - `inst-pb-2`
3. [ ] - `p1` - Build a hash map by `file_id` for O(1) lookup - `inst-pb-3`
4. [ ] - `p1` - **FOR EACH** item in items - `inst-pb-4`
   1. [ ] - `p1` - **IF** lookup misses (row absent for this tenant) - `inst-pb-4a`
      1. [ ] - `p1` - Append `PresignDownloadOutcome { file_id, ok: None, error: NotFound }` - `inst-pb-4a1`
   2. [ ] - `p1` - **IF** `item.etag.is_some() && item.etag != row.etag` (DB-only check; no HEAD against S3) - `inst-pb-4b`
      1. [ ] - `p1` - Append `PresignDownloadOutcome { file_id, ok: None, error: EtagMismatch{ current: row.etag } }` - `inst-pb-4b1`
   3. [ ] - `p1` - API: `authz.authorize(ctx, read, "gts.x.fstorage.file.type.v1~{row.gts_file_type}")` - `inst-pb-4c`
   4. [ ] - `p1` - **IF** denied - `inst-pb-4d`
      1. [ ] - `p1` - Append `PresignDownloadOutcome { file_id, ok: None, error: AccessDenied }` - `inst-pb-4d1`
   5. [ ] - `p1` - Resolve `backend = registry[row.backend_id]` - `inst-pb-4e`
   6. [ ] - `p1` - **IF** backend declares `PresignedUrls` (mandatory) - `inst-pb-4f`
      1. [ ] - `p1` - Derive S3 key from `row.id` at the adapter boundary - `inst-pb-4f1`
      2. [ ] - `p1` - Build `PresignedGetItem { key, params, mime_type_hint = row.mime_type, display_name_hint = row.name, version_id = item.version_id (only when backend.versioning = true; otherwise None) }` - `inst-pb-4f2`
      3. [ ] - `p1` - Adapter call: `issue_presigned_gets([item])`. The adapter sets `response-content-type=row.mime_type` and `response-content-disposition=attachment; filename="<URL-encoded row.name>"` query params on the signed URL. When backend has `PublicReadUrls` capability + `default_public = true`, the adapter MAY return a bare-HTTPS URL with `is_public = true` and no expiry instead - `inst-pb-4f3`
      4. [ ] - `p1` - Append the resulting `PresignedDownload { url, expires_at, is_public }` as `Ok` outcome - `inst-pb-4f4`
   7. [ ] - `p1` - **ELSE** - `inst-pb-4g`
      1. [ ] - `p1` - Append `PresignDownloadOutcome { ok: None, error: CapabilityUnavailable }` - `inst-pb-4g1`
5. [ ] - `p1` - **RETURN** Ok(outcomes) - `inst-pb-5`

## 4. States (CDSL)

This feature is stateless. Issued URLs survive in the wild until their TTL expires; FileStorage tracks no per-URL state.

## 5. Definitions of Done

### Implement presign_urls Batch Method

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-batch-presign-downloads-method`

The system **MUST** implement `presign_urls(ctx, Vec<PresignDownloadItem>) -> Vec<PresignDownloadOutcome>` per `cpt-cf-file-storage-algo-batch-presign-downloads-presign-batch`. Each `PresignDownloadItem` carries `{ file_id, params, etag: Option<Etag>, version_id: Option<String> }`. The `etag` check is DB-only (no HEAD against S3). The `version_id` field is honoured only when the file's hosting backend has `versioning = true`; otherwise it is silently ignored (the URL resolves to current bytes). Per-item failures (NotFound, EtagMismatch, AccessDenied, CapabilityUnavailable) **MUST** surface inside the outcome vector — the outer `Result` fails only for whole-batch faults (DB unavailable, etc.). The batch **MUST** issue exactly one DB SELECT for the whole list (no per-item N+1). Every issued URL **MUST** carry `response-content-type` and `response-content-disposition` overrides from the DB row (`cpt-cf-file-storage-constraint-presigned-download-headers-from-db`); for public-read-backed outcomes (`is_public = true`) the URL is bare HTTPS with no signing and no expiry.

**Implements**:

- `cpt-cf-file-storage-flow-batch-presign-downloads-hand-urls`
- `cpt-cf-file-storage-algo-batch-presign-downloads-presign-batch`

**Constraints**: `cpt-cf-file-storage-constraint-no-ambient-authn`

**Touches**:

- API: SDK `presign_urls`; REST `POST /api/file-storage/v1/presign-batch` (wired in `rest-api`)
- DB: `file_storage.files` (single SELECT WHERE id IN …)

### Enforce Orphan-Delete Grace Invariant

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-batch-presign-downloads-grace-invariant`

The system **MUST** verify at config-load that `orphan_delete_grace_seconds ≥ max_signed_url_ttl + signed_url_clock_skew_margin` and refuse to boot otherwise. This invariant is what turns I6 («issued URL resolves for its full TTL») from best-effort into by-construction: a `delete_file` or supersession-superseded backend object cannot be reclaimed before any presigned URL signed for it has expired.

**Implements**:

- (cross-cutting — protects every URL issued by `algo-batch-presign-downloads-presign-batch` and by the upload coordinator)

**Constraints**: `cpt-cf-file-storage-constraint-static-config-p1`

**Touches**:

- Config: `orphan_delete_grace_seconds`, `max_signed_url_ttl`, `signed_url_clock_skew_margin`

## 6. Acceptance Criteria

- [ ] `presign_urls` with 10 valid items returns 10 `Ok` outcomes with valid SigV4 URLs (or bare-HTTPS for public-read backends).
- [ ] `presign_urls` with a mix of valid items and one item whose `file_id` belongs to another tenant returns 9 `Ok` outcomes and 1 `Err(NotFound)` — no enumeration leak.
- [ ] `presign_urls` with an item carrying `etag = Some(stale)` returns `Err(EtagMismatch{ current })` for that item; the batch otherwise succeeds. The check is DB-only — no HEAD against S3 is issued.
- [ ] `presign_urls` with an item carrying `version_id = Some("v_old")` against a backend with `versioning = true` returns a URL whose query string contains `versionId=v_old`; against a backend with `versioning = false` the field is silently ignored and the URL resolves to current bytes.
- [ ] Every issued URL has `response-content-type` and `response-content-disposition` query params set from the DB row's metadata (verifiable by parsing the URL).
- [ ] For files in a backend with `PublicReadUrls` + `default_public = true`, the outcome carries `is_public = true` and `expires_at` is a far-future sentinel; the URL has no SigV4 signature.
- [ ] The batch issues exactly one DB SELECT regardless of the number of items (verifiable through query logs / EXPLAIN).
- [ ] A URL issued by `presign_urls` continues to resolve for its full TTL even after the underlying file is `delete_file`'d (because the orphan-delete grace period is ≥ max signed-URL TTL).
- [ ] Module fails-fast at boot if `orphan_delete_grace_seconds < max_signed_url_ttl`.
- [ ] An empty input vector returns `Err(BadRequest)`; a vector longer than 100 returns `Err(BadRequest)`.
