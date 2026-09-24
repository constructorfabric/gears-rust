Created:  2026-07-08 by Constructor Tech
Updated:  2026-07-08 by Constructor Tech
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
  - [Concurrent-Migration CAS Resolution](#concurrent-migration-cas-resolution)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Migrate Endpoint with Hash-Verified Backend Relocation](#migrate-endpoint-with-hash-verified-backend-relocation)
  - [Non-Durable-Target Admin Gate](#non-durable-target-admin-gate)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

- [ ] `p2` - `cpt-cf-file-storage-feature-backend-migration`

### 1.1 Overview

`POST /files/{id}/migrate` relocates a **non-versioned** file's content (a
file with exactly one `file_versions` row) from its current storage backend
to a different one, without changing the file's identity (`file_id`,
ownership, metadata) or its content hash. The blob is read from the source
backend, verified against the version's stored hash, written to the
destination backend, and the version row's `(backend_id, backend_path)` are
swapped atomically under a compare-and-swap keyed on the pre-migration
snapshot — all before the source blob is best-effort deleted.

**Traces to**: `cpt-cf-file-storage-fr-backend-migration`, `cpt-cf-file-storage-fr-audit-trail`

### 1.2 Purpose

Let an operator move a file's bytes between backends (e.g. off a
non-durable dev/test backend, or between two durable backends for capacity or
policy reasons) without any downtime or content-identity change from the
caller's point of view — the file's `file_id`, `content_id`/version pointer
shape, and hash all stay the same; only where the bytes physically live
changes. The mandatory hash re-verification before committing the swap means
a corrupted read from the source, or a corrupted write to the destination,
is caught before the file ever points at bad data — the operation either
fully succeeds with a byte-identical copy, or fails and leaves the original
backend binding untouched.

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
- The file has exactly one `Available` version; its content is copied to the
  target backend, the copy's hash is verified against the stored
  `hash_value` (mode-aware per ADR-0006), the version row is atomically
  repointed at the target, a `BackendMigrate` audit row is written, and the
  source blob is best-effort deleted
- Migrating to the backend the file is already on is a no-op: returns success
  immediately, no audit row, no read/write/verify work performed

**Error Scenarios**:
- The file has more than one version (versioned file) — `409`
  (`VersionedFileMigrationNotSupported`); non-versioned files only
- The file's single version is not yet `Available` (still `pending`) — `409`
  (`Conflict`, "cannot migrate a version whose upload has not been finalized")
- The target backend id is unknown — `400` (`UnknownBackend`)
- The re-verified content hash does not match the stored `hash_value` — `400`
  (`HashMismatch`) — the destination write is never committed to the version
  row in this case (the hash check runs **before** the destination `put`)
- The destination backend is **non-durable** and the caller lacks the
  `ADMIN_POLICY` scope — `403` (`Forbidden`)
