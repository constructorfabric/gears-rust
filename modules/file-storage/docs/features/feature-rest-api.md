<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: REST API

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-rest-api`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-file-storage-feature-rest-api`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [HTTP Request Lifecycle](#http-request-lifecycle)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Conditional Header Mapping](#conditional-header-mapping)
  - [ProblemDetails Error Mapping](#problemdetails-error-mapping)
  - [Reconcile Endpoint Algorithm](#reconcile-endpoint-algorithm)
  - [PUT Meta Endpoint Algorithm](#put-meta-endpoint-algorithm)
  - [Delete Endpoint Algorithm](#delete-endpoint-algorithm)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Wire All P1 Endpoints](#wire-all-p1-endpoints)
  - [Implement Conditional Headers Layer](#implement-conditional-headers-layer)
  - [Implement RFC 7807 Error Translation](#implement-rfc-7807-error-translation)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Wire every SDK method to the 7 HTTP endpoints documented in [openapi.yaml](../openapi.yaml). All write operations are PUT-shaped or POST-shaped; there is no PATCH anywhere. **No proxied content endpoint exists in P1** — neither `GET /files/{file_id}/content` nor `PUT /files/{file_id}/content`: byte transfers always flow client ↔ storage backend directly via presigned URLs (or bare HTTPS for public-read backends). In-process consumers use the SDK `read_file` method, which has no REST surface. Conditional headers (`If-Match`, `If-None-Match`) are mapped 1:1 to the SDK's `etag` arguments — except for `POST /files/{id}/meta/reconcile`, which **rejects** `If-Match` with `400`. Errors are RFC 7807 `application/problem+json`.

### 1.2 Purpose

This feature is the externally-visible HTTP contract of FileStorage. Every consumer that lives outside the monolith process (platform UI, future browser-direct flows, integration tests, application backends in different services) depends on this surface. Per `cpt-cf-file-storage-principle-presign-first` the data plane stays off FileStorage — the REST endpoints orchestrate upload/download URL issuance, metadata mutations (with atomic DB+S3 sync via `CopyObject` self-copy), and explicit reconciliation; bytes flow client ↔ S3 directly via presigned URLs.

**Requirements**: `cpt-cf-file-storage-fr-rest-api`, `cpt-cf-file-storage-interface-rest-api`

**Principles**: `cpt-cf-file-storage-principle-presign-first`, `cpt-cf-file-storage-principle-stream-by-default`, `cpt-cf-file-storage-principle-multi-phase-commit`

### 1.3 Actors

- `cpt-cf-file-storage-actor-platform-user` — platform UI sending HTTP requests
- `cpt-cf-file-storage-actor-cf-modules` — application backends in different processes calling FileStorage over HTTP

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (§3.2 REST API component, §3.3 API Contracts)
- **OpenAPI**: [openapi.yaml](../openapi.yaml)
- **Use cases**: every P1 use case in PRD §8
- **Decomposition**: [DECOMPOSITION.md §2.8](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-file-storage-feature-upload-lifecycle`, `cpt-cf-file-storage-feature-read-and-update`, `cpt-cf-file-storage-feature-batch-presign-downloads`

## 2. Actor Flows (CDSL)

### HTTP Request Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-rest-api-request-lifecycle`

**Actor**: `cpt-cf-file-storage-actor-cf-modules` or `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:

- 2xx response on a happy-path call (e.g. `200 + FileInfo` body, `204` on DELETE, `200 + ReconcileResponse` on reconcile)
- `304 Not Modified` on `If-None-Match` GET when the row's etag matches

**Error Scenarios**:

- 401 Unauthorized when ModKit middleware rejects the bearer token
- 403 Forbidden when authz denies the GTS file type
- 404 Not Found when the row is absent for the caller's tenant (no enumeration oracle)
- 409 Conflict / `delete_in_progress` for operations against a `Deleting` row, `conflict` after 3 retries on `reconcile` / `put_file_info`, or `capability_unavailable` for missing capabilities
- 412 Precondition Failed on `If-Match` etag mismatch (DB or, on `PUT /meta` with strong CAS, S3)
- 413 Payload Too Large on body exceeding the backend's `max_file_size_bytes` or aggregate user-metadata exceeding 2 KB
- 5xx wrapped in `ProblemDetails` with stable `code`

**Steps**:

1. [ ] - `p1` - HTTP request hits axum router under `/api/file-storage/v1/` - `inst-rl-1`
2. [ ] - `p1` - ModKit middleware extracts `SecurityContext` from the bearer token - `inst-rl-2`
3. [ ] - `p1` - Axum handler maps the request to the corresponding SDK method - `inst-rl-3`
4. [ ] - `p1` - Conditional headers (`If-Match`, `If-None-Match`) are translated into the SDK `etag` argument (rejected for `POST /files/{id}/meta/reconcile`, optional for `PUT /meta` and `DELETE`) - `inst-rl-4`
5. [ ] - `p1` - SDK method runs (defined in features 2.5 / 2.6 / 2.7 / 2.3) - `inst-rl-5`
6. [ ] - `p1` - **IF** SDK returns Ok - `inst-rl-6`
   1. [ ] - `p1` - Build response: status code + headers (`ETag`, `Content-Type` where applicable) + body (JSON) - `inst-rl-6a`
7. [ ] - `p1` - **ELSE** map `FileStorageError` to RFC 7807 `application/problem+json` - `inst-rl-7`
   1. [ ] - `p1` - Set HTTP status from the canonical mapping (NotFound→404, EtagMismatch→412, DeleteInProgress→409, AccessDenied→403, CapabilityUnavailable→409, BadRequest→400, PayloadTooLarge→413, UploadExpired→410, BackendFailure→502, Internal→500) - `inst-rl-7a`
8. [ ] - `p1` - **RETURN** HTTP response with `X-Request-Id` header for correlation - `inst-rl-8`

## 3. Processes / Business Logic (CDSL)

### Conditional Header Mapping

- [ ] `p2` - **ID**: `cpt-cf-file-storage-algo-rest-api-conditional-headers`

**Input**: Inbound HTTP request with optional `If-Match` and `If-None-Match` headers

**Output**: SDK call arguments with `etag: Option<&Etag>` and short-circuit `304 Not Modified` decisions

**Steps**:

1. [ ] - `p1` - **IF** request has `If-Match` header - `inst-ch-1`
   1. [ ] - `p1` - Strip surrounding quotes per RFC 7232; pass as SDK `etag = Some(value)` - `inst-ch-1a`
2. [ ] - `p1` - **IF** request has `If-None-Match` header - `inst-ch-2`
   1. [ ] - `p1` - Strip quotes; on `GET /files/{file_id}/meta` (the metadata read) the SDK runs the read; if `row.etag == header_value` → return `304 Not Modified` (empty body, `ETag` header set) - `inst-ch-2a`
3. [ ] - `p1` - **IF** both headers present - `inst-ch-3`
   1. [ ] - `p1` - **RETURN** `400 Bad Request` (RFC 9110 §13.1) - `inst-ch-3a`
4. [ ] - `p1` - **IF** an `If-Match` header is supplied on `POST /files/{file_id}/meta/reconcile` - `inst-ch-4`
   1. [ ] - `p1` - **RETURN** `400 Bad Request` — `reconcile` is the explicit reconciliation primitive and rejects preconditions - `inst-ch-4a`

### ProblemDetails Error Mapping

- [ ] `p2` - **ID**: `cpt-cf-file-storage-algo-rest-api-problem-details`

**Input**: A `FileStorageError` variant from any SDK method

**Output**: An `application/problem+json` response with stable `code`, HTTP status, `correlation_id` mirroring `X-Request-Id`

**Steps**:

1. [ ] - `p1` - Map error variant to HTTP status: NotFound→404, AccessDenied→403, BadRequest→400, EtagMismatch→412, DeleteInProgress→409, InvalidStatusTransition→409, CapabilityUnavailable→409, PayloadTooLarge→413, UploadExpired→410, BackendFailure→502, Internal→500 - `inst-pd-1`
2. [ ] - `p1` - Build body: `{ type: "https://example.com/probs/<code>", title: "<short>", status: <int>, detail: "<descriptive>", code: "<stable>", correlation_id: <X-Request-Id> }` - `inst-pd-2`
3. [ ] - `p1` - For `EtagMismatch` include the row's current etag in the `ETag` response header so the caller can update its pin - `inst-pd-3`
4. [ ] - `p1` - For `PayloadTooLarge` caused by the user-metadata budget include `max_metadata_bytes: 2048` extension in the body - `inst-pd-4`
5. [ ] - `p1` - **RETURN** the response - `inst-pd-5`

### Reconcile Endpoint Algorithm

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-rest-api-reconcile`

**Input**: `POST /api/file-storage/v1/files/{file_id}/meta/reconcile` with empty body and **no** `If-Match` header

**Output**: `200 OK + ReconcileResponse { info: FileInfo, s3_etag: string, s3_version_id: [string, 'null'] }` on success; problem-details JSON on failure

**Steps**:

1. [ ] - `p1` - Parse `file_id` from path; reject malformed UUID with `400 bad_request` - `inst-rec-1`
2. [ ] - `p1` - **IF** request body is non-empty - `inst-rec-2`
   1. [ ] - `p1` - **RETURN** `400 bad_request` (reconcile takes no body in P1) - `inst-rec-2a`
3. [ ] - `p1` - **IF** request carries `If-Match` header - `inst-rec-3`
   1. [ ] - `p1` - **RETURN** `400 bad_request` per `cpt-cf-file-storage-algo-rest-api-conditional-headers` - `inst-rec-3a`
4. [ ] - `p1` - SDK call: `files.reconcile(ctx, file_id)` - `inst-rec-4`
5. [ ] - `p1` - **IF** Ok(ReconcileResult) - `inst-rec-5`
   1. [ ] - `p1` - Build response: `200 OK` with body `{ info: <FileInfo>, s3_etag: "<raw S3 ETag>", s3_version_id: <string|null> }`; ETag response header set to `info.etag` - `inst-rec-5a`
6. [ ] - `p1` - **ELSE** translate the error per `cpt-cf-file-storage-algo-rest-api-problem-details` - `inst-rec-6`
   1. [ ] - `p1` - `DeleteInProgress` → 409 - `inst-rec-6a`
   2. [ ] - `p1` - `BackendFailure` → 502 - `inst-rec-6b`
   3. [ ] - `p1` - `UploadExpired` → 410 - `inst-rec-6c`
   4. [ ] - `p1` - 3-retry exhaustion → 409 `code = conflict` - `inst-rec-6d`
   5. [ ] - `p1` - `NotFound` → 404; `AccessDenied` → 403; `Internal` → 500 - `inst-rec-6e`

### PUT Meta Endpoint Algorithm

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-rest-api-put-meta`

**Input**: `PUT /api/file-storage/v1/files/{file_id}/meta` with `FileMetaUpdate` body and **optional** `If-Match` header

**Output**: `200 OK + FileInfo` on success; problem-details JSON on failure

**Steps**:

1. [ ] - `p1` - Parse `file_id` and request body; reject body with unknown fields (notably `gts_file_type`) with `400 bad_request` (the `additionalProperties: false` schema constraint catches this) - `inst-pm-1`
2. [ ] - `p1` - SDK call: `files.put_file_info(ctx, file_id, update, etag?)` per `cpt-cf-file-storage-algo-read-and-update-put-info` (which executes the DB+S3 atomic sync via `CopyObject` self-copy) - `inst-pm-2`
3. [ ] - `p1` - **IF** Ok(FileInfo) - `inst-pm-3`
   1. [ ] - `p1` - Build response: `200 OK` with body `<FileInfo>`; ETag response header set to `info.etag` (the raw S3 ETag returned by the CopyObject) - `inst-pm-3a`
4. [ ] - `p1` - **ELSE** translate per `cpt-cf-file-storage-algo-rest-api-problem-details`. Notable mappings: `EtagMismatch → 412` (DB or S3 or version_id check failed), `PayloadTooLarge → 413` (with `max_metadata_bytes` extension), `BackendFailure → 502` (CopyObject failed) - `inst-pm-4`

### Delete Endpoint Algorithm

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-rest-api-delete`

**Input**: `DELETE /api/file-storage/v1/files/{file_id}` with **optional** `If-Match` header and empty body

**Output**: `204 No Content` on success; problem-details JSON on failure

**Steps**:

1. [ ] - `p1` - Parse `file_id` from path; reject malformed UUID with `400` - `inst-del-1`
2. [ ] - `p1` - SDK call: `files.delete_file(ctx, file_id, etag?)` per `cpt-cf-file-storage-algo-read-and-update-delete-file` (2-phase delete) - `inst-del-2`
3. [ ] - `p1` - **IF** Ok(()) - `inst-del-3`
   1. [ ] - `p1` - **RETURN** `204 No Content` - `inst-del-3a`
4. [ ] - `p1` - **ELSE** translate per `cpt-cf-file-storage-algo-rest-api-problem-details`. Notable mappings: `EtagMismatch → 412` (Phase 1 If-Match check failed), `DeleteInProgress → 409` (row already in `deleting`), `BackendFailure → 502` (Phase 2 backend DELETE failed after retries; row left in `deleting`) - `inst-del-4`

## 4. States (CDSL)

The REST API is stateless per request; no per-connection or per-session state is held in axum handlers.

## 5. Definitions of Done

### Wire All P1 Endpoints

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-rest-api-wire-endpoints`

The system **MUST** wire the following 7 endpoints under `/api/file-storage/v1/` exactly as documented in [openapi.yaml](../openapi.yaml):

- `GET /storages` → `list_backends`
- `GET /files` → `list_files` (P1 query parameters: `owner_id?`, `cursor?`, `limit?`)
- `GET /files/{file_id}/meta` → `get_file_info` (supports `If-None-Match` → `304`)
- `PUT /files/{file_id}/meta` → `put_file_info` (optional `If-Match`; atomic DB+S3 sync via `CopyObject` self-copy with `MetadataDirective: REPLACE`; body declares `name`, `mime_type`, `custom_metadata` only — `gts_file_type` rejected with 400)
- `POST /files/{file_id}/meta/reconcile` → `reconcile` (rejects `If-Match` with 400; empty body; response `ReconcileResponse { info, s3_etag, s3_version_id }`)
- `DELETE /files/{file_id}` → `delete_file` (optional `If-Match`; 2-phase delete with transient `Deleting` status)
- `POST /presign-batch` → unified upload + download endpoint:
  - `kind: "upload"` without `file_id` → `create_presigned_url` (initial upload; `backend_id` optional in body, falls back to `default_private`)
  - `kind: "upload"` with `file_id` → `create_presigned_overwrite_url` (variant-B re-upload; the item MUST NOT carry `meta`, rejected with 400 otherwise)
  - `kind: "download"` → `presign_urls` (per-item `etag?`, `version_id?`)

Neither `GET /files/{file_id}/content` nor `PUT /files/{file_id}/content` is wired in P1: all byte transfers flow client ↔ storage backend directly via presigned URLs (or bare HTTPS for public-read backends). In-process consumers use the SDK `read_file` method (no REST surface) for self-healing-aware streaming reads.

**Implements**:

- `cpt-cf-file-storage-flow-rest-api-request-lifecycle`

**Constraints**: `cpt-cf-file-storage-constraint-no-ambient-authn`

**Touches**:

- Crate: `file-storage` (component: REST API)
- HTTP endpoints (7 routes)

### Implement Conditional Headers Layer

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-rest-api-conditional-headers`

The system **MUST** translate `If-Match` / `If-None-Match` into SDK `etag` arguments per `cpt-cf-file-storage-algo-rest-api-conditional-headers` and emit `ETag` headers on every response carrying a `FileInfo` payload (or 304 Not Modified). The `If-Match` header **MUST** be optional on `PUT /files/{file_id}/meta` and `DELETE /files/{file_id}`. The header **MUST** be rejected on `POST /files/{file_id}/meta/reconcile` — `reconcile` is the explicit reconciliation primitive and deliberately takes no precondition.

**Implements**:

- `cpt-cf-file-storage-algo-rest-api-conditional-headers`

**Constraints**: (no new constraint registrations)

**Touches**:

- Crate: `file-storage` (component: REST API)

### Implement RFC 7807 Error Translation

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-rest-api-error-translation`

The system **MUST** translate every `FileStorageError` variant into RFC 7807 `application/problem+json` per `cpt-cf-file-storage-algo-rest-api-problem-details`. The `code` field **MUST** be one of the documented stable values (see [openapi.yaml](../openapi.yaml) `ProblemDetails`); HTTP status code mapping **MUST** be deterministic. The `correlation_id` field **MUST** mirror the request's `X-Request-Id` header so external observability tools can join. `PayloadTooLarge` errors caused by the user-metadata budget MUST include the `max_metadata_bytes` extension.

**Implements**:

- `cpt-cf-file-storage-algo-rest-api-problem-details`

**Constraints**: (no new constraint registrations)

**Touches**:

- Crate: `file-storage` (component: REST API)
- Schema: `ProblemDetails` (openapi.yaml)

## 6. Acceptance Criteria

- [ ] All 7 P1 endpoints respond with the documented status codes and bodies for the happy paths in [openapi.yaml](../openapi.yaml).
- [ ] `PUT /files/{file_id}/meta` without `If-Match` succeeds (best-effort last-write-wins on metadata); with `If-Match: "<stale>"` returns `412 etag_mismatch`.
- [ ] `PUT /files/{file_id}/meta` with `If-Match` triggers HEAD against S3 to verify `s3_etag` (and on versioning-on backends, `s3_version_id`) before issuing `CopyObject`. Test: re-upload bit-identical bytes via `create_presigned_overwrite_url` + reconcile to rotate `version_id` (versioning-on backend); a subsequent `PUT /meta` with the original `If-Match` returns `412` even though etag is unchanged.
- [ ] `PUT /files/{file_id}/meta` with `gts_file_type` in the body returns `400 bad_request`.
- [ ] `PUT /files/{file_id}/meta` with aggregate user-metadata > 2 KB returns `413 payload_too_large` with `max_metadata_bytes: 2048`.
- [ ] `DELETE /files/{file_id}` without `If-Match` succeeds for an `uploaded` row; with `If-Match: "<stale>"` returns `412 etag_mismatch`.
- [ ] `POST /files/{file_id}/meta/reconcile` with an `If-Match` header returns `400 bad_request`; without it the reconcile runs and returns `200 + ReconcileResponse`.
- [ ] `POST /files/{file_id}/meta/reconcile` against a row in `Deleting` returns `409 delete_in_progress`.
- [ ] `GET /files/{file_id}/meta` with `If-None-Match: "<current_etag>"` returns `304 Not Modified` with no body and the `ETag` header populated.
- [ ] A request that triggers `EtagMismatch` returns `412` with the row's current etag in the `ETag` response header AND inside the `ProblemDetails` body.
- [ ] A request that triggers `CapabilityUnavailable` (e.g. `POST /presign-batch` against a backend without `PresignedUrls`) returns `409 capability_unavailable`.
- [ ] `POST /presign-batch` upload item with `file_id` set AND `meta` populated returns per-item `400 bad_request` with `code = bad_request` (the server pins the row's current meta on variant-B re-upload).
- [ ] Every error response contains `correlation_id` matching the request's `X-Request-Id` header.
- [ ] Neither `GET /files/{file_id}/content` nor `PUT /files/{file_id}/content` is wired — both return `404 Not Found` from axum routing; the openapi spec does not declare them.
