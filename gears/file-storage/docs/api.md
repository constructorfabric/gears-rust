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
- [Response headers (download + HEAD, on the sidecar)](#response-headers-download--head-on-the-sidecar)
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
  - a platform **JWT in `Authorization`** is validated by the sidecar **only** when the token carries a `tok.<claim>`
    predicate (then it matches each claim); absent a predicate, no JWT is required;
  - request-id propagation and per-instance connection/bandwidth limits are the sidecar's own responsibility (it is
    not behind the gateway).

  Because clients never hand-write sidecar URLs (they always receive a ready, opaque signed URL from the control
  plane), the sidecar surface is **outside the generated OpenAPI flow**; its byte-level contract is specified
  normatively in this document. (A standalone OpenAPI document for the sidecar is deferred to P2.)

Encoding conventions:
- Control bodies are `application/json`. The sidecar `PUT` body is the **raw** object bytes (no `multipart/form-data`).
- All error responses follow RFC 7807 (`application/problem+json`).
- `file_id` and `version_id` are UUIDs. A backend object lives at `/{file_id}/{version_id}` and is immutable.

## P1 — Control plane (`/api/file-storage/v1`)

```text
1.  POST   /files                          create file + return a signed upload URL (JSON: metadata; gts_file_type required)
2.  POST   /files/{id}/versions            presign a new-version upload (JSON, optional If-Match) → signed upload URL
3.  POST   /files/{id}/bind                bind/rebind content_id := version_id                          — If-Match
4.  GET    /files/{id}/download-url         issue a signed download URL (pins current content_id, or ?version_id=)
5.  PATCH  /files/{id}                      update custom metadata (JSON Merge Patch)        — If-Match, If-Match-Metadata?
6.  GET    /files/{id}                      file metadata (JSON)                                          — If-None-Match
7.  DELETE /files/{id}                      delete file + all versions                                    — If-Match
8.  GET    /files                           list files (filters, paginated; JSON array of metadata)
9.  GET    /files/{id}/versions             list versions (version_id, size, hash, created_at, is_current)
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
- `POST /files` and `POST /files/{id}/versions` return `{ file_id, version_id, upload_url }` (the control plane
  creates a `pending` `file_versions` row for `version_id` before returning the URL). The client `PUT`s the bytes to
  `upload_url` on the sidecar; the sidecar streams them to the backend, measuring size + SHA-256, then calls the
  control plane's `POST .../versions/{version_id}/finalize` callback (see
  [Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)), which marks the
  version `available`. The sidecar never binds: the client must always follow up with an explicit
  `POST /files/{id}/bind` to swap `content_id := version_id` (see "Upload, bind, and the conflict retry" below).
- `GET /files/{id}/download-url` returns `{ download_url, etag, version_id }`. By default it pins the current
  `content_id`; `?version_id=<v>` pins a specific version.
- Restoring a prior version is `POST /files/{id}/bind` with that `version_id` (a pointer swap, no re-upload).
- `DELETE /files/{id}/versions/{version_id}` cannot delete the file's current version (`409`, "bind another version
  first"); deleting the file's only version instead deletes the whole file.

## P1 — Sidecar (signed-URL authorized)

```text
S1. PUT    <signed upload url>             upload the new version's bytes (raw body)
S2. GET    <signed download url>           download content                                        — If-None-Match, Range
S3. HEAD   <signed download url>           content headers (full-file)                             — If-None-Match
```

The sidecar verifies the signed token and its claims (and a platform JWT only when a `tok.<claim>` predicate is
present) before serving — a valid token is the delegated authorization decision, so there is no request-time PDP
call. On `PUT` it streams bytes to the backend and then calls the control-plane finalize callback, authorized solely
by that same signed upload token (see
[Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated) below) — the P1 sidecar
holds **no** direct DB connection and is a thin, stateless byte-mover (a direct-DB mode is a possible P2+ co-located
optimization; see ADR-0003). The sidecar never binds — see "Upload, bind, and the conflict retry" below.

## Data-plane callbacks (sidecar → control plane, s2s token-authenticated)

These control-plane endpoints are called by the **sidecar**, not by end clients, and are registered `.public()` —
the api-gateway does **not** require an end-user JWT for them. The signed upload/part token (in the `x-fs-token`
request header) is the sole authorization; there is no request-time PDP call. Reflects the current (P2 remediation
0.1, "option 2") implementation: the client-supplied `size`/`hash_hex` are **not** trusted — the control plane reads
the blob back from the backend and recomputes both before persisting anything.

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
  token), `404` (version not found), `409` (already finalized), `500`.
- This endpoint does **not** bind the version as current — `POST /files/{id}/bind` remains a separate, explicit
  client call.

**`D2` report-part** — added in P2 remediation 0.2 group B: called by the sidecar after each successful multipart
part write, closing the gap where nothing previously populated `multipart_upload_parts` in a real (non-test)
deployment.
- **Header**: `x-fs-token: <signed multipart-part token>` (`op` must be `MultipartPart`; `file_id`/`version_id`/
  `upload_id`/`part_number` must match the path).
- **Request body** (`application/json`): `{ "backend_etag": "<string>", "hash_hex": "<64-char lowercase hex>", "size": <i64> }`
  — the backend-assigned ETag for this part plus the part's measured SHA-256 and byte length.
- **Response**: `204 No Content`. Errors: `403`, `404`, `500` (no `400` is currently declared on this route, even
  though a malformed `hash_hex` is rejected via `DomainError::Validation` — not re-verified whether that surfaces
  differently in practice).

## P2 — Multipart upload

> **Implementation status**: **shipped** (server-authoritative flow; see the status note further below). Multipart
> is **server-authoritative**: the client sends desired parameters and the control plane returns the exact parts
> plan (sizes/offsets) with **one signed URL per part** pointing at the sidecar.

```text
P2-1. POST /files/{id}/multipart            initiate (JSON: declared_mime, declared_size, preferred part size, concurrency); returns the parts plan + per-part signed URLs
P2-2. PUT  <signed part url>                upload one part to the sidecar (raw body)
P2-3. POST /files/{id}/multipart/{upload_id}/complete   assemble all reported parts into the final object and mark the version `available`
P2-4. DELETE /files/{id}/multipart/{upload_id}          abort; parts discarded
```

Notes:
- `P2-3` (`complete`) takes **no** `If-Match` and does **not** bind the version as current — like the single-part
  flow, `POST /files/{id}/bind` is a separate, explicit client call. There is also no `400` declared on this route in
  `routes.rs` (only `401`/`403`/`404`/`409`/`500`).
- **`GET /files/{id}/multipart/{upload_id}` (list uploaded parts / introspection) does not exist.** An earlier draft
  of this document listed it; `routes.rs` has no such route. This is
  [`multipart-coordinator.md`](./features/multipart-coordinator.md)'s only remaining unchecked acceptance-criteria
  item (a read-only handler joining `multipart_uploads` + `multipart_upload_parts` by `upload_id` to return the plan
  + received-parts state, for resume). **Not implemented as of this doc pass** — flagged for the team to either fast-
  follow it or formally re-scope it to P3 (see report for this decision request; do not silently leave it unresolved).

**`P2-1` initiate request body** (`application/json`):

| Field | Type | Required | Description |
|---|---|---|---|
| `declared_mime` | `string` | yes | MIME type of the file being uploaded (e.g. `video/mp4`). Validated against the effective allowed-types policy. |
| `declared_size` | `uint64` | yes | Total file size in bytes. The control plane validates this against the effective policy size limit and storage quota at initiate time — exactly like single-part upload does at presign time — so that oversized or quota-exceeding uploads are rejected before any bytes are transferred. `400` if it exceeds the policy size limit; `429` if it would exceed the storage quota. |

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
sidecar **MUST** reject a body whose length ≠ the `size` claim with `413` **before** writing — so per-part size is
enforced at transfer time and oversized bytes never reach the backend. Re-`PUT` of the same part is idempotent
(enables resume). For a `multipart_native` backend the sidecar drives the backend multipart API; otherwise it
offset-writes each part into the single new-version object. Per-part **SHA-256** hashes (not BLAKE3 subtree hashes —
see the implementation-status note below) are reported to the control plane via the `D2` report-part callback (see
[Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)) and persisted in
`multipart_upload_parts.part_hash`; `complete` assembles from the reported parts.

Full request/response envelopes, error taxonomy, token claims, persistence, and resumability are specified in the
FEATURE artifact **[features/multipart-coordinator.md](./features/multipart-coordinator.md)**.

> **Implementation status**: the server-authoritative flow is **shipped**. `POST /files/{id}/multipart` computes the
> parts plan and returns one signed sidecar URL per part (each token carrying `upload_id`, `part_number`, `offset`, and
> the exact `size` claim); the sidecar enforces the per-part size at transfer with `413` **before** any write. The
> initiate-time `declared_size` gate is in place and `declared_size`/`part_size` are persisted on the session row so the
> plan can be reconstituted for resume. The interim client-driven control-plane byte route (`PUT .../parts/{n}`) has
> been **removed** — bytes flow exclusively to the sidecar (ADR-0003, FEATURE §8 migration). The complete-time
> total-size check (assembled size == `declared_size`) remains as the defence-in-depth backstop. Note: per-part hashes
> are **SHA-256** in P2 (not BLAKE3 subtree hashes); BLAKE3 alignment is deferred.

## P2 — Policy engine

Per-tenant and per-user policies (allowed MIME types, size limits, metadata limits, enabled event types). The
**effective** policy for a write is the most-restrictive combination across the applicable levels (tenant ⊕ user).

```text
GET  /policy?scope=<tenant|user>&scope_owner_id=<uuid>   fetch the stored policy for one scope
PUT  /policy                                             upsert (create or replace) the policy for one scope
GET  /policy/effective?user_owner_id=<uuid>              compute the effective (most-restrictive) policy
```

- `GET /policy`: `scope` is required (`"tenant"` or `"user"`); `scope_owner_id` is required when `scope="user"`.
  Returns `204 No Content` (no body) when no policy is configured for that scope — this is a normal, non-error
  outcome, not a `404`.
- `PUT /policy` request body: `{ "scope": "tenant"|"user", "scope_owner_id": "<uuid, omit for tenant>", "body": { "allowed_mime_types": [...], "size_limits": {...}, "metadata_limits": {...}, "enabled_event_types": [...] } }`.
  Response: the stored `PolicyDto` (`200`).
- `GET /policy/effective`: no scope is required to read the caller's own effective policy; `user_owner_id` is an
  optional hint to include a specific user level in the resolution. Response fields are all "effective" (most
  restrictive already resolved): `allowed_mime_types` (`null` = unrestricted), `max_bytes`, `per_mime_max_bytes`,
  `metadata_limits`.
- **There is no `DELETE /policy` route.** To relax a policy, `PUT` a replacement body (e.g. an empty/permissive one);
  there is currently no way to remove a stored policy row entirely via the API.
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
  created `RetentionRuleDto` (`201`).
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
`POST /files/{id}/versions` + upload creates a **new sibling `version_id`** (an unbound version is swept by the P2
cleanup engine, `cpt-cf-file-storage-fr-orphan-reconciliation`). Clients **should** rebind the already-uploaded
`version_id` instead.

## Signed URLs

- **PASETO `v4.public` token, asymmetric, stateless.** The control plane signs with the Ed25519 private key (sole
  minter); the sidecar verifies with the public key and can never mint. **Not JWT** (no `alg` field → no
  algorithm-confusion). No DB lookup to verify. No per-token revocation — emergency revocation is the platform auth
  module's token revocation. P1 uses one static keypair; a `kid` in the PASETO **footer** selects the key in P2
  (rotation). See [ADR-0004](./ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md).
  - **Implementation note (P1):** the shipped P1 codec is an **Ed25519-signed compact token**
    (`base64url(payload).base64url(signature)`) that is **codec-equivalent** to PASETO `v4.public` — same asymmetric
    control-signs/sidecar-verifies property and the same opaque, evolvable claim-set. Because the token is opaque
    (below), the concrete codec is an internal detail of control + sidecar and may move to a literal PASETO library
    without any client-visible change.
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
  carried in `Authorization` — that header always carries the standard platform JWT. `file_id` is the
  URL **path**; `backend_id`/path/size are **not** in the token — the sidecar resolves them from the version row.
- **Claims (inside the token; AND-combined; all optional except `exp` and `op`):**
  | Claim | Req. | Phase | Applies | Violation |
  |---|---|---|---|---|
  | `exp` | yes | P1 | all | `403` (past exp) |
  | `op` (+ method check) | yes | P1 | all | `403` |
  | `ip` (addr/CIDR) | no | P1 | all | `403` |
  | `tok.<claim>` | no | P1 | all (needs JWT) | `403` |
  | `max_size` | no | P1 | upload | `413` |
  | `exact_size` | no | P1 | upload | `400`¹ |
  | `expected_hash` = `<alg>:<hex>` | no | P1 | upload | `400`² |
  | `max_rate` | no | P2 | up/down | throttle |
  | `max_conns` | no | P2 | up/down | `429` |

  ¹ `exact_size` is checked only after the stream fully drains (mismatch → `400`, "size does not match exact_size");
  it can never itself trigger `413` (that's `max_size`'s mid-stream abort, and the two claims are documented as
  mutually exclusive). **Unverified further**: `rg -n "exact_size:" src/` finds the field only in its struct
  definition and in tests — no presign path (`create.rs`/`write.rs`) was found actually setting it on an issued
  token as of this doc pass, so this claim/status pairing may currently be dead code; flagged for the team, not
  fixed here (out of scope for this doc pass).<br>
  ² previously documented as `422`; `bin/sidecar.rs`'s `expected_hash` check returns `(StatusCode::BAD_REQUEST, ...)`
  (`400`), not `422` — no `422` response exists anywhere in this gear.
- **`exp` is mandatory, short by default, and hard-capped.** Every issued URL gets a **short default TTL**
  (`default_url_ttl`, minutes — 15 min in P1) to bound the stale-permission window, and the control plane refuses to
  mint beyond a **hard ceiling** `max_url_ttl` (≤ **7 days**). Both are enforced at signing; the sidecar only checks
  `now ≤ exp`. **Stale-permission trade-off:** authorization is evaluated at signing and there is no per-token
  revocation in P1, so the TTL bounds the exposure window — hence the short default for private content; the 7-day
  ceiling is an explicitly accepted trade-off for low-sensitivity / deliberately long-lived cases (bare query-token
  URLs in particular MUST use a short TTL; durable/anonymous sharing is P3 FileShare). A `tok.<claim>` predicate
  requires a valid platform JWT, which the sidecar validates and matches. "Available to everyone for 5 minutes" =
  only `exp`.
- **`max_size` and `exact_size` are mutually exclusive** (both → `400` at presign / `403` at the sidecar). **Unverified**:
  no enforcement of this was found in `Issuer::issue` (`infra/signed_url/mod.rs`) or elsewhere as of this doc pass —
  see the `exact_size` footnote above; this claim needs re-confirming against the code, not fixed here (out of scope).
- **`expected_hash`** `<alg>` must be in the backend allow-list (P1: `SHA-256`); lowercase hex; baked by the control
  plane (may carry a client-supplied value from the presign request).
- **`max_rate` / `max_conns` are P2** (claim shape from P1; enforcement P2). Scoped to one `(file_id, op)`;
  cross-instance coordination across the sidecar fleet is an open P2 design point.
- **Outside the token:** the `Range` header, conditional headers, and the `PUT` body are not part of the token — so one
  signed URL serves many ranges, and body integrity is enforced by `max_size`/`expected_hash` during the stream + the
  hash at bind.
- **Baked response headers:** the token carries a response-header set the sidecar echoes verbatim (e.g.
  `Content-Disposition`, `Content-Type` override, `Cache-Control`) — no control round-trip.

## Conditional headers

- `If-Match`: required on **bind** (`POST /files/{id}/bind`) and on `DELETE`. Mismatch → `400 Bad Request` on the
  control plane (`FailedPrecondition` collapses to `400` on this platform — see "Status code summary" below). The
  sidecar's data-plane `PUT` does not check `If-Match` at all — it only streams bytes and calls finalize; conditional
  concurrency on content is enforced solely by the control-plane `bind` handler.
- `If-Match-Metadata: <u64>`: **optional** on metadata-only `PATCH`; matched against the current `meta_version` (the
  value published as `X-FS-Metadata-Revision`). Mismatch → `400` (same `FailedPrecondition` → `400` mapping). Absent
  → last-write-wins (back-compatible default); clients keeping meaningful state in custom metadata opt in.
- `If-None-Match`: optional on `GET`/`HEAD` (control metadata and sidecar download); match → `304 Not Modified`.
- ETag is opaque, derived from `(file_id, content_id)`, content-only, and explicitly **not** equal to the content
  hash. It changes exactly when content is (re)bound; a metadata-only `PATCH` does not change it. The content hash is
  exposed separately as `X-FS-Hash-Algorithm` + `X-FS-Hash-Value` (P1: SHA-256, per ADR-0002).

## Range support

Served by the **sidecar**.

- `GET <signed url>` accepts `Range: bytes=<start>-<end>`, `bytes=<start>-`, and `bytes=-<suffix-length>`. A
  well-formed, satisfiable range returns `206 Partial Content` with `Content-Range: bytes <s>-<e>/<n>`. A well-formed
  but **unsatisfiable** range (e.g. `start ≥ size`) returns `416` with `Content-Range: bytes */<n>`.
- A syntactically invalid / unparseable `Range` is **ignored** (RFC 7233 §3.1): `200 OK` with the full body.
- Because `Range` is not part of the signature, **one signed download URL serves many ranges** (random access). Every
  download response includes `Accept-Ranges: bytes`. `HEAD` ignores `Range` and returns full-file headers.
- Multi-range requests are parsed but P1 returns `200 OK` with the full body (no `Content-Range`); `multipart/byteranges`
  may be added later as a backward-compatible upgrade.

## Response headers (download + HEAD, on the sidecar)

```text
ETag: "<opaque>"                                       # (file_id, content_id)-derived
Content-Type: <mime>
Content-Length: <bytes>             # full file on HEAD/200; range bytes on 206
Content-Range: bytes <s>-<e>/<n>    # only on 206
Accept-Ranges: bytes
Last-Modified: <RFC 7231 date>
X-FS-File-Id: <uuid>
X-FS-Version-Id: <uuid>                                # the version being served (current content_id, or pinned version)
X-FS-GTS-File-Type: gts.cf.fstorage.file.type.v1~...
X-FS-Hash-Algorithm: SHA-256                           # of content
X-FS-Hash-Value: <hex>                                 # of content
X-FS-Metadata-Revision: <u64>                          # meta_version; for If-Match-Metadata
X-FS-Owner-Kind: user|app
X-FS-Owner-Id: <uuid>
X-FS-Created-At: <ISO 8601>
<baked response headers echoed verbatim>              # from the token's response-header claims, e.g. Content-Disposition, Cache-Control
X-FS-Meta-<key>: <value>                               # one header per custom metadata key
```

## Status code summary

- `200 OK` — successful control read, metadata `PATCH` with change, bind, presign, or sidecar full download.
- `201 Created` — successful `POST /files` (file created; body carries the upload URL).
- `204 No Content` — successful `DELETE`. The metadata rows (file + all versions) are removed before the best-effort
  backend deletes; re-`DELETE` of an already-deleted `file_id` returns `404` (idempotent).
- `206 Partial Content` — successful range read (sidecar).
- `304 Not Modified` — `If-None-Match` matched the current ETag.
- `400 Bad Request` — malformed request (invalid JSON, missing required fields); an `exact_size` upload whose final
  length is short (sidecar, `PUT`); a content hash mismatch against the `expected_hash` claim (sidecar, `PUT`); a
  malformed token minted at presign (e.g. both `max_size` and `exact_size` claims); the declared file size exceeds
  the effective policy size limit (control plane, `create_file`/`presign_version`/multipart `initiate`); an
  `If-Match`/`If-Match-Metadata` precondition mismatch on control-plane `bind`/`DELETE`/`PATCH` (`FailedPrecondition`
  collapses to `400` on this platform — there is no `412`-mapped canonical-error variant); the finalize callback's
  read-back size/hash/mime not matching the sidecar's claim, or no blob present at the version's backend path at all
  (control plane, `POST .../finalize` — see
  [Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)); or invalid GTS file
  type format (control plane).
- `403 Forbidden` — authorization denied (control), or token verification failed at the sidecar: bad signature,
  expired (`now > exp`), `ip` mismatch, method ≠ the `op` claim, missing/invalid JWT or unmatched token-claim predicate,
  or a malformed (mutually-exclusive) claim set. (The `max_url_ttl` cap is enforced at signing, not re-checked here.)
- `404 Not Found` — file, version, or retention rule does not exist.
- `409 Conflict` — includes, per handler (each via `DomainError::Conflict` → `aborted`):
  - `bind`: the target `version_id`'s upload has not been finalized yet.
  - `delete_version` (`DELETE /files/{id}/versions/{version_id}`): attempting to delete the file's current version
    (bind another version first).
  - `migrate` (`POST /files/{id}/migrate`): the version is not yet finalized, or a concurrent migration to a
    different target already won the race.
  - multipart `complete`/`abort`: the session is not `in_progress` (e.g. completing an already-aborted upload), the
    assembled size does not match `declared_size`, or the pending version row was removed concurrently.
  - `create_file` (idempotent retry): the same `idempotency_key` was reused with a materially different request body.
  - `download_url` (`GET /files/{id}/download-url`): the file has no bound content yet (never bound), or the target
    version's upload has not been finalized. **Not declared** in this route's OpenAPI registration in `routes.rs`
    (only `401`/`403`/`404`/`500` are) even though the domain code returns it — an undocumented-in-OpenAPI real `409`,
    found while auditing conflict cases for this doc pass; flagged for the team alongside the `update_metadata`
    mismatch below.

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
- `429 Too Many Requests` — the `max_conns` claim for this `(file_id, op)` is exceeded (sidecar, P2); or the
  control-plane storage quota would be exceeded on `create_file`/`presign_version`/multipart `initiate`
  (`QuotaExceeded`).

Removed from this table (no corresponding code path found — verified by grepping the gear's `src/` for the status
code and for any `DomainError` variant that could map to it): `422 Unprocessable Entity` (previously claimed for
`expected_hash` mismatch and invalid GTS type — both are actually `400`) and `415 Unsupported Media Type`
(previously claimed for magic-bytes mime mismatch — also actually `400`, via `DomainError::MimeMismatch` →
`invalid_argument`). `507 Insufficient Storage` was already corrected to `429` in a prior doc pass.
