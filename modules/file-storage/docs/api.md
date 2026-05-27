# FileStorage — HTTP API (P1 + declared P2)


<!-- toc -->

- [P1 — Auth-required](#p1--auth-required)
- [P2 — Multipart upload (declared, not implemented in P1)](#p2--multipart-upload-declared-not-implemented-in-p1)
- [P2 — Versioning (when backend declares `versioning_native`)](#p2--versioning-when-backend-declares-versioning_native)
- [POST /files vs PATCH /files/{id}](#post-files-vs-patch-filesid)
- [Conditional headers](#conditional-headers)
- [Range support](#range-support)
- [Response headers (download + HEAD)](#response-headers-download--head)
- [Status code summary](#status-code-summary)

<!-- /toc -->

FileStorage issues exactly one shape of file URL: `/files/{file_id_uuid}` (`GET` / `HEAD` only), served only on the
auth-required prefix and reachable only with a valid platform JWT. FileStorage P1 has **no anonymous surface** —
anonymous/public access, time-bounded URLs, named recipients, download counters, and any other sharing primitives
are deferred to P3 (see DESIGN.md §1.1 "Sharing boundary"); whether they ship as a separate sibling module or as
an extension of FileStorage is left to a future ADR.

Base URL:
- `/api/file-storage/v1` — JWT enforced by API Gateway; standard owner/tenant authorization applies

Encoding conventions:
- Multipart create/update bodies use `multipart/form-data` with two named parts: `metadata` (`application/json`) and `content` (binary, `Content-Type` = declared mime).
- All error responses follow RFC 7807 (`application/problem+json`).
- File ids are UUIDs.

## P1 — Auth-required

```
1.  POST   /files                              create   (multipart: metadata required + content required)
2.  PATCH  /files/{id}                         update   (multipart: metadata optional + content optional)    — If-Match
3.  GET    /files/{id}                         download content                                              — If-Match, If-None-Match, Range
4.  HEAD   /files/{id}                         metadata headers                                              — If-Match, If-None-Match
5.  DELETE /files/{id}                         delete                                                        — If-Match
6.  GET    /files                              list (filters, paginated; JSON array of metadata)
7.  GET    /storages                           list storages + capabilities inline
8.  GET    /storages/{storage_id}              one storage + capabilities
```

## P2 — Multipart upload (declared, not implemented in P1)

```
9.  POST   /files/multipart                                      initiate (JSON metadata); returns {file_id, upload_id, etag}; creates pending file
10. POST   /files/{id}/multipart/{upload_id}/parts/{n}           upload one part (binary body)                                — If-Match
11. POST   /files/{id}/multipart/{upload_id}/complete            finalize; transitions file to available                     — If-Match
12. DELETE /files/{id}/multipart/{upload_id}                     abort; parts discarded                                       — If-Match
13. GET    /files/{id}/multipart/{upload_id}                     list uploaded parts (introspection)
```

## P2 — Versioning (when backend declares `versioning_native`)

```
14. GET    /files/{id}/versions                                  list versions
15. GET    /files/{id}/versions/{version_id}                     download specific version                                    — If-Match, If-None-Match, Range
16. HEAD   /files/{id}/versions/{version_id}                     version metadata headers                                     — If-Match, If-None-Match
17. DELETE /files/{id}/versions/{version_id}                     permanent version delete                                     — If-Match
```

## POST /files vs PATCH /files/{id}

| Aspect                       | `POST /files`                       | `PATCH /files/{id}`                                    |
|------------------------------|-------------------------------------|--------------------------------------------------------|
| Body                         | `multipart/form-data`               | `multipart/form-data`                                  |
| `metadata` part              | required (full metadata document)   | optional (JSON Merge Patch per RFC 7396)               |
| `content` part               | required (binary)                   | optional (binary; replaces content when present)       |
| `If-Match`                   | N/A                                 | required                                               |
| Empty body / no parts        | `400`                               | `400`                                                  |
| State on success             | `available`                         | `available` (with content) / unchanged (metadata only) |

`PATCH` with a `content` part replaces the file content; `content_revision` is bumped, `metadata_revision` is bumped, `hash_value` is recomputed, and `ETag` changes. When the backing storage declares `versioning_native = true`, each content replacement creates a new version retrievable by version id; otherwise the prior content is permanently overwritten.

`PATCH` with a `metadata` part applies JSON Merge Patch semantics to `custom_metadata`: keys present in the patch overwrite their values, keys set to `null` delete the entry, keys absent from the patch are left untouched. Metadata-only updates bump `metadata_revision` and `Last-Modified` but do **not** change `ETag` or `hash_value` — both remain tied to the content.

## Conditional headers

- `If-Match`: required on every write (`PATCH`, `DELETE`) and on every multipart-control endpoint (`POST .../multipart/...`, `DELETE .../multipart/{upload_id}`). On read endpoints (`GET`, `HEAD`) it is optional; non-match returns `412 Precondition Failed`.
- `If-None-Match`: optional on `GET`/`HEAD`; match returns `304 Not Modified` with no body.
- ETag is opaque, deterministic per `(file_id, content_revision)`, and explicitly **not** equal to the content hash. The content hash is exposed as `X-FS-Hash-Algorithm` + `X-FS-Hash-Value` headers (P1: SHA-256 only, per ADR-0002).
- **ETag is content-only.** Metadata-only `PATCH` (no `content` part) does **not** change ETag — only `metadata_revision` and `Last-Modified` are bumped. Consequently `If-Match` on metadata-only `PATCH` protects against concurrent **content** writes but does **not** detect concurrent metadata writes (S3-style limitation; metadata updates are last-write-wins).

## Range support

- `GET /files/{id}` accepts `Range: bytes=<start>-<end>`, `bytes=<start>-`, and `bytes=-<suffix-length>`. Valid ranges return `206 Partial Content` with `Content-Range: bytes <s>-<e>/<n>`. Unsatisfiable ranges return `416 Range Not Satisfiable` with `Content-Range: bytes */<n>`.
- Every download response includes `Accept-Ranges: bytes`.
- `HEAD` ignores the `Range` header and always responds with full-file metadata; the `Accept-Ranges: bytes` header is still set to advertise support on `GET`.
- Multi-range (`multipart/byteranges`) is optional; when unsupported the server may return the full content or a single coalesced range, per RFC 7233.

## Response headers (download + HEAD)

```
ETag: "<opaque>"
Content-Type: <mime>
Content-Length: <bytes>             # full file on HEAD/200; range bytes on 206
Content-Range: bytes <s>-<e>/<n>    # only on 206
Accept-Ranges: bytes
Last-Modified: <RFC 7231 date>
X-FS-File-Id: <uuid>
X-FS-GTS-File-Type: gts.cf.fstorage.file.type.v1~...
X-FS-Hash-Algorithm: SHA-256                            # of content
X-FS-Hash-Value: <hex>                                  # of content
X-FS-Content-Revision: <u64>                            # increments only on content writes
X-FS-Metadata-Revision: <u64>                           # increments on any PATCH
X-FS-Version-Id: <opaque>                               # only on /versions/{version_id} responses (P2)
X-FS-Owner-Kind: user|app
X-FS-Owner-Id: <uuid>
X-FS-Created-At: <ISO 8601>
X-FS-Meta-<key>: <value>                                # one header per custom metadata key
```

## Status code summary

- `200 OK` — successful read or PATCH with state change.
- `201 Created` — successful `POST /files`.
- `204 No Content` — successful `DELETE`.
- `206 Partial Content` — successful range read.
- `304 Not Modified` — `If-None-Match` matched current ETag.
- `400 Bad Request` — malformed request (missing required form parts, invalid JSON, etc.).
- `403 Forbidden` — authorization denied.
- `404 Not Found` — file does not exist or version does not exist.
- `409 Conflict` — multipart state conflicts (e.g., complete on aborted upload).
- `412 Precondition Failed` — `If-Match` mismatch.
- `415 Unsupported Media Type` — declared mime does not match magic-bytes detection.
- `416 Range Not Satisfiable` — invalid `Range` header.
- `422 Unprocessable Entity` — semantic validation failure (e.g., invalid GTS file type format).
- `507 Insufficient Storage` — backend or quota limit exceeded.
