# FileStorage — HTTP API


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
  - a platform **JWT in `Authorization`** would be validated by the sidecar only if the token carried a `tok.<claim>`
    predicate — but `tok.<claim>` (and an `ip`/CIDR constraint) are **not implemented**: `Claims` has no such
    fields, so the sidecar never validates a platform JWT under any circumstance (see "Signed URLs" below);
  - `request_id`/`x-request-id` correlation propagates end-to-end; per-instance connection/bandwidth limits
    (`max_conns`/`max_rate`) are **not implemented** — see "Signed URLs" below.

  Because clients never hand-write sidecar URLs (they always receive a ready, opaque signed URL from the control
  plane), the sidecar surface is **outside the generated OpenAPI flow**; its byte-level contract is specified
  normatively in this document instead.
- **No other surface.** This gear exposes no file-sharing (public/anonymous-link) endpoints and no WebDAV interface;
  every request against it is one of the two planes above.

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
9.  GET    /files/{id}/versions             list versions (version_id, size, hash, hash_mode, part_count?, manifest?, created_at, is_current)
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
- There is **no** `HEAD /files/{id}` route on the control plane.
- `GET /storages` and `GET /storages/{storage_id}` require the caller's `READ` authorization scope (`403`
  otherwise).
- `POST /files` request body (`application/json`, `CreateFileReq`): `{ "owner_kind": "user"|"app", "owner_id":
  "<uuid>", "name": "<string>", "gts_file_type": "<gts uri>", "mime_type": "<string>", "custom_metadata":
  [{"key": "...", "value": "..."}] (optional, default []), "idempotency_key": "<string>" (optional),
  "multipart": { "declared_size": <u64>, "preferred_part_size": <u64>? } (optional),
  "bind": "auto"|"manual" (optional, default "auto") }`. `idempotency_key`
  is the field documented under "Idempotent-create semantics" (`operations.md`) — see the `409` cause in "Status code
  summary" for what happens on a reused key with a different body; it is rejected (`400`) together with `multipart`.
  Creating a file with `(owner_kind, owner_id)` equal to the caller's own kind and subject id proceeds under the
  ordinary `WRITE` grant; any other `(owner_kind, owner_id)` pair additionally requires the caller's `ADMIN_POLICY`
  authorization scope (`403` otherwise) — plain `WRITE` must not let any tenant member create files on another
  subject's behalf. The comparison is on the *pair*, not `owner_id` alone: `user` and `app` are disjoint owner
  spaces that can legitimately share the same UUID, so a caller could otherwise pass their own id under
  `owner_kind: "app"` and have the self-service check pass while actually authoring the file into the other
  owner space (a different effective policy and a different owner's quota/listing than their own).
- **Multipart plan inline in `POST /files`.** With the `multipart` block and a server plan of **≥2 parts**, the `201` response carries
  the full parts plan instead of a single-part URL: `{ file_id, version_id, multipart: { upload_id, version_id,
  part_hash_algorithm, part_size, parts: [{part_number, offset, size, upload_url}], expires_at } }` — no separate
  `POST /files/{id}/multipart` call, and no orphan single-part pending version. Resume is unchanged:
  `GET /files/{id}/multipart/{upload_id}` (the `upload_id` now arrives from `POST /files`). A plan that collapses to
  one part falls back to the single-part `upload_url` path below. `bind: "auto"` (default) makes the upload itself
  bind the first content — see the `X-FS-Bound` contract and the `complete` `bind_state` field below — for a total of
  **2 requests** single-part and **N+2** multipart; `bind: "manual"` keeps the staged flow (explicit `bind`, see
  "Upload, bind, and the conflict retry" below).
- `POST /files/{id}/versions` takes **no** request body and does **not** read `If-Match`.
- `GET /files` **requires** both `owner_kind` and `owner_id` query params (`400` if either is missing/invalid). A
  caller listing with `(owner_kind, owner_id)` equal to the caller's own kind and subject id proceeds under the
  ordinary `READ` grant; **any other pair** additionally requires the caller's `ADMIN_POLICY` authorization scope
  (`403` otherwise) — this closes an enumeration vector where any tenant member could otherwise list an arbitrary
  other subject's files via `?owner_kind=user&owner_id=<victim>`. The comparison is on the *pair*, not `owner_id`
  alone, for the same reason `POST /files` checks it that way (see above): `user` and `app` are disjoint owner
  spaces, so a caller could otherwise pass their own id under the *other* `owner_kind` and have the self-service
  check pass while actually listing the other owner space's files. Each returned item's `custom_metadata` is real,
  batch-fetched per page (one `IN (...)` query), not an always-empty placeholder. Results are ordered
  `created_at DESC` with `file_id DESC` as a deterministic tie-breaker — `created_at` has only millisecond
  resolution, so rows can share an instant, and without the tie-breaker an offset-paginated page could skip or
  repeat a row at that boundary.
- Both listing endpoints (`GET /files`, `GET /files/{id}/versions`) page via `?limit`/`?offset`. The effective
  limit on every request is `min(requested-or-default, configured max_page_size)`: `default_page_size` (50 by
  default) is used when `?limit` is omitted, and `max_page_size` (1000 by default) is a **hard ceiling** —
  `MAX_PAGE_SIZE_CEILING` = 1000 — that a configured `max_page_size` can never exceed regardless of what an
  operator sets, enforced at config-validation time (gear init fails if `max_page_size` is configured above it),
  independent of and in addition to the ordinary `default_page_size ≤ max_page_size` sanity check. A caller can
  therefore never receive more than the configured `max_page_size` items in one page, and that configured value
  itself can never exceed 1000.
- `POST /files` and `POST /files/{id}/versions` return `{ file_id, version_id, upload_url }` (the control plane
  creates a `pending` `file_versions` row for `version_id` before returning the URL). The client `PUT`s the bytes to
  `upload_url` on the sidecar; the sidecar streams them to the backend, measuring size + SHA-256, then calls the
  control plane's `POST .../versions/{version_id}/finalize` callback (see
  [Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)), which marks the
  version `available`.
