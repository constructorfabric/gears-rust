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
ownership, metadata) or its content hash. The blob is streamed directly from
the source backend into the destination backend — never fully buffered in
memory — with its content hash verified incrementally as the bytes stream
past; the pass/fail verdict is only known once the destination has finished
receiving the whole stream. If it fails, nothing is committed and the
destination object is best-effort cleaned up (see below). If it passes, the
version row's `(backend_id, backend_path)` are swapped atomically under a
compare-and-swap keyed on the pre-migration snapshot — all before the source
blob is best-effort deleted.

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
- The file has exactly one `Available` version; its content streams to the
  target backend while its hash is verified incrementally (mode-aware per
  ADR-0006) against the stored `hash_value`; once the transfer completes and
  verification passes, the version row is atomically repointed at the
  target, a `BackendMigrate` audit row is written, and the source blob is
  best-effort deleted
- Migrating to the backend the file is already on is a no-op: returns success
  immediately, no audit row, no read/write/verify work performed

**Error Scenarios**:
- The file has more than one version (versioned file) — `409`
  (`VersionedFileMigrationNotSupported`); non-versioned files only
- The file's single version is not yet `Available` (still `pending`) — `409`
  (`Conflict`, "cannot migrate a version whose upload has not been finalized")
- The target backend id is unknown — `400` (`UnknownBackend`)
- The re-verified content hash (or the transferred length) does not match
  the stored version — `400` (`HashMismatch`) — the version row is never
  updated in this case. Because verification can only complete once the
  whole stream has been transferred, the destination object may already
  physically exist by the time the mismatch is discovered; this call cleans
  it up best-effort, but **only if this call itself created it** — see
  [Mode-Aware Content-Hash Verification Before Commit](#mode-aware-content-hash-verification-before-commit)
- The source stream breaks before finishing (a transport error mid-transfer)
  — the underlying transport error surfaces to the caller; same non-commit,
  same conditional cleanup as a hash mismatch
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
6. [x] - `p1` - Open a stream from the source backend at the version's `backend_path` and write it, chunk by chunk, straight to the destination backend at the canonical path `/{file_id}/{version_id}` (create-exclusive — see [Concurrent-Migration CAS Resolution](#concurrent-migration-cas-resolution) for why), never buffering the whole object - `inst-migrate-stream-transfer`
7. [x] - `p1` - Algorithm: verify the stream's hash incrementally, mode-aware per ADR-0006, using `cpt-cf-file-storage-algo-backend-migration-verify` below; its verdict is only available once the destination has finished receiving the stream - `inst-migrate-verify`
8. [x] - `p1` - **IF** verification failed (hash mismatch, wrong length, or the source stream broke before finishing): do **not** proceed to the CAS step; best-effort delete the destination object, but only if this call is the one that created it (see below) - `inst-migrate-verify-failed-no-commit`
9. [x] - `p1` - DB: `rebind_version_backend` — CAS the version row's `(backend_id, backend_path)` from the pre-migration snapshot to the destination, in the same transaction as a `BackendMigrate` audit row - `inst-migrate-cas-rebind`
10. [x] - `p1` - **IF** the CAS lost: resolve using `cpt-cf-file-storage-algo-backend-migration-race-resolve` (below) — RETURN `404`/`409`/success-as-no-op depending on what actually happened - `inst-migrate-cas-race`
11. [x] - `p1` - **IF** the CAS won: best-effort delete the source blob (failures logged, not surfaced to the caller — an orphan-cleanup concern, not a migration-correctness one) - `inst-migrate-cleanup-source`
12. [x] - `p1` - RETURN `204 No Content` - `inst-migrate-return`

## 3. Processes / Business Logic (CDSL)

### Mode-Aware Content-Hash Verification Before Commit

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-migration-verify`

**Input**: the object's bytes, streamed from the source backend as they are
written to the destination (never buffered whole), the version's `hash_mode`
(`whole-sha256` | `multipart-composite-sha256`), its stored `hash_value`, its
declared size, and — only for `multipart-composite-sha256` — the version's
`version_hash_manifest` row

**Output**: `Ok(())` once the whole stream has been transferred and every
byte verified, or a `HashMismatch`/database-consistency error — available
only after the destination has finished receiving the stream, never before

**Steps**:
1. [x] - `p1` - Parse `version.hash_mode` into `HashMode`; a value the parser does not recognize is a database-consistency error (`DomainError::database`), not a hash mismatch - `inst-verify-migrate-parse-mode`
2. [x] - `p1` - **IF** `HashMode::WholeSha256`: no manifest needed; the stream is hashed as one contiguous span - `inst-verify-migrate-whole`
3. [x] - `p1` - **IF** `HashMode::MultipartCompositeSha256`: fetch the version's `version_hash_manifest` row up front; its absence is a database-consistency error (every `multipart-composite-sha256` version has exactly one such row by construction — ADR-0006 §5's `1:1` FK); each of its recorded part offsets becomes a span the stream is hashed against as the corresponding bytes pass through - `inst-verify-migrate-fetch-manifest`
4. [x] - `p1` - As bytes stream past, hash each span incrementally and, for composite mode, compare each finished span's digest against the manifest's recorded digest for that part; once every span is complete, rebuild the root the same way [Content-Hash Modes](content-hash-modes.md) does and compare it to `hash_value` (whole-object mode compares the single accumulated digest to `hash_value` directly) — this is the streaming form of the shared `cpt-cf-file-storage-algo-content-hash-modes-verify` algorithm, reusing its manifest/root construction rather than re-deriving it - `inst-verify-migrate-shared-algo`
5. [x] - `p1` - Independently track the total number of bytes streamed against the version's declared size; a mismatch (the source yielded too few or too many bytes) is also a verification failure - `inst-verify-migrate-length-check`
6. [x] - `p1` - For `multipart-composite-sha256`, this verification is **fully self-contained from the streamed bytes + the stored manifest row alone** — it has no dependency on the multipart session's `multipart_upload_parts` rows still existing - `inst-verify-migrate-no-parts-dependency`
7. [x] - `p1` - **RETURN** `Ok(())` if the (re-derived) hash and length match; `HashMismatch` otherwise. Because the destination write and the verification happen on the same streamed pass, this verdict is necessarily known only *after* the destination has already received the (possibly bad) bytes — the CAS step is skipped either way, and a failed verification triggers the destination cleanup described in [Concurrent-Migration CAS Resolution](#concurrent-migration-cas-resolution) - `inst-verify-migrate-return`

### Concurrent-Migration CAS Resolution

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-migration-race-resolve`

**Why the destination write is create-exclusive, not overwrite**: the
destination path is deterministic (`/{file_id}/{version_id}`), so two
concurrent migrations to the *same* target both write to the identical
location. Verification only completes once each racer's own transfer has
already landed at the destination (see the previous section), so an
ordinary overwriting write would let a second racer's bytes silently replace
a first racer's — and if the second racer's own source read turns out
corrupted, that overwrite would destroy an already-good copy before its own
verification even has a chance to fail. Writing create-exclusive closes
this: whichever racer's bytes land first stays there untouched: a second
racer to the same path always finds the object already present and leaves
it alone, regardless of what its own (possibly bad) bytes would have been.
Combined with the fact that both racers are transferring the *same*,
already-hash-committed version, this is what keeps a same-target race safe
without requiring either racer to know about the other.

**Input**: the destination blob already written by this call (either
because this call created it, or because it already existed — see above),
the pre-migration `(backend_id, backend_path)` snapshot, and the version
row's *current* state after the CAS attempt reports it lost

**Output**: `Ok(())` (treated as a successful no-op), `Err(VersionNotFound)`,
or `Err(Conflict)` — plus a decision on whether to delete this call's own
destination write

**Steps**:
1. [x] - `p1` - Re-fetch the version row by `(file_id, version_id)` after the CAS reports `updated == false` - `inst-race-refetch`
2. [x] - `p1` - **IF** the version is now gone entirely: best-effort delete this call's destination blob (it is a genuine orphan) and RETURN `VersionNotFound` - `inst-race-gone`
3. [x] - `p1` - **IF** the current row's `(backend_id, backend_path)` already equals **this call's own** destination: a concurrent migration to the identical target won the race first (deterministic canonical path, `/{file_id}/{version_id}`, means both racers wrote to the same location) — RETURN `Ok(())` as a no-op and do **NOT** delete the destination blob, since it is the winner's live content, not this call's to clean up - `inst-race-same-target-winner`
4. [x] - `p1` - **ELSE** (a different concurrent migration won, to a different target): best-effort delete this call's own destination blob (guarded by a belt-and-suspenders re-check that it doesn't coincidentally equal the live pointer for some other reason) and RETURN `Conflict` ("concurrent backend migration in progress") - `inst-race-different-winner`

