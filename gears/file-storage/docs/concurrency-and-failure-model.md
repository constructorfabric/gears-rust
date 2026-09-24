# FileStorage — Concurrency & Failure Model of the Upload Flow

**ID**: `cpt-cf-file-storage-doc-concurrency-failure-model`

The exhaustive state / race / failure model of the write path, **as implemented** — every claim below is
traceable to a function in `gears/file-storage/file-storage/src/`. Companion to [DESIGN.md](./DESIGN.md)
(§3.6 upload sequences and the auto-bind-on-finalize behavior, §4.5 signed URLs, §4.7 multipart worked
example) and [api.md](./api.md) (wire contract — `X-FS-Bound` / `bind_state` / `202 completing` are all documented
there too). Where this document and those disagree, the code cited here wins; file an issue.

- [1. Ground rules](#1-ground-rules)
- [2. State graph](#2-state-graph)
  - [2.1 Single-part upload](#21-single-part-upload)
  - [2.2 Multipart upload](#22-multipart-upload)
  - [2.3 Clock & timeout inventory](#23-clock--timeout-inventory)
- [3. Failure matrix](#3-failure-matrix)
- [4. Race catalog](#4-race-catalog)
- [5. Invariants](#5-invariants)
- [6. Traceability](#6-traceability)

## 1. Ground rules

Two properties shape everything below:

1. **No DB transaction or lock is ever held across network I/O.** Every state transition is either a
   single conditional `UPDATE ... WHERE <expected state>` (a CAS) or a short multi-statement transaction
   whose statements are all local DB writes. Backend I/O (S3 `PutObject`/`UploadPart`/
   `CompleteMultipartUpload`, local-fs writes, the read-back, the MIME-sniff read) always happens
   **between** transitions, never inside one. Consequence: a crashed participant leaves a *state*, never
   a held lock — recovery is always "observe the state, CAS forward".
2. **Every retry of a keyed create or an upload/multipart op is safe by construction; a keyless `POST /files` is not.**
   Idempotency is not best-effort: `publish_exclusive` refuses
   double-writes at the backend (`infra/backend/*::publish_exclusive`), finalize converges replays
   (`domain/service/write.rs::finalize_upload_by_token`), `complete` replays its persisted result
   (`domain/multipart_service.rs::replay_completed`), and the content pointer moves only via
   `FileRepo::bind_content_cas` (`infra/storage/repo/file_repo.rs`) — one atomic
   `UPDATE files SET content_id = :new WHERE file_id = :id AND content_id IS NULL / = :expected`
   (PRD `cpt-cf-file-storage-fr-conditional-requests`, §5.10 amendment). A retried `POST /files`
   **with** an `idempotency_key` replays the stored ticket; **without** one it creates a second file
   (see F1) — only the keyed path is idempotent.

## 2. State graph

### 2.1 Single-part upload

`POST /files` (default `bind: "auto"`, no `multipart` block or a plan that collapses to one part) → `PUT` →
done. Two requests total (DESIGN §3.6 amendment).

```text
 (none) ──create──► file row (content_id NULL) + version PENDING
                          │  client PUT bytes → sidecar
                          ▼
                    bytes streaming ──publish_exclusive──► blob published (immutable)
                          │  sidecar → control finalize callback
                          ▼
                    version AVAILABLE ──(bind_on_finalize claim, same tx)──► content_id CAS
                          │                                                    │
                          ▼                                                    ▼
                    PUT 200 + X-FS-Bound: true + ETag              PUT 200 + X-FS-Bound: conflict
                                                                   + X-FS-Current-ETag (version stays
                                                                     AVAILABLE, manually bindable)
```

| # | Transition | DB writes | Backend I/O | What is in flight / which clock bounds it |
|---|---|---|---|---|
| S1 | create | **One tx**: `files` insert (`content_id NULL`) + `file_versions` insert (`pending`) + custom metadata + audit + event outbox [+ idempotency row] — `Store::create_file_with_pending_version_and_event` (`infra/storage/store/files.rs`). Signed URL minted **before** the tx (pure CPU, `FileService::sign_url_with_bind`) | none | The `POST /files` HTTP request. Nothing long-running |
| S2 | bytes streaming | none | Sidecar streams chunks to the backend while hashing incrementally (`bin/sidecar.rs::upload`), enforcing `max_size` mid-stream (`413`) and `exact_size`/`expected_hash` at end-of-stream. Publish is **create-exclusive** (`publish_exclusive`) — a replay against an already-published path writes nothing | The client's `PUT` connection, for the whole stream. Bounded by the token's `exp` *at request start* (verification precedes streaming; `exp` is not re-checked mid-stream) and by `FS_SIDECAR_MAX_BODY_BYTES` (default 5 GiB). Time between chunks is separately bounded by a per-chunk idle timeout, `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` (default 60 s, `idle_timeout_stream`): a client that goes idle mid-stream longer than that gets `408`, and the partial object is handled exactly like F2 below. There is deliberately **no** absolute deadline on the stream's total duration — a slow-but-alive multi-GiB upload that never pauses past the idle limit still completes. The stream may therefore outlive the token's own TTL, and the finalize callback that follows it accepts an `exp` already in the past for up to `finalize_token_grace_secs` (default 1 h) — without that grace a slow-but-live upload would get its bytes durably published and then have finalize rejected as an expired token. Past TTL + grace the callback does reject, so the *effective* ceiling on a single upload is `default_url_ttl_secs + finalize_token_grace_secs`; see the `token exp` row in §2.3 |
| S3 | finalize | **One tx**: version `pending → available` CAS (`VersionRepo::finalize` requires `status = 'pending'`) + audit + — when the verified token carries `bind_on_finalize` — the `content_id IS NULL` bind CAS + current-flag flip + bind audit + `file.content_updated` event (`Store::finalize_version` with `AutoBindOnFinalize`, `infra/storage/store/versions.rs`) | **Before** the tx: the finalize handler re-verifies the same PUT token (`Verifier::verify_with_grace`, `infra/signed_url/mod.rs`, called from `api/rest/handlers.rs` on the finalize path) — signature, `op` and the `(file_id, version_id)` binding strictly, `exp` against finalize-time with `finalize_token_grace_secs` of slack (S2/§2.3) — then control plane re-reads the whole blob (`read_back_and_hash_streaming`, streamed) to independently recompute size/hash, plus MIME magic-byte validation on the leading 8 KiB (`infra/content/mime.rs`) | The client's `PUT` is **still open**: the sidecar answers only after the finalize callback returns. The callback wraps its **entire** retry loop — all `CALLBACK_MAX_ATTEMPTS = 3` attempts and the 100 ms delays between them — in one deadline sized to `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` (default **10 s**, plus `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` for the connect phase of each attempt); it is a **total budget** for the whole loop, not a per-attempt allowance (`post_with_retry`, `bin/sidecar.rs`), so the callback never holds the `PUT` open for longer than that configured value regardless of how many attempts it takes. ⚠ For very large single-part objects the read-back can still exceed that budget — see F5 in the matrix |
| S4 | report to client | none | none | Sidecar copies the finalize response's `x-fs-bound`/`etag`/`x-fs-current-etag` headers verbatim onto its `200` (`uploaded_response`, `bin/sidecar.rs`); it never interprets them (ADR-0003 amendment) |

`bind: "manual"` (or `POST /files/{id}/versions`, which never mints the bind claim —
`infra/signed_url/mod.rs::Claims::bind_on_finalize`) stops after "version AVAILABLE"; the client issues the
classic `POST /files/{id}/bind` (`FileService::bind`, `If-Match` CAS).

### 2.2 Multipart upload

`POST /files` with the `multipart` block (plan ≥ 2 parts) → `PUT × N` → `POST complete`. N+2 requests.
The session is a lease-guarded state machine; `bind` happens inside `complete` for `auto_bind` sessions.

```text
 (none) ──create+plan──► file row + version PENDING + session IN_PROGRESS(auto_bind)
                              │ per part: (not uploaded) ──sidecar PUT──► written ──report-part──► REPORTED (row in multipart_upload_parts)
                              ▼ client POST complete
                    IN_PROGRESS ──acquire lease CAS──► COMPLETING(lease_owner, lease_until)
                          losers ──► 202 {state: completing}                │ detached assembly task
                                                                            ▼
                                                       CompleteMultipartUpload + 8 KiB MIME sniff
                                                                            │
                                                       finalize tx (available + bind CAS) ── on error: release-lease CAS ──► IN_PROGRESS
                                                                            ▼
                                                       COMPLETED(complete_result JSON)   [ABORTED via client abort / cleanup sweep]
```

| # | Transition | DB writes | Backend I/O | In flight / clocks |
|---|---|---|---|---|
| M1 | create+plan | Tx A: file row only (`Store::create_file_with_event` — **no** single-part pending version, so no presign orphan); then `file_versions` pending insert; then session insert (`MultipartRepo::create`, `auto_bind` recorded). Session-insert failure triggers best-effort compensation (`compensate_failed_session_create`, aborts the backend handle + deletes the pending version row). More generally, an error from **any** step of `initiate_multipart_upload` after Tx A — quota/capability rejection, backend initiate failure, or the session-insert failure above — makes the handler synchronously call `FileService::compensate_failed_multipart_initiate`: a best-effort, guarded delete of the versionless `files` row (`Store::delete_orphan_file_with_event`, the same primitive the sweep uses, re-verifying zero versions + `content_id IS NULL` inside the delete transaction). A control-plane crash between Tx A and the version insert, or a failed compensation itself, leaves that versionless row behind; it is reclaimed by the cleanup sweep's step 1 second phase (`sweep_versionless_files`) once it ages past `orphan_grace_secs` | `StorageBackend::initiate_multipart` (S3 `CreateMultipartUpload`) between the version insert and the session insert | The `POST /files` request. Per-part URLs share one `exp` (`default_url_ttl_secs`); the session gets its own `expires_at` (`multipart_session_ttl_secs`) |
| M2 | part upload | none by the sidecar; the **control plane** upserts the part row on the token-authenticated report callback (`MultipartStore::upsert_multipart_part` — single upsert) | Sidecar streams the part (`upload_part` / S3 `UploadPart`), enforcing the token's **exact** `size` claim, hashing on the fly | The part's `PUT` connection; report callback bounded like finalize (10 s / 3 attempts). Part URL `exp` bounds *starting* an upload; the session `expires_at` bounds the whole endeavour (two-clock model, DESIGN §4.7 Phase C) |
| M3 | acquire lease | **Single CAS**: `state='completing', lease_until=now+K, lease_owner=:me WHERE upload_id=:id AND (state='in_progress' OR (state='completing' AND lease_until < now))` — `MultipartRepo::acquire_complete_lease`. One statement covers fresh acquire **and** dead-owner takeover | none | Read-only pre-flight (missing-parts diff, size, policy) runs **before** the CAS, so a deterministic rejection never occupies the lease. `K = multipart_complete_lease_secs` (default 120 s) |
| M4 | assembly | none | Winner's **detached task** (`tokio::spawn` in `complete_multipart_upload`; the HTTP handler awaits its `JoinHandle`, but a dropped request future cannot cancel the work): S3 `CompleteMultipartUpload` (manifest + root folded from reported part rows — **no re-read**, ADR-0006), then one ~8 KiB ranged `get_range` for MIME sniffing. Takeover recovery: if the backend handle was already consumed but the assembled object exists, `(manifest, root)` are rebuilt locally from the part rows (`assemble_and_finish_inner`) | The client's `complete` request is open but expendable (F5-safe). The lease clock bounds how long the state stays `completing` unobserved |
| M5 | finalize (+bind) | **One tx**: version `pending → available` + hash/manifest row + audit + (`session.auto_bind`) the bind CAS against the `content_id` validated by the endpoint's `If-Match` + bind audit + event (`Store::finalize_version`) | none | — |
| M6 | finish | **One tx**: CAS `completing → completed` + persist `complete_result` JSON (`StoredCompleteResult`) + audit — `MultipartRepo::finish_complete`. On a failed assembly instead: release-lease CAS `completing → in_progress WHERE lease_owner = :me` (`release_complete_lease`), so the next `complete` retries immediately | none | — |

### 2.3 Clock & timeout inventory

| Clock | Default | Bounds | Source |
|---|---|---|---|
| token `exp` | 15 min (`default_url_ttl_secs`, hard cap `max_url_ttl_secs` ≤ 7 d) | starting any signed operation (PUT/part/GET); *not* an in-flight stream — but the same token is re-verified by the control plane on the finalize/report-part callbacks (S3), there with the grace below, so the effective scope is URL issuance through finalize completion rather than request start alone (see S2) | `config.rs`; DESIGN §4.5 |
| finalize token grace | 1 h (`finalize_token_grace_secs`); `0` enforces `exp` strictly | how far past `exp` the **s2s finalize/report-part callbacks** still accept the upload token they were handed — the slack that lets an upload slower than its own TTL still commit. Never applies to the sidecar's check at the start of a PUT/part/GET | `config.rs`, `infra/signed_url/mod.rs::verify_with_grace` |
| session `expires_at` | 24 h (`multipart_session_ttl_secs`) | the whole multipart endeavour incl. resume; `complete` rejects past it | `config.rs`; DESIGN §4.7 |
| completion lease | 120 s (`multipart_complete_lease_secs`) | one `complete`'s exclusive assembly window; takeover after | `config.rs`, `multipart_service.rs` |
| body idle timeout | 60 s per chunk-to-chunk gap (and before the first chunk); `0` disables | max pause in an in-flight `PUT`/part-`PUT` body stream (S2 above) — *not* the stream's total duration, which stays unbounded as long as chunks keep arriving within this gap (the token `exp` re-check on the finalize callback, plus the grace below, caps the *useful* total — see the two rows above and below); on expiry the client gets `408` and the partial object is handled like F2 | `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS`, `bin/sidecar.rs::idle_timeout_stream` |
| finalize/report callback | `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` (default 10 s) — a **total budget** for the whole retry loop (all `CALLBACK_MAX_ATTEMPTS = 3` attempts and the 100 ms delays between them), not a per-attempt allowance | sidecar → control callbacks, *after* the body stream (above) has already finished | `FS_SIDECAR_FINALIZE_TIMEOUT_SECS`, `bin/sidecar.rs` |
| cleanup grace | `orphan_grace_secs` | when abandoned `pending` versions / expired sessions become sweepable | `domain/cleanup.rs` |

## 3. Failure matrix

"Cleanup" = the P2 cleanup engine (`domain/cleanup.rs`,
`cpt-cf-file-storage-fr-orphan-reconciliation`): sweeps `pending` versions older than
`orphan_grace_secs` (+ their blobs, + zero-version orphan `files` rows), and aborts expired sessions —
`in_progress` past `expires_at`, **and** `completing` past `expires_at` whose lease has also expired
(`MultipartRepo::list_expired`, `abort_expired_completing` — a live lease is never reaped mid-flight).

| State when the failure hits | Client dies | Control plane dies | Sidecar dies | Backend (S3) down | Network drops after the op, before the response |
|---|---|---|---|---|---|
| F1: after create, before any bytes | `pending` version (+ file row) idles; **garbage**: pending version, swept by cleanup after grace; file row swept once versionless. Retry `POST /files` with the same `idempotency_key` replays the same ticket (fresh-signed URL) instead of a second file | create is one tx — either fully committed or nothing. Client retry: idempotency replay (committed) or plain re-create (not) | n/a (not involved yet) | n/a | same as "control plane dies": the tx committed; an idempotency-key retry converges on it, a keyless retry makes a second file (first becomes cleanup fodder) |
| F2: mid-stream (single-part PUT / part PUT) — includes a client that goes **idle** past `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` (`idle_timeout_stream` ends the stream with the same kind of `Err` a genuinely broken connection would produce, so a stalled-but-still-connected client is handled identically to one that actually disconnected, just answered `408` instead of a connection error) | Sidecar sees the broken stream, never publishes / never reports; best-effort deletes the partial object. Version stays `pending` (single-part → cleanup fodder; multipart → the part is simply "missing", resume via `GET status`) | stream is sidecar↔backend; unaffected until finalize/report — see F5 | client gets a connection error (or, for an idle-timeout, an explicit `408`); nothing published (create-exclusive publish is all-or-nothing per object; an S3 part that never completed is invisible). Retry the PUT / the part with the same token (until `exp`) or a fresh resume URL | sidecar's backend write fails → 5xx to client; partial object best-effort deleted; retry later. If S3 stays down: version stays `pending` → cleanup | n/a (the op did not finish) |
| F3: blob published, finalize callback never sent (sidecar crashed in between) | — | — | Version stays `pending` with real bytes at `backend_path` — **garbage** until: (a) the client retries the `PUT` (replay: `publish_exclusive` refuses the write, finalize is still attempted with the retry's digest and converges — `bin/sidecar.rs::upload`'s `!created` decision table), or (b) cleanup sweeps blob+row after grace | — | — |
| F4: finalize succeeded, response lost (client saw no 200 / sidecar died before answering) | Version is `available` (+bound if auto). Client retries the `PUT`: publish refused (`created: false`), finalize replays and **converges idempotently** — same size/hash against the stored version → 200 (`finalize_upload_by_token`'s already-`available` branch; mismatched bytes still rejected via hash compare). That convergence branch applies **regardless of the token's bind mode** — the sidecar publishes every single-part upload through the same replay-safe `publish_exclusive` path whether or not the token carries `bind_on_finalize`, so a manual-bind retry converges exactly like an auto-bind one, simply with no bind-outcome headers to report. `409` ("version already finalized") is now reserved for a genuine **concurrent** double-finalize (two callbacks racing before either observes the other's already-`available` status), not for an ordinary sequential retry — still never a silent overwrite. See operations.md's 502-retry guidance. The replay's `X-FS-Bound`/`ETag` headers are recomputed against the file's *current* `content_id`, so a concurrent bind in between can flip `bound`↔`conflict` | finalize tx committed or not — atomic. Not committed: F3. Committed: this cell | same convergence via PUT replay | n/a | this **is** that case |
| F5: finalize in progress, control plane dies (or the 10 s callback timeout fires mid-read-back) | — | Handler future is cancelled wherever it's parked: cancelled **before** `Store::finalize_version`'s `COMMIT` (the common case — read-back dominates the wall-clock) → tx never committed → version stays `pending`, blob published = F3; cancelled **after** `COMMIT` → version is already `available` (+ bound, if auto-bind) = F4. Sidecar retries the callback up to 3× (transport/timeout only), then answers `502` (fresh publish) or `409` (replay) — a `502` is not proof nothing happened, only that the callback did not return a response; the client re-`PUT`s later and converges either way. ⚠ a single-part object whose read-back reliably exceeds 10 s will loop here — raise `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` or use multipart (its complete has no full read-back, ADR-0006) | — | read-back fails → finalize 5xx → F3 recovery | — |
| F6: session `in_progress`, some parts reported, client dies | Durable part rows survive (`multipart_upload_parts`). Anyone with `{file_id, upload_id}` resumes: `GET /files/{id}/multipart/{upload_id}` → `received` + `missing` with fresh URLs (`introspect_multipart_upload`) — until `expires_at`, after which cleanup aborts the session (backend `AbortMultipartUpload`, part rows + pending version deleted) and resume gets `404` | part reports are lost only for parts whose callback didn't land — those parts read as "missing", re-uploadable. No corruption | in-flight parts lost (→ missing), reported parts durable | parts can't be written until S3 returns; session survives up to `expires_at` | a part written but its report lost: part reads as missing; re-uploading it hits S3 `UploadPart` idempotently (same part number overwrites) and re-reports |
| F7: `completing`, winner alive | The **detached task keeps running** (client disconnect ≠ cancellation); result is persisted. Any later `complete` (F5-refresh, other tab) gets `202` while the lease is live, then the replayed result | see F8 | sidecar not involved in complete | assembly fails → release-lease CAS → back to `in_progress`, error returned; immediate retry possible | response lost after `completed`: re-`complete` replays `complete_result` verbatim — idempotent by design |
| F8: `completing`, winner (control-plane node) dies mid-assembly | — | State stuck at `completing` until `lease_until` passes; then the next `complete` **takes over** via the same acquire CAS: version already `available` → finish-only fast path; backend handle consumed but object assembled → local manifest rebuild from part rows; otherwise full re-assembly (`assemble_and_finish_inner`). If nobody ever calls `complete` again: once `expires_at` *and* the lease have both passed, cleanup aborts the session (this doc's model; `list_expired`) | — | takeover inherits the same S3-down behaviour as F7 | — |
| F9: `completed`, bind was `conflict` | Nothing is lost: the version is `available`; the response (or its idempotent replay) carries `current_etag`; one manual `POST bind` with that `If-Match` makes it live — **no re-upload** (PRD §5.10) | — | — | — | — |

## 4. Race catalog

1. **Two concurrent `complete`s.** Both pass the read-only pre-flight; exactly one wins the lease CAS
   (single conditional UPDATE — the DB serializes it). The loser immediately re-reads the session:
   `completing` → `202 {state: "completing", retry_after_secs}` (+`Retry-After`); `completed` → replay.
   Polling = re-issuing the same idempotent call. No double assembly on the success path.
2. **Takeover after a dead lease owner.** The acquire CAS's second arm
   (`state='completing' AND lease_until < now`) makes takeover the *same* operation as acquisition — no
   separate recovery protocol. The taker distrusts the dead owner's progress and re-derives it from
   durable state: version row status, backend object existence, part rows (§2.2 M4). A slow-but-alive
   original owner that finishes assembly after losing its lease cannot corrupt anything: its
   `finish_complete` CAS (`WHERE state='completing'`) still succeeds only if no one else finished first,
   and `VersionRepo::finalize`'s own `status='pending'` CAS makes the version flip once-only; a lost
   finish converges via `replay_completed` (`finish_session`'s not-finished branch).
3. **PUT replay on the same token** (until `exp`). Unpublished path: lands as an ordinary write.
   Published path: `publish_exclusive` refuses (**bytes never mutate in place**); finalize still runs
   with the replay's digest and either converges (same bytes) or is rejected by the stored-hash /
   read-back comparison (different bytes) — the live object is never re-labelled. DESIGN §4.5
   "Bound by the signature" + `bin/sidecar.rs::upload` decision table.
4. **Two upload tokens for one new file** (double `POST /files`, or create-retry without an idempotency
   key). Two files in the retry case (see F1). Within one file (create + `POST /files/{id}/versions`,
   or an idempotency replay racing the original PUT): both finalize; the first `bind_on_finalize` wins
   the `content_id IS NULL` CAS, the second gets `X-FS-Bound: conflict` + `X-FS-Current-ETag` — its
   version stays `available` and manually bindable. Nothing is silently overwritten.
5. **`complete` vs concurrent manual `bind`.** `complete` validates its optional `If-Match` against the
   file row it read, then passes that exact `content_id` as the bind CAS expectation into the finalize
   tx. A manual `bind` (or another complete) landing in between moves the pointer → the embedded CAS
   loses → `bind_state: "conflict"` + `current_etag`; the upload itself still succeeds.
6. **Concurrent content replacement in general.** All pointer movement funnels through
   `bind_content_cas`; a loser re-reads the current ETag and re-binds the **already-uploaded**
   `version_id` — the retry never re-uploads bytes (PRD §5.10; DESIGN §3.6 conflict note).
7. **`POST /files` retry.** With `idempotency_key`: replay of the stored ticket, bound to the same
   subject and request hash, URL re-minted under the *current* policy (`create.rs`). Without: a second
   file. With the `multipart` block the key is rejected (`400`) — multipart create-retry recovery is
   client-side (`{file_id, upload_id}` persistence + introspect), a known gap noted in api.md.
8. **F5 / page reload — the two-clock model.** Per-part URL `exp` (minutes) only forces re-presigning
   via introspect; session `expires_at` (24 h) bounds actual progress loss (DESIGN §4.7 Phases B–C).
   A reload racing an in-flight `complete` re-issues it: `202` while the (disconnect-immune, §2.2 M4)
   detached task runs, then the recorded result.
9. **`DELETE /files/{id}` vs a concurrent new version.** The version list used for the backend-blob
   cleanup, the audit `version_count`, and the `file.deleted` event used to come from a plain
   `list_versions` read taken well before the delete transaction opened — a version a concurrent
   `presign_version`/`initiate_multipart_upload` committed after that read but before the delete's
   commit was cascade-removed from the DB along with the file, but never appeared in that list: its
   backend blob was never queued for cleanup, and once its row was gone the cleanup engine (which only
   ever looks at rows still in the database) could never find it either — a permanent storage leak.
   Fixed, strictly: `Store::delete_file_collecting_versions` locks the `files` row (`SELECT ... FOR
   UPDATE`, `FileRepo::lock_for_update`) as the transaction's first statement, before it re-lists the
   file's versions, immediately before the `DELETE`. A concurrent version insert takes `FOR KEY SHARE`
   on the same row for its FK check, which conflicts with that lock: it either commits strictly before
   the lock is granted (and is then necessarily visible to the version re-list, so it lands in the
   caller's cleanup/audit/usage accounting) or blocks until this transaction ends and then fails its own
   FK check against the now-deleted file — surfaced to that caller as `404 FileNotFound`, not a generic
   `500`. There is no longer a window in which a version is inserted, committed, and silently
   cascade-removed unaccounted-for; the client either sees its version-creation call fail with
   `FileNotFound`, or sees it succeed and its version correctly appear in the delete's own accounting.
10. **`DELETE /files/{id}/versions/{vid}` vs a concurrent new version.** Deleting a file's only version
    is equivalent to deleting the whole file (PRD `cpt-cf-file-storage-fr-delete-file`; api.md). The
    "is `vid` the file's only version?"
    decision used to be made from the same kind of pre-transaction `list_versions` snapshot: a second
    version committed after that snapshot but before the delete made the stale count wrongly say
    "yes", so the whole file — and that brand-new, never-requested version — was deleted, even though
    the caller only ever asked to remove `vid`. Fixed, strictly: `Store::delete_version_or_whole_file`
    locks the `files` row first (same mechanism as #9), then re-lists the file's versions, then decides.
    Client-visible outcome of the race: either the concurrent version's own `presign_version`/
    `initiate_multipart_upload` call committed before this transaction's lock was granted — in which
    case it is necessarily visible to the re-list that follows, so it is correctly treated as a second,
    surviving version, and only `vid` is removed (`VersionRemoved`) — or it lost the row-lock race and,
    once this transaction actually deletes the whole file (the lock-time re-list still showed `vid` as
    the only version), fails its own FK check and surfaces `404 FileNotFound` to its own caller. Both
    outcomes are exact, not merely "narrowed" — there is no third, silent-data-loss interleaving left.
11. **Orphan-file reclaim vs a concurrent new version (or multipart session).** The cleanup sweep's
    zero-version-orphan reclaim (`CleanupEngine::maybe_delete_orphaned_file` →
    `Store::delete_orphan_file_with_event`) used to guard its `DELETE` with a single conditional
    statement (`FileRepo::delete_if_orphan`'s `content_id IS NULL AND NOT EXISTS (... file_versions ...)`).
    Evaluated alone, that guard is not airtight on `PostgreSQL`: under `READ COMMITTED` its `NOT EXISTS`
    subquery is evaluated against the snapshot at the start of the statement, and a concurrent
    `insert_pending_version` that commits *after* that snapshot but *before* the `DELETE` actually
    resumes (it was only blocked, momentarily, by the FK's `FOR KEY SHARE` on the parent row) is invisible
    to it — `PostgreSQL` does not re-evaluate a subquery inside a statement's own `WHERE` on resume (no
    `EvalPlanQual` recheck there), so the stale verdict stands and `ON DELETE CASCADE` removes the
    freshly-inserted version along with the file. Fixed, strictly: `Store::delete_orphan_file_with_event`
    now locks the `files` row first, then re-checks `content_id IS NULL`, "zero versions", and "no active
    (`in_progress`/`completing`) multipart session" fresh, all inside that same lock, before ever reaching
    `delete_if_orphan`'s own conditional `DELETE` (kept as a redundant second line of defense, not the
    sole guard). A racing insert either committed before the lock (visible to the fresh checks, correctly
    aborting the reclaim) or blocks and then fails FK, surfacing `FileNotFound` to its own caller. The
    invariant "a version that exists at the moment its file is reclaimed as an orphan is never silently
    destroyed" now holds exactly, not just for a shrunken window.

    Races #9–#11 above are only exercisable against real `PostgreSQL` (`tests/pg_concurrency_test.rs`,
    `testcontainers`): on `SQLite`, `.lock(LockType::Update)` renders no `FOR UPDATE` at all (`sea-query`
    has no row-lock support for that backend), but correctness holds anyway because `SQLite` has a single
    writer — any write takes a database-level `RESERVED` lock that serializes every writer regardless of
    what any `SELECT` asked for, so the dangerous interleaving these three entries describe cannot occur
    there in the first place.

## 5. Invariants

- **Pointer moves only by CAS.** Every `files.content_id` change — manual `bind`, auto-bind on finalize,
  bind inside `complete` — is the same single conditional UPDATE (`bind_content_cas`). No path writes the
  pointer unconditionally.
- **At most one effective assembly per success.** The lease CAS serializes completers; a takeover
  re-derives progress from durable state and reuses the already-assembled object where possible; the
  version's `pending → available` flip is itself a once-only CAS.
- **Every retry is safe**: create (idempotency key), PUT (create-exclusive publish + convergent
  finalize), part PUT (exact-size claim + upsert), report/finalize callbacks (CAS-guarded),
  `complete` (persisted-result replay), `bind` (CAS + re-readable ETag).
- **A lost bind CAS loses nothing.** The bytes are uploaded, the version is `available`, the response
  names the current ETag; one manual re-bind finishes the job.
- **Bytes never mutate in place.** Backend objects are immutable per `(file_id, version_id)`
  (`publish_exclusive`; new content = new version + pointer swap, DESIGN §3.1/§3.6).
- **Garbage is bounded and owned**, with one documented exception. Every failure leaves at most: a
  `pending` version (+blob), an unbound `available` version, a versionless `files` row (a multipart create
  that died after Tx A, or whose synchronous `compensate_failed_multipart_initiate` compensation itself
  failed — see M1), orphan S3 parts of an aborted handle, or a lease-expired `completing` session — each
  enumerated in §3 with its reaper (cleanup engine) or its converging retry, except the versionless `files`
  row, whose reaper is the sweep's step 1 second phase (`sweep_versionless_files`; see operations.md's
  cleanup-sweep section) rather than a row in §3. The exception is the backend multipart handle:
  `initiate_multipart` runs between the pending-version insert and the session insert (M1), and the
  compensation that aborts the handle (`compensate_failed_session_create`) only covers a *returned* error,
  not a process crash in that window — a handle orphaned that way has no persisted correlation, so no sweep
  can ever find it. The same is true of a best-effort backend abort that fails after the session has already
  flipped to `aborted`, since later passes only list `in_progress` and lease-expired `completing` sessions.
  No bytes are at stake in either case (the parts were never uploaded, or are already abandoned), but S3
  bills for incomplete uploads: put an `AbortIncompleteMultipartUpload` lifecycle rule on the bucket
  (`DaysAfterInitiation >= ceil(multipart_session_ttl_secs / 86400) + 1`, i.e. 2 days at the default;
  rationale in operations.md → s3_backends) as the backstop reaper for both.

## 6. Traceability

- **Code**: `domain/multipart_service.rs` (`complete_multipart_upload`, `assemble_and_finish*`,
  `replay_completed`, `finish_session`), `domain/service/write.rs` (`finalize_upload_by_token`,
  `bind`), `domain/service/create.rs`, `infra/storage/repo/multipart_repo.rs` (lease CASes),
  `infra/storage/repo/file_repo.rs::bind_content_cas`, `infra/storage/store/versions.rs::finalize_version`,
  `bin/sidecar.rs` (`upload`, `post_with_retry`, `uploaded_response`), `domain/cleanup.rs`.
- **PRD**: `cpt-cf-file-storage-fr-conditional-requests` (§5.10 + amendment),
  `cpt-cf-file-storage-fr-multipart-upload`, `cpt-cf-file-storage-fr-upload-idempotency`,
  `cpt-cf-file-storage-fr-orphan-reconciliation`.
- **DESIGN**: §3.6 (sequences + auto-bind amendment), §4.5 (signed URLs), §4.7 (worked example, two-clock
  resume). **ADRs**: 0003 (sidecar data plane + bind amendment), 0004 (token transport), 0006
  (content-hash modes; no-re-read complete).