- **Single-part bind outcome headers.** For a file created with `bind: "auto"` (the default),
  the finalize also binds the first content under a strict `content_id IS NULL` CAS, and the sidecar's `200` `PUT`
  response forwards the outcome transparently as fixed headers — the single-part half of the ONE bind-state model it
  shares with `complete`'s `bind_state`: `X-FS-Bound: true` + `ETag: <new content etag>` (bound), or
  `X-FS-Bound: conflict` + `X-FS-Current-ETag: <current etag>` (CAS lost — e.g. two create-tokens on one new file;
  the version is `available`, resolve with a manual `bind` using that ETag as `If-Match`, no re-upload). No headers
  for `bind: "manual"` uploads. An honest `PUT` retry (lost response) is idempotent: `publish_exclusive` is
  replay-safe and finalize converges an already-`available` version with matching size/hash to the same headers —
  never a 409 — **regardless of the upload's bind mode**: the sidecar publishes every single-part upload through the
  same replay-safe path whether or not the token carries the auto-bind claim, so a `bind: "manual"` retry converges
  exactly like an auto-bind one, simply with no `X-FS-Bound`/`ETag` headers to report (as on the first call). An
  auto-bind retry that WON the CAS on its original call replays that exact `X-FS-Bound: true` + `ETag` outcome, even
  if the file's content has legitimately moved on to a different version since — never the current pointer's
  `conflict`, which would contradict what the original call already returned. With
  `bind: "manual"` (or `POST /files/{id}/versions`, which never auto-binds), the client follows up
  with an explicit `POST /files/{id}/bind` (see "Upload, bind, and the conflict retry" below).
- `GET /files/{id}/versions` returns a JSON array of version objects, ordered `created_at DESC` with `version_id
  DESC` as a deterministic tie-breaker — same reasoning as `GET /files` above. Each carries
  `{ version_id, mime_type, size, hash_algorithm, hash, hash_mode, part_count?, manifest?, status, is_current, created_at }`
  (ADR-0006). `hash` is lowercase-hex; `hash_algorithm` is always `"SHA-256"`. `hash_mode` is `"whole-sha256"` (then
  `hash` = `sha256(object bytes)` and `part_count`/`manifest` are omitted) or `"multipart-composite-sha256"` (then
  `hash` = `sha256(manifest)`, the offset-manifest composite root, `part_count` is the number of parts, and
  `manifest` is the canonical offset-manifest wire text — the same string the multipart `complete` response carries,
  re-served from the stored `version_hash_manifest` row so a client can re-verify `hash` for an already-existing
  version, per [content-hash-modes.md](./features/content-hash-modes.md) §"Client-Side Manifest Re-Verification").
  Note that a multipart upload whose plan had exactly **one part** finalizes as `whole-sha256` (ADR-0006 single-part
  amendment), so it too carries no `part_count`/`manifest` here. `?limit` itself is **not** additionally capped for
  this — a page of ordinary `whole-sha256` versions is sized exactly like any other listing (`?limit` clamped only
  to `max_page_size`). Instead, the manifests actually attached to a page are bounded by a 4 MiB aggregate budget:
  if attaching the next `multipart-composite-sha256` version's manifest, in page order, would push the running
  total over budget, the page is cut short right before that version (the version already-included even if its own
  single manifest exceeds the budget alone, so the listing always makes forward progress). Such a page can
  therefore come back shorter than `?limit` with more versions still to list. There is no `has_more`/`next_offset`
  field for this — as with any other short page, the client resumes at `offset + <versions actually received>`.
- `GET /files/{id}/download-url` returns `{ download_url, etag, version_id }`. By default it pins the current
  `content_id`; `?version_id=<v>` pins a specific version.
- Restoring a prior version is `POST /files/{id}/bind` with that `version_id` (a pointer swap, no re-upload).
- `DELETE /files/{id}/versions/{version_id}` cannot delete the file's current version (`409`, "bind another version
  first"); deleting the file's only version instead deletes the whole file.

## P1 — Sidecar (signed-URL authorized)

```text
S1. PUT    <signed upload url>             upload the new version's bytes (raw body)
S2. GET    <signed download url>           download content                                        — Range
S3. HEAD   <signed download url>           same auth/`404` contract as GET, no body
```

