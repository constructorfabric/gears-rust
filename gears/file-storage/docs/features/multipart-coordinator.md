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

> **Caveat (P2 0.2 — current backend support)**: `LocalFsBackend.multipart_native == false` (it advertises `range_native: true` only), so today `POST /files/{id}/multipart` returns `422 multipart_not_supported` against the real default topology (`local-fs` as the default backend). Multipart uploads only work when a `multipart_native` backend is configured as the default: today that means the non-durable `InMemoryBackend` (dev/test only — see item 0.5), and going forward the S3 backend (Tier 1 item 1.7). A true offset-write `LocalFsBackend` implementation is intentionally deferred until 1.7, since it requires widening `StorageBackend::upload_part` to carry `offset`/`part_size` — the same trait-signature change 1.7.4 (S3 streaming) already plans to make.

> **Caveat (P2, dated 2026-07-07 — quota implementation status)**: `initiate`'s `check_quota_bytes` call
> (`multipart_service.rs`) is real and fail-closed when a `QuotaClient` is wired, but `gear.rs` always constructs
> `MultipartService` with `quota_client: None` (Tier 1 item 1.4). No client is wired in any deployment today, so
> the quota check is a permissive/fail-**open** no-op — the `declared_size exceeds available storage quota` error
> scenario below and the quota DoD line further down are exercised only by unit tests that inject a mock
> `QuotaClient` (`tests/enforce_test.rs`), not by any real deployment. Blocked on a Quota Enforcement SDK crate;
> `gears/system/quota-enforcement/` is docs-only. See `../operations.md#storage-quota-not-enforced`.



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
- **ADR**: [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md) -- Signed-URL transport (PASETO v4.public design; **P2 ships a codec-equivalent bespoke Ed25519 token instead, with no `kid` -- see that ADR's "Implementation note"**)
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
- Part body is accepted, written, and its hash is persisted (via the sidecar's report-part callback to the control
  plane -- the sidecar itself has no DB connection, ADR-0003); re-PUT of same (upload_id, part_number) is idempotent
  (overwrite)

**Error Scenarios**:
- Request body length does not match the size claim in the signed token -- 413 before any bytes written
- Signed token is invalid, expired, or tampered -- 401/403
- Sidecar backend write failure -- 500

**Steps** (current, shipped behavior):
1. [x] - `p1` - Client: PUT <signed_part_url> with raw body of exactly `size` bytes - `inst-part-request`
2. [x] - `p1` - Sidecar: verify the signed token (asymmetric Ed25519; sidecar cannot mint tokens -- ADR-0004; **not
   literal PASETO -- see that ADR's Implementation note**) - `inst-part-verify-token`
3. [x] - `p1` - **IF** token invalid or expired **RETURN** 403 Forbidden - `inst-part-token-reject`
4. [x] - `p1` - Algorithm: enforce per-part size claim using `cpt-cf-file-storage-algo-enforce-part-size` -- RETURN 413/PAYLOAD_TOO_LARGE before/after writing if mismatch - `inst-part-size-enforce`
5. [x] - `p1` - **IF** backend is multipart_native (`InMemoryBackend` today): call backend PutPart(upload_handle, part_number, body) - `inst-part-write-native`
6. [x] - `p1` - **ELSE** write the part to a **separate per-part backend object** `{backend_path}.part.{part_number}` (`bin/sidecar.rs`) -- **not** an offset-write into the shared `/{file_id}/{version_id}` object as earlier drafts of this doc described. This is moot against the real default topology today: `local-fs` is not `multipart_native`, so this branch is unreachable in production until a true offset-write/native `local-fs` implementation or the S3 backend lands (Tier 1 item 1.7; see the caveat above) - `inst-part-write-offset`
7. [x] - `p1` - Sidecar: compute the SHA-256 hash of the written part bytes (BLAKE3 deferred per ADR-0002; SHA-256 effective in P2) - `inst-part-hash`
8. [x] - `p1` - Sidecar: POST the report-part callback to the control plane, `.../versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report {backend_etag, hash_hex, size}`, authorized solely by the same signed `fs-token` (no separate app-token, no on-behalf-of delegation) - `inst-part-report`
9. [x] - `p1` - Control plane: UPSERT multipart_upload_parts (upload_id, part_number, backend_etag, part_hash, size) -- idempotent overwrite on re-PUT/re-report - `inst-part-db-upsert`
10. [x] - `p1` - Sidecar: RETURN 200 {part_number, etag, hash_algorithm, hash} to the client once the report-part callback succeeds (no `size` field in this response body today) - `inst-part-return`

### Complete Multipart Upload

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-multipart-complete`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

> **Caveat (current, shipped behavior vs. this section's original design).** `complete` today: takes **no `If-Match`**
> (any such header is ignored -- no CAS, no `412`); returns **`204 No Content`** with **no response body** (not the
> `200 {version_id, content_hash, size}` this section originally specified); and does **not** enumerate missing part
> numbers on a size mismatch -- it only compares `SUM(reported part sizes)` against `declared_size` and returns a
> generic `409 Conflict` if they differ (a missing part is indistinguishable from a short part in the error). Critically,
> `complete` **finalizes** the version (`pending -> available`, real assembled size/hash) but does **not bind** it --
> `content_id` is untouched, exactly like single-shot upload's finalize/bind split (ADR-0003, DESIGN.md §3.6). The
> client must issue a separate `POST /files/{id}/bind {version_id}` under `If-Match` afterwards to make the assembled
> content live. The richer contract below (`If-Match`/`412`, `200` body, `409`-with-missing-parts) is the **intended**
> design and is tracked as a follow-up (P2 remediation plan, item 3.3, steps 2-4) -- it is not yet implemented and
> must not be assumed by clients today.

**Success Scenarios**:
- All reported parts are assembled and verified by the backend; the version is **finalized** (`pending -> available`)
  with the real assembled size/hash; session marked completed. Binding the version as the file's current content is a
  **separate**, later `POST /files/{id}/bind` call -- `complete` does not bind (see caveat above)

**Error Scenarios**:
- Assembled `SUM(part.size)` != `declared_size` -- `409 Conflict` (generic message; **no** missing-part-numbers list today -- intended follow-up)
- Policy size limit exceeded by the assembled total -- policy-size-exceeded error
- Session not `in_progress` (already completed/aborted, or foreign to this `file_id`) -- `404`-shaped "not found" / conflict
- `If-Match` is accepted **only** by the separate later `bind` call, not by `complete` itself (see caveat above)

**Steps** (current, shipped behavior):
1. [x] - `p1` - Client: POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete (no request body; `If-Match` is not read) - `inst-complete-request`
2. [x] - `p1` - Control plane: authorize `write`; load the session by `upload_id`; verify it belongs to `file_id` and is `in_progress` and not expired - `inst-complete-load-session`
3. [x] - `p1` - DB: SELECT all reported rows from multipart_upload_parts WHERE upload_id = ? - `inst-complete-load-parts`
4. [x] - `p1` - Verify `SUM(part.size) == declared_size`; RETURN 409 (generic, no missing-part-numbers list) if mismatch - `inst-complete-size-verify`
5. [x] - `p1` - Policy size check against the assembled total - `inst-complete-policy-check`
6. [x] - `p1` - Backend: `CompleteMultipartUpload`/assemble the parts and compute the SHA-256 of the **actually assembled bytes** (not a combination of the stored per-part hashes) - `inst-complete-assemble`
7. [x] - `p1` - DB: finalize the version row (`status: pending -> available`, real assembled `size`/`content_hash`); does **not** touch `content_id` - `inst-complete-finalize-version`
8. [x] - `p1` - DB: UPDATE multipart_uploads SET status=completed WHERE upload_id = ? - `inst-complete-db-session`
9. [x] - `p1` - RETURN **204 No Content** (no body) - `inst-complete-return`
10. [ ] - `p2` - Client: separately calls `POST /files/{id}/bind {version_id}` under `If-Match` to swap `content_id` and make the content live - `inst-complete-bind-followup`

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

> **Current, shipped behavior**: there is no explicit "verify no gaps in [1..N]" step -- a missing part is only ever
> caught indirectly, as a `SUM(part.size) != declared_size` mismatch (see the `complete` caveat above). The SHA-256
> path (P2's effective algorithm) is folded directly into the backend's `complete_multipart` call, which assembles
> the parts and hashes the result in one step, rather than a separate combine-then-verify pass as drafted below.

**Steps** (as designed; BLAKE3 combination remains deferred per ADR-0002):
1. [ ] - `p2` - Sort parts by part_number ascending; verify no gaps in [1..N] - `inst-combine-sort`
2. [ ] - `p2` - **IF** part_hash_algorithm == BLAKE3: combine subtree hashes using BLAKE3 parent-node chaining to derive the root hash (ADR-0002) - `inst-combine-blake3`
3. [x] - `p1` - **ELSE** (SHA-256, effective in P2): the backend assembles the parts and computes a single hash over the full assembled content as part of `complete_multipart` - `inst-combine-sha256`
4. [x] - `p1` - RETURN root_hash - `inst-combine-return`

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

- [x] `p1` - **ID**: `cpt-cf-file-storage-dod-multipart-sidecar-enforcement`

**Shipped**: the sidecar part-upload handler verifies the signed token (Ed25519, codec-equivalent to but not literal
PASETO -- ADR-0004's Implementation note); calls `cpt-cf-file-storage-algo-enforce-part-size` to reject with HTTP 413
if the body length does not match the size claim (enforced as a streaming `max_size` abort plus a post-write
exact-length check, not a pre-write `Content-Length` check); writes the part bytes to the backend (`PutPart` for
`multipart_native`, or a separate `{backend_path}.part.{n}` object for non-native backends -- not an offset-write into
the shared version object); computes the per-part hash; and reports it to the control plane over a token-authenticated
callback, which upserts the part row (the sidecar itself never touches the DB). Re-PUT of the same (upload_id,
part_number) is idempotent.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-upload-part`
- `cpt-cf-file-storage-algo-enforce-part-size`

**Touches**:
- API: `PUT <sidecar signed part URL>`
- DB Table: `multipart_upload_parts`

### Complete Endpoint with Hash Combination

- [x] `p1` - **ID**: `cpt-cf-file-storage-dod-multipart-complete`

**Shipped today**: `POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete` verifies `SUM(reported part
sizes) == declared_size` (generic `409` on mismatch, no missing-part-numbers list); asks the backend to assemble the
parts and hash the result; **finalizes** the version (`pending -> available`) with the real size/hash; marks the
session completed; returns `204 No Content`. It does **not** accept `If-Match` and does **not bind** the version --
binding is a separate, later client-issued `POST /files/{id}/bind`.

**Deferred (tracked, P2 remediation item 3.3 steps 2-4; not yet implemented -- do not assume)**:
- [ ] `p2` - `If-Match`/`412` support directly on `complete`
- [ ] `p2` - a `200 {version_id, content_hash, size}` response instead of `204`
- [ ] `p2` - an explicit `409` body listing missing part numbers instead of a bare size-mismatch comparison

**Implements**:
- `cpt-cf-file-storage-flow-multipart-complete`
- `cpt-cf-file-storage-algo-combine-part-hashes` (SHA-256 path only; BLAKE3 combination remains `p2`/deferred)

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
- [x] `POST .../complete` rejects with a generic `409 Conflict` when `SUM(reported part sizes) != declared_size`
  (a missing part surfaces only as this size mismatch -- **not** an enumerated missing-part-numbers list; that is a
  tracked follow-up, see the DoD above)
- [x] `POST .../complete` **finalizes** the file version (`pending -> available`) with the real assembled size/hash
  and returns `204 No Content` (no body); session status becomes completed. It does **not** bind the version --
  `content_id` is untouched, and it does **not** accept `If-Match`/`412` today (both are tracked follow-ups)
- [x] `DELETE .../multipart/{upload_id}` marks the session aborted, deletes part rows and the pending version, and aborts the backend handle for multipart_native backends
- [x] Initiating a multipart upload with declared_size exceeding the effective size-limit policy returns 413; exceeding storage quota returns 507; unsupported MIME returns 415
  (**quota implementation status, P2, dated 2026-07-07**: this `[x]` is exercised only by `tests/enforce_test.rs`
  injecting a mock `QuotaClient` — no real deployment has one wired, `gear.rs`'s `quota_client: None`, Tier 1 item
  1.4 — so this rejection path does not fire in production today; see `../operations.md#storage-quota-not-enforced`.
  Separately, `../api.md` notes the status code was corrected to `429` in a later doc pass; the `507` here is stale
  and out of scope for this note)
- [x] multipart_uploads rows carry version_id, declared_size, and part_size columns (migration m20260701_000002_multipart_plan_columns)
- [x] Against the real default topology (`local-fs`, not `multipart_native`), `POST /files/{id}/multipart` is rejected
  (see the caveat in §1.2) -- multipart is only functional today against a `multipart_native` backend
  (`InMemoryBackend`, dev/test only)
- [ ] `POST .../complete` accepts `If-Match` and returns `412` on a stale precondition, directly (not via a separate `bind` call) (p2 follow-up, remediation item 3.3)
- [ ] `POST .../complete` returns `200 {version_id, content_hash, size}` instead of `204` (p2 follow-up, remediation item 3.3)
- [ ] `POST .../complete` enumerates missing part numbers in its `409` body instead of a bare size comparison (p2 follow-up, remediation item 3.3)
- [ ] Non-native backends write parts as offset-writes into the shared version object instead of per-part objects (p2 follow-up, tied to Tier 1 item 1.7 / a true offset-write `local-fs` implementation)
- [ ] `GET .../multipart/{upload_id}` returns the plan recomputed from persisted columns and re-issues fresh signed URLs for missing parts (p2 resumability)