The same "only delete what this call actually created" rule governs the
verification-failure path (previous section): a failed verification deletes
the destination object only when this call's own write is what created it —
never an object that already existed there, since that content was put there
by someone else's already-verified transfer and is not this call's to touch.

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
for non-versioned files only: stream the source blob directly to the
destination backend at the canonical path (create-exclusive, never fully
buffered in memory), verify its hash mode-awarely against the stored
`(hash_mode, hash_value[, manifest], size)` incrementally as it streams, and
only once that verification passes proceed to atomically CAS the version
row's backend pointer alongside a `BackendMigrate` audit row. It MUST NOT
commit the CAS when verification fails, and MUST resolve lost-CAS races
without ever destroying a concurrent winner's blob, best-effort cleaning up
a destination object only when this call is the one that created it, and
best-effort clean up the source blob only after the CAS has won.

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
- [x] For a `multipart-composite-sha256` version, `migrate_backend` verifies using only the streamed object bytes and the stored `version_hash_manifest` row — with the multipart session's `multipart_upload_parts` rows already deleted
- [x] `migrate_backend`'s hash check is mode-aware (ADR-0006): whole-object incremental re-hash for `whole-sha256`, incremental split-rehash-rebuild-compare against the stored manifest for `multipart-composite-sha256` — it never hard-codes a whole-object-only comparison
- [x] The migrate endpoint is restricted to non-versioned files by design — this is a permanent scope boundary (see §1.1), not a tracked gap
- [x] The source-to-destination transfer never materializes the whole object in memory: an object that arrives in many chunks streams through in many chunks, for both content-hash modes
- [x] A source read that comes back with corrupted bytes (same length, different content) fails verification, leaves the version pointing at the source backend, and leaves nothing behind at the destination
- [x] A source stream that breaks before finishing fails the migration the same way — version untouched, destination left empty