`HEAD` is its own route rather than falling through to axum's default GET-derived `HEAD` handling — the latter would
run the full `download` handler, including streaming the entire object off the backend, only to discard the body
afterwards. It resolves existence and size in a single combined backend round trip (`stat(2)` on `local-fs`,
`HeadObject` on `S3Backend`) and returns the same `Accept-Ranges`/`Content-Type`/`ETag` headers as `download`'s
`200`, plus an explicit `Content-Length` (a real `200`/`206` gets this for free from its body; `HEAD` has no body to
derive it from). Nothing stored at the path → `404` (same as `GET`'s missing-blob case); a `stat` failure distinct from
"not found" is `503` + `Retry-After: 5` when the backend reports it as transient, `500` otherwise (same
`BackendUnavailable`/`Backend` split described under "Status code summary" below).

**Planned / not implemented**: `If-None-Match` → `304` support on the sidecar `GET`/`HEAD`. Every download token is
already single-use-scoped to one `(file_id, version_id)`, so the bandwidth win of a conditional download is small.
(`If-None-Match` → `304` **is** implemented on the control plane's `GET /files/{id}`, which is a distinct surface —
see "Conditional headers" below.)

**A transient read race.** Both `GET` and `HEAD` resolve the object's size with one `stat`/`HeadObject` call before
streaming or measuring it. If the backend object's size disagrees with that already-resolved length by the time the
actual read (full or ranged) reaches the backend, the mismatch is always caught — but *when* depends on whether the
backend can learn the object's real length before handing back any bytes:

- **Local-fs, and S3 when the `GetObject`/ranged response carries a `Content-Length` header:** the backend re-stats
  (local-fs) or reads that header (S3) before streaming a single byte, and refuses **before the first byte is
  streamed**: `503 Service Unavailable` with `Retry-After: 1` and a short text body (`"object changed during read,
  retry"`). This is a transient-race signal, not a fault — logged at `warn`, not `error` — and the recovery is a
  plain retry.
- **S3 when the response has no `Content-Length` at all** (a chunked-transfer-encoded response — the S3-compatible
  store did not, or could not, declare a length up front): there is no length to check before streaming starts, so
  the sidecar has already sent its own response headers (status, the `Content-Length` it committed to from the
  earlier `stat`) by the time a length guard wrapping the stream catches the disagreement mid-transfer. The client
  does **not** see a `503` in this case — the connection ends with a body shorter (or, for a chunk that would have
  overrun the promised length, capped at) than the `Content-Length` already sent, i.e. a truncated/incomplete
  response, the same way any other server-side stream failure partway through a response looks to an HTTP client.

Any other backend read failure (not a length mismatch) is classified by the same `BackendUnavailable`/`Backend`
split as everywhere else in this gear: a transient fault (network, timeout, backend overload) is `503 Service
Unavailable` with `Retry-After: 5` (`"backend temporarily unavailable, retry"`), logged at `warn`; a permanent one
(bad config, a protocol violation by the backend, an internal invariant) stays `500`, logged at `error`.

The sidecar verifies the signed token and its claims before serving — a valid token is the delegated authorization
decision, so there is no request-time PDP call and no platform-JWT check of any kind (the `tok.<claim>` predicate
described above is not implemented). On `PUT` it streams bytes to the backend and then calls the control-plane
finalize callback, authorized solely by that same signed upload token (see
[Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated) below) — the sidecar
holds **no** direct DB connection and is a thin, stateless byte-mover (a direct-DB mode is a possible future
co-located optimization; see ADR-0003). The sidecar never binds — see "Upload, bind, and the conflict retry" below.

## Data-plane callbacks (sidecar → control plane, s2s token-authenticated)

These control-plane endpoints are called by the **sidecar**, not by end clients, and are registered `.public()` —
the api-gateway does **not** require an end-user JWT for them. The signed upload/part token (in the `x-fs-token`
request header) is the sole authorization; there is no request-time PDP call. The client-supplied `size`/`hash_hex`
are not trusted: the control plane reads the blob back from the backend and recomputes both before persisting
anything.

**Internal credential.** The `fs-token` is client-visible (returned in plaintext inside `upload_url`), so on its own
it does not prove the caller is the sidecar rather than the uploading client itself. Both routes below optionally
require a second factor: when `FileStorageConfig::finalize_internal_secret` is configured, the request must also
carry a matching `x-fs-internal-token` header (constant-time-compared; `403` on missing/mismatch), checked *after*
`fs-token` verification. `None` (the default) preserves the token-only behavior above; see
`docs/ADR/0003-…-sidecar-data-plane.md`'s trust-model section for the mechanism (interim gear-local shared secret)
and the required rollout order (`FS_SIDECAR_INTERNAL_TOKEN` must reach every sidecar before
`require_finalize_internal_secret` is flipped `true`). With this second factor configured, a client-visible
`fs-token` alone can no longer drive these two routes.

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
  above), `404` (version not found), `409` (already finalized), `500` (a permanent backend fault reading the blob
  back), `503` (a transient one — network, timeout, a dropped connection partway through the read-back, backend
  overload — carries `Retry-After`).
- For a `bind: "manual"` upload (and `POST /files/{id}/versions`, which never auto-binds) this endpoint does **not**
  bind the version as current — `POST /files/{id}/bind` remains a separate, explicit client call. For the default
  `bind: "auto"` it binds inline under the same CAS and reports the outcome instead — see "Single-part bind outcome
  headers" above.

**`D2` report-part** — called by the sidecar after each successful multipart part write; this callback is what
populates `multipart_upload_parts`, the table `complete` assembles from.
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

Multipart is **server-authoritative**: the client sends desired parameters and the control plane returns the exact
parts plan (sizes/offsets) with **one signed URL per part** pointing at the sidecar.

```text
P2-1. POST /files/{id}/multipart            initiate (JSON: declared_mime, declared_size, preferred part size); returns the parts plan + per-part signed URLs
P2-2. PUT  <signed part url>                upload one part to the sidecar (raw body)
P2-3. POST /files/{id}/multipart/{upload_id}/complete   assemble all reported parts into the final object, mark the version `available`, and return version/size/composite-hash
P2-4. DELETE /files/{id}/multipart/{upload_id}          abort; parts discarded
P2-5. GET /files/{id}/multipart/{upload_id}             introspect/resume; returns state + received/missing parts, with fresh resume URLs for missing parts of a live session
```

Notes:
- `P2-1` (`initiate`) always targets the backend registry's **default** backend (`FileStorageConfig::default_backend_id`,
  or `local-fs` if unset — see `operations.md`); there is no per-request target-backend choice, unlike `migrate`. That
  backend must advertise the `multipart_native` capability, and `local-fs` does **not** — so multipart initiate
  against the plain default configuration is rejected with `400` (`MULTIPART_NOT_SUPPORTED`). A configured S3 backend
  or the dev/test `memory` backend both advertise `multipart_native: true`, so multipart becomes available once one
  of them is made the default (`default_backend_id`) or the only backend.
- For a `bind: "manual"` session (and a standalone `POST /files/{id}/multipart` initiate), `P2-3` (`complete`) does
  **not** bind the version as current — like the single-part manual flow, `POST /files/{id}/bind` is a separate,
  explicit client call. For the default `bind: "auto"` it binds inline and reports `bind_state` — see "Bind inside
  complete" below. It takes an **optional** `If-Match` header: a concrete value is checked
  against the file's current content ETag (`400` on mismatch — `FailedPrecondition` collapses to `400` on this
  platform); `*` or an absent header is unconditional. The route declares `401`/`403`/`404`/`409`/`400`/`500`/`503`
  (the winning completer's assembly calls `StorageBackend::complete_multipart`; a transient backend fault surfaces
  as `503` + `Retry-After`, distinct from the `202`/`Retry-After` lease-poll signal above).
- `P2-3` (`complete`) returns **`200`** with a JSON body — see the response shape below.
- `P2-5` (`GET .../multipart/{upload_id}`, introspect/resume) is authorized on `write` (like initiate/complete/abort,
  not `read`) since it hands out live resume upload URLs. A foreign or missing `upload_id` is masked as `404`,
  identical to `complete`'s guard. The route declares `401`/`403`/`404`/`500`. See the response shape below.
- There is no control-plane route for uploading individual parts (e.g. a `PUT /files/{id}/multipart/{upload_id}/parts/{n}`
  against the control plane); bytes flow exclusively to the sidecar via the per-part signed URLs in the initiate
  response (ADR-0003, "no bytes through the control plane").

