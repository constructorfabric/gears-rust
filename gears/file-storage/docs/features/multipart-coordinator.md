Created:  2026-07-02 by Constructor Tech
Updated:  2026-07-02 by Constructor Tech
# Feature: Multipart Upload Coordinator

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-multipart-coordinator-implemented`



<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Initiate Multipart Upload](#initiate-multipart-upload)
  - [Upload a Part](#upload-a-part)
  - [Complete Multipart Upload](#complete-multipart-upload)
  - [Abort Multipart Upload](#abort-multipart-upload)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Compute Parts Plan](#compute-parts-plan)
  - [Enforce Per-Part Size Claim at Sidecar](#enforce-per-part-size-claim-at-sidecar)
  - [Combine Part Hashes at Complete](#combine-part-hashes-at-complete)
- [4. States (CDSL)](#4-states-cdsl)
  - [Multipart Session State Machine](#multipart-session-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Initiate Endpoint with Server-Authoritative Plan](#initiate-endpoint-with-server-authoritative-plan)
  - [Sidecar Per-Part Enforcement](#sidecar-per-part-enforcement)
  - [Complete Endpoint with Hash Combination](#complete-endpoint-with-hash-combination)
  - [Abort Endpoint](#abort-endpoint)
  - [Introspect and Resume Endpoint](#introspect-and-resume-endpoint)
  - [Schema: multipart_uploads Plan Columns](#schema-multipart_uploads-plan-columns)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

- [ ] `p2` - `cpt-cf-file-storage-feature-multipart-coordinator`

### 1.1 Overview

Server-authoritative multipart upload coordinator for file-storage: the client declares total size and a preferred part size; the control plane computes the exact parts plan (part_number, offset, size) and returns one signed sidecar URL per part. The client uploads each part directly to the sidecar, which enforces the declared size before writing any bytes. The control plane then combines per-part hashes into the root hash at complete and binds the new file version atomically.

**Traces to**: `cpt-cf-file-storage-fr-multipart-upload`, `cpt-cf-file-storage-fr-size-limits-policy`, `cpt-cf-file-storage-fr-storage-quota`

### 1.2 Purpose

Provide a safe, resumable, server-controlled multipart upload path that eliminates the abuse vector of the earlier client-driven model: a client declaring a small `declared_size` but uploading arbitrarily large parts. Because each part's exact byte length is a claim inside its signed URL and the sidecar enforces it with HTTP 413 before writing, oversized bytes never reach the backend.

This feature supersedes the interim P2-M3 client-driven implementation in which parts were PUT to the control-plane route `PUT /files/{id}/multipart/{upload_id}/parts/{part_number}`. That control-plane byte route is removed (ADR-0003). Byte movement now flows exclusively through sidecar signed URLs (ADR-0004). Part boundaries are aligned to BLAKE3 chunk boundaries so per-part subtree hashes compose into the root hash at complete (ADR-0002; SHA-256 is the effective algorithm in P2, with BLAKE3 deferred).

**Requirements**: `cpt-cf-file-storage-fr-multipart-upload`, `cpt-cf-file-storage-fr-size-limits-policy`, `cpt-cf-file-storage-fr-storage-quota`

**Principles**: `cpt-cf-file-storage-principle-control-no-content`, `cpt-cf-file-storage-principle-signed-urls`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-file-storage-actor-platform-user` | Initiates a multipart upload by declaring intent; receives the parts plan; PUTs each part body to the sidecar URL; calls complete or abort |
| `cpt-cf-file-storage-actor-cf-gears` | Peer gear / service that drives multipart upload on behalf of a user; also subject to the same plan, quota, and enforcement rules |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) -- Section 4.6 (Multipart upload shape)
- **API contract**: [api.md](../api.md) -- P2 Multipart upload endpoints
- **ADR**: [ADR-0002](../ADR/0002-cpt-cf-file-storage-adr-content-hash-selection.md) -- Content hash selection (BLAKE3 subtree)
- **ADR**: [ADR-0003](../ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md) -- Sidecar data plane (no bytes through control plane)
- **ADR**: [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md) -- Signed-URL transport (PASETO v4.public)
- **Dependencies**: Signed-URL transport (ADR-0004), sidecar data plane (ADR-0003)

## 2. Actor Flows (CDSL)