- Caller lacks `WRITE` authorization on the file — `403`
- A concurrent migration of the same file already moved the version's
  `(backend_id, backend_path)` pointer away from the snapshot this call
  started from — `409` (`Conflict`, "concurrent backend migration in
  progress"); this call's own destination write is cleaned up unless it
  happens to coincide with the winner's (see
  [Concurrent-Migration CAS Resolution](#concurrent-migration-cas-resolution))
- The version disappears entirely between the pre-migration read and the CAS
  attempt — `404` (`VersionNotFound`); this call's destination write is
  cleaned up as a genuine orphan

**Steps**:
1. [x] - `p1` - Client: POST /api/file-storage/v1/files/{id}/migrate with body {target_backend_id} - `inst-migrate-request`
2. [x] - `p1` - Control plane: load the file scoped to the caller's tenant; authorize `WRITE` on `file_id` - `inst-migrate-authz`
3. [x] - `p1` - Control plane: list the file's versions; RETURN `409` if there is not exactly 1, or if that one version's status is not `Available` - `inst-migrate-single-version-check`
4. [x] - `p1` - **IF** the version's current `backend_id` already equals `target_backend_id`: RETURN success immediately (no-op, no read/write/verify, no audit row) - `inst-migrate-noop`
5. [x] - `p1` - **IF** the target backend's capabilities report `durable == false`: additionally authorize `ADMIN_POLICY` on the file - `inst-migrate-nondurable-gate`
6. [x] - `p1` - Read the full blob from the source backend at the version's `backend_path` - `inst-migrate-read-source`
7. [x] - `p1` - Algorithm: verify the blob's hash using `cpt-cf-file-storage-algo-content-hash-modes-verify` (mode-aware per ADR-0006) using `cpt-cf-file-storage-algo-backend-migration-verify` below - `inst-migrate-verify`
8. [x] - `p1` - Write the verified blob to the destination backend at the canonical path `/{file_id}/{version_id}` - `inst-migrate-write-dest`
9. [x] - `p1` - DB: `rebind_version_backend` — CAS the version row's `(backend_id, backend_path)` from the pre-migration snapshot to the destination, in the same transaction as a `BackendMigrate` audit row - `inst-migrate-cas-rebind`
10. [x] - `p1` - **IF** the CAS lost: resolve using `cpt-cf-file-storage-algo-backend-migration-race-resolve` (below) — RETURN `404`/`409`/success-as-no-op depending on what actually happened - `inst-migrate-cas-race`
11. [x] - `p1` - **IF** the CAS won: best-effort delete the source blob (failures logged, not surfaced to the caller — an orphan-cleanup concern, not a migration-correctness one) - `inst-migrate-cleanup-source`
12. [x] - `p1` - RETURN `204 No Content` - `inst-migrate-return`

## 3. Processes / Business Logic (CDSL)

### Mode-Aware Content-Hash Verification Before Commit

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-migration-verify`

**Input**: the object bytes read from the source backend, the version's
`hash_mode` (`whole-sha256` | `multipart-composite-sha256`), its stored
`hash_value`, and — only for `multipart-composite-sha256` — the version's
`version_hash_manifest` row

**Output**: `Ok(())`, or a `HashMismatch`/database-consistency error that
aborts the migration before the destination write

**Steps**:
1. [x] - `p1` - Parse `version.hash_mode` into `HashMode`; a value the parser does not recognize is a database-consistency error (`DomainError::database`), not a hash mismatch - `inst-verify-migrate-parse-mode`
2. [x] - `p1` - **IF** `HashMode::WholeSha256`: no manifest needed - `inst-verify-migrate-whole`
3. [x] - `p1` - **IF** `HashMode::MultipartCompositeSha256`: fetch the version's `version_hash_manifest` row; its absence is a database-consistency error (every `multipart-composite-sha256` version has exactly one such row by construction — ADR-0006 §5's `1:1` FK) - `inst-verify-migrate-fetch-manifest`
4. [x] - `p1` - Call the shared `cpt-cf-file-storage-algo-content-hash-modes-verify` algorithm (owned by [Content-Hash Modes](content-hash-modes.md)) with the blob, mode, `hash_value`, and manifest (`None` for whole-object mode) - `inst-verify-migrate-shared-algo`
5. [x] - `p1` - For `multipart-composite-sha256`, this verification is **fully self-contained from the object bytes + the stored manifest row alone** — it has no dependency on the multipart session's `multipart_upload_parts` rows still existing - `inst-verify-migrate-no-parts-dependency`
6. [x] - `p1` - **RETURN** `Ok(())` if the (re-derived) hash matches; `HashMismatch` otherwise, aborting before any destination write - `inst-verify-migrate-return`

### Concurrent-Migration CAS Resolution

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-migration-race-resolve`

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
mode-awarely against the stored `(hash_mode, hash_value[, manifest])` before
ever writing to the destination, write to the destination backend at the
canonical path, atomically CAS the version row's backend pointer alongside a
`BackendMigrate` audit row, resolve lost-CAS races without ever destroying a
concurrent winner's blob, and best-effort clean up the source blob only after
the CAS has won.

**Implements**:
- `cpt-cf-file-storage-flow-backend-migration`
- `cpt-cf-file-storage-algo-backend-migration-verify`
- `cpt-cf-file-storage-algo-backend-migration-race-resolve`

**Touches**:
- API: `POST /api/file-storage/v1/files/{id}/migrate`
- DB Table: `file_versions`
- DB Table: `version_hash_manifest` (read-only, for multipart-composite versions)
- DB Table: `audit_outbox`

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
- [x] A concurrent migration to the **same** target resolves as a successful no-op and does **not** delete the winning blob
- [x] For a `multipart-composite-sha256` version, `migrate_backend` verifies using only the object bytes and the stored `version_hash_manifest` row — with the multipart session's `multipart_upload_parts` rows already deleted
- [x] `migrate_backend`'s hash check is mode-aware (ADR-0006): whole-object re-hash for `whole-sha256`, split-rehash-rebuild-compare against the stored manifest for `multipart-composite-sha256` — it never hard-codes a whole-object-only comparison
- [x] The migrate endpoint is restricted to non-versioned files by design — this is a permanent scope boundary (see §1.1), not a tracked gap