**`P2-1` initiate request body** (`application/json`):

| Field | Type | Required | Description |
|---|---|---|---|
| `declared_mime` | `string` | yes | MIME type of the file being uploaded (e.g. `video/mp4`). Validated against the effective allowed-types policy. |
| `declared_size` | `uint64` | yes | Total file size in bytes. The control plane validates this against the effective policy size limit and storage quota at initiate time — exactly like single-part upload does at presign time — so that oversized or quota-exceeding uploads are rejected before any bytes are transferred. `400` if it exceeds the policy size limit; `429` if it would exceed the storage quota. The `429` quota path only fires when a `QuotaClient` is configured; none is wired in any deployment (`gear.rs`'s `quota_client: None`), so callers do not currently observe quota rejections — see [operations.md](./operations.md#storage-quota-not-enforced). |
| `preferred_part_size` | `uint64` | no | Client hint for the part size in bytes; the server may widen it (see the `MAX_PART_COUNT` note below) or otherwise adjust it to satisfy backend minimums. Rejected with `400` if outside `[DEFAULT_MIN_PART_SIZE (5 MiB), MAX_PART_SIZE (5 GiB)]`. |

The server-computed parts plan is capped at `MAX_PART_COUNT = 10_000` parts. If
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
  part body is streamed straight into the backend's native `UploadPart` call — never buffered whole — with the
  declared `size` sent as the request's exact `Content-Length`; an undersized body simply fails that PUT (S3 never
  receives a complete request), so nothing was ever stored and nothing needs cleanup. For a non-native backend (the
  `local-fs`-style offset-object model), the part *was* already written to its own backend object
  (`{backend_path}.part.{n}`) as bytes streamed in, so the sidecar explicitly **deletes that partial object**
  before returning `400`, rather than leaving a mismatched part object behind.

Re-`PUT` of the same part is idempotent (enables resume — a fresh `PUT` simply overwrites/re-streams). For a
`multipart_native` backend the sidecar drives the backend multipart API; the sidecar's write path also has a
non-native, `local-fs`-style offset-object fallback, though it is not reachable through the real initiate flow today
since initiate rejects any default backend that isn't `multipart_native` (see the Notes above). Per-part **SHA-256**
hashes are reported to the control plane via the `D2` report-part callback (see
[Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)) and persisted in
`multipart_upload_parts.part_hash`; `complete` assembles from the reported parts. The sidecar's `200` response body
for a successful part `PUT` is `{ "part_number": <u32>, "etag": "<backend-assigned or hash-derived string>",
"hash_algorithm": "SHA-256", "hash": "<64-char lowercase hex>" }`.

**`P2-3` complete response** (`application/json`, `200`):

```json
{
  "version_id": "uuid",
  "size": 10485763,
  "hash_algorithm": "SHA-256",
  "content_hash": "<64-char lowercase hex — sha256(manifest), ADR-0006 composite root>",
  "hash_mode": "multipart-composite-sha256",
  "part_count": 2,
  "manifest": "v1,0:<64-hex>,8388608:<64-hex>",
  "bind_state": "bound",
  "etag": "\"<content etag — bind_state == bound only>\""
}
```

**Bind inside complete.** `bind_state` is the multipart half of the shared bind-state model
(single-part: the `X-FS-Bound` header): `"bound"` — an `auto_bind` session (opened by `POST /files` with
`bind: "auto"`) was bound here, in the same transaction as the finalize, under the endpoint's `If-Match` CAS
(first content → `content_id IS NULL`), with the new content `etag`; `"conflict"` — the CAS lost, the response
carries `current_etag` instead (manual rebind, no re-upload); `"manual"` — a `bind: "manual"` or standalone-initiate
session, client binds explicitly. `complete` is **idempotent** (a retry — including after a page reload — replays the
persisted result verbatim, "already bound" included) and is serialized by a completion **lease state machine**
(`in_progress → completing(lease) → completed(result)`; instant conditional UPDATEs, no DB transaction across backend
I/O): while another caller holds a live lease the response is `202 Accepted`
`{ "state": "completing", "retry_after_secs": 2 }` (+`Retry-After`) — poll by re-issuing the same `complete`. A
completer that crashes mid-assembly leaves `completing` behind; after `lease_until` (config
`multipart_complete_lease_secs`, default 120s) the next `complete` takes the lease over and finishes (re-using the
already-assembled object where possible). Sessions stuck in `completing` past `expires_at` (with an expired lease)
are backstopped by the cleanup engine's abandoned-session sweep. The complete per-state failure matrix and race
catalog for both upload paths is [concurrency-and-failure-model.md](./concurrency-and-failure-model.md).

**Wire-spelling stability (`bind_state` / `state`).** `bind_state` (`"bound"`/`"conflict"`/`"manual"`) and the
multipart session's `state` field (`"in_progress"`/`"completing"`/`"completed"`/`"aborted"`) are stable string
values, not integers to be renumbered — the spellings above are exhaustive today, but a future release **may add** a
new value to either set as a backward-compatible, additive change (removing or repurposing an existing value would
instead be breaking and would require a new API version). A client **MUST** treat any `bind_state`/`state` value it
does not recognize as "not yet resolved, re-check" — re-`GET`/re-`complete` and read the fresh response — rather
than as an error, the same posture already required for a recognized `"completing"`. The single-part `X-FS-Bound`
header carries the same three-value bind-state model (spelled `true`/`conflict`, with no header at all standing in
for `manual`) and is covered by the same stability contract.