User-facing interactions that start with an actor (human or external system) and describe the end-to-end flow of a use case.

### Initiate Multipart Upload

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-multipart-initiate`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:
- Client receives the exact parts plan (part_number, offset, size) and one signed sidecar URL per part; a pending version is pre-registered; multipart session is in_progress

**Error Scenarios**:
- `declared_mime` rejected by effective allowed-types policy -- 415 Unsupported Media Type
- `declared_size` exceeds effective size limit -- 413 Content Too Large
- `declared_size` exceeds available storage quota -- 507 Insufficient Storage
- File not found or client lacks write permission -- 404 / 403

**Steps**:
1. [ ] - `p1` - Client: POST /api/file-storage/v1/files/{id}/multipart with body {declared_mime, declared_size, preferred_part_size?, concurrency?} - `inst-init-request`
2. [ ] - `p1` - API: validate declared_mime against the effective allowed-types policy; RETURN 415 if rejected - `inst-init-mime-check`
3. [ ] - `p1` - API: validate declared_size <= effective per-file size limit; RETURN 413 if exceeded - `inst-init-size-check`
4. [ ] - `p1` - API: validate declared_size against storage quota; RETURN 507 if exceeded - `inst-init-quota-check`
5. [ ] - `p1` - Algorithm: compute parts plan using `cpt-cf-file-storage-algo-compute-parts-plan` - `inst-init-plan`
6. [ ] - `p1` - DB: INSERT into multipart_uploads (upload_id, file_id, version_id, declared_size, part_size, status=in_progress, expires_at) - `inst-init-db-session`
7. [ ] - `p1` - DB: INSERT pending version row into file_versions (version_id, file_id, status=pending) - `inst-init-db-version`
8. [ ] - `p1` - FOR EACH part in the plan: mint a PASETO v4.public signed URL with claims {upload_id, file_id, version_id, part_number, offset, size, op="multipart_part", exp} - `inst-init-sign-urls`
9. [ ] - `p1` - RETURN 200 {upload_id, version_id, part_hash_algorithm, part_size, parts: [{part_number, offset, size, upload_url}], expires_at} - `inst-init-return`

### Upload a Part

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-multipart-upload-part`

**Actor**: `cpt-cf-file-storage-actor-platform-user` (part PUT goes directly to sidecar, not through control plane)

**Success Scenarios**:
- Part body is accepted, written, and its subtree hash is persisted; re-PUT of same (upload_id, part_number) is idempotent (overwrite)

**Error Scenarios**:
- Request body length does not match the size claim in the signed token -- 413 before any bytes written
- Signed token is invalid, expired, or tampered -- 401 Unauthorized
- Sidecar backend write failure -- 500

**Steps**:
1. [ ] - `p1` - Client: PUT <signed_part_url> with raw body of exactly `size` bytes - `inst-part-request`
2. [ ] - `p1` - Sidecar: verify PASETO token (asymmetric; sidecar cannot mint tokens -- ADR-0004) - `inst-part-verify-token`
3. [ ] - `p1` - **IF** token invalid or expired **RETURN** 401 Unauthorized - `inst-part-token-reject`
4. [ ] - `p1` - Algorithm: enforce per-part size claim using `cpt-cf-file-storage-algo-enforce-part-size` -- RETURN 413 before writing if mismatch - `inst-part-size-enforce`
5. [ ] - `p1` - **IF** backend is multipart_native: call backend PutPart(upload_handle, part_number, body) - `inst-part-write-native`
6. [ ] - `p1` - **ELSE** offset-write body into /{file_id}/{version_id} at offset from token (never mutating an existing version object) - `inst-part-write-offset`
7. [ ] - `p1` - Sidecar: compute SHA-256 subtree hash of the written part bytes (BLAKE3 deferred per ADR-0002; SHA-256 effective in P2) - `inst-part-hash`
8. [ ] - `p1` - DB: UPSERT multipart_upload_parts (upload_id, part_number, size, part_hash) -- idempotent overwrite on re-PUT - `inst-part-db-upsert`
9. [ ] - `p1` - RETURN 200 {part_number, size, part_hash} - `inst-part-return`

