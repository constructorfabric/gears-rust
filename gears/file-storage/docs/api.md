# FileStorage — HTTP API (P1 + declared P2)


<!-- toc -->

- [Two planes](#two-planes)
- [P1 — Control plane (`/api/file-storage/v1`)](#p1--control-plane-apifile-storagev1)
- [P1 — Sidecar (signed-URL authorized)](#p1--sidecar-signed-url-authorized)
- [Data-plane callbacks (sidecar → control plane, s2s token-authenticated)](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)
- [P2 — Multipart upload](#p2--multipart-upload)
- [P2 — Policy engine](#p2--policy-engine)
- [P2 — Retention rules](#p2--retention-rules)
- [P2 — Backend migration](#p2--backend-migration)
- [P2 — Ownership transfer](#p2--ownership-transfer)
- [Upload, bind, and the conflict retry](#upload-bind-and-the-conflict-retry)
- [Signed URLs](#signed-urls)
- [Conditional headers](#conditional-headers)
- [Range support](#range-support)
- [Response headers (download, on the sidecar)](#response-headers-download-on-the-sidecar)
- [Status code summary](#status-code-summary)

<!-- /toc -->

FileStorage is split into a **control plane** (metadata + signed-URL issuance; never carries content) and a **sidecar**
data plane (the only thing that moves bytes, addressed only by control-issued signed URLs). See
[ADR-0003](./ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md) and [DESIGN.md](./DESIGN.md). Every content
operation is at least two requests: a control request to obtain a signed URL, then a data request against the sidecar.

## Two planes

- **Control plane** base URL: `/api/file-storage/v1` — a normal gear REST surface: **JWT enforced by API Gateway**,
  standard owner/tenant authorization (PEP) applies, routes auto-described via OperationBuilder → generated OpenAPI.
  **JSON only — no request or response body ever contains file content.**
- **Sidecar**: its own domain; reachable only with a valid control-issued **signed URL**. The signed URL always points
  at the sidecar, never at a backend.

  The sidecar is a deliberate **platform-level exception** to "API Gateway owns REST hosting" — it is **not** fronted
  by the gateway and does **not** receive a gateway-derived `SecurityContext`. Its authorization model:
  - the **signed token is the delegated authorization artifact** for exactly one resource + operation until `exp`; a
    valid token *is* the access decision (made by the control plane at signing). The sidecar performs **no
    request-time PDP/AuthZ call** and reads no tenant/owner permission state;
  - a platform **JWT in `Authorization`** would be validated by the sidecar when the token carries a `tok.<claim>`
    predicate — but `tok.<claim>` (and the `ip`/CIDR constraint) are **NOT implemented**: `Claims` has no such
    fields, so the sidecar never validates a platform JWT under any circumstance (see "Signed URLs" below);
  - request-id propagation is shipped (`request_id`/`x-request-id`); per-instance connection/bandwidth limits
    (`max_conns`/`max_rate`) are **NOT implemented** — see "Signed URLs" below.

  Because clients never hand-write sidecar URLs (they always receive a ready, opaque signed URL from the control
  plane), the sidecar surface is **outside the generated OpenAPI flow**; its byte-level contract is specified
  normatively in this document. (A standalone OpenAPI document for the sidecar is deferred to P2.)

Encoding conventions:
- Control bodies are `application/json`. The sidecar `PUT` body is the **raw** object bytes (no `multipart/form-data`).
- All error responses follow RFC 7807 (`application/problem+json`).
- `file_id` and `version_id` are UUIDs. A backend object lives at `/{file_id}/{version_id}` and is immutable.

## P1 — Control plane (`/api/file-storage/v1`)

```text
1.  POST   /files                          create file + return a signed upload URL (JSON body — see below; gts_file_type required)
2.  POST   /files/{id}/versions            presign a new-version upload (no request body, no If-Match) → signed upload URL
3.  POST   /files/{id}/bind                bind/rebind content_id := version_id                          — If-Match
4.  GET    /files/{id}/download-url         issue a signed download URL (pins current content_id, or ?version_id=)
5.  PATCH  /files/{id}                      update custom metadata (JSON Merge Patch)        — If-Match-Metadata?
6.  GET    /files/{id}                      file metadata (JSON)                                          — If-None-Match
7.  DELETE /files/{id}                      delete file + all versions                                    — If-Match
8.  GET    /files                           list files (owner_kind + owner_id required; paginated; JSON array of metadata incl. custom_metadata)
9.  GET    /files/{id}/versions             list versions (version_id, size, hash, hash_mode, part_count?, created_at, is_current)
10. DELETE /files/{id}/versions/{version_id} delete a single, non-current version                          — 409 if current
11. GET    /storages                        list storages + capabilities inline
12. GET    /storages/{storage_id}           one storage + capabilities
13. GET    /policy                          get the stored policy for a scope (?scope=tenant|user)
14. PUT    /policy                          upsert the policy for a scope
15. GET    /policy/effective                compute the effective (most-restrictive) policy
16. GET    /retention-rules                 list retention rules for the caller's tenant
17. POST   /retention-rules                 create a retention rule
18. DELETE /retention-rules/{rule_id}       delete a retention rule
19. POST   /files/{id}/migrate              migrate a non-versioned file's content to a different backend
20. POST   /files/{id}/transfer             transfer ownership of a file to a new owner
```

Notes:
- There is **no** `HEAD /files/{id}` route (an earlier draft of this document listed one; no such route exists in
  `src/api/rest/routes.rs`, and there is no other evidence it was ever separately planned).
- `POST /files` request body (`application/json`, `CreateFileReq`): `{ "owner_kind": "user"|"app", "owner_id":
  "<uuid>", "name": "<string>", "gts_file_type": "<gts uri>", "mime_type": "<string>", "custom_metadata":
  [{"key": "...", "value": "..."}] (optional, default []), "idempotency_key": "<string>" (optional) }`. `idempotency_key`
  is the field documented under "Idempotent-create semantics" (`operations.md`) — see the `409` cause in "Status code
  summary" for what happens on a reused key with a different body.
- `POST /files/{id}/versions` takes **no** request body and does **not** read `If-Match` (`handlers::presign_version`
  has no JSON extractor and no header parameter) — despite an earlier draft of this doc describing one.
- `GET /files` **requires** both `owner_kind` and `owner_id` query params (`400` if either is missing/invalid). A
  caller listing their own files (`owner_id == ` the caller's own subject id) proceeds under the ordinary `READ`
  grant; listing **any other** owner's files additionally requires the caller's `ADMIN_POLICY` authorization scope
  (`403` otherwise) — this closes an enumeration vector where any tenant member could otherwise list an arbitrary
  other subject's files via `?owner_kind=user&owner_id=<victim>`. Each returned item's `custom_metadata` is real,
  batch-fetched per page (one `IN (...)` query), not an always-empty placeholder.
- `POST /files` and `POST /files/{id}/versions` return `{ file_id, version_id, upload_url }` (the control plane
  creates a `pending` `file_versions` row for `version_id` before returning the URL). The client `PUT`s the bytes to
  `upload_url` on the sidecar; the sidecar streams them to the backend, measuring size + SHA-256, then calls the
  control plane's `POST .../versions/{version_id}/finalize` callback (see
  [Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)), which marks the
  version `available`. The sidecar never binds: the client must always follow up with an explicit
  `POST /files/{id}/bind` to swap `content_id := version_id` (see "Upload, bind, and the conflict retry" below).
- `GET /files/{id}/versions` returns a JSON array of version objects. Each carries
  `{ version_id, mime_type, size, hash_algorithm, hash, hash_mode, part_count?, status, is_current, created_at }`
  (ADR-0006). `hash` is lowercase-hex; `hash_algorithm` is always `"SHA-256"`. `hash_mode` is `"whole-sha256"` (then
  `hash` = `sha256(object bytes)` and `part_count` is omitted) or `"multipart-composite-sha256"` (then `hash` =
  `sha256(manifest)`, the offset-manifest composite root, and `part_count` is the number of parts). The manifest text
  itself is stored server-side (`version_hash_manifest`) and used by `migrate_backend`/re-verification; it is not
  currently surfaced as a REST field.
- `GET /files/{id}/download-url` returns `{ download_url, etag, version_id }`. By default it pins the current
  `content_id`; `?version_id=<v>` pins a specific version.
- Restoring a prior version is `POST /files/{id}/bind` with that `version_id` (a pointer swap, no re-upload).
- `DELETE /files/{id}/versions/{version_id}` cannot delete the file's current version (`409`, "bind another version
  first"); deleting the file's only version instead deletes the whole file.

## P1 — Sidecar (signed-URL authorized)

```text
S1. PUT    <signed upload url>             upload the new version's bytes (raw body)
S2. GET    <signed download url>           download content                                        — Range
```

**Planned, not implemented**: a sidecar `HEAD <signed download url>` route and `If-None-Match` → `304` support on the
sidecar `GET`. Neither exists in `src/bin/sidecar.rs` today — the router has no `HEAD` route at all, and `download`'s
only conditional-ish behavior is the `Range` handling below. (`If-None-Match` → `304` **is** implemented on the
control plane's `GET /files/{id}`, which is a distinct surface — see "Conditional headers" below.)

The sidecar verifies the signed token and its claims before serving — a valid token is the delegated authorization
decision, so there is no request-time PDP call and no platform-JWT check of any kind (the `tok.<claim>` predicate
mentioned as a design intent above is not implemented). On `PUT` it streams bytes to the backend and then calls the
control-plane finalize callback, authorized solely by that same signed upload token (see
[Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated) below) — the P1 sidecar
holds **no** direct DB connection and is a thin, stateless byte-mover (a direct-DB mode is a possible P2+ co-located
optimization; see ADR-0003). The sidecar never binds — see "Upload, bind, and the conflict retry" below.

## Data-plane callbacks (sidecar → control plane, s2s token-authenticated)

These control-plane endpoints are called by the **sidecar**, not by end clients, and are registered `.public()` —
the api-gateway does **not** require an end-user JWT for them. The signed upload/part token (in the `x-fs-token`
request header) is the sole authorization; there is no request-time PDP call. Reflects the current (P2 remediation
0.1, "option 2") implementation: the client-supplied `size`/`hash_hex` are **not** trusted — the control plane reads
the blob back from the backend and recomputes both before persisting anything.

**Internal credential (P2 remediation 0.1, remaining half).** The `fs-token` is client-visible (returned in
plaintext inside `upload_url`), so on its own it does not prove the caller is the sidecar rather than the
uploading client itself. Both routes below optionally require a second factor: when
`FileStorageConfig::finalize_internal_secret` is configured, the request must also carry a matching
`x-fs-internal-token` header (constant-time-compared; `403` on missing/mismatch), checked *after* `fs-token`
verification. `None` (the default) preserves the token-only behavior above; see
`docs/ADR/0003-…-sidecar-data-plane.md`'s trust-model section for the mechanism (interim gear-local shared
secret) and the required rollout order (`FS_SIDECAR_INTERNAL_TOKEN` must reach every sidecar before
`require_finalize_internal_secret` is flipped `true`). With this second factor configured, the `fs-token`
remaining client-visible is no longer a way to drive these two routes.

```text
D1. POST /files/{file_id}/versions/{version_id}/finalize
    D2. POST /files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report
```

**`D1` finalize** — called once after a successful single-part `PUT` (or, from the sidecar's own perspective, after
each part write in the multipart case — see `D2`).
- **Header**: `x-fs-token: <signed upload token>` (the same token minted for the `PUT`; `op` must be `Put` and the
  token's `file_id`/`version_id` must match the path).
- **Request body** (`application/json`): `{ "size": <i64>, "hash_hex": "<64-char lowercase hex>" }` — the size and
  SHA-256 hash the sidecar itself measured while streaming.
- **Server behavior**: re-enforces the policy size ceiling, then reads the blob back from the backend at the
  version's `backend_path`, recomputes its actual size + SHA-256, and rejects (`400`) if either does not match the
  request body, or if no blob is present at that path at all (upload never completed). On success the version's
  `mime_type` is also re-validated/resolved from the real bytes (magic-byte sniffing) and persisted, and the version
  is marked `available`.
- **Response**: `204 No Content`. Errors: `400` (validation/read-back mismatch), `403` (bad/expired/mismatched
  token, **or** missing/mismatched `x-fs-internal-token` when `finalize_internal_secret` is configured — see
  above), `404` (version not found), `409` (already finalized), `500`.
- This endpoint does **not** bind the version as current — `POST /files/{id}/bind` remains a separate, explicit
  client call.

**`D2` report-part** — added in P2 remediation 0.2 group B: called by the sidecar after each successful multipart
part write, closing the gap where nothing previously populated `multipart_upload_parts` in a real (non-test)
deployment.
- **Header**: `x-fs-token: <signed multipart-part token>` (`op` must be `MultipartPart`; `file_id`/`version_id`/
  `upload_id`/`part_number` must match the path).
- **Request body** (`application/json`): `{ "backend_etag": "<string>", "hash_hex": "<64-char lowercase hex>", "size": <i64> }`
  — the backend-assigned ETag for this part plus the part's measured SHA-256 and byte length. `hash_hex` that does
  not decode to exactly 32 bytes is rejected with `400` (mirrors `D1` finalize's identical check) — persisting a
  wrong-length hash here would otherwise only surface later as an opaque `400` at `complete`, charged against
  whichever caller happens to call `complete`, not the one that reported the bad hash.
- **Response**: `204 No Content`. Errors: `400` (malformed/wrong-length `hash_hex`, or a reported `size` that does not
  match the per-part size minted into the token at initiate time), `403` (bad/expired/mismatched token, **or**
  missing/mismatched `x-fs-internal-token` when configured — see above), `404`, `409` (the session is no longer
  `in_progress` — already completed/aborted/expired), `500`.

## P2 — Multipart upload

> **Implementation status**: **shipped** (server-authoritative flow; see the status note further below). Multipart
> is **server-authoritative**: the client sends desired parameters and the control plane returns the exact parts
> plan (sizes/offsets) with **one signed URL per part** pointing at the sidecar.

```text
P2-1. POST /files/{id}/multipart            initiate (JSON: declared_mime, declared_size, preferred part size, concurrency); returns the parts plan + per-part signed URLs
P2-2. PUT  <signed part url>                upload one part to the sidecar (raw body)
P2-3. POST /files/{id}/multipart/{upload_id}/complete   assemble all reported parts into the final object, mark the version `available`, and return version/size/composite-hash
P2-4. DELETE /files/{id}/multipart/{upload_id}          abort; parts discarded
P2-5. GET /files/{id}/multipart/{upload_id}             introspect/resume; returns state + received/missing parts, with fresh resume URLs for missing parts of a live session
```

Notes:
- `P2-3` (`complete`) does **not** bind the version as current — like the single-part flow, `POST /files/{id}/bind`
  is a separate, explicit client call. It takes an **optional** `If-Match` header (item 3.3): a concrete value is
  checked against the file's current content ETag (`400` on mismatch — `FailedPrecondition` collapses to `400` on
  this platform); `*` or an absent header is unconditional (backward compatible with the pre-3.3 contract). As of
  item 3.3 the route declares `401`/`403`/`404`/`409`/`400`/`500`.
- `P2-3` (`complete`) returns **`200`** (not `204` as of item 3.3) with a JSON body — see the response shape below.
- `P2-5` (`GET .../multipart/{upload_id}`, introspect/resume) is **shipped** (item 3.4, SHIP decision). It is
  authorized on `write` (like initiate/complete/abort, not `read`) since it hands out live resume upload URLs. A
  foreign or missing `upload_id` is masked as `404`, identical to `complete`'s guard. The route declares
  `401`/`403`/`404`/`500`. See the response shape below.

**`P2-1` initiate request body** (`application/json`):

| Field | Type | Required | Description |
|---|---|---|---|
| `declared_mime` | `string` | yes | MIME type of the file being uploaded (e.g. `video/mp4`). Validated against the effective allowed-types policy. |
| `declared_size` | `uint64` | yes | Total file size in bytes. The control plane validates this against the effective policy size limit and storage quota at initiate time — exactly like single-part upload does at presign time — so that oversized or quota-exceeding uploads are rejected before any bytes are transferred. `400` if it exceeds the policy size limit; `429` if it would exceed the storage quota. **Implementation status (P2)**: the `429` quota path only fires when a `QuotaClient` is configured — none is, in any deployment (`gear.rs`'s `quota_client: None`, Tier 1 item 1.4) — so callers do not observe quota rejections; see [operations.md](./operations.md#storage-quota-not-enforced). |
| `preferred_part_size` | `uint64` | no | Client hint for the part size in bytes; the server may widen it (see the `MAX_PART_COUNT` note below) or otherwise adjust it to satisfy backend minimums. Rejected with `400` if outside `[DEFAULT_MIN_PART_SIZE (5 MiB), MAX_PART_SIZE (5 GiB)]`. |
| `concurrency` | `uint32` | no | Advisory hint for client-side upload concurrency; does not change the parts plan itself. |

The server-computed parts plan is capped at `MAX_PART_COUNT = 10_000` parts (`domain::multipart::compute_plan`). If
the chosen part size would produce more parts than that, `part_size` is **widened** (never past `MAX_PART_SIZE`, 5
GiB) just enough to bring the plan back under the ceiling; if even the maximum part size cannot fit `declared_size`
within 10,000 parts, initiate is rejected with `400` before any parts vector is allocated.

The multipart **session** (`expires_at` on the `multipart_uploads` row) and the per-part signed **URLs** it returns
have independent TTLs: the session lives for `multipart_session_ttl_secs` (24h default — a real time budget for a
multi-GB upload) while each part URL is signed for the much shorter `default_url_ttl_secs` (15 min default). A
still-`in_progress`, unexpired session can re-mint fresh part URLs via `P2-5` introspect as earlier ones expire,
without re-initiating.

**`P2-1` initiate response** (`application/json`) — the server-computed plan:

```json
{
  "upload_id": "uuid",
  "version_id": "uuid",
  "part_hash_algorithm": "SHA-256",
  "part_size": 8388608,
  "parts": [
    { "part_number": 1, "offset": 0, "size": 8388608, "upload_url": "https://sidecar/…?fs-token=…" },
    { "part_number": 2, "offset": 8388608, "size": 2097152, "upload_url": "…" }
  ],
  "expires_at": "RFC3339"
}
```

**`P2-2` upload part** — the client `PUT`s each part's raw body to its `upload_url` on the sidecar. Each URL is a
signed token (ADR-0004) carrying the part's `upload_id`, `part_number`, `offset`, and **exact `size`** as claims. The
size contract is enforced asymmetrically, since only the "too many bytes" direction can be caught before the body
finishes streaming:
- **Oversized** (body would exceed the `size` claim): aborted **mid-stream**, the moment the running byte count would
  cross the claim, with `413 Payload Too Large` — before the excess bytes are ever written.
- **Undersized** (body is shorter than the `size` claim): only detectable once the stream is fully drained, so it
  streams to completion and is then rejected with `400 Bad Request`. For a `multipart_native` backend (e.g. S3) the
  part was only ever buffered in memory pending the native `UploadPart` call, so nothing needs cleanup; for a
  non-native backend (the `local-fs`-style offset-object model), the part *was* already written to its own backend
  object (`{backend_path}.part.{n}`) as bytes streamed in, so the sidecar explicitly **deletes that partial object**
  before returning `400`, rather than leaving a mismatched part object behind.

Re-`PUT` of the same part is idempotent (enables resume — a fresh `PUT` simply overwrites/re-buffers). For a
`multipart_native` backend the sidecar drives the backend multipart API; otherwise it offset-writes each part into
its own object as above. Per-part **SHA-256** hashes are reported to the control plane via the `D2` report-part
callback (see
[Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)) and persisted in
`multipart_upload_parts.part_hash`; `complete` assembles from the reported parts. The sidecar's `200` response body
for a successful part `PUT` is `{ "part_number": <u32>, "etag": "<backend-assigned or hash-derived string>",
"hash_algorithm": "SHA-256", "hash": "<64-char lowercase hex>" }`.

**`P2-3` complete response** (`application/json`, `200` — item 3.3, replacing the previous bare `204`):

```json
{
  "version_id": "uuid",
  "size": 10485763,
  "hash_algorithm": "SHA-256",
  "content_hash": "<64-char lowercase hex — sha256(manifest), ADR-0006 composite root>",
  "hash_mode": "multipart-composite-sha256",
  "part_count": 2,
  "manifest": "v1,0:<64-hex>,8388608:<64-hex>"
}
```

Before `complete` assembles anything it now also diffs the plan's expected part numbers (`ceil(declared_size /
part_size)`) against the parts actually reported; a non-empty diff is rejected with `409` and the missing part
numbers in the error detail, **before** ever calling the backend's native multipart completion — a caller debugging
a stalled upload gets an actionable list instead of the opaque total-assembled-size mismatch that was previously the
only signal. `manifest` lets a client independently re-verify the composite hash (see
[content-hash-modes.md](./features/content-hash-modes.md) §"Client-Side Manifest Re-Verification") without a second
round-trip.

**`P2-5` introspect response** (`application/json`, `200` — item 3.4):

```json
{
  "upload_id": "uuid",
  "version_id": "uuid",
  "state": "in_progress",
  "declared_mime": "video/mp4",
  "declared_size": 10485763,
  "part_size": 8388608,
  "created_at": "RFC3339",
  "expires_at": "RFC3339",
  "received": [
    { "part_number": 1, "size": 8388608, "uploaded_at": "RFC3339" }
  ],
  "missing": [
    { "part_number": 2, "offset": 8388608, "size": 2097155, "upload_url": "https://sidecar/…?fs-token=…" }
  ]
}
```

`received` lists parts already reported (via the sidecar's report-part callback); `missing` lists the rest, with
their `(offset, size)` recomputed from the session's persisted `declared_size`/`part_size` columns. `upload_url` on a
`missing` entry is present only while the session is still `in_progress` and unexpired — its token `exp` is capped at
the session's own `expires_at`, never a fresh full TTL, so a resumed upload cannot outlive the session it resumes. A
terminal (`completed`/`aborted`) or expired session still returns `state` and the `received`/`missing` accounting,
but every `missing` entry omits `upload_url`.

Full request/response envelopes, error taxonomy, token claims, persistence, and resumability are specified in the
FEATURE artifact **[features/multipart-coordinator.md](./features/multipart-coordinator.md)**.

> **Implementation status**: the server-authoritative flow is **shipped**. `POST /files/{id}/multipart` computes the
> parts plan and returns one signed sidecar URL per part (each token carrying `upload_id`, `part_number`, `offset`, and
> the exact `size` claim); the sidecar enforces the per-part size at transfer with `413` **before** any write. The
> initiate-time `declared_size` gate is in place and `declared_size`/`part_size` are persisted on the session row so the
> plan can be reconstituted for resume. The interim client-driven control-plane byte route (`PUT .../parts/{n}`) has
> been **removed** — bytes flow exclusively to the sidecar (ADR-0003, FEATURE §8 migration). The complete-time
> total-size check (assembled size == `declared_size`) remains as the defence-in-depth backstop. Per-part hashes are
> **SHA-256** in P2. The introspect/resume endpoint (`P2-5`, item 3.4) is also **shipped**: it reconstitutes the
> plan's missing parts from the session's persisted columns and re-mints their signed URLs for a live session.

## P2 — Policy engine

Per-tenant and per-user policies (allowed MIME types, size limits, metadata limits, enabled event types). The
**effective** policy for a write is the most-restrictive combination across the applicable levels (tenant ⊕ user).

```text
GET  /policy?scope=<tenant|user>&scope_owner_id=<uuid>   fetch the stored policy for one scope
PUT  /policy                                             upsert (create or replace) the policy for one scope
GET  /policy/effective?user_owner_id=<uuid>              compute the effective (most-restrictive) policy
```

- `GET /policy`: `scope` is required (`"tenant"` or `"user"`); `scope_owner_id` is required when `scope="user"` —
  omitting it is rejected with `400` (`policy_service.rs::get_own_policy`), not silently treated as tenant scope.
  Returns `204 No Content` (no body) when no policy is configured for that scope — this is a normal, non-error
  outcome, not a `404`.
- `PUT /policy` request body: `{ "scope": "tenant"|"user", "scope_owner_id": "<uuid, omit for tenant>", "body": { "allowed_mime_types": [...], "size_limits": {...}, "metadata_limits": {...}, "enabled_event_types": [...] } }`.
  Response: the stored `PolicyDto` (`200`).
- `GET /policy/effective`: no scope is required to read the caller's own effective policy; `user_owner_id` is an
  optional hint to include a specific user level in the resolution. Response fields are all "effective" (most
  restrictive already resolved): `allowed_mime_types` (`null` = unrestricted), `max_bytes`, `per_mime_max_bytes`,
  `metadata_limits`.
- **There is no `DELETE /policy` route.** To relax a policy, `PUT` a replacement body (e.g. an empty/permissive one);
  there is no way to remove a stored policy row entirely via the API.
- A concurrent `PUT /policy` race for the same scope is closed at the DB level by two partial unique indexes on
  `(tenant_id, scope, scope_owner_id)` (see `docs/migration.sql`); the upsert itself is wrapped in a transaction.

## P2 — Retention rules

Tenant/user/file-scoped rules (age-based, inactivity-based, or custom-metadata-value-based) evaluated by the
background cleanup sweep (see `docs/operations.md`), which deletes files matching an active rule's criteria.

```text
GET    /retention-rules             list all retention rules for the caller's tenant
POST   /retention-rules             create a retention rule
DELETE /retention-rules/{rule_id}   delete a retention rule
```

- `POST /retention-rules` request body: `{ "scope": "tenant"|"user"|"file", "scope_target_id": "<uuid, omit for tenant>", "body": { "age": {"max_age_days": N}, "inactivity": {"inactivity_days": N}, "metadata": {"key": "...", "value": "..."} } }`
  (`age`/`inactivity`/`metadata` are each optional; a rule may combine more than one criterion). Response: the
  created `RetentionRuleDto` (`201`). Semantic validation (`policy_service.rs::validate_retention_rule`) rejects with
  `400`: a body with **all three** of `age`/`inactivity`/`metadata` absent (a rule that could never match any file);
  `age.max_age_days` or `inactivity.inactivity_days` **less than 1** (either would match every file in the tenant on
  the very next sweep tick); and `scope` ∈ `{user, file}` with `scope_target_id` omitted.
- `GET /retention-rules` returns all rules for the caller's tenant across every scope (no scope filter query param).
- `DELETE /retention-rules/{rule_id}` → `204`, or `404` if the rule does not exist.

## P2 — Backend migration

```text
POST /files/{id}/migrate   { "target_backend_id": "<string>" }   → 204
```

Migrates a file's content to a different configured storage backend, preserving the file's identity (`file_id`
unchanged). **Non-versioned files only** — a file with more than one `file_versions` row is rejected
(`VersionedFileMigrationNotSupported`, `409`). The version must already be `available` (`409` otherwise). The
content hash is re-verified against the source backend's blob before the destination write is committed. Migrating
onto a non-durable backend (e.g. a dev/test `memory` backend) additionally requires the caller's `ADMIN_POLICY`
authorization scope, not just `WRITE`, since it risks silent data loss on the next restart.

## P2 — Ownership transfer

```text
POST /files/{id}/transfer   { "new_owner_kind": "user"|"app", "new_owner_id": "<uuid>" }   → 200, FileDto
```

Atomically replaces the file's `owner_kind` + `owner_id`, records an audit row (`TransferOwnership`), and enqueues a
`file.owner_transferred` event in the same transaction.

## Upload, bind, and the conflict retry

Content is an immutable blob per version; a file's live content is the `content_id` pointer, swapped under optimistic
CAS. Every write is **presign (control) → `PUT` (data) → finalize (data-plane callback) → bind (control)**:

1. **Presign**: `POST /files` (or `POST /files/{id}/versions`) → `{ file_id, version_id, upload_url }`. The control
   plane creates a `pending` `file_versions` row for `version_id` before returning the signed `upload_url`.
2. **Upload**: `PUT upload_url` to the sidecar (raw body). The sidecar streams the bytes to the backend, measuring
   size + SHA-256 as they land. It does not check `If-Match` and does not bind — it only moves bytes.
3. **Finalize**: once the `PUT` completes, the sidecar calls the control plane's token-authenticated
   `POST /files/{id}/versions/{version_id}/finalize` callback (see
   [Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)). The control plane
   reads the blob back, verifies size + SHA-256, and flips the version `pending → available`. This step never
   touches `content_id`.
4. **Bind**: the client separately calls `POST /files/{id}/bind { version_id }` with `If-Match: "<current content
   ETag>"` to swap `content_id := version_id` under optimistic CAS. Binding a version whose upload has not yet been
   finalized (still `pending`) fails with `409`.

Backend content is never mutated in place; a replacement is always a new version + a pointer swap.

On a **bind conflict** — the file's content changed concurrently, so `If-Match` no longer matches the current ETag —
the control plane rejects the bind with `400 Bad Request` (`FailedPrecondition` collapses to `400` on this platform,
see "Status code summary" below). There is no sidecar-side conflict check: the sidecar never binds, so this is purely
a control-plane concern. The client re-reads the file's current ETag (e.g. via `GET /files/{id}`) and replays
`POST /files/{id}/bind` with that `version_id` and the fresh `If-Match` — **no byte re-upload**, because the
already-`available` version persists.

**On a bind conflict, re-bind — do not re-presign or re-upload.** Rebinding is a control-plane call
(`POST /files/{id}/bind`), **independent of the signed upload URL** — so the upload URL's `exp` is irrelevant to the
retry and the bytes are not re-sent (the version persists as-is). Re-presigning is **not** idempotent: a fresh
`POST /files/{id}/versions` + upload creates a **new sibling `version_id`**. If that sibling is abandoned before
`finalize`, the cleanup engine's abandoned-pending sweep reclaims it after `orphan_grace_secs`
(`cpt-cf-file-storage-fr-orphan-reconciliation`) — but if it is finalized (`available`) and simply never bound, it is
**not** swept by anything: it persists as an extra stored version until it is either bound or explicitly
deleted. Clients **should** rebind the already-uploaded `version_id` instead, both to avoid the wasted upload and to
avoid leaving this unswept sibling behind.

## Signed URLs

- **Ed25519-signed compact token, asymmetric, stateless — codec-equivalent to PASETO `v4.public`, not literal
  PASETO.** ADR-0004 specifies PASETO `v4.public`; what ships is `base64url(JSON payload).base64url(signature)`
  (`infra::signed_url::Issuer`/`Verifier`) — same asymmetric control-signs/sidecar-verifies property and the same
  opaque, evolvable claim-set, but **no PASETO footer and no `kid`** (key rotation is not implemented; one static
  keypair today). The control plane signs with the Ed25519 private key (sole minter); the sidecar verifies with the
  public key and can never mint. **Not JWT** (no `alg` field → no algorithm-confusion). No DB lookup to verify. No
  per-token revocation — emergency revocation is the platform auth module's token revocation. See
  [ADR-0004](./ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md).
  - **Implementation note:** because the token is opaque (below), the concrete codec is an internal detail of
    control + sidecar and may move to a literal PASETO library (with a real footer/`kid`) later without any
    client-visible change.
  - **FIPS posture:** Ed25519 is FIPS 186-5 approved, but a FIPS deployment requires the sign/verify primitive to run
    inside a FIPS-validated module (the platform's `rustls-corecrypto-provider`). The primitive sits behind an in-house
    `SignatureProvider` abstraction and we **MUST NOT** pull in any crate that hard-wires a non-FIPS algorithm we
    cannot replace; a FIPS-approved alternative (e.g. ECDSA P-256) is reachable behind the same opaque token without a
    codec change. See ADR-0004 "FIPS posture".
- **Opaque to everyone but control + sidecar.** The token's claim-set and crypto are private to the minter and verifier;
  every other participant (browser, CDN, proxy, app, logs, SDK transport) MUST treat it as **opaque bytes** and never
  parse it — the format can and will change ("Token Opacity Contract").
- **Two carriers, same bytes:** the `fs-token` **query** parameter (`?fs-token=<token>`, bare embeddable URL) **or** the
  `X-FS-Token` **header** (programmatic / batch — credential out of the URL, stable cacheable URL). The token is **never**
  carried in `Authorization` — that header always carries the standard platform JWT. `file_id` is **also** the URL
  **path**. **`backend_id` and `backend_path` ARE carried in the token** (`Claims::backend_id`/`backend_path`,
  `infra/signed_url/mod.rs`) — this is deliberate, not an oversight: the sidecar has **no DB connection at all** (see
  "Response headers" below and ADR-0003), so it cannot resolve them any other way. The sidecar resolves the object
  purely from the verified claims, never from a "version row" lookup.
- **Claims (inside the token; AND-combined; all optional except `exp` and `op`):**
  | Claim | Req. | Status | Applies | Violation |
  |---|---|---|---|---|
  | `exp` | yes | **shipped** | all | `403` (at/past `exp`, exclusive boundary) |
  | `op` (+ method check) | yes | **shipped** | all | `403` |
  | `backend_id` / `backend_path` | yes | **shipped** | all | n/a — used to resolve the object, no DB lookup |
  | `ip` (addr/CIDR) | no | **NOT implemented** (planned) | all | — |
  | `tok.<claim>` | no | **NOT implemented** (planned; would need JWT) | all | — |
  | `max_size` | no | **shipped** | upload | `413` |
  | `exact_size` | no | **shipped** | upload | `400`¹ |
  | `expected_hash` = `<alg>:<hex>` | no | **shipped** | upload | `400`² |
  | `max_rate` | no | **NOT implemented** (planned) | up/down | — |
  | `max_conns` | no | **NOT implemented** (planned) | up/down | — |
  | `content_type` | no | **shipped** (P2 1.11) | download (`op = get`) | n/a — echoed as `Content-Type`³ |
  | `etag` | no | **shipped** (P2 1.11) | download (`op = get`) | n/a — echoed as `ETag`³ |

  `Claims` has no `ip`, `tok.<claim>`, `max_rate`, or `max_conns` field today (`infra/signed_url/mod.rs`) — the
  sidecar performs no client-address check, no platform-JWT validation of any kind, and no rate/connection limiting.
  These rows are retained as tracked design intent, not shipped behavior.

  ¹ `exact_size` is checked only after the stream fully drains (mismatch → `400`, "size does not match exact_size");
  it can never itself trigger `413` (that's `max_size`'s mid-stream abort, and the two claims are documented as
  mutually exclusive). **Unverified further**: `rg -n "exact_size:" src/` finds the field only in its struct
  definition and in tests — no presign path (`create.rs`/`write.rs`) was found actually setting it on an issued
  token as of this doc pass, so this claim/status pairing may be dead code; flagged for the team, not
  fixed here (out of scope for this doc pass).<br>
  ² previously documented as `422`; `bin/sidecar.rs`'s `expected_hash` check returns `(StatusCode::BAD_REQUEST, ...)`
  (`400`), not `422` — no `422` response exists anywhere in this gear.<br>
  ³ these two claims are populated only on a **download** (`op = get`) token, at `download-url` issuance time — the
  control plane reads `version.mime_type` and computes the content ETag (`domain::etag::content_etag`) once and
  stamps both into the claims, so the sidecar (no DB access) can emit the real `Content-Type`/`ETag` response
  headers instead of a generic `application/octet-stream` fallback with no `ETag` at all. `#[serde(default)]` keeps
  verification tolerant of a token minted before these fields existed — such a token still falls back exactly as
  before. Never populated on upload (`op = put`) or multipart-part (`op = multipart_part`) tokens.
- **`exp` is mandatory, short by default, and hard-capped.** Every issued URL gets a **short default TTL**
  (`default_url_ttl_secs`, minutes — 15 min default) to bound the stale-permission window, and `Issuer::issue`
  **silently clamps** `exp` down to a **hard ceiling** `max_url_ttl_secs` (≤ **7 days** default) rather than refusing
  to mint. Multipart additionally has a third, independent knob, `multipart_session_ttl_secs` (24h default), that
  bounds the multipart *session's* own lifetime separately from the per-part URL TTL above (see `operations.md`).
  The sidecar rejects at `now >= exp` (expiry is exclusive: a token stops working exactly at `exp`, not one second
  later). **Stale-permission trade-off:** authorization is evaluated at signing and there is no per-token
  revocation, so the TTL bounds the exposure window — hence the short default for private content; the 7-day
  ceiling is an explicitly accepted trade-off for low-sensitivity / deliberately long-lived cases (bare query-token
  URLs in particular MUST use a short TTL; durable/anonymous sharing is P3 FileShare). "Available to everyone for 5
  minutes" = only `exp` (the `tok.<claim>` predicate mentioned in some earlier drafts of this doc is **not
  implemented** — see the claims table above).
- **`max_size` and `exact_size` are mutually exclusive by construction** — no code path mints a token with both set —
  but this is **not independently validated** as a "both present" error at presign or verify time; there is no
  dedicated rejection path for that combination.
- **`expected_hash`** `<alg>` must be in the backend allow-list (P1: `SHA-256`); lowercase hex; baked by the control
  plane (may carry a client-supplied value from the presign request).
- **`max_rate` / `max_conns` are NOT implemented.** No such claims exist on `Claims` and the sidecar enforces no
  per-URL rate/connection cap; this remains an open design point (scoping to one `(file_id, op)` and cross-instance
  coordination across the sidecar fleet), not a shipped capability.
- **Outside the token:** the `Range` header, conditional headers, and the `PUT` body are not part of the token — so one
  signed URL serves many ranges, and body integrity is enforced by `max_size`/`expected_hash` during the stream plus a
  control-plane re-verification at `finalize`, which re-reads the stored blob and recomputes its size/hash rather than
  trusting the sidecar's report (single-part); a multipart upload instead derives the composite hash from the
  reported per-part hashes at `finalize`. `bind` performs no integrity check of its own — it only swaps `content_id`
  to point at an already-finalized (`Available`) version, guarded by the `If-Match` content-ETag precondition above.
- **"Baked response headers" claim — NOT implemented.** The token carries only the two specific `content_type`/`etag`
  claims (download-only, above); there is no general response-header-set claim and the sidecar does not echo an
  arbitrary `Content-Disposition`/`Cache-Control`/etc. from the token. See "Response headers" below for what the
  sidecar actually emits.

## Conditional headers

- `If-Match`: required on **bind** (`POST /files/{id}/bind`) and on `DELETE`. Mismatch → `400 Bad Request` on the
  control plane (`FailedPrecondition` collapses to `400` on this platform — see "Status code summary" below). The
  sidecar's data-plane `PUT` does not check `If-Match` at all — it only streams bytes and calls finalize; conditional
  concurrency on content is enforced solely by the control-plane `bind` handler.
- `If-Match-Metadata: <u64>`: **optional** on metadata-only `PATCH`; matched against the current `meta_version` (the
  value published as `X-FS-Metadata-Revision`). Mismatch → `400` (same `FailedPrecondition` → `400` mapping). Absent
  → last-write-wins (back-compatible default); clients keeping meaningful state in custom metadata opt in.
- `If-None-Match`: optional on control-plane `GET /files/{id}` (metadata) only; match → `304 Not Modified`. **Not**
  implemented on the sidecar's download `GET` (see "P1 — Sidecar" above) — there is also no `HEAD` route on either
  plane.
- ETag is opaque, derived from `(file_id, content_id)`, content-only, and explicitly **not** equal to the content
  hash. It changes exactly when content is (re)bound; a metadata-only `PATCH` does not change it. The content
  hash algorithm+value are exposed in the `GET /files/{id}/versions` body (`hash_algorithm`, `hash`), not as
  response headers (see "Response headers" below — the sidecar does not emit `X-FS-Hash-*`). Content-hash modes
  (whole-object vs. multipart offset-manifest composite) are **shipped**, not proposed —
  see [ADR-0006](./ADR/0006-cpt-cf-file-storage-adr-content-hash-modes.md) and `hash_mode`/`part_count` above (§P1
  control plane notes).

## Range support

Served by the **sidecar**.

- `GET <signed url>` accepts `Range: bytes=<start>-<end>`, `bytes=<start>-`, and `bytes=-<suffix-length>`. A
  well-formed, satisfiable range returns `206 Partial Content` with `Content-Range: bytes <s>-<e>/<n>`. A well-formed
  but **unsatisfiable** range (e.g. `start ≥ size`) returns `416` with `Content-Range: bytes */<n>` **and**
  `Accept-Ranges: bytes` (the `416` path is not an exception to the "every download response includes
  `Accept-Ranges`" rule below).
- The parser (`infra::content::range`) is strict: only ASCII-digit byte positions are accepted (no signs, no interior
  whitespace), and a syntactically invalid/unparseable `Range` — including a well-formed `N-M` pair where `M < N`
  (last-byte-pos less than first-byte-pos, invalid syntax per RFC 9110 §14.1.1, not "unsatisfiable") — is **ignored**:
  `200 OK` with the full body, per RFC 7233 §3.1.
- Because `Range` is not part of the signature, **one signed download URL serves many ranges** (random access). Every
  download response (`200`, `206`, and `416`) includes `Accept-Ranges: bytes`.
- Multi-range (`bytes=a-b,c-d`) requests are **not supported** at all — any comma in the header value makes the whole
  `Range` header fail to parse, so it is ignored and the full body is served with `200` (not a `multipart/byteranges`
  response).

## Response headers (download, on the sidecar)

Shipped (`src/bin/sidecar.rs`'s `download`/`download_range`/`download_whole` handlers):

```text
Accept-Ranges: bytes
Content-Type: <mime>                # from the token's content_type claim; "application/octet-stream" fallback
ETag: "<opaque>"                     # from the token's etag claim; header omitted entirely if the claim is empty
Content-Range: bytes <s>-<e>/<n>     # only on 206 (and "bytes */<n>" on 416)
```

`Content-Length` is set by the HTTP framework from the response body, not hand-rolled by the sidecar.

`ETag` and `Content-Type` are, as of P2 1.11, genuinely sourced per-request from the download token's
`etag`/`content_type` claims (see the Claims table above) rather than a control-plane round trip — the sidecar has
no DB access, so the token is its only source for either. A token minted before P2 1.11 (or, in principle, any
other producer that leaves either claim empty) falls back to `Content-Type: application/octet-stream` and omits
`ETag` entirely, rather than sending an empty header.

**Planned (not implemented) — do not rely on these today:**

```text
Last-Modified: <RFC 7231 date>
X-FS-File-Id: <uuid>
X-FS-Version-Id: <uuid>
X-FS-GTS-File-Type: gts.cf.fstorage.file.type.v1~...
X-FS-Hash-Algorithm: SHA-256
X-FS-Hash-Value: <hex>
X-FS-Metadata-Revision: <u64>
X-FS-Owner-Kind: user|app
X-FS-Owner-Id: <uuid>
X-FS-Created-At: <ISO 8601>
X-FS-Meta-<key>: <value>
<baked response headers>            # see "the 'baked response headers' claim — NOT implemented" above
```

None of the `X-FS-*` headers above are emitted by the sidecar today — they were an earlier design intent (a "baked
response headers" claim that never shipped, see "Signed URLs" above) and would require the sidecar to either gain DB
access or carry substantially more per-request state in the token than it does now. `HEAD` and sidecar-side
`If-None-Match` → `304` are likewise not implemented (see "P1 — Sidecar" above) — this is a pre-existing gap, not
something P2 1.11 introduced or closes.

## Status code summary

- `200 OK` — successful control read, metadata `PATCH` with change, bind, presign, sidecar full download, or a
  sidecar multipart-part `PUT` (JSON body `{ part_number, etag, hash_algorithm, hash }` — see `P2-2` above).
- `201 Created` — successful `POST /files` (file created; body carries the upload URL).
- `204 No Content` — successful `DELETE`. The metadata rows (file + all versions) are removed before the best-effort
  backend deletes; re-`DELETE` of an already-deleted `file_id` returns `404` (idempotent).
- `206 Partial Content` — successful range read (sidecar).
- `304 Not Modified` — `If-None-Match` matched the current ETag (control-plane `GET /files/{id}` only — not
  implemented on the sidecar, see "Response headers" above).
- `400 Bad Request` — malformed request (invalid JSON, missing required fields; e.g. `GET /files` missing
  `owner_kind`/`owner_id`, or `GET /policy?scope=user` missing `scope_owner_id`); an `exact_size` upload whose final
  length is short, or an undersized multipart part (sidecar, `PUT`); a content hash mismatch against the
  `expected_hash` claim (sidecar, `PUT`); a malformed/wrong-length `hash_hex` on the `D1`/`D2` callbacks, or a
  reported multipart-part `size` that does not match the token's claim (control plane); a retention rule failing
  semantic validation (all three predicates absent, a zero-day age/inactivity field, or a missing
  `scope_target_id` for `user`/`file` scope); the declared file size exceeds
  the effective policy size limit (control plane, `create_file`/`presign_version`/multipart `initiate`); an
  `If-Match`/`If-Match-Metadata` precondition mismatch on control-plane `bind`/`DELETE`/`PATCH`/multipart `complete`
  (item 3.3; `FailedPrecondition` collapses to `400` on this platform — there is no `412`-mapped canonical-error
  variant); the finalize callback's
  read-back size/hash/mime not matching the sidecar's claim, or no blob present at the version's backend path at all
  (control plane, `POST .../finalize` — see
  [Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)); or invalid GTS file
  type format (control plane).
- `401 Unauthorized` — the sidecar's `PUT`/`GET`/multipart-part routes require the signed token via the `fs-token`
  query param or `X-FS-Token` header; a request that supplies **no** token at all gets `401` (a request with a token
  that fails to verify gets `403` instead — see below).
- `403 Forbidden` — authorization denied (control), or token verification failed at the sidecar: bad signature/
  encoding, expired (`now >= exp` — expiry is exclusive), method ≠ the `op` claim, part-number mismatch on a
  multipart-part route, or (finalize/report-part only) a missing/mismatched `x-fs-internal-token` when
  `finalize_internal_secret` is configured. (The `ip`/`tok.<claim>` checks and the `max_url_ttl` cap described
  elsewhere in this doc as constraints are, respectively, not implemented and enforced at signing rather than
  re-checked here.)
- `404 Not Found` — file, version, or retention rule does not exist.
- `409 Conflict` — includes, per handler (each via `DomainError::Conflict` → `aborted`):
  - `bind`: the target `version_id`'s upload has not been finalized yet.
  - `delete_version` (`DELETE /files/{id}/versions/{version_id}`): attempting to delete the file's current version
    (bind another version first).
  - `migrate` (`POST /files/{id}/migrate`): the version is not yet finalized, or a concurrent migration to a
    different target already won the race.
  - multipart `complete`/`abort`: the session is not `in_progress` (e.g. completing an already-aborted upload), one
    or more planned parts have not been reported yet (item 3.3 — `MultipartPartsMissing`, error detail lists the
    missing part numbers, checked **before** the size check below), the assembled size does not match
    `declared_size`, or the pending version row was removed concurrently.
  - `create_file` (idempotent retry): the same `idempotency_key` was reused with a materially different request body.
  - `download_url` (`GET /files/{id}/download-url`): the file has no bound content yet (never bound), or the target
    version's upload has not been finalized. **Not declared** in this route's OpenAPI registration in `routes.rs`
    (only `401`/`403`/`404`/`500` are) even though the domain code returns it — an undocumented-in-OpenAPI real `409`,
    found while auditing conflict cases for this doc pass; flagged for the team alongside the `update_metadata`
    mismatch below.
  - **sidecar `PUT` (replay)**: a `PUT` to an `upload_url` whose `backend_path` already holds a published blob (a
    genuine token replay after the version was already finalized, as opposed to a benign retry of the same in-flight
    upload) gets `409 Conflict` from the sidecar itself — `publish_exclusive`'s create-exclusive write refuses to
    overwrite the existing object, and the live bytes are never touched. A benign retry (the earlier publish landed
    but finalize had not yet run) instead converges to `200` once finalize succeeds on this attempt.

  Note: `update_metadata` (`PATCH /files/{id}`) declares a `409` response in its OpenAPI registration
  (`routes.rs`), but no domain code path for it was found returning `DomainError::Conflict` as of this doc pass —
  its only observed failure beyond validation is the `If-Match-Metadata` mismatch, which is `400`
  (`PreconditionFailed`). Flagged as unverified/possibly-stale route metadata; not asserted as a real `409` case here.
- `412 Precondition Failed` — **not used anywhere in this gear.** This platform's canonical-error taxonomy has no
  `412`-mapped variant; `FailedPrecondition` collapses to `400` on the control plane (see above), and the sidecar
  never performs an `If-Match`/conditional check at all — it only streams bytes and calls finalize, so there is no
  data-plane `412` either. `grep -n "412" src/bin/sidecar.rs` returns no matches. A bind conflict is always a `400`
  from the control-plane `bind` handler (see "Upload, bind, and the conflict retry" above).
- `413 Payload Too Large` — upload exceeds the `max_size` claim, aborted mid-stream (sidecar, `PUT`).
- `416 Range Not Satisfiable` — a well-formed `Range` that cannot be satisfied against the size (sidecar). An
  unparseable `Range` is **not** a `416` — it is ignored and the full body is served with `200`.
- `429 Too Many Requests` — **NOT implemented** as a sidecar per-URL `max_conns` cause (that claim does not exist,
  see "Signed URLs" above); the only live `429` source is the control-plane storage quota check on
  `create_file`/`presign_version`/multipart `initiate` (`QuotaExceeded`). **Implementation status (P2)**: that
  `QuotaExceeded` case is itself only reachable when a `QuotaClient` is wired. None is, in any deployment — `gear.rs`
  always passes `quota_client: None` (Tier 1 item 1.4), so `check_quota`/`check_quota_bytes` are a permissive no-op
  and this `429` cause cannot currently occur either. See [operations.md](./operations.md#storage-quota-not-enforced).
- `502 Bad Gateway` — the sidecar's own response to the client when its finalize or report-part callback to the
  control plane fails (transport error after retries, or the control plane rejects the callback with a 4xx/5xx). This
  is a **retry signal**: the callback failure does not mean the bytes failed to land — see "Data-plane callbacks"
  above (`FS_SIDECAR_FINALIZE_TIMEOUT_SECS`/`FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS`, `post_with_retry`) and the
  `upload`/`upload_multipart_part` handlers' own doc comments in `src/bin/sidecar.rs` for the exact decision table
  between `200`, `409`, and `502` on a replayed `PUT`.

Removed from this table (no corresponding code path found — verified by grepping the gear's `src/` for the status
code and for any `DomainError` variant that could map to it): `422 Unprocessable Entity` (previously claimed for
`expected_hash` mismatch and invalid GTS type — both are actually `400`) and `415 Unsupported Media Type`
(previously claimed for magic-bytes mime mismatch — also actually `400`, via `DomainError::MimeMismatch` →
`invalid_argument`). `507 Insufficient Storage` was already corrected to `429` in a prior doc pass.
