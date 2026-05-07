---
status: accepted
date: 2026-05-05
supersedes_revisions: [2026-04-27, 2026-05-04]
---
# ADR-0004: Self-Healing Reconciliation as the Base Correctness Mechanism for Presigned-First Overwrites


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A — Physical key separation for every overwrite](#option-a--physical-key-separation-for-every-overwrite)
  - [Option B — Self-healing via S3-ETag, in-place overwrite for presigned-first](#option-b--self-healing-via-s3-etag-in-place-overwrite-for-presigned-first)
  - [Option C — Backend events / S3 notifications](#option-c--backend-events--s3-notifications)
- [More Information](#more-information)
  - [The single-axis desync invariant](#the-single-axis-desync-invariant)
  - [The two trigger points](#the-two-trigger-points)
  - [Concurrent `reconcile` is safe by construction](#concurrent-reconcile-is-safe-by-construction)
  - [Why `gts_file_type` is excluded from the reconcile pull](#why-gts_file_type-is-excluded-from-the-reconcile-pull)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-self-healing-reconciliation`

## Context and Problem Statement

The presigned-first lifecycle hands the end-client a presigned PUT URL and expects the application's backend to commit the upload by calling `reconcile(file_id)` afterwards. The two writes — bytes-to-S3 and `reconcile`-to-FileStorage-DB — are not atomic. The browser may close, the network may drop, the user may walk away, the application backend itself may crash between receiving the client's ack and forwarding it to FileStorage. In all of these cases:

- The S3 object at the row's derived key has the **new** bytes (S3 PUT succeeded).
- The FileStorage row still says `etag = e_old` and `status = pending_upload` (or `etag = e_old` and `status = uploaded` after a re-upload).
- No future `reconcile` for that `file_id` will ever arrive (because the original caller is gone).

This produces a real-world inconsistency window between FileStorage's database and the S3 backend along the `(content, etag, version_id, S3-mirrored metadata)` axis. **`gts_file_type` cannot drift** because it lives exclusively in the FileStorage database and is never written to the S3 object (specific exception to the meta-mirror rule — see `cpt-cf-file-storage-constraint-meta-mirrored-via-put-meta`). The presigned PUT URL pins all other mirrored fields into the SigV4 signature; even if the client wanted to inject something else, the backend would reject the PUT with `SignatureDoesNotMatch`.

The decision is **how to detect and repair this drift** — and whether detection has to happen synchronously (preventing the desync from forming in the first place) or whether eventual repair is acceptable.

## Decision Drivers

- **Bounded drift surface** — only `(content, etag, version_id, S3-mirrored metadata)` can drift. `gts_file_type` is DB-only and cannot drift. S3 is the authoritative source of `etag` (returned in the `ETag` HTTP header on every `GetObject` / `HeadObject`) and of every other mirrored field. Repair from S3 to DB is always well-defined.
- **No external infrastructure** — the project is committed to "FileStorage runs against an S3-shaped bucket and a SQL DB, that's it" (per ADR-0001). Solutions that require S3 event notifications (SQS, EventBridge, webhooks) trade architectural simplicity for marginally faster repair. They also fragment per backend kind: future `webdav` would have no event surface.
- **Lifecycle correctness, not just data preservation** — even if data is preserved, the application's downstream business logic (antivirus, LLM, UI rendering) depends on the row reaching `Uploaded`. Repair must converge to a state where reads succeed with consistent `(etag, content, mirrored metadata)` and consumers see the post-upload semantics they expected.
- **Cost of physical-key separation** — keeping every overwrite in a fresh physical key is a clean way to eliminate the desync window entirely (the row only ever points at one set of bytes), but it forces an orphan-delete grace period sized to the maximum signed-URL TTL, requires a sparse pending-key column on every row, and complicates `delete_file` and supersession transactions.
- **Backend-side preconditions are uneven** — AWS S3, modern MinIO, and modern Ceph RGW honour `If-Match` on `PutObject`; GCS S3-compat silently ignores it; many on-prem appliances are inconsistent. Building the correctness story around backend-enforced `If-Match` would either bake a per-backend capability matrix into P1 or make GCS a second-class deployment. P1 deliberately ships without any backend-side preconditions on the upload presign path (see ADR-0003) and lets self-healing carry the full correctness load.

## Considered Options

* Option A — Physical key separation for every overwrite
* Option B — Self-healing via S3-ETag, in-place overwrite for presigned-first
* Option C — Backend events / S3 notifications

## Decision Outcome

Chosen option: **Option B — Self-healing via S3-ETag, in-place overwrite for presigned-first**.

Self-healing is the **base correctness mechanism** for the presigned-first overwrite path. The contract:

- The presigned PUT URL targets the row's existing backend object key (derived from `file_id`). No new physical key is minted on overwrite.
- After a presigned PUT, the row's `etag` (and any other mirrored field that the client could legitimately have changed via the SigV4-pinned headers) may diverge from the actual S3 object — until the next FileStorage operation that touches both: either an explicit `reconcile(file_id)` (eager, REST-driven) or a `read_file(file_id, …)` (lazy, in-process).
- Both triggers run a conditional UPDATE that copies the S3-side state into the row. The repair runs under the same optimistic-concurrency contract as every other mutation (DESIGN §2.1 / `cpt-cf-file-storage-principle-optimistic-concurrency`); a concurrent caller that beat us to the repair simply leaves us with a no-op UPDATE and a still-correct row.
- `reconcile` is concurrent-safe by construction: HEAD S3 first, then conditional UPDATE; if the UPDATE matches `0` rows because somebody else's `reconcile` already converged the row, the implementation re-reads, confirms the row is at the post-reconcile state, and returns the same `Ok`.

`reconcile` is the explicit reconciliation primitive. Its REST counterpart is `POST /files/{file_id}/meta/reconcile`, which rejects `If-Match` with `400`. The SDK takes only `file_id`; FileStorage HEADs the backend itself and is the sole arbiter of the row's etag, version_id, and S3-mirrored metadata.

`reconcile` always pulls **all** mirrored metadata from the HEAD response (etag, version_id, Content-Type, Content-Disposition, every `x-amz-meta-<k>`, Content-Length). It **never** pulls `gts_file_type` from S3 — that field is DB-only and is preserved as-is on the row. See [Why `gts_file_type` is excluded from the reconcile pull](#why-gts_file_type-is-excluded-from-the-reconcile-pull).

For the proxy `put_file` in-process SDK convenience (P1 ships as a stub), the future implementation will drive the full lifecycle internally and end with an automatic `reconcile`.

### Consequences

- Good, because **the database schema simplifies**: the backend object key is derived deterministically from `file_id` at the adapter boundary; no separate column for it, no separate "pending key" column. The orphan-delete grace period for already-issued download URLs only needs to cover `delete_file` (Phase 2), not presigned overwrite.
- Good, because **GCS S3-compat is a first-class deployment**. Self-healing does not depend on `If-Match`; it works on every S3-compatible backend that returns `ETag` on `GetObject` / `HeadObject` (which is universal).
- Good, because **the drift axis is narrow and well-defined**: bytes, etag, version_id, and S3-mirrored metadata. `gts_file_type` cannot drift. There is no class of inconsistency self-healing cannot repair within that axis.
- Good, because **consumers see eventual consistency, not pinned errors**. A reader that pinned `etag = e_pinned` and finds the row at `e_old` after a zombie upload sees `EtagMismatch{ current: derived }` with the **already-repaired** etag in the error — they retry once and proceed. A reader that did not pin gets a transparent repair and a consistent `FileReadHandle`.
- Good, because **`reconcile` is the single explicit reconciliation endpoint** — application backends do not need to plumb a possibly-wrong S3 etag through their own status surfaces. The chat-frontend's "I observed ETag=e2 from S3" hint, if any, is irrelevant to FileStorage; FileStorage HEADs S3 and trusts only that.
- Bad, because **a row that no one ever reads or reconciles can stay desynced forever**. The application backend's downstream business logic (antivirus, LLM scan) is gated on the application calling `reconcile` (or the file being read). This is mitigated by the application-side responsibility to call `reconcile` after expecting an upload, and by the optional P2 sweep that runs `reconcile` on aged `pending_upload` rows. Neither the sweep is in P1; in P1 the application backend is responsible for re-poking files whose `reconcile` never came.
- Bad, because **the system-context UPDATE** that performs the repair (lazy in-process trigger only — `reconcile` runs under the caller's `SecurityContext`) runs without a `SecurityContext`. This is a privileged maintenance operation; we explicitly exempt it from the `cpt-cf-file-storage-constraint-no-ambient-authn` rule. Doc'd in DESIGN §2.2.
- Bad, because **detection of "object missing from backend" still requires a HEAD**. `read_file`'s lazy path does the GET anyway, so detection is free for that flow; `get_file_info` reads only from the SQL row and does **not** detect that the backend lost the object — that gap is closed by the P2 inverse-sweep GC pass.

### Confirmation

- DESIGN.md §3.9 / "Self-healing reconciliation" subsection documents the two trigger points (lazy `read_file`, eager `reconcile`).
- The `migration.sql` schema has `etag` (raw S3 ETag) and `version_id` (raw S3 VersionId, nullable) columns; backend-key separation is intentionally absent.
- A regression test in the testkit reproduces the zombie-upload scenario:
  1. `create_presigned_url` for a new file or `create_presigned_overwrite_url` for an existing one.
  2. PUT new bytes directly via `aws-sdk-s3` against `s3s-fs`.
  3. **Skip** any explicit reconcile.
  4. Call `read_file(file_id, etag = None)` → returns new bytes with the repaired etag; row in DB is at the new etag.
  5. Subsequent call `reconcile(file_id)` → returns `Ok` with the same etag (no-op, idempotent).
- A second regression test reproduces the etag-pinning scenario:
  1. Same setup as above.
  2. Call `read_file(file_id, etag = Some(e_pinned_old))` → returns `EtagMismatch { current: derived }` where `derived` matches the actual S3 bytes.
  3. The row in DB is at `derived` — not at `e_pinned_old` — confirming self-healing fired.
- A third regression test reproduces the `reconcile`-driven commit:
  1. `create_presigned_url`.
  2. External PUT.
  3. Call `reconcile(file_id)` → returns `Ok(ReconcileResult)`; row is `uploaded` with `etag = s3_etag`.
  4. Concurrent `reconcile` from another caller → also returns `Ok` (idempotent convergence).

## Pros and Cons of the Options

### Option A — Physical key separation for every overwrite

Every overwrite (presigned-first AND any future proxy mode) mints a new backend object key. The row carries a sparse pending-key column while a write is in flight; an atomic single-statement UPDATE swaps the active key on commit. The previous key is queued for orphan-delete with a grace period covering the longest signed-URL TTL.

* Good, because the desync window for presigned-first is **eliminated by construction** — there is no point in time at which the row's `(etag, key)` tuple disagrees with the bytes the active key resolves to. A reader pinned to `e_pinned` always reads the bytes that produced `e_pinned`, end-to-end.
* Good, because already-issued signed download URLs inherit the orphan-delete grace period — they always resolve to a coherent snapshot.
* Bad, because **every row carries a sparse pending-key column** plus an extra index, even though the column is meaningful only during a small fraction of row-life.
* Bad, because **the orphan-delete grace period must cover the maximum signed-URL TTL** (sometimes 7 days for AWS), tying GC scheduling to URL configuration. This couples two concerns that are otherwise independent.
* Bad, because **the contract for `delete_file` becomes more complex**: it must defer the actual key-deletion to the orphan queue rather than deleting synchronously, and the queue must guarantee TTL-respecting eligibility.
* Bad, because **the simplicity argument from `cpt-cf-file-storage-principle-stream-by-default` is undermined**: presigned-first is supposed to stay off the data plane, but its commit path now requires bookkeeping (queue an orphan, swap a pointer) that proxy-mode-style coordinator already does.

### Option B — Self-healing via S3-ETag, in-place overwrite for presigned-first

Presigned-first overwrites reuse the row's existing backend object key. The S3 PUT lands at the same key the row points at; the row's `etag` (and other mirrored fields) may briefly diverge from the actual content fingerprint until the next FileStorage operation observes the divergence and runs a conditional UPDATE to repair. `reconcile` is the eager reconciliation primitive; `read_file` is the lazy in-process trigger.

* Good, because **the row schema simplifies** — no pending-key column; the backend object key is derived from `file_id` at the adapter boundary.
* Good, because **GCS S3-compat works as a first-class backend** without `If-Match`. The data `reconcile` reads (etag, version_id, Content-Type, Content-Disposition, x-amz-meta-*, Content-Length) is universally available on S3 HEAD responses.
* Good, because **`reconcile` is a single explicit reconciliation primitive** that is concurrent-safe by construction (HEAD first, then conditional UPDATE, retry-loop on contention).
* Bad, because **a desync window exists** between successful S3 PUT and the next read or `reconcile`. During this window, a reader without pinned etag would see the new bytes but the old `etag` field — but the reader doesn't actually see the disagreement because self-healing repairs the row before returning the response.
* Bad, because **a row that no one reads or reconciles stays desynced indefinitely**. Application backends that need eager post-upload processing (antivirus, LLM) cannot rely on FileStorage to surface "the file is now committed" — they need to call `reconcile` after expecting an upload, or rely on the optional sweeper (P2). This is a real cost.
* Bad, because **system-context maintenance UPDATEs** (the lazy in-process repair on `read_file`) require an explicit exemption from the no-ambient-authn rule. The exemption is narrow and documented but does add a privileged code path.

### Option C — Backend events / S3 notifications

S3 (and MinIO, Ceph) emits `s3:ObjectCreated` events to SQS/SNS/EventBridge/webhook. FileStorage subscribes; on an event for a known backend object key whose row is in `pending_upload`, FileStorage promotes the row.

* Good, because **detection is real-time** — the desync window collapses to the event-delivery latency.
* Good, because **`reconcile` becomes redundant** for the corner case where the application backend dies — events do the commit unconditionally.
* Bad, because **the project is committed to "S3 + SQL only" architecturally** (per ADR-0001 and §3.5). SQS/SNS/EventBridge/Kafka is a new operational dependency.
* Bad, because **events do not exist for future `webdav`**. Self-healing would still be needed for that.
* Bad, because **events are "at-least-once"**: the receiver must dedupe and handle stale events. This is more complexity, not less.
* Bad, because **GCS S3-compat does not emit S3 events** through the interoperability surface (GCS has its own Pub/Sub-based notifications, mapped via the JSON API, not visible through `storage.googleapis.com` S3 endpoint).

## More Information

### The single-axis desync invariant

The presigned PUT URL pins, via SigV4 SignedHeaders, every system-managed metadata field that FileStorage wants to land on the S3 object: `Content-Type` (from `meta.mime_type`), `Content-Disposition` (from `meta.name`), every `x-amz-meta-<k>` (from `meta.custom_metadata`). A client cannot send different values without breaking the signature; the backend rejects with `SignatureDoesNotMatch`.

Therefore: after a successful presigned PUT, every field except `(content, etag, version_id, S3-mirrored metadata that the client could legitimately set via the pinned headers)` matches what FileStorage planned at presign time. **`gts_file_type` cannot drift** because it is DB-only.

This is why self-healing is well-defined: there is exactly one bounded axis to repair, and S3 is the authoritative source of truth for that axis.

### The two trigger points

Self-healing runs on:

1. **`reconcile(file_id)` — eager, REST-driven, concurrent-safe by construction.**
   - SELECT row by file_id with tenant scope. `Deleting` → `DeleteInProgress`.
   - HEAD `derive(file_id)` on backend → `(s3_etag, s3_version_id, content_type, content_disposition, content_length, x-amz-meta-*)`. 404 → `BackendFailure`.
   - Build `new_meta_from_s3` (`name` from Content-Disposition, `mime_type` from Content-Type, `custom_metadata` from `x-amz-meta-*`, `size_bytes` from Content-Length, `gts_file_type` kept from DB).
   - Conditional UPDATE setting `status = uploaded, etag, version_id, name, mime_type, custom_metadata, size_bytes, updated_at` with the row's `(etag, updated_at[, xmin])` in the WHERE clause.
   - `0` rows → race detected; retry up to 3 times. After 3 unsuccessful attempts → `Conflict`.
   - Return `ReconcileResult { info, s3_etag, s3_version_id }`.

2. **`read_file(file_id, etag?)` — lazy, in-process.**
   SELECT row → `e_db = row.etag`. Open backend GET on the derived key; capture `s3_etag` (and other metadata) from the response. If `s3_etag != row.etag`, run a system-context conditional UPDATE pulling the same fields `reconcile` pulls.
     - If the caller pinned `etag = Some(e)` and `e != s3_etag` → return `Err(EtagMismatch{ current: s3_etag })` AFTER repair.
     - If the caller pinned no etag → return `Ok(FileReadHandle { info: refreshed, bytes: stream })` transparently.

### Concurrent `reconcile` is safe by construction

Two concurrent `reconcile(file_id)` calls produce the same final state regardless of interleaving:

1. Both HEAD S3, observe the same `(s3_etag, s3_version_id, …)`.
2. Both attempt the conditional UPDATE with the same captured `(etag_db, updated_at_db)` from their respective SELECTs.
3. The first to land the UPDATE wins (1 row affected).
4. The second sees `0` rows affected, re-SELECTs in the retry loop, observes the row already at the post-reconcile state, attempts the same SET → succeeds as a no-op-equivalent (or retries until convergence).

Both callers return `Ok(ReconcileResult)` with the same `s3_etag` and `s3_version_id`. A `Conflict` outcome surfaces from `reconcile` only when contention from a different operation (e.g. `put_file_info` rotating the row in parallel) keeps the retry loop from converging within 3 attempts.

### Why `gts_file_type` is excluded from the reconcile pull

Two reasons:

1. **Authz dependency.** Every authz call uses `gts_file_type` as the resource type. A scenario where a malicious operator (or a backend bug) injected a different `x-amz-meta-gts-file-type` on the S3 object would, if reconcile blindly pulled it, downgrade or upgrade the file's authz scope without any FileStorage-side decision. Keeping the field DB-only forecloses that vector entirely.
2. **Asymmetry with the upload contract.** FileStorage NEVER signs `x-amz-meta-gts-file-type` into the presigned PUT (initial upload or variant-B re-upload). The S3 object simply does not carry the field through the FileStorage code paths. If somebody put it there out-of-band, that is not a state FileStorage should adopt.

The cost is asymmetry inside reconcile: every other meta field is pulled from S3, this one is preserved. The cost is paid in code clarity (the algorithm has one explicit exception) and in the form of `cpt-cf-file-storage-constraint-meta-mirrored-via-put-meta`'s exception clause. The benefit — closing the authz-spoof vector — outweighs the asymmetry.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md) §2.1 (`cpt-cf-file-storage-principle-optimistic-concurrency`, `cpt-cf-file-storage-principle-multi-phase-commit`), §3.9 ("Self-healing reconciliation").
- **Related ADRs**: [ADR-0001](./0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md), [ADR-0002](./0002-cpt-cf-file-storage-adr-opaque-file-ids.md), [ADR-0003](./0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md) (P1 ships PUT-SigV4 without any backend-side preconditions; this ADR carries the full correctness load), [ADR-0005](./0005-cpt-cf-file-storage-adr-versioning-and-aba.md) (versioning and ABA-CAS strategy).
- **Companion specs**: [rust-traits.md](../rust-traits.md) (`read_file`, `reconcile`, `create_presigned_url`, `create_presigned_overwrite_url` doc-comments)

This decision directly addresses:

* `cpt-cf-file-storage-fr-upload-file` — overwrite via presigned-first reuses the derived backend key; correctness held by `reconcile`-driven reconciliation and lazy self-healing on next read.
* `cpt-cf-file-storage-fr-download-file` — `read_file` is the lazy in-process self-healing trigger.
* `cpt-cf-file-storage-fr-conditional-requests` — the etag the API returns on conditional checks is always the post-repair etag; consumers do not see the desync window.
* `cpt-cf-file-storage-principle-optimistic-concurrency` — self-healing UPDATEs run under the same `(etag, updated_at[, xmin])`-conditional contract as every other mutation; the principle is preserved.
* `cpt-cf-file-storage-principle-stream-by-default` — presigned-first stays off the data plane; self-healing's HEAD-on-divergence is bounded to the rare race window.
* `cpt-cf-file-storage-component-files-repo` — owns the repair UPDATE and the `reconcile` retry loop.