### Complete Multipart Upload

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-multipart-complete`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:
- All parts received; root hash computed from part hashes; file version bound and made active under If-Match CAS; session marked completed

**Error Scenarios**:
- Not all parts have been uploaded -- 409 Conflict (missing parts list returned)
- Assembled total size != declared_size -- 409 / 413
- If-Match ETag does not match current version -- 412 Precondition Failed
- Magic-bytes of first part mismatch declared_mime -- reject and auto-abort

**Steps**:
1. [ ] - `p1` - Client: POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete with optional If-Match header - `inst-complete-request`
2. [ ] - `p1` - DB: SELECT all rows from multipart_upload_parts WHERE upload_id = ? ORDER BY part_number - `inst-complete-load-parts`
3. [ ] - `p1` - **IF** any part_number in plan [1..N] is missing from the rows **RETURN** 409 with list of missing part numbers - `inst-complete-missing-parts`
4. [ ] - `p1` - Algorithm: combine part hashes into root hash using `cpt-cf-file-storage-algo-combine-part-hashes` - `inst-complete-combine-hashes`
5. [ ] - `p1` - Verify SUM(part.size) == declared_size; RETURN 409 if mismatch - `inst-complete-size-verify`
6. [ ] - `p1` - **IF** If-Match header present: DB: optimistic CAS -- verify current version ETag matches; RETURN 412 if not - `inst-complete-cas`
7. [ ] - `p1` - DB: UPDATE file_versions SET status=active, content_hash=<root_hash>, size=declared_size WHERE version_id = ? - `inst-complete-activate-version`
8. [ ] - `p1` - DB: UPDATE multipart_uploads SET status=completed WHERE upload_id = ? - `inst-complete-db-session`
9. [ ] - `p1` - RETURN 200 {version_id, content_hash, size} - `inst-complete-return`

### Abort Multipart Upload

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-multipart-abort`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:
- Session marked aborted; pending version deleted; backend multipart handle aborted; uploaded part bytes discarded

**Error Scenarios**:
- Session already completed or aborted -- 409 Conflict
- Session not found or client lacks write permission -- 404 / 403

**Steps**:
1. [ ] - `p1` - Client: DELETE /api/file-storage/v1/files/{id}/multipart/{upload_id} - `inst-abort-request`
2. [ ] - `p1` - DB: SELECT multipart_uploads WHERE upload_id = ? -- verify status == in_progress; RETURN 409 if already completed/aborted - `inst-abort-check-status`
3. [ ] - `p1` - **IF** backend is multipart_native: call backend AbortMultipart(upload_handle) to discard backend-side parts - `inst-abort-backend`
4. [ ] - `p1` - DB: DELETE FROM multipart_upload_parts WHERE upload_id = ? - `inst-abort-delete-parts`
5. [ ] - `p1` - DB: DELETE pending version row from file_versions WHERE version_id = ? AND status = pending - `inst-abort-delete-version`
6. [ ] - `p1` - DB: UPDATE multipart_uploads SET status=aborted WHERE upload_id = ? - `inst-abort-db-session`
7. [ ] - `p1` - RETURN 204 No Content - `inst-abort-return`

## 3. Processes / Business Logic (CDSL)

Internal system functions that do not interact with actors directly; called by actor flows.

### Compute Parts Plan

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-compute-parts-plan`

**Input**: declared_size (uint64), preferred_part_size (uint64 or null), backend.min_part_size (uint64), backend.allowed_algorithms
**Output**: {part_size, parts: [{part_number, offset, size}], part_hash_algorithm}

**Steps**:
1. [ ] - `p1` - Compute candidate_part_size = max(preferred_part_size ?? backend.min_part_size, backend.min_part_size) - `inst-plan-candidate`
2. [ ] - `p1` - Round candidate_part_size up to the nearest BLAKE3 chunk-tree boundary (1 MiB multiple) to make part hashes composable (ADR-0002) - `inst-plan-round`
3. [ ] - `p1` - Compute part_count = ceil(declared_size / part_size) - `inst-plan-count`
4. [ ] - `p1` - FOR EACH i in [1..part_count]: compute offset = (i-1) * part_size; size = min(part_size, declared_size - offset) - `inst-plan-parts`
5. [ ] - `p1` - **IF** BLAKE3 is in backend.allowed_algorithms: set part_hash_algorithm = BLAKE3 - `inst-plan-algo-blake3`
6. [ ] - `p1` - **ELSE** set part_hash_algorithm = first algorithm in allowed_algorithms (SHA-256 effective in P2) - `inst-plan-algo-fallback`
7. [ ] - `p1` - RETURN {part_size, parts, part_hash_algorithm} -- the plan is deterministic from (declared_size, part_size) and can be recomputed for resume from the persisted columns - `inst-plan-return`

### Enforce Per-Part Size Claim at Sidecar

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-enforce-part-size`