**One-part plans degenerate to `whole-sha256`** (ADR-0006 single-part amendment): when the plan had exactly one
part, `hash_mode` is `"whole-sha256"`, `content_hash` is plain `sha256(object bytes)` (identical to the single
part's digest), and `manifest` is **omitted** — there is no composite and no stored manifest row; the version then
also reports `whole-sha256` with no `part_count`/`manifest` in `GET /files/{id}/versions`. `part_count` in this
response still reports `1` (the plan's factual part count).

Before `complete` assembles anything it diffs the plan's expected part numbers (`ceil(declared_size / part_size)`)
against the parts actually reported; a non-empty diff is rejected with `409` and the missing part numbers in the
error detail, **before** ever calling the backend's native multipart completion — giving a caller debugging a
stalled upload an actionable list instead of an opaque total-assembled-size mismatch. `manifest` lets a client
independently re-verify the composite hash (see [content-hash-modes.md](./features/content-hash-modes.md)
§"Client-Side Manifest Re-Verification") without a second round-trip. The same manifest text is also re-served
later by `GET /files/{id}/versions` (the `manifest` field on `multipart-composite-sha256` versions — see
"P1 — Control plane" above), so a client that discarded the `complete` response can still re-verify.

**`P2-5` introspect response** (`application/json`, `200`):

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

## P2 — Policy engine

Per-tenant and per-user policies (allowed MIME types, size limits, metadata limits, enabled event types). The
**effective** policy for a write is the most-restrictive combination across the applicable levels (tenant ⊕ user).

```text
GET  /policy?scope=<tenant|user>&scope_owner_id=<uuid>   fetch the stored policy for one scope
PUT  /policy                                             upsert (create or replace) the policy for one scope
GET  /policy/effective?user_owner_id=<uuid>              compute the effective (most-restrictive) policy
```

- `GET /policy`: `scope` is required (`"tenant"` or `"user"`); `scope_owner_id` is required when `scope="user"` —
  omitting it is rejected with `400`, not silently treated as tenant scope.
  Returns `204 No Content` (no body) when no policy is configured for that scope — this is a normal, non-error
  outcome, not a `404`.
- `PUT /policy` request body: `{ "scope": "tenant"|"user", "scope_owner_id": "<uuid, omit for tenant>", "body": { "allowed_mime_types": [...], "size_limits": {...}, "metadata_limits": {...}, "enabled_event_types": [...] } }`.
  Response: the stored `PolicyDto` (`200`).
- `GET /policy/effective`: no scope is required to read the caller's own effective policy; `user_owner_id` includes
  a specific user level in the resolution, but it is not a free-form hint — passing any id other than the caller's
  own subject id additionally requires the caller's `ADMIN_POLICY` authorization scope (`403` otherwise), since it
  would otherwise let any tenant member read another subject's user-level policy. Response fields are all
  "effective" (most restrictive already resolved): `allowed_mime_types` (`null` = unrestricted), `max_bytes`,
  `per_mime_max_bytes`, `metadata_limits`.
- `enabled_event_types` (inside the policy body) is stored and returned but does not currently gate anything: file
  events are enqueued into `events_outbox` unconditionally, independent of this field's configured value.
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
  created `RetentionRuleDto` (`201`). Semantic validation rejects with
  `400`: a body with **all three** of `age`/`inactivity`/`metadata` absent (a rule that could never match any file);
  `age.max_age_days` or `inactivity.inactivity_days` **less than 1** (either would match every file in the tenant on
  the very next sweep tick); and `scope` ∈ `{user, file}` with `scope_target_id` omitted.
- `GET /retention-rules` (no scope filter query param) returns every rule in the caller's tenant, across every
  scope, only when the caller holds `ADMIN_POLICY`. A non-admin caller instead gets a filtered view: all
  `tenant`-scope rules, `user`-scope rules that target themselves, and `file`-scope rules whose target file they
  own (compared as the `(owner_kind, owner_id)` pair, not `owner_id` alone) — the underlying store query has no
  owner/target filter, so without this filtering step any tenant member could otherwise enumerate every other
  member's retention configuration. A `file`-scope rule is deleted together with its target file (there is no
  FK/cascade at the DB level, so the delete path removes it explicitly), so it can no longer outlive the file and go
  invisible that way. One remaining caveat on the `file`-scope case: a rule created via delegated `WRITE` on a file
  the creator does not own stays invisible to that creator, since visibility is gated on file *ownership*, not on
  having created the rule.
- `POST /retention-rules` with `scope="tenant"` requires the caller's `ADMIN_POLICY` authorization scope, with no
  fallback to `WRITE` — a tenant-scope rule is a standing instruction for the background sweep to permanently
  delete every matching file for every subject in the tenant, so ordinary file-`WRITE` is not enough.
- `DELETE /retention-rules/{rule_id}` → `204`, or `404` if the rule does not exist.

## P2 — Backend migration

```text
POST /files/{id}/migrate   { "target_backend_id": "<string>" }   → 204
```

Migrates a file's content to a different configured storage backend, preserving the file's identity (`file_id`
unchanged). **Non-versioned files only** — a file with more than one `file_versions` row is rejected
(`VersionedFileMigrationNotSupported`, `409`). The version must already be `available` (`409` otherwise). The
content streams straight from the source backend into the destination, with its hash re-verified incrementally on
the same pass; the verdict is only known once the destination has received the whole stream, and the version's
backend pointer is only ever repointed (CAS) once that verification has passed — see
[backend-migration.md](./features/backend-migration.md) for the full failure/cleanup contract. Migrating
onto a non-durable backend (e.g. a dev/test `memory` backend) additionally requires the caller's `ADMIN_POLICY`
authorization scope, not just `WRITE`, since it risks silent data loss on the next restart.

## P2 — Ownership transfer

```text
POST /files/{id}/transfer   { "new_owner_kind": "user"|"app", "new_owner_id": "<uuid>" }   → 200, FileDto
```

Atomically replaces the file's `owner_kind` + `owner_id`, records an audit row (`TransferOwnership`), and enqueues a
`file.owner_transferred` event in the same transaction. Authorized on the file's ordinary `WRITE` grant, not
`ADMIN_POLICY` — this gear has no principal directory, so it cannot verify `new_owner_id` names a real, same-tenant
principal, only that it is not the nil UUID (`400` otherwise); a cross-tenant transfer is structurally impossible,
since the updated row's `tenant_id` always comes from the existing file, never the request. If the file is deleted
concurrently between the caller's read and the atomic ownership update, the update matches no row and the response
is `404` — identical to transferring a `file_id` that never existed.

## Upload, bind, and the conflict retry

Content is an immutable blob per version; a file's live content is the `content_id` pointer, swapped under optimistic
CAS. This section spells out the **`bind: "manual"`** flow (and `POST /files/{id}/versions`, which never auto-binds):
presign (control) → `PUT` (data) → finalize (data-plane callback) → bind (control) — three control-plane touches and
one data-plane touch, the last control touch an explicit, separate client call. **`bind: "auto"` (the default for
`POST /files`) skips step 4**: finalize itself swaps `content_id` inline, under the same CAS, and reports the
outcome via the `X-FS-Bound` header (single-part) / `complete`'s `bind_state` field (multipart) instead of requiring
a separate `bind` call — see "Single-part bind outcome headers" and "Bind inside complete" above (§P1 control plane
/ §P2 multipart upload). Everything below — the conflict, the retry, and the "don't re-presign" guidance — applies
identically to a manual bind and to an auto-bind whose CAS lost and reported `bind_state: "conflict"`.

1. **Presign**: `POST /files` (or `POST /files/{id}/versions`) → `{ file_id, version_id, upload_url }`. The control
   plane creates a `pending` `file_versions` row for `version_id` before returning the signed `upload_url`.
2. **Upload**: `PUT upload_url` to the sidecar (raw body). The sidecar streams the bytes to the backend, measuring
   size + SHA-256 as they land. It does not check `If-Match` and does not bind — it only moves bytes.
3. **Finalize**: once the `PUT` completes, the sidecar calls the control plane's token-authenticated
   `POST /files/{id}/versions/{version_id}/finalize` callback (see
   [Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)). The control plane
   reads the blob back, verifies size + SHA-256, and flips the version `pending → available`. For a `bind: "manual"`
   upload this step never touches `content_id`; for `bind: "auto"` it also performs the pointer swap inline, in the
   same transaction, under CAS (see above) — there is no separate step 4 in that case.
4. **Bind** (`bind: "manual"` only, or a rebind after an auto-bind's CAS lost): the client calls
   `POST /files/{id}/bind { version_id }` with `If-Match: "<current content
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
  PASETO.** ADR-0004 specifies PASETO `v4.public`; the wire format is `base64url(JSON payload).base64url(signature)`
  (`infra::signed_url::Issuer`/`Verifier`) — same asymmetric control-signs/sidecar-verifies property and the same
  opaque, evolvable claim-set, but **no PASETO footer and no `kid`** — key rotation is instead handled by the
  sidecar verifying against a small ordered set of public keys (active + previously-active,
  `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`), so no `kid` is needed to select one; see `docs/operations.md`'s
  `signing_key_seed` → Rotation section for the zero-outage procedure. The
  control plane signs with the Ed25519 private key (sole minter); the sidecar verifies with the public key and can
  never mint. **Not JWT** (no `alg` field → no algorithm-confusion). No DB lookup to verify. No per-token
  revocation — emergency revocation is the platform auth module's token revocation. See
  [ADR-0004](./ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md).
  - **Implementation note:** because the token is opaque (below), the concrete codec is an internal detail of
    control + sidecar and may move to a literal PASETO library (with a real footer/`kid`) later without any
    client-visible change.
  - **FIPS posture:** the sign/verify primitive sits behind an in-house `SignatureProvider` abstraction specifically
    so a FIPS-validated module or algorithm can replace it without a codec change; see
    [DESIGN.md](./DESIGN.md) §4.8 and ADR-0004 "FIPS posture" for the detail.
- **Opaque to everyone but control + sidecar.** The token's claim-set and crypto are private to the minter and verifier;
  every other participant (browser, CDN, proxy, app, logs, SDK transport) MUST treat it as **opaque bytes** and never
  parse it — the format can and will change ("Token Opacity Contract").
- **Two carriers, same bytes:** the `fs-token` **query** parameter (`?fs-token=<token>`, bare embeddable URL) **or** the
  `X-FS-Token` **header** (programmatic / batch — credential out of the URL, stable cacheable URL). Both may be present
  on the same request: if they agree, either serves; if they **disagree**, the sidecar treats this as a malformed
  request (a caller or intermediary that attached two different credentials), rejecting with `400` before either
  value is ever handed to the verifier — not a query-wins/header-wins ambiguity resolved silently one way. The token
  is **never**
  carried in `Authorization` — that header always carries the standard platform JWT. `file_id` is **also** the URL
  **path**. **`backend_id` and `backend_path` ARE carried in the token** (`Claims::backend_id`/`backend_path`,
  `infra/signed_url/mod.rs`) — this is deliberate, not an oversight: the sidecar has **no DB connection at all** (see
  "Response headers" below and ADR-0003), so it cannot resolve them any other way. The sidecar resolves the object
  purely from the verified claims, never from a "version row" lookup.
- **Claims (inside the token; AND-combined; all optional except `exp` and `op`):**
  | Claim | Req. | Enforced? | Applies | Violation |
  |---|---|---|---|---|
  | `exp` | yes | yes | all | `403` (at/past `exp`, exclusive boundary) |
  | `op` (+ method check) | yes | yes | all | `403` |
  | `backend_id` / `backend_path` | yes | yes | all | n/a — used to resolve the object, no DB lookup |
  | `ip` (addr/CIDR) | no | **not implemented (planned)** | all | — |
  | `tok.<claim>` | no | **not implemented (planned; would need JWT)** | all | — |
  | `max_size` | no | yes | upload | `413` |
  | `exact_size` | no | yes | upload | `400`¹ |
  | `expected_hash` = `<alg>:<hex>` | no | yes | upload | `400`² |
  | `max_rate` | no | **not implemented (planned)** | up/down | — |
  | `max_conns` | no | **not implemented (planned)** | up/down | — |
  | `content_type` | no | yes | download (`op = get`) | n/a — echoed as `Content-Type`³ |
  | `etag` | no | yes | download (`op = get`) | n/a — echoed as `ETag`³ |

  `Claims` has no `ip`, `tok.<claim>`, `max_rate`, or `max_conns` field (`infra/signed_url/mod.rs`). The sidecar
  validates only: the token's signature and expiry (`exp`); `op` against the HTTP method; the `file_id`/`version_id`
  (and, for a multipart part, `upload_id`/`part_number`) binding against the request path; and the upload size/hash
  constraints (`max_size`/`exact_size`/`expected_hash`) below. It performs no client-address check, no platform-JWT
  validation of any kind, and no rate/connection limiting.

  ¹ `exact_size` is checked only after the stream fully drains (mismatch → `400`, "size does not match exact_size");
  it can never itself trigger `413` (that's `max_size`'s mid-stream abort, and the two claims are mutually
  exclusive). No presign path (`create.rs`/`write.rs`) currently sets this claim on an issued token, so `exact_size`
  enforcement exists in the verifier but is not currently exercised by any control-plane code path.<br>
  ² the sidecar's `expected_hash` check always returns `400` — no `422` response exists anywhere in this gear.<br>
  ³ these two claims are populated only on a **download** (`op = get`) token, at `download-url` issuance time — the
  control plane reads `version.mime_type` and computes the content ETag once and
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
  later). Authorization is evaluated once, at signing, with no per-token revocation, so the TTL is the only bound on
  a stale-permission window — see [DESIGN.md](./DESIGN.md) §4.5 "Stale-permission window" for the full trade-off;
  bare query-token URLs in particular MUST use a short TTL (durable/anonymous sharing is a separate, not-yet-built
  FileShare gear — see "Two planes" above). "Available to everyone for 5 minutes" = only `exp` (the `tok.<claim>`
  predicate is not implemented — see the claims table above).
- **`max_size` and `exact_size` are mutually exclusive by construction** — no code path mints a token with both set —
  but this is **not independently validated** as a "both present" error at presign or verify time; there is no
  dedicated rejection path for that combination.
- **`expected_hash`** `<alg>` must be in the backend allow-list (currently `SHA-256` only); lowercase hex; baked by
  the control plane (may carry a client-supplied value from the presign request).
- **`max_rate` / `max_conns` are not implemented.** No such claims exist on `Claims` and the sidecar enforces no
  per-URL rate/connection cap; this remains an open design point (scoping to one `(file_id, op)` and cross-instance
  coordination across the sidecar fleet).
- **Outside the token:** the `Range` header, conditional headers, and the `PUT` body are not part of the token — so one
  signed URL serves many ranges, and body integrity is enforced by `max_size`/`expected_hash` during the stream plus a
  control-plane re-verification at `finalize`, which re-reads the stored blob and recomputes its size/hash rather than
  trusting the sidecar's report (single-part); a multipart upload instead derives the composite hash from the
  reported per-part hashes during `complete` (no full assembled-object read-back — only a bounded MIME-sniff read).
  `bind` performs no integrity check of its own — it only swaps `content_id`
  to point at an already-finalized (`Available`) version, guarded by the `If-Match` content-ETag precondition above.
- **"Baked response headers" claim — not implemented.** The token carries only the two specific `content_type`/`etag`
  claims (download-only, above); there is no general response-header-set claim and the sidecar does not echo an
  arbitrary `Content-Disposition`/`Cache-Control`/etc. from the token. See "Response headers" below for what the
  sidecar actually emits.

## Conditional headers

- `If-Match`: required on **bind** (`POST /files/{id}/bind`) and on `DELETE`. Mismatch → `400 Bad Request` on the
  control plane (`FailedPrecondition` collapses to `400` on this platform — see "Status code summary" below). The
  sidecar's data-plane `PUT` does not check `If-Match` at all — it only streams bytes and calls finalize; conditional
  concurrency on content is enforced solely by the control-plane `bind` handler.
- `If-Match-Metadata: <u64>`: **optional** on metadata-only `PATCH`; matched against the current `meta_version`.
  Mismatch → `400` (same `FailedPrecondition` → `400` mapping). `meta_version` is returned in the JSON body
  (`FileDto.meta_version`) on every file read/mutation response; there is **no** `X-FS-Metadata-Revision` response
  header (see "Response headers" below). Clients that omit `If-Match-Metadata` get last-write-wins; clients that
  want to detect concurrent metadata edits opt in by sending it.
- `If-None-Match`: optional on control-plane `GET /files/{id}` (metadata) only; match → `304 Not Modified`. **Not**
  implemented on the sidecar's download `GET`/`HEAD` (see "P1 — Sidecar" above); the control plane also has no
  `HEAD /files/{id}` route (see "P1 — Control plane" above).
- ETag is opaque, derived from `(file_id, content_id)`, content-only, and explicitly **not** equal to the content
  hash. It changes exactly when content is (re)bound; a metadata-only `PATCH` does not change it. The content
  hash algorithm+value are exposed in the `GET /files/{id}/versions` body (`hash_algorithm`, `hash`), not as
  response headers (see "Response headers" below — the sidecar does not emit `X-FS-Hash-*`). Content-hash modes
  (whole-object vs. multipart offset-manifest composite) are implemented — see
  [ADR-0006](./ADR/0006-cpt-cf-file-storage-adr-content-hash-modes.md) and `hash_mode`/`part_count` above (§P1
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

Emitted by the sidecar's download handlers:

```text
Accept-Ranges: bytes
Content-Type: <mime>                # from the token's content_type claim; "application/octet-stream" fallback
ETag: "<opaque>"                     # from the token's etag claim; header omitted entirely if the claim is empty
Content-Range: bytes <s>-<e>/<n>     # only on 206 (and "bytes */<n>" on 416)
```

`Content-Length` is set by the HTTP framework from the response body, not hand-rolled by the sidecar.

`ETag` and `Content-Type` are sourced per-request from the download token's `etag`/`content_type` claims (see the
Claims table above) rather than a control-plane round trip — the sidecar has no DB access, so the token is its only
source for either. A token that leaves either claim empty falls back to `Content-Type: application/octet-stream` and
omits `ETag` entirely, rather than sending an empty header.

If the backend read fails *after* a `200`/`206` response's status and headers (including `Content-Length`) are
already committed, the sidecar cannot retroactively switch to `503` — it aborts the connection instead, so the body
ends up shorter than the `Content-Length` it already promised. A well-behaved HTTP client sees this as a read error,
not as a short-but-successful response, and should retry with `Range: bytes=<bytes already received>-` against the
same signed URL to resume from where it left off.

**Planned / not implemented:**

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

None of the `X-FS-*` headers above are emitted by the sidecar: doing so would require the sidecar to either gain DB
access or carry substantially more per-request state in the token than it does today. `HEAD` and sidecar-side
`If-None-Match` → `304` are likewise not implemented (see "P1 — Sidecar" above).

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
  (`FailedPrecondition` collapses to `400` on this platform — there is no `412`-mapped canonical-error variant); a
  multipart initiate whose target backend does not support native multipart (`MULTIPART_NOT_SUPPORTED`); the
  finalize callback's read-back size/hash/mime not matching the sidecar's claim, or no blob present at the
  version's backend path at all (control plane, `POST .../finalize` — see
  [Data-plane callbacks](#data-plane-callbacks-sidecar--control-plane-s2s-token-authenticated)); invalid GTS file
  type format (control plane); or the sidecar's `fs-token` query param and `X-FS-Token` header both present but
  disagreeing on the same request (sidecar, any signed-URL route — see "Signed URLs" above).
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
    or more planned parts have not been reported yet (`MultipartPartsMissing`; the error detail lists the missing
    part numbers, checked **before** the size check below), the assembled size does not match `declared_size`, or
    the pending version row was removed concurrently.
  - `create_file` (idempotent retry): the same `idempotency_key` was reused with a materially different request body
    — including `bind` alone, which is deliberately excluded from the request-hash comparison but still checked
    separately, so a retry that flips `bind` gets this same `409`; the file's owner has changed since the ticket was
    created (e.g. via `POST /files/{id}/transfer`), so a stale-owner replay no longer matches the request's
    `(owner_kind, owner_id)`; or the ticket's target version is no longer `pending` (an earlier, successfully
    delivered `PUT`+finalize already completed it, and only the `201` response back to the client was lost — a
    replay must not re-mint a fresh upload token against content that already exists). A retry against a key whose
    file has since been **deleted** is not one of these cases: `idempotency_keys.file_id` carries `ON DELETE CASCADE`
    from `files`, so the key is removed along with the file, and the retry creates a brand-new file exactly as if the
    key had never been used. See [operations.md](./operations.md#idempotent-create-semantics) for the full replay
    contract.
  - `download_url` (`GET /files/{id}/download-url`): the file has no bound content yet (never bound), or the target
    version's upload has not been finalized. This route's OpenAPI registration in `routes.rs` declares only
    `401`/`403`/`404`/`500`, so this `409` is not represented in the generated OpenAPI schema even though the domain
    code returns it.
  - **sidecar `PUT` (replay)**: a `PUT` to an `upload_url` whose `backend_path` already holds a published blob (a
    genuine token replay after the version was already finalized, as opposed to a benign retry of the same in-flight
    upload) gets `409 Conflict` from the sidecar itself — `publish_exclusive`'s create-exclusive write refuses to
    overwrite the existing object, and the live bytes are never touched. A benign retry (the earlier publish landed
    but finalize had not yet run) instead converges to `200` once finalize succeeds on this attempt.

  Note: `update_metadata` (`PATCH /files/{id}`) declares a `409` response in its OpenAPI registration
  (`routes.rs`), but no domain code path returns `DomainError::Conflict` for this handler — its only failure mode
  beyond request validation is an `If-Match-Metadata` mismatch, which maps to `400` (`PreconditionFailed`). The
  declared `409` does not correspond to a reachable code path.
- `412 Precondition Failed` — **not used anywhere in this gear.** This platform's canonical-error taxonomy has no
  `412`-mapped variant; `FailedPrecondition` collapses to `400` on the control plane (see above), and the sidecar
  never performs an `If-Match`/conditional check at all — it only streams bytes and calls finalize, so there is no
  data-plane `412` either. A bind conflict is always a `400`
  from the control-plane `bind` handler (see "Upload, bind, and the conflict retry" above).
- `413 Payload Too Large` — upload exceeds the `max_size` claim, aborted mid-stream (sidecar, `PUT`).
- `416 Range Not Satisfiable` — a well-formed `Range` that cannot be satisfied against the size (sidecar). An
  unparseable `Range` is **not** a `416` — it is ignored and the full body is served with `200`.
- `503 Service Unavailable` — two causes, both a signal to retry, never a fault:
  - the sidecar's own response when a backend read (`GET`/`HEAD`, full or range) finds the object's size disagrees
    with the length already resolved by the caller's earlier `stat`/`HeadObject` — a transient read race, not a
    fault (see "P1 — Sidecar" above). Carries `Retry-After: 1`; the recovery is a plain retry.
  - any storage-backend call (control plane or sidecar) failing with a **transient** backend fault — network,
    timeout, backend overload/5xx, or the losing side of a concurrent object change (`DomainError::BackendUnavailable`,
    per `docs/arch/errors/categories/14-service-unavailable.md`). Carries `retry_after_seconds` in the response body
    and a matching `Retry-After: 5` header; the recovery is a plain retry once that window has elapsed. Distinct
    from `502` (control-plane callback failure) and `500` (a **permanent** backend fault — bad config, a protocol
    violation by the backend, or an internal invariant — which keeps the previous, non-retryable `500` and is
    logged at `error`, not `warn`).
- `429 Too Many Requests` — not implemented as a sidecar per-URL `max_conns` cause (that claim does not exist, see
  "Signed URLs" above); the only live `429` source is the control-plane storage quota check on
  `create_file`/`presign_version`/multipart `initiate` (`QuotaExceeded`). That check is itself only reachable when a
  `QuotaClient` is wired, and none is — `gear.rs` always passes `quota_client: None`, so `check_quota`/
  `check_quota_bytes` are a permissive no-op and this `429` cause cannot currently occur. See
  [operations.md](./operations.md#storage-quota-not-enforced).
- `502 Bad Gateway` — the sidecar's own response to the client when its finalize or report-part callback to the
  control plane fails (transport error after retries, or the control plane rejects the callback with a 4xx/5xx). This
  is a **retry signal**: the callback failure does not mean the bytes failed to land — see "Data-plane callbacks"
  above (`FS_SIDECAR_FINALIZE_TIMEOUT_SECS`/`FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS`) for the retry/timeout
  configuration and the decision between `200`, `409`, and `502` on a replayed `PUT`.

`422 Unprocessable Entity`, `415 Unsupported Media Type`, and `507 Insufficient Storage` are not used anywhere in this
gear: an `expected_hash` mismatch and an invalid GTS file type both map to `400`; a magic-bytes MIME mismatch
(`DomainError::MimeMismatch`) also maps to `400` (`invalid_argument`); and the storage-quota-exceeded case maps to
`429`, not `507`.
