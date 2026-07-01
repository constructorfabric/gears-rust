# FEATURE — `multipart-coordinator` (server-authoritative multipart upload)

**Component**: `multipart-coordinator`
**Implements**: PRD `cpt-cf-file-storage-fr-multipart-upload`
**Depends on**: `cpt-cf-file-storage-adr-sidecar-data-plane` (ADR-0003),
`cpt-cf-file-storage-adr-signed-url-transport` (ADR-0004),
`cpt-cf-file-storage-adr-content-hash-selection` (ADR-0002)
**Also enforces**: PRD `cpt-cf-file-storage-fr-size-limits-policy`,
`cpt-cf-file-storage-fr-storage-quota`, `cpt-cf-file-storage-fr-allowed-types-policy`,
`cpt-cf-file-storage-fr-audit-trail`, `cpt-cf-file-storage-fr-file-events`
**Status**: authored (P2). Supersedes the interim client-driven multipart shipped
under P2-M3 — see [§8 Migration from the interim implementation](#8-migration-from-the-interim-implementation).

DESIGN [§4.6](../DESIGN.md) fixes the *shape* of multipart upload; this document
owns the detailed contract (request/response envelopes, error codes, token
claims, persistence, resumability) that DESIGN deliberately deferred.

<!-- toc -->

- [1. Principle: the server owns the plan](#1-principle-the-server-owns-the-plan)
- [2. Lifecycle & endpoints](#2-lifecycle--endpoints)
- [3. The parts plan](#3-the-parts-plan)
- [4. Per-part signed URL & the size claim](#4-per-part-signed-url--the-size-claim)
- [5. Hashing (BLAKE3 subtree)](#5-hashing-blake3-subtree)
- [6. Persistence & schema deltas](#6-persistence--schema-deltas)
- [7. Enforcement matrix (size / quota / type)](#7-enforcement-matrix-size--quota--type)
- [8. Migration from the interim implementation](#8-migration-from-the-interim-implementation)
- [9. Traceability](#9-traceability)

<!-- /toc -->

## 1. Principle: the server owns the plan

Multipart upload is **server-authoritative**. The client declares its *intent*
(total size, preferred part size, desired concurrency); the control plane
computes and returns the **exact** parts plan — every part's `part_number`,
`offset`, and `size`, plus **one signed URL per part** pointing at the sidecar.
The client never chooses part boundaries and can never upload a part that
deviates from the plan.

This reverses the earlier client-driven `.../parts/{n}` draft (DESIGN §4.6). Two
properties fall out of server-authority that the client-driven model cannot give:

- **Per-part size is enforced before any bytes are stored.** Each part's exact
  size is a **claim inside its signed URL**; the sidecar rejects a body whose
  length does not match the claim, so oversized bytes never reach the backend.
  This is what fully closes the abuse vector left open by the interim model
  (a client declaring a small `declared_size` but uploading large parts).
- **Part boundaries can be aligned to the BLAKE3 chunk tree**, making the
  per-part subtree hashes composable into the root at `complete` (ADR-0002).

Byte movement never touches the control plane (ADR-0003): the control plane is
JSON-only and returns opaque signed URLs; all part bytes flow to the **sidecar**.

## 2. Lifecycle & endpoints

Control-plane routes are under `/api/file-storage/v1`; the part-upload route is a
sidecar URL the client receives ready-made (it is never hand-constructed).

| # | Method / path | Plane | Purpose |
|---|---|---|---|
| P2-1 | `POST /files/{id}/multipart` | control | Initiate: validate intent, pre-register a `pending` version, create the backend session, return the parts plan + per-part signed URLs. |
| P2-2 | `PUT <signed part url>` | **sidecar** | Upload one part (raw body). Sidecar enforces the size claim, stores the part, persists its BLAKE3 subtree hash. |
| P2-3 | `POST /files/{id}/multipart/{upload_id}/complete` | control | Combine subtree hashes → root, verify total size, bind the version under `If-Match` (CAS). |
| P2-4 | `DELETE /files/{id}/multipart/{upload_id}` | control | Abort: mark aborted, abort the backend handle, discard parts, delete the pending version. |
| P2-5 | `GET /files/{id}/multipart/{upload_id}` | control | Introspect: return the plan + which parts are uploaded (drives resume). |

Every mutating step **MUST** apply the same authorization, audit, and event
requirements as single-part upload (PRD `cpt-cf-file-storage-fr-multipart-upload`).

## 3. The parts plan

**`P2-1` request** (`application/json`):

| Field | Type | Req | Description |
|---|---|---|---|
| `declared_mime` | string | yes | Validated against the effective allowed-types policy (`415` on reject). |
| `declared_size` | uint64 | yes | Total object size. Gated at initiate against the effective size limit (`413`) and storage quota (`507`) — see [§7](#7-enforcement-matrix-size--quota--type). |
| `preferred_part_size` | uint64 | no | Client hint. The server **MAY** override it to satisfy the backend's minimum part size and BLAKE3 alignment. |
| `concurrency` | uint32 | no | Advisory hint for how many parts the client intends to upload in parallel; does not change the plan. |

**`P2-1` response** (`application/json`):

```json
{
  "upload_id": "uuid",
  "version_id": "uuid",
  "part_hash_algorithm": "BLAKE3",
  "part_size": 8388608,
  "parts": [
    { "part_number": 1, "offset": 0,       "size": 8388608, "upload_url": "https://sidecar/.../part?fs-token=…" },
    { "part_number": 2, "offset": 8388608, "size": 8388608, "upload_url": "…" },
    { "part_number": 3, "offset": 16777216,"size": 2097152, "upload_url": "…" }
  ],
  "expires_at": "RFC3339"
}
```

The server chooses `part_size` (uniform except the final part) as
`max(preferred_part_size, backend.min_part_size)`, rounded to a BLAKE3-friendly
boundary, and derives `parts.len() = ceil(declared_size / part_size)`. The plan is
**deterministic** from `(declared_size, part_size)`, so it can be recomputed for
resume without persisting every row (see [§6](#6-persistence--schema-deltas)).

## 4. Per-part signed URL & the size claim

Each `upload_url` is a PASETO `v4.public` token (ADR-0004) whose claim-set binds
the part to the plan. Claims **MUST** include:

| Claim | Purpose |
|---|---|
| `upload_id`, `file_id`, `version_id` | Scope the URL to this session/version. |
| `part_number`, `offset` | Where the part lands. |
| `size` | **Exact** byte length the sidecar will accept. |
| `op = "multipart_part"`, `exp` | Verb + expiry. |

On `PUT`, the sidecar **MUST**:

1. Verify the token (asymmetric; sidecar can never mint — ADR-0004).
2. **Reject with `413` if the request body length ≠ `size`** — before writing
   anything. This is the point that makes oversized parts unstorable.
3. Write the part: for a `multipart_native` backend via `PutPart`
   (`backend_upload_handle`); otherwise offset-write into the single new-version
   object `/{file_id}/{version_id}` at `offset` (never mutating an existing
   object).
4. Compute the part's BLAKE3 subtree hash and persist the part row via the SDK
   ([§5](#5-hashing-blake3-subtree), [§6](#6-persistence--schema-deltas)).

Re-`PUT` of the same `(upload_id, part_number)` is **idempotent** (overwrite),
which is what makes resume safe. If a part URL has expired, the client re-fetches
fresh URLs from `P2-5` (which re-issues them for missing parts).

## 5. Hashing (BLAKE3 subtree)

Part boundaries are chosen so each part is a BLAKE3 **subtree**; the sidecar
persists each part's subtree hash in `multipart_upload_parts.part_hash`, and
`complete` combines them into the root (ADR-0002). The effective algorithm is
bounded by the backend's `allowed_algorithms`; when a backend does not allow
BLAKE3, the coordinator falls back to a streaming single-pass algorithm from the
allow-list and computes the root at `complete` from the reassembled object.
Persisted part hashes make an upload **resumable** and **crash-durable**.

## 6. Persistence & schema deltas

Existing tables (`file_storage.multipart_uploads`,
`file_storage.multipart_upload_parts`) need the following deltas to carry the
plan. (Schema changes ship with the feature's migration; documented here, not
applied by this doc.)

`multipart_uploads` — add:

- `version_id uuid NOT NULL` — the pre-registered pending version this session
  binds at complete (today the linkage is not persisted on the session row).
- `declared_size bigint NOT NULL CHECK (declared_size >= 0)` — the gated total.
- `part_size bigint NOT NULL` — the server-chosen plan unit; with `declared_size`
  this reconstitutes the full plan for resume without a per-part plan table.

`multipart_upload_parts` — the existing `size` column stores the **actual**
written length; the **expected** size is authoritative in the signed token, so no
`expected_size` column is required. `part_hash` continues to hold the BLAKE3
subtree hash.

Validated-but-not-persisted today: `declared_size` is currently validated only at
initiate (see the F2 fix). Persisting it (above) lets `complete` and resume verify
actual-vs-declared without re-summing, and lets `P2-5` return the plan.

## 7. Enforcement matrix (size / quota / type)

Defence in depth — each gate is normative:

| Gate | Where | On violation |
|---|---|---|
| Allowed MIME | initiate, against effective allowed-types policy | `415` |
| Declared size ≤ effective max (policy ⋈ backend) | initiate, against `declared_size` | `413` |
| Storage quota | initiate, quota checked against `declared_size` (not a pessimistic ceiling) | `507` |
| **Per-part size = token claim** | **sidecar, per `PUT`** | `413` (body never stored) |
| Total assembled size = `declared_size` | complete, summing actual part sizes | `409`/`413` |
| First-part magic-bytes vs `declared_mime` | sidecar/complete (`cpt-cf-file-storage-fr-content-type-validation`) | reject + abort |

The initiate gate blocks *starting* an oversized/over-quota session; the per-part
claim blocks *storing* oversized bytes; the complete check is the final backstop.
Abandoned in-flight sessions are reclaimed by the orphan/TTL sweep
(`cpt-cf-file-storage-fr-orphan-reconciliation`).

## 8. Migration from the interim implementation

The former P2-M3 multipart was **client-driven**: the client picked `part_number`
and `PUT` raw bytes to a **control-plane** route
(`PUT /files/{id}/multipart/{upload_id}/parts/{part_number}`), which proxied them
to the backend. That contradicted DESIGN §4.6 (server owns the plan) and ADR-0003
(no bytes through the control plane), and it is why per-part size could not be
enforced.

This feature **has superseded it** (shipped):

- `initiate` returns a parts plan + per-part sidecar signed URLs (was: a single
  session handle).
- Part bytes move to the **sidecar** via those URLs; the control-plane
  `.../parts/{n}` byte route is **removed**.
- Per-part size is enforced at the sidecar via the token `size` claim (`413`
  before any write).
- `declared_size` and `part_size` are persisted on `multipart_uploads`
  (migration `m20260701_000002_multipart_plan_columns`) so the plan is
  reconstitutable for resume.

The already-landed initiate-time `declared_size` gate (PR #4170 F2) is retained
as the up-front rejection, and the complete-time total-size check
(assembled size == `declared_size`) remains as defence-in-depth.

**Implementation note:** per-part hashes are **SHA-256** in P2 (see §5 — the
BLAKE3 subtree scheme is deferred). For a `multipart_native` backend the sidecar
drives the backend's native multipart API; the thin local-fs sidecar binary
offset-writes each part into a per-part object and relies on `complete` to
assemble.

## 9. Traceability

| Artifact | Link |
|---|---|
| Requirement | PRD `cpt-cf-file-storage-fr-multipart-upload` |
| Design shape | [DESIGN §4.6](../DESIGN.md) |
| HTTP contract | [api.md — P2 Multipart upload](../api.md) |
| Sidecar data plane | [ADR-0003](../ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md) |
| Signed-URL transport | [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md) |
| Content-hash selection | [ADR-0002](../ADR/0002-cpt-cf-file-storage-adr-content-hash-selection.md) |