**Input**: request body (stream), size_claim (uint64 from signed token)
**Output**: accepted body bytes, or 413 rejection before any write

**Steps**:
1. [ ] - `p1` - Read Content-Length header from the incoming PUT request - `inst-enforce-read-cl`
2. [ ] - `p1` - **IF** Content-Length is present AND Content-Length != size_claim: RETURN HTTP 413 without buffering or writing any bytes - `inst-enforce-cl-reject`
3. [ ] - `p1` - Stream the body; count bytes as they arrive - `inst-enforce-stream`
4. [ ] - `p1` - **IF** byte count exceeds size_claim before body ends: RETURN HTTP 413 -- abort the write mid-stream; rollback any partially written bytes - `inst-enforce-oversize`
5. [ ] - `p1` - **IF** body ends before size_claim bytes received: RETURN HTTP 400 Bad Request (short body) - `inst-enforce-undersize`
6. [ ] - `p1` - RETURN accepted bytes (exactly size_claim bytes) -- proceed to write - `inst-enforce-accept`

### Combine Part Hashes at Complete

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-combine-part-hashes`

**Input**: ordered list of (part_number, part_hash) from multipart_upload_parts; part_hash_algorithm
**Output**: root_hash (hex string)

**Steps**:
1. [ ] - `p1` - Sort parts by part_number ascending; verify no gaps in [1..N] - `inst-combine-sort`
2. [ ] - `p1` - **IF** part_hash_algorithm == BLAKE3: combine subtree hashes using BLAKE3 parent-node chaining to derive the root hash (ADR-0002) - `inst-combine-blake3`
3. [ ] - `p1` - **ELSE** (SHA-256 or other fallback in P2): retrieve the assembled object from the backend and compute a streaming single-pass hash over the full content - `inst-combine-sha256`
4. [ ] - `p1` - RETURN root_hash - `inst-combine-return`

## 4. States (CDSL)

### Multipart Session State Machine

- [ ] `p1` - **ID**: `cpt-cf-file-storage-state-multipart-session`

**States**: in_progress, completed, aborted

**Initial State**: in_progress

**Transitions**:
1. [ ] - `p1` - **FROM** in_progress **TO** completed **WHEN** complete flow verifies all parts and activates the file version - `inst-st-to-completed`
2. [ ] - `p1` - **FROM** in_progress **TO** aborted **WHEN** abort flow is called explicitly by the client - `inst-st-to-aborted`
3. [ ] - `p1` - **FROM** in_progress **TO** aborted **WHEN** TTL/orphan-reconciliation sweep expires an unfinished session (`cpt-cf-file-storage-fr-orphan-reconciliation`) - `inst-st-ttl-abort`

## 5. Definitions of Done

### Initiate Endpoint with Server-Authoritative Plan

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-multipart-initiate`

The system **MUST** implement `POST /api/file-storage/v1/files/{id}/multipart` on the control plane. The endpoint validates declared_mime, declared_size, and storage quota; calls `cpt-cf-file-storage-algo-compute-parts-plan`; pre-registers a pending version; persists the multipart session with declared_size and part_size; mints one PASETO v4.public signed URL per part (claims: upload_id, file_id, version_id, part_number, offset, size, op, exp); and returns the full parts plan.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-initiate`
- `cpt-cf-file-storage-algo-compute-parts-plan`

**Touches**:
- API: `POST /api/file-storage/v1/files/{id}/multipart`
- DB Table: `multipart_uploads`
- DB Table: `file_versions`

### Sidecar Per-Part Enforcement

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-multipart-sidecar-enforcement`

