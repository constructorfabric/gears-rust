Created:  2026-07-08 by Constructor Tech
Updated:  2026-10-05 by Constructor Tech
# Feature: Backend Migration

- [ ] `p2` - **ID**: `cpt-cf-file-storage-featstatus-backend-migration-implemented`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Migrate a File's Content to a Different Backend](#migrate-a-files-content-to-a-different-backend)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Mode-Aware Content-Hash Verification Before Commit](#mode-aware-content-hash-verification-before-commit)
  - [Migration Lease and Destination-Tail Resolution](#migration-lease-and-destination-tail-resolution)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Migrate Endpoint with Hash-Verified Backend Relocation](#migrate-endpoint-with-hash-verified-backend-relocation)
  - [Non-Durable-Target Admin Gate](#non-durable-target-admin-gate)
  - [Migration Lease](#migration-lease)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

- [ ] `p2` - `cpt-cf-file-storage-feature-backend-migration`

### 1.1 Overview

`POST /files/{id}/migrate` relocates a **non-versioned** file's content (a
file with exactly one `file_versions` row) from its current storage backend
to a different one, without changing the file's identity (`file_id`,
ownership, metadata) or its content hash. The control plane reads the blob
from the source backend, verifies its content hash (mode-aware, before anything
is written to the destination), writes it to the destination backend at the
deterministic path `/{file_id}/{version_id}`, and then swaps the version row's
`(backend_id, backend_path)` atomically under a compare-and-swap keyed on the
pre-migration snapshot, in the same transaction as a `BackendMigrate` audit
row. A concurrent migration of the same version that already moved the pointer
makes this call's CAS lose; the loss is resolved without destroying the
winner's blob (see [Migration Lease and Destination-Tail
Resolution](#migration-lease-and-destination-tail-resolution)). Once the CAS
has won, the source blob is best-effort deleted. A transient backend failure
(`BackendUnavailable`) surfaces as `503` with `Retry-After: 5`.

**Not implemented yet**: a per-version migration lease that rejects a second
attempt up front, a time budget for the whole attempt (with the cleanup and
`503` that exceeding it would imply), a create-exclusive destination write
with destination-tail resolution, a pre-CAS re-`stat` of the destination, and
a streaming (non-buffered) transfer. Today the blob is read into memory in one
piece, and migration attempts of the same version are serialized only by the
CAS.

**Traces to**: `cpt-cf-file-storage-fr-backend-migration`, `cpt-cf-file-storage-fr-audit-trail`

### 1.2 Purpose

Let an operator move a file's bytes between backends (e.g. off a
non-durable dev/test backend, or between two durable backends for capacity or
policy reasons) without any downtime or content-identity change from the
caller's point of view — the file's `file_id`, `content_id`/version pointer
shape, and hash all stay the same; only where the bytes physically live
changes. The mandatory hash re-verification of the source bytes before
committing the swap means a corrupted read from the source is caught before
the file ever points at bad data — the operation either fully succeeds or
fails and leaves the original backend binding untouched. The bytes written
to the destination are not read back: their integrity is the destination
backend's write-path responsibility (see [Mode-Aware Content-Hash Verification
Before Commit](#mode-aware-content-hash-verification-before-commit)).

**Requirements**: `cpt-cf-file-storage-fr-backend-migration`

**Principles**: `cpt-cf-file-storage-principle-control-no-content` (the
migration still moves content through the control plane's process, not a
signed sidecar URL — this feature is explicitly an operator/admin path, not a
regular user upload/download path, so ADR-0003's sidecar-only rule does not
apply to it)

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-file-storage-actor-platform-user` | Calls `POST /files/{id}/migrate` with `WRITE` authorization on the file; needs the elevated `ADMIN_POLICY` scope in addition when the destination backend is non-durable |
| `cpt-cf-file-storage-actor-cf-gears` | Peer gear / operational tooling invoking the same endpoint as part of a backend-decommissioning or rebalancing workflow |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADR**: [ADR-0006](../ADR/0006-cpt-cf-file-storage-adr-content-hash-modes.md) —
  content-hash modes; `migrate_backend`'s verification step is one of this
  ADR's three call sites for the shared mode-aware verify algorithm
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md)
- **Dependencies**: [Content-Hash Modes](content-hash-modes.md)
  (`cpt-cf-file-storage-feature-content-hash-modes`) — `migrate_backend`'s
  pre-commit hash check is mode-aware per that feature's
  `cpt-cf-file-storage-algo-content-hash-modes-verify` algorithm, not a
  hard-coded whole-object SHA-256 check; [Audit Trail](audit-trail.md) for the
  `BackendMigrate` audit row's transactional guarantee

## 2. Actor Flows (CDSL)

### Migrate a File's Content to a Different Backend

- [x] `p1` - **ID**: `cpt-cf-file-storage-flow-backend-migration`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:
- The file has exactly one `Available` version; its content is read from the
  source backend and its hash is verified (mode-aware per ADR-0006) against
  the stored `hash_value`; once verification passes the content is written to
  the target backend, the version row is atomically repointed at the target,
  a `BackendMigrate` audit row is written, and the source blob is best-effort
  deleted
- Migrating to the backend the file is already on is a no-op: returns success
  immediately, no audit row, no read/write/verify work performed

**Error Scenarios**:
- The file has more than one version (versioned file) — `409`
  (`VersionedFileMigrationNotSupported`); non-versioned files only
- The file's single version is not yet `Available` (still `pending`) — `409`
  (`Conflict`, "cannot migrate a version whose upload has not been finalized")
- The target backend id is unknown — `400` (`UnknownBackend`)
- The re-verified content hash (or the length) does not match the stored
  version — `400` (`HashMismatch`) — the version row is never updated and
  nothing has been written to the destination yet, because verification runs
  on the source bytes before the destination write — see
  [Mode-Aware Content-Hash Verification Before Commit](#mode-aware-content-hash-verification-before-commit)
- The source read fails — the underlying error surfaces to the caller (a
  transient backend failure is `503` with `Retry-After: 5`); nothing is
  written and the version row is not touched

- The destination backend is **non-durable** and the caller lacks the
  `ADMIN_POLICY` scope — `403` (`Forbidden`)
- Caller lacks `WRITE` authorization on the file — `403`
- Another migration attempt of the same version already running (migration
  lease), the whole attempt exceeding a time budget, and a destination object
  that already exists at the canonical path (destination tail) are **not
  implemented yet** — see [Migration Lease and Destination-Tail
  Resolution](#migration-lease-and-destination-tail-resolution)

- A concurrent migration of the same file already moved the version's
  `(backend_id, backend_path)` pointer away from the snapshot this call
  started from, discovered only once this call's own CAS attempt loses (the
  CAS is currently the only thing serializing concurrent migrations of one
  version — see [Migration Lease and Destination-Tail
  Resolution](#migration-lease-and-destination-tail-resolution))
  — `409` (`Conflict`, "concurrent backend migration in progress"); this
  call's own destination write is cleaned up unless it happens to coincide
  with the winner's
- The version disappears entirely between the pre-migration read and the CAS
  attempt — `404` (`VersionNotFound`); this call's destination write is
  cleaned up as a genuine orphan

**Steps**:
1. [x] - `p1` - Client: POST /api/file-storage/v1/files/{id}/migrate with body {target_backend_id} - `inst-migrate-request`
2. [x] - `p1` - Control plane: load the file scoped to the caller's tenant; authorize `WRITE` on `file_id` - `inst-migrate-authz`
3. [x] - `p1` - Control plane: list the file's versions; RETURN `409` if there is not exactly 1, or if that one version's status is not `Available` - `inst-migrate-single-version-check`
4. [x] - `p1` - **IF** the version's current `backend_id` already equals `target_backend_id`: RETURN success immediately (no-op, no read/write/verify, no audit row) - `inst-migrate-noop`
5. [x] - `p1` - **IF** the target backend's capabilities report `durable == false`: additionally authorize `ADMIN_POLICY` on the file - `inst-migrate-nondurable-gate`
6. [x] - `p1` - Read the whole source blob at the version's `backend_path` into memory - `inst-migrate-read-source`
7. [x] - `p1` - Algorithm: verify the bytes' hash, mode-aware per ADR-0006, using `cpt-cf-file-storage-algo-backend-migration-verify` below, **before** anything is written to the destination. **IF** it fails: RETURN `400` (`HashMismatch`) — no destination write, no CAS - `inst-migrate-verify`
8. [x] - `p1` - Write the bytes to the destination backend at the canonical path `/{file_id}/{version_id}` - `inst-migrate-write-dest`
9. [x] - `p1` - DB: `rebind_version_backend` — CAS the version row's `(backend_id, backend_path)` from the pre-migration snapshot to the destination, in the same transaction as a `BackendMigrate` audit row - `inst-migrate-cas-rebind`
10. [x] - `p1` - **IF** the CAS lost: resolve using `cpt-cf-file-storage-algo-backend-migration-race-resolve` (below) — RETURN `404`/`409`/success-as-no-op depending on what actually happened - `inst-migrate-cas-race`
11. [x] - `p1` - **IF** the CAS won: best-effort delete the source blob (failures logged, not surfaced to the caller — an orphan-cleanup concern, not a migration-correctness one) - `inst-migrate-cleanup-source`
12. [x] - `p1` - RETURN `204 No Content` - `inst-migrate-return`
13. [ ] - `p2` - **Not implemented yet**: DB: acquire a per-version migration lease (CAS on a lease owner/expiry, timed by the database's own clock) before touching any backend, `409` if it is held; release it after the source-blob delete - `inst-migrate-lease-acquire`
14. [ ] - `p2` - **Not implemented yet**: run the transfer, verification and CAS under a time budget; on expiry clean up only a destination object this call created and return a retryable `503` - `inst-migrate-timeout`

## 3. Processes / Business Logic (CDSL)

### Mode-Aware Content-Hash Verification Before Commit

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-migration-verify`

This algorithm runs once per migration attempt, against the bytes read from the
source backend and **before** anything is written to the destination, so a
failed verification leaves nothing behind at the destination. The bytes
written to the destination afterwards are not read back and re-hashed: the
service trusts the destination backend's write path to store exactly the bytes
it was handed. This is unlike upload finalization, which re-reads the stored
object after the write and recomputes its hash/size/MIME from those actual
bytes. A source read that came back corrupted or short is caught by this
verification; a destination write that silently corrupts otherwise-good bytes
is not independently caught by a second read here.

**Input**: the object's bytes as read from the source backend — the version's
`hash_mode` (`whole-sha256` | `multipart-composite-sha256`), its stored
`hash_value`, and — only for `multipart-composite-sha256` — the version's
`version_hash_manifest` row

**Output**: `Ok(())` once every byte is verified, or a
`HashMismatch`/database-consistency error — always known before the
destination write

**Steps**:
1. [x] - `p1` - Parse `version.hash_mode` into `HashMode`; a value the parser does not recognize is a database-consistency error (`DomainError::database`), not a hash mismatch - `inst-verify-migrate-parse-mode`
2. [x] - `p1` - **IF** `HashMode::WholeSha256`: no manifest needed; the bytes are hashed as one contiguous span - `inst-verify-migrate-whole`
3. [x] - `p1` - **IF** `HashMode::MultipartCompositeSha256`: fetch the version's `version_hash_manifest` row up front; its absence is a database-consistency error (every `multipart-composite-sha256` version has exactly one such row by construction — ADR-0006 §5's `1:1` FK); each of its recorded part offsets becomes a span the bytes are hashed against - `inst-verify-migrate-fetch-manifest`
4. [x] - `p1` - Hash each span and, for composite mode, compare each finished span's digest against the manifest's recorded digest for that part; once every span is complete, rebuild the root the same way [Content-Hash Modes](content-hash-modes.md) does and compare it to `hash_value` (whole-object mode compares the single accumulated digest to `hash_value` directly) — this reuses the shared `cpt-cf-file-storage-algo-content-hash-modes-verify` algorithm, reusing its manifest/root construction rather than re-deriving it - `inst-verify-migrate-shared-algo`
5. [x] - `p1` - Compare the total number of bytes read against the version's declared size; a mismatch is also a verification failure - `inst-verify-migrate-length-check`
6. [x] - `p1` - For `multipart-composite-sha256`, this verification is **fully self-contained from the source bytes + the stored manifest row alone** — it has no dependency on the multipart session's `multipart_upload_parts` rows still existing - `inst-verify-migrate-no-parts-dependency`
7. [x] - `p1` - **RETURN** `Ok(())` if the (re-derived) hash and length match; `HashMismatch` otherwise — the destination write and the CAS are skipped on a mismatch - `inst-verify-migrate-return`

