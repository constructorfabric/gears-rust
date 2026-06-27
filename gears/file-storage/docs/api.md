# FileStorage — HTTP API (P1 + declared P2)


<!-- toc -->

- [Two planes](#two-planes)
- [P1 — Control plane (`/api/file-storage/v1`)](#p1--control-plane-apifile-storagev1)
- [P1 — Sidecar (signed-URL authorized)](#p1--sidecar-signed-url-authorized)
- [P2 — Multipart upload (declared, not implemented in P1)](#p2--multipart-upload-declared-not-implemented-in-p1)
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
7.  HEAD   /files/{id}                      file metadata headers                                         — If-None-Match
8.  DELETE /files/{id}                      delete file + all versions                                    — If-Match
9.  GET    /files                           list files (filters, paginated; JSON array of metadata)
10. GET    /files/{id}/versions             list versions (version_id, size, hash, created_at, is_current)
11. GET    /storages                        list storages + capabilities inline
12. GET    /storages/{storage_id}           one storage + capabilities
```

Notes:
- `POST /files` and `POST /files/{id}/versions` return `{ file_id, version_id, upload_url }`. The client `PUT`s the
  bytes to `upload_url` on the sidecar; the sidecar pre-registers the `pending` version, streams to the backend, and
  **auto-binds** it. The client may instead bind explicitly via `POST /files/{id}/bind` (and must, to recover from a
  bind `412` — see below).
- `GET /files/{id}/download-url` returns `{ download_url, etag, metadata }`. By default it pins the current
  `content_id`; `?version_id=<v>` pins a specific version.
- Restoring a prior version is `POST /files/{id}/bind` with that `version_id` (a pointer swap, no re-upload).

## P1 — Sidecar (signed-URL authorized)

```text
S1. PUT    <signed upload url>             upload the new version's bytes (raw body)               — If-Match (relayed)
S2. GET    <signed download url>           download content                                        — If-None-Match, Range
S3. HEAD   <signed download url>           content headers (full-file)                             — If-None-Match
```

The sidecar verifies the signed token and its claims (and a platform JWT only when a `tok.<claim>` predicate is
present) before serving — a valid token is the delegated authorization decision, so there is no request-time PDP
call. On `PUT` it pre-registers + binds against the control plane **on the user's behalf** via the FS SDK in **s2s
REST mode** (its own app-token plus an on-behalf-of `<user>` claim) — the P1 sidecar holds **no** direct DB
connection and is a thin, stateless byte-mover (the direct-DB mode is a P2 co-located optimization; see ADR-0003).

## P2 — Multipart upload (declared, not implemented in P1)

Multipart is **server-authoritative**: the client sends desired parameters and the control plane returns the exact
parts plan (sizes/offsets) with **one signed URL per part** pointing at the sidecar.

```text
P2-1. POST /files/{id}/multipart            initiate (JSON: size, preferred part size, concurrency); returns the parts plan + per-part signed URLs
P2-2. PUT  <signed part url>                upload one part to the sidecar (raw body)
P2-3. POST /files/{id}/multipart/{upload_id}/complete   finalize (combine BLAKE3 subtree hashes → root); binds the version  — If-Match
P2-4. DELETE /files/{id}/multipart/{upload_id}          abort; parts discarded
P2-5. GET  /files/{id}/multipart/{upload_id}            list uploaded parts (introspection)
```

For a `multipart_native` backend the sidecar drives the backend multipart API; otherwise it offset-writes each part
into the single new-version object. Per-part BLAKE3 subtree hashes are persisted in `multipart_upload_parts.part_hash`
and combined into the root at `complete`. Detailed envelope/error shapes are owned by the P2 FEATURE.

## Upload, bind, and the conflict retry

Content is an immutable blob per version; a file's live content is the `content_id` pointer, swapped under optimistic
CAS. The flow:

1. **Control**: `POST /files` (or `POST /files/{id}/versions`) → `{ file_id, version_id, upload_url }`.
2. **Data**: `PUT upload_url` to the sidecar with `If-Match: "<current content ETag>"`. The sidecar pre-registers the
   `pending` version (checking `If-Match` as an early fail — if the file already moved on, it errors **before** the
   bytes are uploaded), streams to the backend computing size + SHA-256, then **binds** `content_id := version_id`
   under `If-Match`.
3. On a **bind conflict** the sidecar returns `412` **and the `version_id`**; the client re-reads the current ETag and
   replays `POST /files/{id}/bind` with that `version_id` and the fresh `If-Match` — **no byte re-upload**, because the
   version already exists.

`If-Match` is therefore checked twice (opportunistically at pre-register, authoritatively at bind). Backend content is
never mutated in place; a replacement is always a new version + a pointer swap.

**On a `412`, re-bind — do not re-presign or re-upload.** Rebinding is a control-plane call (`POST /files/{id}/bind`),
**independent of the signed upload URL** — so the upload URL's `exp` is irrelevant to the retry and the bytes are not
re-sent (the version persists). The `412` response carries the current content ETag, which the client uses as the fresh
`If-Match` to replay `POST /files/{id}/bind` immediately. Re-presigning is **not** idempotent: a fresh
`POST /files/{id}/versions` + upload creates a **new sibling `version_id`** (the unbound one is swept by the P2 cleanup
engine, `cpt-cf-file-storage-fr-orphan-reconciliation`). Clients **should** rebind the returned `version_id` instead.

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
  | `exact_size` | no | P1 | upload | `413`/`400` |
  | `expected_hash` = `<alg>:<hex>` | no | P1 | upload | `422` |
  | `max_rate` | no | P2 | up/down | throttle |
  | `max_conns` | no | P2 | up/down | `429` |
- **`exp` is mandatory, short by default, and hard-capped.** Every issued URL gets a **short default TTL**
  (`default_url_ttl`, minutes — 15 min in P1) to bound the stale-permission window, and the control plane refuses to
  mint beyond a **hard ceiling** `max_url_ttl` (≤ **7 days**). Both are enforced at signing; the sidecar only checks
  `now ≤ exp`. **Stale-permission trade-off:** authorization is evaluated at signing and there is no per-token
  revocation in P1, so the TTL bounds the exposure window — hence the short default for private content; the 7-day
  ceiling is an explicitly accepted trade-off for low-sensitivity / deliberately long-lived cases (bare query-token
  URLs in particular MUST use a short TTL; durable/anonymous sharing is P3 FileShare). A `tok.<claim>` predicate
  requires a valid platform JWT, which the sidecar validates and matches. "Available to everyone for 5 minutes" =
  only `exp`.
- **`max_size` and `exact_size` are mutually exclusive** (both → `400` at presign / `403` at the sidecar).
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

- `If-Match`: required on **bind** (`POST /files/{id}/bind`, and relayed on the sidecar `PUT` for the embedded
  pre-register/bind) and on `DELETE`. Mismatch → `412 Precondition Failed`.
- `If-Match-Metadata: <u64>`: **optional** on metadata-only `PATCH`; matched against the current `meta_version` (the
  value published as `X-FS-Metadata-Revision`). Mismatch → `412`. Absent → last-write-wins (back-compatible default);
  clients keeping meaningful state in custom metadata opt in.
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
  length is short; or a malformed token minted at presign (e.g. both `max_size` and `exact_size` claims).
- `403 Forbidden` — authorization denied (control), or token verification failed at the sidecar: bad signature,
  expired (`now > exp`), `ip` mismatch, method ≠ the `op` claim, missing/invalid JWT or unmatched token-claim predicate,
  or a malformed (mutually-exclusive) claim set. (The `max_url_ttl` cap is enforced at signing, not re-checked here.)
- `404 Not Found` — file or version does not exist.
- `409 Conflict` — multipart state conflicts (e.g., complete on an aborted upload) (P2).
- `412 Precondition Failed` — `If-Match` (content ETag) mismatch on bind/delete, or `If-Match-Metadata` mismatch
  against the current `meta_version`. On a bind `412` the response carries the uploaded `version_id` for rebind.
- `413 Payload Too Large` — upload exceeds the `max_size` / `exact_size` claim (sidecar; aborted mid-stream).
- `415 Unsupported Media Type` — declared mime does not match magic-bytes detection (sidecar, on upload).
- `416 Range Not Satisfiable` — a well-formed `Range` that cannot be satisfied against the size (sidecar). An
  unparseable `Range` is **not** a `416` — it is ignored and the full body is served with `200`.
- `422 Unprocessable Entity` — semantic validation failure (e.g., invalid GTS file type format), or an upload whose
  content does not match the `expected_hash` claim (sidecar; not bound).
- `429 Too Many Requests` — (P2) the `max_conns` claim for this `(file_id, op)` is exceeded.
- `507 Insufficient Storage` — backend or quota limit exceeded.