The system **MUST** implement the sidecar part-upload handler: verify the PASETO token; call `cpt-cf-file-storage-algo-enforce-part-size` to reject with HTTP 413 before writing if the body length does not match the size claim; write the part bytes to the backend (PutPart for multipart_native, offset-write for offset backends); compute the per-part hash; and upsert the part row. Re-PUT of the same (upload_id, part_number) MUST be idempotent.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-upload-part`
- `cpt-cf-file-storage-algo-enforce-part-size`

**Touches**:
- API: `PUT <sidecar signed part URL>`
- DB Table: `multipart_upload_parts`

### Complete Endpoint with Hash Combination

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-multipart-complete`

The system **MUST** implement `POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete`: verify all plan parts are present; call `cpt-cf-file-storage-algo-combine-part-hashes` to derive the root hash; verify assembled size == declared_size; apply If-Match CAS if present; activate the file version; mark the session completed.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-complete`
- `cpt-cf-file-storage-algo-combine-part-hashes`

**Touches**:
- API: `POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete`
- DB Table: `multipart_uploads`
- DB Table: `multipart_upload_parts`
- DB Table: `file_versions`

### Abort Endpoint

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-multipart-abort`

The system **MUST** implement `DELETE /api/file-storage/v1/files/{id}/multipart/{upload_id}`: verify session is in_progress; abort the backend handle (multipart_native only); delete part rows and the pending version; mark the session aborted.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-abort`

**Touches**:
- API: `DELETE /api/file-storage/v1/files/{id}/multipart/{upload_id}`
- DB Table: `multipart_uploads`
- DB Table: `multipart_upload_parts`
- DB Table: `file_versions`

### Introspect and Resume Endpoint

- [ ] `p2` - **ID**: `cpt-cf-file-storage-dod-multipart-introspect`

The system **MUST** implement `GET /api/file-storage/v1/files/{id}/multipart/{upload_id}`: return the original plan (recomputed from declared_size + part_size) and the list of uploaded parts (from multipart_upload_parts); re-issue fresh signed URLs for parts not yet uploaded, enabling resumable multipart sessions after expiry.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-initiate`

**Touches**:
- API: `GET /api/file-storage/v1/files/{id}/multipart/{upload_id}`
- DB Table: `multipart_uploads`
- DB Table: `multipart_upload_parts`

### Schema: multipart_uploads Plan Columns

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-multipart-schema-plan-columns`

The system **MUST** add `version_id uuid NOT NULL`, `declared_size bigint NOT NULL CHECK (declared_size >= 0)`, and `part_size bigint NOT NULL` to the `multipart_uploads` table via migration `m20260701_000002_multipart_plan_columns`. These three columns make the plan deterministic from the session row (no per-part plan table needed), enable complete-time size verification without re-summing parts, and allow the introspect endpoint to reconstruct the plan for resume.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-initiate`
- `cpt-cf-file-storage-flow-multipart-complete`
- `cpt-cf-file-storage-algo-compute-parts-plan`

**Touches**:
- DB Table: `multipart_uploads`

## 6. Acceptance Criteria

- [x] `POST /api/file-storage/v1/files/{id}/multipart` returns a parts plan with one signed sidecar URL per part; each URL token includes part_number, offset, size, op, and exp claims
- [x] The parts plan is server-computed from declared_size and the effective part_size; clients cannot choose part boundaries
- [x] The control-plane route `PUT /files/{id}/multipart/{upload_id}/parts/{part_number}` does not exist; all part bytes flow through sidecar signed URLs only (ADR-0003)
- [x] The sidecar rejects a PUT whose body length does not match the size claim in the token with HTTP 413 before writing any bytes
- [x] Re-PUT of the same (upload_id, part_number) is idempotent; the part row is overwritten and no duplicate rows are created
- [x] `POST .../complete` rejects with 409 if any part from the plan is missing; assembled size mismatch also returns 409/413
- [x] `POST .../complete` activates the file version with the root hash derived from part hashes; session status becomes completed
- [x] `DELETE .../multipart/{upload_id}` marks the session aborted, deletes part rows and the pending version, and aborts the backend handle for multipart_native backends
- [x] Initiating a multipart upload with declared_size exceeding the effective size-limit policy returns 413; exceeding storage quota returns 507; unsupported MIME returns 415
- [x] multipart_uploads rows carry version_id, declared_size, and part_size columns (migration m20260701_000002_multipart_plan_columns)
- [ ] `GET .../multipart/{upload_id}` returns the plan recomputed from persisted columns and re-issues fresh signed URLs for missing parts (p2 resumability)
