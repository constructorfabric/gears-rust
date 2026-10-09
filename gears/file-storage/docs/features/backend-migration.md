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
ownership, metadata) or its content hash. Because the destination path is
deterministic (`/{file_id}/{version_id}`), two migration attempts of the
*same* version would otherwise race on the identical destination object; this
is prevented up front by a per-version **migration lease**
(`file_versions.migration_lease_owner`/`migration_lease_until`, timed by the
database's own clock) — a second attempt while the lease is held gets `409
Conflict` immediately, before it ever touches a backend. The whole attempt —
transfer, verification, and the commit CAS — then runs under a
`migrate_timeout_secs` time budget; exceeding it best-effort deletes only the
destination object this call itself created and fails with a retryable `503`.

Once the lease is held, the blob is streamed directly from the source backend
into the destination backend — never fully buffered in memory — with its
content hash verified incrementally as the bytes stream past; the pass/fail
verdict is only known once the destination has finished receiving the whole
stream. If it fails, nothing is committed and the destination object is
best-effort cleaned up (only if this call itself created it — see below). If
a destination object already exists at the canonical path before this call
ever wrote anything there, the lease removes the need to read it back and
re-verify it: nothing else can be a *legitimate* concurrent writer to this
exact path while the lease is held, so the version's own pointer already
answers the question — still on the pre-migration source snapshot means the
object is a tail an earlier, interrupted attempt left behind (safe to delete
and retry once), while a pointer that has already moved means a concurrent
migration already claimed the path as live content (left untouched, `409
Conflict`, no read-back, no hash check). If verification passes, the
destination object's presence and size are re-confirmed immediately before
the swap — this narrows but does not close the window before the version
row's `(backend_id, backend_path)` are then swapped atomically under a
compare-and-swap keyed on the pre-migration snapshot AND this call's own
lease ownership (a known limitation for an external actor outside this
gear's own coordination, tracked as issue #5013; see below) — all before the
source blob is best-effort deleted, and the lease released.

**Traces to**: `cpt-cf-file-storage-fr-backend-migration`, `cpt-cf-file-storage-fr-audit-trail`

### 1.2 Purpose

Let an operator move a file's bytes between backends (e.g. off a
non-durable dev/test backend, or between two durable backends for capacity or
policy reasons) without any downtime or content-identity change from the
caller's point of view — the file's `file_id`, `content_id`/version pointer
shape, and hash all stay the same; only where the bytes physically live
changes. The mandatory hash re-verification of the source stream before
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
- Another migration attempt of this exact version already holds the
  migration lease — `409` (`Conflict`, "a migration of this version is
  already in progress"); returned before this call ever touches a backend —
  see [Migration Lease and Destination-Tail Resolution](#migration-lease-and-destination-tail-resolution)
- The re-verified content hash (or the transferred length) does not match
  the stored version — `400` (`HashMismatch`) — the version row is never
  updated in this case. Because verification can only complete once the
  whole stream has been transferred, the destination object may already
  physically exist by the time the mismatch is discovered; this call cleans
  it up best-effort, but **only if this call itself created it** — see
  [Mode-Aware Content-Hash Verification Before Commit](#mode-aware-content-hash-verification-before-commit)
- The destination object already exists before this call ever wrote anything
  (an earlier migration attempt of the same version got interrupted after
  writing but before reaching its own commit) and the version's pointer is
  still on the pre-migration source — treated as a tail, not a mismatch: the
  object is deleted under this call's own held lease and the publish is
  retried once, with no read-back or hash check of the stale bytes at all —
  see [Migration Lease and Destination-Tail Resolution](#migration-lease-and-destination-tail-resolution)
- The destination object already exists and the version's pointer has
  already moved off the pre-migration source (a concurrent migration —
  necessarily one that has since taken over the lease, since this call still
  holds it as of its own read — already claimed this path as live content) —
  `409` (`Conflict`); the object is left untouched, never read back, never
  deleted
- The source stream breaks before finishing (a transport error mid-transfer)
  — the underlying transport error surfaces to the caller; same non-commit,
  same conditional cleanup as a hash mismatch
- The destination object vanishes, or its size changes, between this call's
  own successful verification and the CAS immediately below — always
  treated as a transient, concurrent-change race: `503`
  (`service_unavailable`) with `Retry-After`; the CAS is never attempted and
  the version stays on the source backend, and retrying the migration is
  safe
- The destination backend is **non-durable** and the caller lacks the
  `ADMIN_POLICY` scope — `403` (`Forbidden`)
- Caller lacks `WRITE` authorization on the file — `403`
- The whole attempt (transfer + verification + CAS) exceeds
  `migrate_timeout_secs` — `503` (`service_unavailable`) with `Retry-After`;
  a destination object this call itself created is best-effort deleted
  (never one it did not write, and never once its own CAS attempt has
  started — see [Migration Lease and Destination-Tail Resolution](#migration-lease-and-destination-tail-resolution)),
  the version's pointer is never observed to change, and retrying is safe
- A concurrent migration of the same file already moved the version's
  `(backend_id, backend_path)` pointer away from the snapshot this call
  started from, discovered only once this call's own CAS attempt loses (the
  narrow window this can still happen in despite the lease — see
  [Migration Lease and Destination-Tail Resolution](#migration-lease-and-destination-tail-resolution))
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
6. [x] - `p1` - DB: `acquire_migration_lease` — CAS the version's `migration_lease_owner`/`migration_lease_until` from free-or-expired to a fresh owner + `migrate_timeout_secs + migrate_lease_margin_secs`, timed by the database's own clock. **IF** it loses: RETURN `409` (`Conflict`, "a migration of this version is already in progress") immediately — `inst-migrate-lease-acquire`
7. [x] - `p1` - Run the rest of this flow up to and including the source-blob delete (steps 8-15 below; the lease release in step 16 is outside it) under a `tokio::time::timeout(migrate_timeout_secs, ..)`. **IF** it elapses: best-effort delete the destination object, but only if this call is known to have created it AND its own CAS attempt (step 13) had not yet started; RETURN a retryable error (`503 service_unavailable` with `Retry-After`); the version's pointer is never observed to change - `inst-migrate-timeout`
8. [x] - `p1` - Open a stream from the source backend at the version's `backend_path` and write it, chunk by chunk, straight to the destination backend at the canonical path `/{file_id}/{version_id}` (create-exclusive — see [Migration Lease and Destination-Tail Resolution](#migration-lease-and-destination-tail-resolution) for why), never buffering the whole object - `inst-migrate-stream-transfer`
9. [x] - `p1` - Algorithm: verify the stream's hash incrementally, mode-aware per ADR-0006, using `cpt-cf-file-storage-algo-backend-migration-verify` below; its verdict is only available once the destination has finished receiving the stream - `inst-migrate-verify`
10. [x] - `p1` - **IF** verification of the streamed source content failed (hash mismatch, wrong length, or the source stream broke before finishing): do **not** proceed to the CAS step; best-effort delete the destination object, but only if this call is the one that created it (see below) - `inst-migrate-verify-failed-no-commit`
11. [x] - `p1` - **IF** source verification passed but the destination object already existed before this call wrote anything (`created: false`): re-fetch the version row and compare its `(backend_id, backend_path)` against the pre-migration snapshot this call started from, rather than reading the object back at all — the lease already held since step 6 rules out any other *legitimate* concurrent writer to this exact path. **IF** the pointer is still on that snapshot: the object is a tail an earlier, interrupted attempt at this same path left behind — best-effort delete it and retry the publish exactly once (a second `created: false` after that retry is `Conflict`, no further retry). **IF** the pointer has already moved: RETURN `409` (`Conflict`) without deleting the object at all - `inst-migrate-tail-resolve`
12. [x] - `p1` - Immediately before the CAS step, re-`stat` the destination object and confirm it still exists at the expected size. **IF** it does not: do **not** proceed to the CAS step; RETURN a retryable error (`503 service_unavailable` with `Retry-After`; retrying the migration is safe) - `inst-migrate-precommit-stat`
13. [x] - `p1` - DB: `rebind_version_backend` — CAS the version row's `(backend_id, backend_path)` from the pre-migration snapshot to the destination AND `migration_lease_owner` from this call's own owner, in the same transaction as a `BackendMigrate` audit row; the lease itself is left held on a win (see step 16) - `inst-migrate-cas-rebind`
14. [x] - `p1` - **IF** the CAS lost: resolve using `cpt-cf-file-storage-algo-backend-migration-race-resolve` (below) — RETURN `404`/`409`/success-as-no-op depending on what actually happened - `inst-migrate-cas-race`
15. [x] - `p1` - **IF** the CAS won: best-effort delete the source blob (failures logged, not surfaced to the caller — an orphan-cleanup concern, not a migration-correctness one) - `inst-migrate-cleanup-source`
16. [x] - `p1` - Best-effort release the migration lease regardless of how the attempt above ended, only after step 15's source-delete attempt has already run on a won CAS — releasing any earlier would let a second migration of the same version re-acquire the lease and move it back onto the backend step 15 is still about to delete from (the destination path is deterministic, so both migrations target the same path), turning step 15's delayed delete into one that destroys live content instead of the stale object it was meant to remove; a release that itself fails is not fatal, the lease simply expires on its own - `inst-migrate-lease-release`
17. [x] - `p1` - RETURN `204 No Content` - `inst-migrate-return`

## 3. Processes / Business Logic (CDSL)

### Mode-Aware Content-Hash Verification Before Commit

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-migration-verify`

This algorithm runs once per migration attempt, against the object's bytes as
they stream from the source backend to the destination — never against a
pre-existing destination object read back after the fact. A destination
object that already sits at the canonical path before this call ever wrote
anything there is resolved entirely from the version's own pointer under this
call's held migration lease (see [Migration Lease and Destination-Tail
Resolution](#migration-lease-and-destination-tail-resolution)), never by
reading it back and re-hashing it: the lease already rules out any other
*legitimate* concurrent writer to this exact deterministic path, which is
what makes a pointer-only check sufficient in place of a content re-check.

The verification below is computed against the bytes on their way *to* the
destination, as they are written — it is never re-derived from a second,
independent read of the object once the write finishes. The service trusts
the destination backend's write path to durably store exactly the bytes
`publish_exclusive` was handed once that call reports success; it does not
re-open and re-hash the object it just wrote to confirm the bytes landed
intact. This is unlike upload finalization, which does re-read the stored
object after the write and recomputes its hash/size/MIME from those actual
on-disk bytes before trusting it. A source read that came back corrupted, or
a source stream that broke mid-transfer, is still caught by this same
verification (it sees those bad or short bytes as they stream past); a
destination write that silently corrupts otherwise-good bytes on the way in
is not independently caught by a second read here.

**Input**: the object's bytes, streamed from the source backend as they are
written to the destination (never buffered whole) — the version's
`hash_mode` (`whole-sha256` | `multipart-composite-sha256`), its stored
`hash_value`, its declared size, and — only for `multipart-composite-sha256`
— the version's `version_hash_manifest` row

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
7. [x] - `p1` - **RETURN** `Ok(())` if the (re-derived) hash and length match; `HashMismatch` otherwise. Because the destination write and the verification happen on the same streamed pass, this verdict is necessarily known only *after* the destination has already received the (possibly bad) bytes — the CAS step is skipped either way, and a failed verification triggers the destination cleanup described in [Migration Lease and Destination-Tail Resolution](#migration-lease-and-destination-tail-resolution) - `inst-verify-migrate-return`

### Migration Lease and Destination-Tail Resolution

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-migration-race-resolve`

**The migration lease** (`file_versions.migration_lease_owner`/
`migration_lease_until`) makes at most one migration attempt of a given
version live at a time, closing the race a deterministic destination path
would otherwise invite between two independent `migrate_backend` calls of
the *same* version. It is acquired with a single conditional `UPDATE` before
this call ever writes to a backend: it matches only when no lease is
currently held (`migration_lease_until IS NULL`) or the held one has already
expired, and both that check and the new expiry are computed **by the
database itself** — `now()`/`CURRENT_TIMESTAMP`, never this instance's own
clock — so two instances racing to acquire the same lease read the exact
same notion of "now" no matter how far their wall clocks have drifted apart.
A losing acquire returns `Conflict` (409) immediately, before any backend is
ever touched. The lease's duration is `migrate_timeout_secs +
migrate_lease_margin_secs`: the timeout bounds the attempt itself, and the
margin absorbs the gap between the database's clock and this instance's own
`tokio::time::timeout` (which still fires on its local clock) plus the tail
of a backend call already in flight when that local timeout fires.

**Why the destination write is still create-exclusive, not overwrite**:
even with the lease serializing migration attempts of one version against
each other, create-exclusive is what lets this call tell "nothing has ever
been written here" apart from "something already has" without a read —
`publish_exclusive`'s `created` flag is exactly that signal, and it is what
[Mode-Aware Content-Hash Verification Before Commit](#mode-aware-content-hash-verification-before-commit)
and the destination-tail rule below both key off.

**Destination-tail rule** (`created: false`, i.e. something already sits at
the deterministic destination path before this call's own write): resolved
entirely from the version's own pointer, under this call's held lease,
without ever reading the object back:
- the pointer is **still on the pre-migration source** snapshot this call
  started from → nothing has claimed this object as live content (the lease
  rules out any other legitimate writer to this exact path right now), so it
  can only be a tail an earlier attempt at this same path left behind —
  wrote successfully, then crashed, was cancelled, or timed out before ever
  reaching its own CAS. Delete it and retry the publish exactly once; a
  second `created: false` after that retry stops the retries and returns
  `Conflict` instead of looping.
- the pointer has **already moved** → a concurrent migration (to this same
  target or a different one) has already claimed this path as live content.
  Leave it untouched — never read, never hash-checked, never deleted — and
  fail with `Conflict`.

This replaces reading the pre-existing object back and re-verifying its
hash: the lease is what makes that unnecessary. A caller that gets
`Conflict` here can simply retry `migrate_backend` — if the concurrent
migration it lost to happened to be targeting the very backend this call
also wanted, the retry's own top-of-flow "already on target" check resolves
it as a no-op immediately, without ever reaching this algorithm again.

Combined with the fact that both racers under contention are transferring
the *same*, already-hash-committed version, this is what keeps a same-target
race safe without requiring either racer to
know about the other — every racer under contention necessarily lost the
lease acquire and never reached the transfer step at all, so the only way
this section's recovery logic below still triggers is the narrower set of
races the lease does not cover (see the "Known gap" paragraph below).

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

**Known gap**: the lease closes every race *between migration attempts of
this gear*, but not a change made by something outside that coordination
entirely — an external actor, an operator, direct backend surgery, or this
gear's own timeout racing its own CAS (see `migrate_backend`'s doc for why a
cancelled-but-maybe-already-applied CAS is handled by simply not attempting
a cleanup in that ambiguous case, rather than risking deleting a pointer
that just went live). The pre-CAS `stat` re-check narrows, but does not
close, the window immediately before the commit — it is not atomic with the
CAS itself, so an object deleted or rewritten strictly between the `stat`
and the CAS is not caught by it. This remains tracked as issue #5013.

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
for non-versioned files only: acquire a per-version migration lease before
touching any backend, stream the source blob directly to the destination
backend at the canonical path (create-exclusive, never fully buffered in
memory) under a time budget, verify its hash mode-awarely against the stored
`(hash_mode, hash_value[, manifest], size)` incrementally as it streams, and
only once that verification passes proceed to atomically CAS the version
row's backend pointer — gated additionally on this call's own lease
ownership — alongside a `BackendMigrate` audit row.

The system **MUST** reject a second migration attempt of the same version
with `409 Conflict` while a live migration lease is held, and **MUST**
compute both the lease's expiry check and its new expiry value using the
database's own clock, never the acquiring instance's, so that instance clock
skew can never make a live lease look expired (or an expired one look live)
to a competing attempt. The system **MUST** run the transfer, verification,
and CAS as one unit under a `migrate_timeout_secs` time budget; when that
budget is exceeded, it **MUST NOT** delete a destination object unless this
call is known to have created it, and **MUST NOT** attempt that cleanup at
all once its own CAS attempt has started, and **MUST** surface the timeout
as a retryable `503 service_unavailable` with `Retry-After`.

When the destination object already exists before this call ever wrote
anything there (`created: false`), the system **MUST NOT** read it back or
hash-check it — the held lease already rules out any other legitimate
concurrent writer to this exact deterministic path, so the version's own
pointer alone **MUST** decide the outcome: still on the pre-migration source
snapshot **MUST** be resolved as a tail from an earlier interrupted attempt
(delete it and retry the publish exactly once; a second `created: false`
after that retry **MUST** stop retrying and return `Conflict`), while a
pointer that has already moved elsewhere **MUST** leave the object untouched
and return `Conflict` without deleting anything.

Immediately before committing the CAS, the system **MUST** re-confirm the
destination object still exists at the expected size, and **MUST NOT**
commit the CAS otherwise; a vanished or resized object at this point **MUST**
surface as `503 service_unavailable` with `Retry-After` (a concurrent-change
race, not a fault) rather than `500`. This pre-CAS re-`stat` narrows, but
does not eliminate, the race between verification and commit for a change
made by something outside this gear's own coordination: it is not atomic
with the CAS itself, so an object deleted or rewritten strictly between the
`stat` and the CAS is not caught by it — a known limitation, tracked as
issue #5013.

The system **MUST NOT** commit the CAS when any of these verifications
fails, and **MUST** resolve lost-CAS races without ever destroying a
concurrent winner's blob, best-effort cleaning up a destination object only
when doing so cannot destroy a concurrent winner's already-live content
(this call created it and its own verification failed, or the destination
tail rule above found it safe to reclaim), and best-effort clean up the
source blob only after the CAS has won. The system **MUST** best-effort
release the migration lease on every exit path, whether or not the release
itself succeeds.

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

- [x] `p1` - **ID**: `cpt-cf-file-storage-dod-backend-migration-lease`

The system **MUST** size the migration lease as `migrate_timeout_secs +
migrate_lease_margin_secs`, and **MUST** reject a `migrate_timeout_secs` or
`migrate_lease_margin_secs` configuration value of `0` (a zero timeout would
abort every attempt before it could transfer any bytes; a zero margin leaves
no slack for clock skew or an in-flight backend request that outlives the
timeout). Both **MUST** be capped (`MAX_MIGRATE_TIMEOUT_SECS`,
`MAX_MIGRATE_LEASE_MARGIN_SECS` respectively) so an oversized configuration
value cannot let a stuck migration hold the lease, and therefore block every
other attempt at the same version, indefinitely.

**Implements**:
- `cpt-cf-file-storage-flow-backend-migration`
- `cpt-cf-file-storage-algo-backend-migration-race-resolve`

**Touches**:
- API: `POST /api/file-storage/v1/files/{id}/migrate`
- DB Table: `file_versions`
- Config: `migrate_timeout_secs`, `migrate_lease_margin_secs`

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
- [x] A second migration attempt of the same version is rejected with `Conflict` while the first attempt's migration lease is still live, before any backend is touched, and neither the pointer nor the first attempt's destination object is affected
- [x] Once a live lease expires (database-clock-side), a new migration attempt of the same version acquires it and proceeds normally
- [x] A destination tail from an earlier, interrupted attempt at the same deterministic path (object present, no live lease, version's pointer still on the pre-migration source) is deleted and the publish retried once, and the migration completes successfully with the correct content
- [x] When the migration lease is taken over by a different owner between this call's own transfer and its CAS (an out-of-band change to the lease row), the CAS loses, the version's pointer is left unchanged, and the call fails
- [x] A migration attempt that exceeds `migrate_timeout_secs` fails with a retryable `503 service_unavailable` + `Retry-After`, the version's pointer stays on the source backend, and the migration lease is released
- [x] `migrate_timeout_secs` and `migrate_lease_margin_secs` both reject `0` and both reject a value above their respective ceiling
- [x] A concurrent migration to a **different** target correctly loses the CAS, gets `Conflict`, and has its own orphaned destination blob cleaned up, while the winner's blob is untouched
- [x] A concurrent migration to the **same** target that lands before this call's own publish (`created: false`, pointer already moved) is rejected with `Conflict` and does **not** delete the winning blob; retrying the migration then resolves as a no-op via the top-of-flow "already on target" check
- [x] For a `multipart-composite-sha256` version, `migrate_backend` verifies using only the streamed object bytes and the stored `version_hash_manifest` row — with the multipart session's `multipart_upload_parts` rows already deleted
- [x] `migrate_backend`'s hash check is mode-aware (ADR-0006): whole-object incremental re-hash for `whole-sha256`, incremental split-rehash-rebuild-compare against the stored manifest for `multipart-composite-sha256` — it never hard-codes a whole-object-only comparison
- [x] The migrate endpoint is restricted to non-versioned files by design — this is a permanent scope boundary (see §1.1), not a tracked gap
- [x] The source-to-destination transfer never materializes the whole object in memory: an object that arrives in many chunks streams through in many chunks, for both content-hash modes
- [x] A source read that comes back with corrupted bytes (same length, different content) fails verification, leaves the version pointing at the source backend, and leaves nothing behind at the destination
- [x] A source stream that breaks before finishing fails the migration the same way — version untouched, destination left empty
- [x] A destination object vanishes between this call's own successful verification and the CAS — the migration fails with `503 service_unavailable` + `Retry-After` (a concurrent-change race, not a fault), and the version stays on the source backend; retrying is safe