### Migration Lease and Destination-Tail Resolution

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-migration-race-resolve`

**Not implemented yet**: the per-version **migration lease**
(`file_versions.migration_lease_owner`/`migration_lease_until`, timed by the
database's own clock, with a `migrate_timeout_secs` time budget for the whole
attempt), the create-exclusive destination write and **destination-tail
rule** that would rely on it, and the pre-CAS re-`stat` of the destination.
None of these exist in the code and there are no corresponding configuration
keys. Today two migrations of the same version can both read, verify and write
the same deterministic destination path; the CAS on the pre-migration
`(backend_id, backend_path)` snapshot is what decides the winner, and the
resolution below makes sure the loser never destroys the winner's blob.

**Input**: the destination blob already written by this call, the
pre-migration `(backend_id, backend_path)` snapshot, and the version row's
*current* state after the CAS attempt reports it lost

**Output**: `Ok(())` (treated as a successful no-op), `Err(VersionNotFound)`,
or `Err(Conflict)` — plus a decision on whether to delete this call's own
destination write

**Steps**:
1. [x] - `p1` - Re-fetch the version row by `(file_id, version_id)` after the CAS reports `updated == false` - `inst-race-refetch`
2. [x] - `p1` - **IF** the version is now gone entirely: best-effort delete this call's destination blob (it is a genuine orphan) and RETURN `VersionNotFound` - `inst-race-gone`
3. [x] - `p1` - **IF** the current row's `(backend_id, backend_path)` already equals **this call's own** destination: a concurrent migration to the identical target won the race first (deterministic canonical path, `/{file_id}/{version_id}`, means both racers wrote to the same location) — RETURN `Ok(())` as a no-op and do **NOT** delete the destination blob, since it is the winner's live content, not this call's to clean up - `inst-race-same-target-winner`
4. [x] - `p1` - **ELSE** (a different concurrent migration won, to a different target): best-effort delete this call's own destination blob (guarded by a belt-and-suspenders re-check that it doesn't coincidentally equal the live pointer for some other reason) and RETURN `Conflict` ("concurrent backend migration in progress") - `inst-race-different-winner`

**Known gap**: with no lease, the CAS is the only thing that serializes
concurrent migrations of one version, and the destination write is not
create-exclusive, so two racers can write the same bytes to the same
deterministic path; this is safe only because both transfer the same
hash-verified content and the resolution above never deletes a blob the live
pointer references. A change made by something outside this gear's
coordination (an operator, direct backend surgery) between the read and the
CAS is not detected. The related re-`stat` and tail handling are tracked as
issue #5013.

## 4. States (CDSL)

**Not applicable.** A version's `(backend_id, backend_path)` pair is a plain
CAS-guarded attribute, not a modeled state machine — every backend/path
combination that resolves to a real, registered backend is a valid value, and
the CAS resolution logic above (§3) is a conflict-resolution algorithm over a
single attribute swap, not a multi-state lifecycle.

## 5. Definitions of Done

### Migrate Endpoint with Hash-Verified Backend Relocation

- [x] `p1` - **ID**: `cpt-cf-file-storage-dod-backend-migration-endpoint`

The system **MUST** implement `POST /api/file-storage/v1/files/{id}/migrate`
for non-versioned files only: read the source blob, verify its hash
mode-awarely against the stored `(hash_mode, hash_value[, manifest], size)`
before writing anything to the destination, write it to the destination
backend at the canonical path, and only then atomically CAS the version row's
backend pointer from the pre-migration snapshot alongside a `BackendMigrate`
audit row. The system **MUST NOT** commit the CAS when verification fails, and
**MUST** resolve a lost CAS without ever destroying a concurrent winner's
blob: best-effort clean up this call's destination object only when it is
not the live pointer, and best-effort clean up the source blob only after the
CAS has won. A transient backend failure **MUST** surface as `503` with
`Retry-After`.

**Not implemented yet**: acquiring a per-version migration lease before
touching any backend (with database-clock expiry), a time budget for the whole
attempt, a create-exclusive destination write with destination-tail
resolution, a pre-CAS re-`stat` of the destination, and a streaming transfer
that never buffers the whole object (tracked as issue #5013 for the residual
race). The code reads the whole blob into memory.

**Implements**:
- `cpt-cf-file-storage-flow-backend-migration`
- `cpt-cf-file-storage-algo-backend-migration-verify`
- `cpt-cf-file-storage-algo-backend-migration-race-resolve`

**Touches**:
- API: `POST /api/file-storage/v1/files/{id}/migrate`
- DB Table: `file_versions`
- DB Table: `version_hash_manifest` (read-only, for multipart-composite versions)
- DB Table: `audit_outbox`

### Migration Lease

- [ ] `p2` - **ID**: `cpt-cf-file-storage-dod-backend-migration-lease`

**Not implemented yet.** When added, the system **MUST** size the migration
lease from a configurable time budget plus a margin, **MUST** reject a
zero value for either, and **MUST** cap both so a stuck migration cannot hold
the lease and block every other attempt at the same version indefinitely.
There are no such configuration keys today.

**Implements**:
- `cpt-cf-file-storage-flow-backend-migration`
- `cpt-cf-file-storage-algo-backend-migration-race-resolve`

**Touches**:
- API: `POST /api/file-storage/v1/files/{id}/migrate`
- DB Table: `file_versions`

### Non-Durable-Target Admin Gate

- [x] `p2` - **ID**: `cpt-cf-file-storage-dod-backend-migration-durability-gate`

The system **MUST** require the elevated `ADMIN_POLICY` authorization scope
(in addition to ordinary `WRITE`) before migrating content onto a backend
whose `capabilities().durable == false` (e.g. the non-durable in-memory
backend), since doing so risks silent data loss on the next process restart.
An ordinary `WRITE`-authorized caller must not be able to trigger this
implicitly.

**Implements**:
- `cpt-cf-file-storage-flow-backend-migration`

**Touches**:
- API: `POST /api/file-storage/v1/files/{id}/migrate`

## 6. Acceptance Criteria

- [x] Migrating a non-versioned file's content to a different backend updates the version row's `backend_id` and writes a `backend_migrate` audit row
- [x] Migrating to the backend the file is already on is a no-op: no audit row is written
- [x] A versioned file (more than 1 version) is rejected with `VersionedFileMigrationNotSupported`
- [x] A non-admin caller is rejected with `Forbidden` when the target backend is non-durable, and the version row is left unchanged
- [x] An admin-scoped caller may migrate onto a non-durable target
- [x] A concurrent migration to a **different** target correctly loses the CAS, gets `Conflict`, and has its own orphaned destination blob cleaned up, while the winner's blob is untouched
- [x] For a `multipart-composite-sha256` version, `migrate_backend` verifies using only the source object bytes and the stored `version_hash_manifest` row — with the multipart session's `multipart_upload_parts` rows already deleted
- [x] `migrate_backend`'s hash check is mode-aware (ADR-0006): whole-object re-hash for `whole-sha256`, split-rehash-rebuild-compare against the stored manifest for `multipart-composite-sha256` — it never hard-codes a whole-object-only comparison
- [x] The migrate endpoint is restricted to non-versioned files by design — this is a permanent scope boundary (see §1.1), not a tracked gap
- [x] A source read that comes back with corrupted bytes (same length, different content) fails verification, leaves the version pointing at the source backend, and leaves nothing behind at the destination
- [x] A concurrent migration to the **same** target that already moved the pointer makes this call's CAS lose; it resolves as a no-op and does **not** delete the winning blob
- [x] A transient backend failure during migration surfaces as `503` with `Retry-After: 5`, and the version stays on the source backend
- [ ] **Not implemented yet**: a second migration of the same version is rejected up front by a migration lease; a migration attempt exceeding a time budget fails with a retryable `503`; a destination tail from an interrupted attempt is reclaimed; the transfer streams without buffering the whole object