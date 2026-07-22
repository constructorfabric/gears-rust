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
  - [Introspect and Resume Multipart Upload](#introspect-and-resume-multipart-upload)
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

This feature supersedes the interim P2-M3 client-driven implementation in which parts were PUT to the control-plane route `PUT /files/{id}/multipart/{upload_id}/parts/{part_number}`. That control-plane byte route is removed (ADR-0003). Byte movement now flows exclusively through sidecar signed URLs (ADR-0004). See [Combine Part Hashes at Complete](#combine-part-hashes-at-complete) for how the content hash is computed.

**Requirements**: `cpt-cf-file-storage-fr-multipart-upload`, `cpt-cf-file-storage-fr-size-limits-policy`, `cpt-cf-file-storage-fr-storage-quota`

**Principles**: `cpt-cf-file-storage-principle-control-no-content`, `cpt-cf-file-storage-principle-signed-urls`

> **Caveat (P2 0.2 — current backend support)**: `LocalFsBackend.multipart_native == false` (it advertises `range_native: true` only), so `POST /files/{id}/multipart` returns `422 multipart_not_supported` against the real default topology (`local-fs` as the default backend). Multipart uploads only work when a `multipart_native` backend is configured as the default: that means the non-durable `InMemoryBackend` (dev/test only — see item 0.5), and going forward the S3 backend (Tier 1 item 1.7). A true offset-write `LocalFsBackend` implementation is intentionally deferred until 1.7, since it requires widening `StorageBackend::upload_part` to carry `offset`/`part_size` — the same trait-signature change 1.7.4 (S3 streaming) already plans to make.

> **Caveat (P2 — quota implementation status)**: `initiate`'s `check_quota_bytes` call
> (`multipart_service.rs`) is real and fail-closed when a `QuotaClient` is wired, but `gear.rs` always constructs
> `MultipartService` with `quota_client: None` (Tier 1 item 1.4). No `QuotaClient` is wired in any deployment, so
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
- **ADR**: [ADR-0002](../ADR/0002-cpt-cf-file-storage-adr-content-hash-selection.md) -- Content hash selection; its P2 hash-policy vision is superseded (see ADR-0006)
- **ADR**: [ADR-0006](../ADR/0006-cpt-cf-file-storage-adr-content-hash-modes.md) -- Content-hash modes (whole-object + multipart offset-manifest composite, SHA-256-only)
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

- [x] `p1` - **ID**: `cpt-cf-file-storage-flow-multipart-upload-part`

**Actor**: `cpt-cf-file-storage-actor-platform-user` (part PUT goes directly to sidecar, not through control plane)

**Success Scenarios**:
- Part body is accepted, written, and its hash is persisted (via the sidecar's report-part callback to the control
  plane -- the sidecar itself has no DB connection, ADR-0003); re-PUT of same (upload_id, part_number) is idempotent
  (overwrite)

**Error Scenarios**:
- Request body length does not match the size claim in the signed token -- 413 before any bytes written
- Signed token is invalid, expired, or tampered -- 403
- Sidecar backend write failure -- 500

**Steps** (current, shipped behavior):
1. [x] - `p1` - Client: PUT <signed_part_url> with raw body of exactly `size` bytes - `inst-part-request`
2. [x] - `p1` - Sidecar: verify the signed token (asymmetric Ed25519; sidecar cannot mint tokens -- ADR-0004; **not
   literal PASETO -- see that ADR's Implementation note**) - `inst-part-verify-token`
3. [x] - `p1` - **IF** token invalid or expired **RETURN** 403 Forbidden - `inst-part-token-reject`
4. [x] - `p1` - Algorithm: enforce per-part size claim using `cpt-cf-file-storage-algo-enforce-part-size` -- RETURN 413/PAYLOAD_TOO_LARGE before/after writing if mismatch - `inst-part-size-enforce`
5. [x] - `p1` - **IF** backend is multipart_native (`InMemoryBackend`, the only multipart_native backend): call backend PutPart(upload_handle, part_number, body) - `inst-part-write-native`
6. [x] - `p1` - **ELSE** write the part to a **separate per-part backend object** `{backend_path}.part.{part_number}` (`bin/sidecar.rs`) -- **not** an offset-write into the shared `/{file_id}/{version_id}` object as earlier drafts of this doc described. This is moot against the real default topology: `local-fs` is not `multipart_native`, so this branch is unreachable in production until a true offset-write/native `local-fs` implementation or the S3 backend lands (Tier 1 item 1.7; see the caveat above) - `inst-part-write-offset`
7. [x] - `p1` - Sidecar: compute the SHA-256 hash of the written part bytes - `inst-part-hash`
8. [x] - `p1` - Sidecar: POST the report-part callback to the control plane, `.../versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report {backend_etag, hash_hex, size}`, authorized solely by the same signed `fs-token` (no separate app-token, no on-behalf-of delegation) - `inst-part-report`
9. [x] - `p1` - Control plane: UPSERT multipart_upload_parts (upload_id, part_number, backend_etag, part_hash, size) -- idempotent overwrite on re-PUT/re-report - `inst-part-db-upsert`
10. [x] - `p1` - Sidecar: RETURN 200 {part_number, etag, hash_algorithm, hash} to the client once the report-part callback succeeds (no `size` field in this response body) - `inst-part-return`

### Complete Multipart Upload

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-multipart-complete`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

> **Status (P2 remediation item 3.3, shipped).** `complete` now returns **`200`** with a JSON body
> (`version_id`, `size`, `hash_algorithm`, `content_hash`, `hash_mode`, `part_count`, `manifest`) instead of the
> earlier bare `204 No Content`. It accepts an **optional** `If-Match` header: a concrete value is checked against
> the file's current content ETag and a mismatch is rejected (`400` -- `FailedPrecondition` collapses to `400` on
> this platform, there is no `412`-mapped canonical-error variant); `*` or an absent header is unconditional
> (backward compatible with the pre-3.3 contract). Before assembling, `complete` also diffs the plan's expected part
> numbers against the parts actually reported and returns `409` with the missing part numbers if any are absent, in
> front of the pre-existing generic `SUM(part.size) != declared_size` check. Critically, `complete` still
> **finalizes** the version (`pending -> available`, real assembled size/hash) but does **not bind** it --
> `content_id` is untouched, exactly like single-shot upload's finalize/bind split (ADR-0003, DESIGN.md §3.6). The
> client must still issue a separate `POST /files/{id}/bind {version_id}` under `If-Match` afterwards to make the
> assembled content live -- `complete`'s own `If-Match` only guards against the file's content pointer moving out
> from under a concurrent complete before that later bind, not the bind itself.

`complete` computes the version's content hash per ADR-0006's **`multipart-composite-sha256`** mode
(`cpt-cf-file-storage-adr-content-hash-modes`): the backend builds an offset-manifest from the per-part digests
already persisted in `multipart_upload_parts` (`part_hash`, `offset`) and the root hash is `sha256(manifest)` --
**no re-read/re-assembly of the object** is performed to compute it. The manifest text is persisted in
`version_hash_manifest` and returned verbatim in the `complete` response body (`manifest` field) so a client can
independently re-verify the composite hash (see [content-hash-modes.md](./content-hash-modes.md) §"Client-Side
Manifest Re-Verification").

**Success Scenarios**:
- All reported parts are assembled and verified by the backend; the version is **finalized** (`pending -> available`)
  with the real assembled size + ADR-0006 composite hash; session marked completed; the response body carries
  `version_id`/`size`/`hash_algorithm`/`content_hash`/`hash_mode`/`part_count`/`manifest`. Binding the version as the
  file's current content is a **separate**, later `POST /files/{id}/bind` call -- `complete` does not bind

**Error Scenarios**:
- One or more planned parts have not been reported yet -- `409 Conflict` with the missing part numbers in the error
  detail (item 3.3; checked **before** the size-mismatch guard below)
- Assembled `SUM(part.size)` != `declared_size` (residual guard, e.g. an over-reported part) -- `409 Conflict`
- Policy size limit exceeded by the assembled total -- policy-size-exceeded error
- Session not `in_progress` (already completed/aborted, or foreign to this `file_id`) -- `404`-shaped "not found" / conflict
- `If-Match` supplied and it does not match the file's current content ETag -- `400` (`FailedPrecondition`)

**Steps** (current, shipped behavior):
1. [x] - `p1` - Client: POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete (no request body; optional `If-Match` header). Control plane: authorize `write`; if `If-Match` is present and not `*`, compare it against the file's current content ETag and reject on mismatch - `inst-complete-request`
2. [x] - `p1` - Control plane: load the session by `upload_id`; verify it belongs to `file_id` and is `in_progress` and not expired - `inst-complete-load-session`
3. [x] - `p1` - DB: SELECT all reported rows from multipart_upload_parts WHERE upload_id = ? - `inst-complete-load-parts`
4. [x] - `p1` - Diff the plan's expected part numbers (`ceil(declared_size / part_size)`) against the reported part numbers; RETURN 409 with the missing part numbers if any are absent - `inst-complete-missing-parts`
5. [x] - `p1` - Verify `SUM(part.size) == declared_size` (residual guard); RETURN 409 (generic) if mismatch - `inst-complete-size-verify`
6. [x] - `p1` - Policy size check against the assembled total - `inst-complete-policy-check`
7. [x] - `p1` - Backend: `CompleteMultipartUpload`, building the ADR-0006 offset-manifest and its root hash from the already-persisted per-part digests+offsets -- **no re-read** of the assembled object - `inst-complete-assemble`
8. [x] - `p1` - Post-assembly, pre-finalize: sniff the assembled object's leading bytes and validate against `session.declared_mime` (P2 remediation item 1.10, `cpt-cf-file-storage-fr-content-type-validation`); re-check the policy size ceiling against the resolved MIME; on mismatch fail **before** any DB finalize (the assembled blob becomes an orphan reclaimed by the orphan-reconciliation sweep) - `inst-complete-mime-validate`
9. [x] - `p1` - DB: finalize the version row (`status: pending -> available`, real assembled `size`/composite `content_hash`/`hash_mode`/`part_count`/`manifest`); does **not** touch `content_id` - `inst-complete-finalize-version`
10. [x] - `p1` - DB: UPDATE multipart_uploads SET status=completed WHERE upload_id = ? - `inst-complete-db-session`
11. [x] - `p1` - RETURN **200** `{version_id, size, hash_algorithm, content_hash, hash_mode, part_count, manifest}` - `inst-complete-return`
12. [ ] - `p2` - Client: separately calls `POST /files/{id}/bind {version_id}` under `If-Match` to swap `content_id` and make the content live - `inst-complete-bind-followup`

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
4. [x] - `p1` - DB: DELETE FROM multipart_upload_parts WHERE upload_id = ? - `inst-abort-delete-parts`
5. [ ] - `p1` - DB: DELETE pending version row from file_versions WHERE version_id = ? AND status = pending - `inst-abort-delete-version`
6. [ ] - `p1` - DB: UPDATE multipart_uploads SET status=aborted WHERE upload_id = ? - `inst-abort-db-session`
7. [ ] - `p1` - RETURN 204 No Content - `inst-abort-return`

### Introspect and Resume Multipart Upload

- [x] `p2` - **ID**: `cpt-cf-file-storage-flow-multipart-introspect`

**Item 3.4, shipped -- SHIP decision.**

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:
- Client receives the session's current state, the parts already reported (`received`), and the parts still missing
  (`missing`). While the session is still `in_progress` and unexpired, each missing part also carries a freshly
  minted resume `upload_url` so an interrupted upload can continue without re-initiating; a terminal
  (`completed`/`aborted`) or expired session reports state/accounting only, with no URLs

**Error Scenarios**:
- Session not found, or its `upload_id` belongs to a different `file_id` -- `404`-shaped "not found" (masked
  identically to `complete`/`abort`'s foreign-session guard)
- Caller lacks `write` permission on the file -- `403`

**Steps**:
1. [x] - `p1` - Client: GET /api/file-storage/v1/files/{id}/multipart/{upload_id} (no request body) - `inst-introspect-request`
2. [x] - `p1` - Control plane: authorize `write` on `file_id` (same gate as initiate/complete/abort -- introspect hands out live resume URLs, so it is not opened to a read-capable-but-not-write principal) - `inst-introspect-authz`
3. [x] - `p1` - DB: SELECT multipart_uploads WHERE upload_id = ?; verify it belongs to `file_id` -- mask a foreign or missing `upload_id` as `404` identically to `complete`'s guard - `inst-introspect-load-session`
4. [x] - `p1` - DB: SELECT all reported rows from multipart_upload_parts WHERE upload_id = ? - `inst-introspect-load-parts`
5. [x] - `p1` - Diff the plan's expected part numbers against the reported ones using the same `cpt-cf-file-storage-algo-compute-parts-plan`-derived helper `complete` uses (item 3.3's `missing_part_numbers`) -- `inst-introspect-diff`
6. [x] - `p1` - FOR EACH missing part number: recompute its `(offset, size)` from the session's persisted `(declared_size, part_size)` columns - `inst-introspect-recompute-bounds`
7. [x] - `p1` - **IF** state == in_progress AND expires_at > now: mint a fresh signed part URL for each missing part, reusing the initiate path's per-part token-minting helper, with the token `exp = min(session.expires_at, now + url_ttl_secs)` -- capped at the session's own expiry, but never longer than the standard per-part URL TTL either, so a long-lived session cannot be used to mint effectively-non-expiring resume URLs - `inst-introspect-mint-urls`
8. [x] - `p1` - **ELSE** (terminal or expired session): omit `upload_url` from every missing part - `inst-introspect-no-urls`
9. [x] - `p1` - RETURN 200 {upload_id, version_id, state, declared_mime, declared_size, part_size, created_at, expires_at, received: [{part_number, size, uploaded_at}], missing: [{part_number, offset, size, upload_url?}]} - `inst-introspect-return`

## 3. Processes / Business Logic (CDSL)

Internal system functions that do not interact with actors directly; called by actor flows.

### Compute Parts Plan

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-compute-parts-plan`

**Input**: declared_size (uint64), preferred_part_size (uint64 or null), backend.min_part_size (uint64)
**Output**: {part_size, parts: [{part_number, offset, size}], part_hash_algorithm}

`part_hash_algorithm` is always SHA-256, and part sizing has no chunk-tree-boundary constraint.

**Steps**:
1. [ ] - `p1` - Compute candidate_part_size = max(preferred_part_size ?? backend.min_part_size, backend.min_part_size) - `inst-plan-candidate`
2. [ ] - `p1` - Round candidate_part_size up to backend.min_part_size's granularity - `inst-plan-round`
3. [ ] - `p1` - Compute part_count = ceil(declared_size / part_size) - `inst-plan-count`
4. [ ] - `p1` - FOR EACH i in [1..part_count]: compute offset = (i-1) * part_size; size = min(part_size, declared_size - offset) - `inst-plan-parts`
5. [ ] - `p1` - Set part_hash_algorithm = SHA-256 - `inst-plan-algo-fallback`
6. [ ] - `p1` - RETURN {part_size, parts, part_hash_algorithm} -- the plan is deterministic from (declared_size, part_size) and can be recomputed for resume from the persisted columns - `inst-plan-return`

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

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-combine-part-hashes`

**Input**: ordered list of (part_number, offset, part_hash) from multipart_upload_parts; part_hash_algorithm (always SHA-256)
**Output**: root_hash (hex string), manifest (wire-format text)

As of P2 remediation item 3.3, `complete` explicitly diffs the plan's expected part numbers against the reported
ones and rejects with `409` + the missing part numbers before ever reaching this combination step (see
[Complete Multipart Upload](#complete-multipart-upload)'s `inst-complete-missing-parts`) -- `inst-combine-sort` below
is that same gap check, just scoped to this process's own input/output framing. The combination itself is ADR-0006's
`multipart-composite-sha256` mode (`cpt-cf-file-storage-adr-content-hash-modes`): the backend builds an
offset-manifest from the already-persisted per-part digests+offsets and computes `root_hash = sha256(manifest)` --
**no re-read/re-assembly of the object** happens to compute it (superseding this section's original
re-hash-of-assembled-bytes design, `inst-combine-sha256` below is now the manifest-composite path).

**Steps**:
1. [x] - `p1` - Sort parts by part_number ascending (the repo's `list_parts` returns `ORDER BY part_number`); verify no gaps in [1..N] (item 3.3, `missing_part_numbers`) - `inst-combine-sort`
2. [x] - `p1` - The backend builds the offset-manifest from the per-part digests+offsets and computes `root_hash = sha256(manifest.to_wire_string())` (ADR-0006 `multipart-composite-sha256`) - `inst-combine-sha256`
3. [x] - `p1` - RETURN root_hash + manifest text (persisted in `version_hash_manifest`, returned in the `complete` response body) - `inst-combine-return`

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

**Implemented**: `POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete` diffs the plan's expected part
numbers against the reported ones and rejects with `409` + the missing part numbers if any are absent (item 3.3);
then verifies `SUM(reported part sizes) == declared_size` as a residual guard (generic `409` on mismatch); asks the
backend to build the ADR-0006 offset-manifest and compute its composite root hash from the already-persisted
per-part digests (no re-read of the assembled object); **finalizes** the version (`pending -> available`) with the
real size + composite hash/mode/part-count/manifest; marks the session completed; returns **`200`** with
`{version_id, size, hash_algorithm, content_hash, hash_mode, part_count, manifest}`. It accepts an **optional**
`If-Match` header (a concrete value is checked against the file's current content ETag, `*`/absent is unconditional)
but does **not bind** the version -- binding is a separate, later client-issued `POST /files/{id}/bind`.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-complete`
- `cpt-cf-file-storage-algo-combine-part-hashes` (ADR-0006 manifest-composite path, `inst-combine-sort`/`inst-combine-sha256`/`inst-combine-return`)

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

- [x] `p2` - **ID**: `cpt-cf-file-storage-dod-multipart-introspect`

**Shipped (item 3.4, SHIP decision).** `GET /api/file-storage/v1/files/{id}/multipart/{upload_id}` returns the
session's current state, the parts already reported (from `multipart_upload_parts`), and the parts still missing
(their `(offset, size)` recomputed from the session's persisted `declared_size`/`part_size` columns via the same
`missing_part_numbers` helper item 3.3 extracted for `complete`). While the session is still `in_progress` and
unexpired, each missing part also gets a freshly-minted resume URL, reusing the initiate path's per-part
token-minting helper, with `exp = min(session.expires_at, now + url_ttl_secs)`: capped so a resumed upload can never
outlive the session it resumes, but also never minted with a longer TTL than any other signed URL just because the
session itself is long-lived (`multipart_session_ttl_secs`, e.g. 24h, is deliberately decoupled from the short
per-part URL TTL -- see `operations.md`'s `multipart_session_ttl_secs` entry). A terminal (`completed`/`aborted`) or expired session reports state and
part accounting only, with no URLs. Authorized on `actions::WRITE` (not `READ`), matching initiate/complete/abort --
introspect hands out live upload URLs, so it is not opened to a read-capable-but-not-write principal. A foreign or
missing `upload_id` is masked as `404`, identical to `complete`'s guard.

**Implements**:
- `cpt-cf-file-storage-flow-multipart-introspect`
- `cpt-cf-file-storage-flow-multipart-initiate`
- `cpt-cf-file-storage-algo-compute-parts-plan`

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
- [x] `POST .../complete` rejects with `409 Conflict` and the missing part numbers in the error detail when one or
  more planned parts have not been reported yet (item 3.3); a residual generic `409` still guards
  `SUM(reported part sizes) != declared_size` for cases the missing-parts diff cannot catch (e.g. an over-reported part)
- [x] `POST .../complete` **finalizes** the file version (`pending -> available`) with the real assembled size and
  ADR-0006 composite hash, and returns **`200`** with `{version_id, size, hash_algorithm, content_hash, hash_mode,
  part_count, manifest}`; session status becomes completed. It does **not** bind the version -- `content_id` is
  untouched. It accepts an **optional** `If-Match` header (checked against the file's current content ETag; `400`
  on mismatch, `FailedPrecondition` collapses to `400` on this platform -- there is no `412`-mapped canonical-error
  variant; `*`/absent is unconditional)
- [x] `DELETE .../multipart/{upload_id}` marks the session aborted, deletes part rows and the pending version, and aborts the backend handle for multipart_native backends
- [x] Initiating a multipart upload with declared_size exceeding the effective size-limit policy returns 413; exceeding storage quota returns 507; unsupported MIME returns 415
  (**quota implementation status, P2**: this `[x]` is exercised only by `tests/enforce_test.rs`
  injecting a mock `QuotaClient` — no real deployment has one wired, `gear.rs`'s `quota_client: None`, Tier 1 item
  1.4 — so this rejection path does not fire in production; see `../operations.md#storage-quota-not-enforced`.
  Separately, `../api.md` notes the status code was corrected to `429` in a later doc pass; the `507` here is stale
  and out of scope for this note)
- [x] multipart_uploads rows carry version_id, declared_size, and part_size columns (migration m20260701_000002_multipart_plan_columns)
- [x] Against the real default topology (`local-fs`, not `multipart_native`), `POST /files/{id}/multipart` is rejected
  (see the caveat in §1.2) -- multipart is only functional against a `multipart_native` backend
  (`InMemoryBackend`, dev/test only)
- [x] `POST .../complete` accepts `If-Match` and returns `400` (`FailedPrecondition`; this platform has no `412`
  canonical-error variant) on a stale precondition, directly (not via a separate `bind` call) (P2 remediation item 3.3)
- [x] `GET /files/{id}/multipart/{upload_id}` (introspect) returns the session state plus `received`/`missing` part
  lists; missing parts of a still-`in_progress`, unexpired session each carry a fresh resume `upload_url` whose
  `exp = min(session.expires_at, now + url_ttl_secs)` (never longer than the standard per-part URL TTL, even though
  the session itself may still have much longer left to live); a terminal or expired session omits the URLs; a
  foreign or missing `upload_id` is masked as `404` (item 3.4, SHIP decision)
- [x] `POST .../complete` returns `200 {version_id, size, hash_algorithm, content_hash, hash_mode, part_count, manifest}`
  instead of `204` (P2 remediation item 3.3)
- [x] `POST .../complete` enumerates missing part numbers in its `409` body instead of a bare size comparison (P2 remediation item 3.3)
- [ ] Non-native backends write parts as offset-writes into the shared version object instead of per-part objects (p2 follow-up, tied to Tier 1 item 1.7 / a true offset-write `local-fs` implementation)
- [ ] `GET .../multipart/{upload_id}` returns the plan recomputed from persisted columns and re-issues fresh signed URLs for missing parts (p2 resumability)
