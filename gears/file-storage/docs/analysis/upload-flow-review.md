# Review: file-storage upload flow — request-count reduction

Before/after of the file-storage upload flow, plus verified findings, for
reviewers. Every claim is stated against actual committed code — verify against
the cited paths/lines. Findings were cross-checked by two independent passes and
by direct code reading.

## Where to verify

- Branch `docs-file-storage-final-state` (PR #4231, base `main`), commit
  **`aba5d9de`**. Not on `main` yet — check out that branch (its working tree is
  the code) or use `git show aba5d9de:<path>`.
- Line numbers below are at `aba5d9de`.
- Key files (all under `gears/file-storage/file-storage/`):
  `src/api/rest/handlers.rs`, `src/api/rest/dto.rs`, `src/api/rest/routes.rs`,
  `src/domain/multipart.rs`, `src/domain/multipart_service.rs`,
  `src/domain/service/create.rs`, `src/domain/cleanup.rs`, `src/gear.rs`,
  `src/infra/storage/repo/{file_repo,multipart_repo,version_repo}.rs`,
  `src/infra/backend/{in_memory,local_fs}.rs`. Companion:
  `gears/file-storage/docs/concurrency-and-failure-model.md`.

## Runtime caveat (read first — qualifies F1/F3)

In the current build the Gear wires **no quota client and no usage reporter** —
`FileService`/`MultipartService`/`CleanupEngine` all get `None`
(`gear.rs:189–217`, `:216–217`, `:230`, `:236`, `:264`, with explicit
`TODO(P2)`). So today: no quota is enforced, and no usage delta is emitted at
all. The usage-accounting defects below (F1, F3) are latent — they manifest only
once the usage reporter is wired. The durable-orphan-row half of F1/F10 is real
regardless.

---

## How it was (before)

Two planes (ADR-0003): control plane owns metadata + auth and issues signed
URLs; the sidecar moves bytes. Content is an immutable blob per version
(`/{file_id}/{version_id}`); the live content is the `content_id` pointer,
swapped by a separate **bind** under optimistic CAS.

Client-visible HTTP requests to upload a **new** file:

- **Single-part: 3 requests** — `POST /files` (create + pre-register a `pending`
  version + single-part `PUT` URL) → `PUT` (sidecar; on success it calls the
  control-plane **finalize** callback `pending→available` — a sidecar→control
  callback, not a client request) → `POST /files/{id}/bind`.
- **Multipart: N+4 requests** — `POST /files` (create; also pre-registers a
  single-part version + presign a multipart upload then abandons → orphan
  `pending`) → `POST /files/{id}/multipart` (initiate) → `PUT × N` →
  `POST .../complete` (assemble + finalize, **does not bind**) →
  `POST /files/{id}/bind`.

**Why it was like that (deliberate):** bind is separate because it is the CAS
arbitration point for concurrent content writes (PRD: retry bind without
re-uploading), the shared "make live" primitive (restore / staged upload), and
runs under a fresh JWT + fresh authz at publish time. Initiate is separate
because it is a distinct resource (status/resume/abort), runs the backend
`CreateMultipartUpload` + capability check, and serves new-version-of-existing.
Cost: for a new-file upload — often single-chunk — initiate duplicates create
and the first bind cannot conflict (`content_id` is `NULL`), so both are
ceremony.

## How it became (after — `aba5d9de`)

- **Single-part: 2 requests** — `POST /files` (create + single-part
  `upload_url`; `bind:"auto"` makes the token instruct finalize to bind under a
  `content_id IS NULL` CAS) → `PUT` (sidecar forwards `X-FS-Bound: true`+`ETag`,
  or `X-FS-Bound: conflict`+`X-FS-Current-ETag`).
- **Multipart: N+2 requests** — `POST /files` with a `multipart` block →
  response carries the full server-authoritative plan (no separate initiate, no
  throwaway single-part version) → `PUT × N` → `POST .../complete` (assemble +
  finalize **and bind** under the endpoint's optional `If-Match`).

Mechanisms (verified): merged create/plan + optional URL `handlers.rs:178–243`;
DTO `dto.rs:140–157`; `MultipartUploadState::Completing` + lease
`multipart.rs:15–24`; 202 + `Retry-After` `handlers.rs:677–691`; standalone
routes retained — `POST /files/{id}/multipart` (`routes.rs:463`),
`POST /files/{id}/bind` (`routes.rs:155`), `GET /files/{id}/multipart/{upload_id}`
(`routes.rs:539`). Bind CAS `bind_content_cas` (`file_repo.rs:110–119`): sets
`content_id` where `content_id = expected` when `expected` is `Some`, **or**
`IS NULL` when `None` — not a blanket `IS NULL OR = expected`.

**Why we changed it:** for single-chunk new-file uploads (the dominant case) the
old flow spent 3 where 2 suffice and N+4 where N+2 suffice; the removed steps
were provably redundant. Every escape hatch stays as an explicit endpoint.

---

## Findings (verified against the code at `aba5d9de`)

The core redesign and request counts are real and correct. Below: F1–F3, F9 are
code defects; F4 is a behavioral edge; F5–F8 are contract/doc corrections; F10
extends F1.

### Code defects

**F1 — HIGH: orphan file (+ latent usage overcount) when multipart initiation
fails.** The merged path persists a **bare file before** initiation
(`create_file_bare`, `handlers.rs:201`), reporting `file_count_delta: +1`
(`create.rs:466–471`). A capability rejection (`local-fs` / non-`multipart_native`)
or backend-initiation failure happens **before any pending version exists**
(`multipart_service.rs:479–482`; version insert is later at `:557–567`). The
only orphan-parent deletion runs *after* reclaiming a pending version
(`cleanup.rs:321–352`); with no pending version ever created, nothing triggers
it. There is no handler rollback and no blanket zero-version sweep. Result: a
version-less `files` row with no automatic recovery — it is removable only by an
explicit delete or a matching retention rule (`read_ops.rs:284–325`,
`cleanup.rs:883–945`), not reclaimed on its own. (Note: even the *successful*
multipart compensation path deletes only the version, not its bare parent —
`multipart_service.rs:357–373`.) Usage overcount is latent (see Runtime caveat).

**F2 — MEDIUM (worst case: session stranded until expiry): completion is not
owner-fenced end-to-end; the earlier proposed one-line fix is insufficient.**
The lease primitives are partly fenced — `release_complete_lease` is owner-scoped
(`multipart_repo.rs:181–204`) and `abort_expired_completing` is expiry-scoped
(`:210–233`). But (a) there is no lease renewal around the long backend await
(`multipart_service.rs:1143–1150`); (b) `finish_complete` filters on
`state = completing` only, not `lease_owner` (`multipart_repo.rs:244–260`); and
(c) — the decisive gap — the version finalize/auto-bind that runs *before* finish
is fenced only by pending-status, **not** by lease owner
(`multipart_service.rs:1294–1313`, `version_repo.rs:188–200`).
Stranding counterexample: A's lease expires → B takes over (`multipart_repo.rs:152–168`)
→ stale A wins the version-finalize CAS → B sees `updated=false`, errors, and
releases **B's own** live lease back to `in_progress`
(`multipart_service.rs:1001–1008`, `:1313–1321`); an owner-fenced A cannot
finish either; a later fresh acquire runs with `takeover=false` (`:853–887`),
skips the available-version recovery fast-path (`:1027–1034`), and backend-error
recovery is takeover-only (`:1152–1179`) — so the session can stay
unrecoverable until `expires_at`, not merely hand one caller a transient error.
Proper fix: fence ownership through finalize **and** finish (or add explicit
convergence when finalize loses the CAS), plus lease renewal across every long
await. The pending-only version-finalize CAS still prevents double DB
finalization, but duplicate backend work remains possible; "assembly runs at
most once" is **not** guaranteed.

**F3 — MEDIUM–HIGH (latent): crash-recovery undercounts usage.** If the process
dies after the version finalize but before session completion, takeover sees the
version `Available`, finishes the session, and returns early
(`multipart_service.rs:1027–1034`), bypassing the only multipart byte-credit
call (`:1361–1373`); no takeover/finish path compensates. Latent until the usage
reporter is wired (see Runtime caveat).

**F9 — MEDIUM: multipart auto-bind is NOT restricted to `content_id IS NULL`; it
can silently clobber content bound after create.** `complete`'s `If-Match` is
optional and, when omitted, unconditional (`multipart_service.rs:786–806`). The
embedded auto-bind uses `expected_content_id = file.content_id` **observed at
completion** (`:1273–1274`), and `bind_content_cas` replaces a non-NULL pointer
that matches (`file_repo.rs:110–114`). So for an existing file whose content was
(re)bound between the multipart create and its complete, a no-`If-Match`
`complete` overwrites that newer content with no client-supplied CAS token. The
`IS NULL` restriction the doc/code comment describes holds only for the
**single-part** finalize path (whose bind claim is minted at create time), not
for multipart complete.

### Behavioral edge

**F4 — MEDIUM: a completed upload's retry is not always replayed.** A concrete
`If-Match` is validated **before** the session is loaded/replayed
(`multipart_service.rs:796–806` vs `:809–833`). If content changed after
successful completion (incl. completion's own auto-bind), the exact retry returns
a precondition error instead of the stored `200` result.

### Contract / documentation corrections

**F5** — `POST /files` with **both** `multipart` and `idempotency_key` is
rejected `400` before plan computation (`handlers.rs:178–188`). "Idempotent
`POST /files`" holds only for the plain (non-multipart) create path.

**F6** — single-part `bind:"manual"` emits **no** `X-FS-Bound` header (match
falls through, `handlers.rs:895–922`). `BindState::Manual` surfaces only in
`complete`'s JSON `bind_state`.

**F7** — multipart gate is the `multipart_native` capability, not backend
identity (`multipart_service.rs:479–482`); in-memory advertises it
(`in_memory.rs:64–72`), only `local-fs` (`local_fs.rs:245–251`) is rejected.
"S3 required" is too strong.

**F8** — page-reload resume needs a client-retained `upload_id`; the API exposes
keyed introspection (`GET /files/{id}/multipart/{upload_id}`) but **no** session
discovery/list route (`routes.rs:539–564`).

**F10 — MEDIUM: expired merged-upload sessions strand zero-version parents too
(second path into F1).** Parent deletion is blocked by *any* `in_progress`
session regardless of expiry (`has_in_progress_for_file`,
`multipart_repo.rs:426–453`). Cleanup reclaims the pending version before the
expired-session abort step runs; by the time abort fires, no pending row remains
to trigger parent cleanup (`cleanup.rs:151–163`, version-age gate
`version_repo.rs:412–418`, abort `cleanup.rs:638–710`) — leaving a version-less
parent.

---

## Invariants (with caveats from findings)

1. Content pointer changes only via the atomic `bind_content_cas` — holds, but
   multipart complete's CAS target is the observed pointer, not `NULL` (F9).
2. **"Assembly runs at most once" — NOT guaranteed** (F2); a session can even be
   stranded until expiry.
3. Retry safety — mostly holds, but a completed upload's exact retry can return
   a precondition error rather than the stored result (F4).
4. A lost CAS loses nothing (rebind without re-upload) — holds.
5. Bytes are never mutated in place (immutable blob per version) — holds.
6. Usage accounting is neither exact (F1, F3, F10) nor currently wired at all
   (Runtime caveat).

## Scenarios that must not regress (reviewer checklist)

- [ ] Resume / докачка mid-upload — only missing parts re-upload; two-clock
      model intact. **See F8** (needs retained `upload_id`).
- [ ] Restore a prior version via `POST /files/{id}/bind` — retained.
- [ ] Concurrent content replacement — loser gets `Conflict` + current ETag,
      rebinds without re-uploading. **See F9** (multipart complete can clobber
      newer content without a client CAS token).
- [ ] Staged upload `bind:"manual"` — publishes only on explicit later bind.
      **See F6** (no header on single-part manual).
- [ ] New version of an existing file — `POST /files/{id}/multipart` retained.
- [ ] Idempotent retries — **See F5** (multipart-create + key rejected), **F4**
      (completed retry may return a precondition error).
- [ ] Multipart backend gating — **See F7**. **F1/F10** (failed / expired
      initiation strands an orphan parent).
- [ ] MIME / content-type validation — enforced post-write on control plane.
- [ ] Versioning + content-only ETag.
- [ ] Tenant isolation + authz on every path.
- [ ] Crash/failure handling — see `concurrency-and-failure-model.md`. **See F2,
      F3**.
- [ ] Pre-existing single-part composite versions still verify by stored
      `hash_mode` + retained manifest.

## Deliberate limitations (confirm intended)

- Auto-bind on finalize is `content_id IS NULL`-restricted **only on the
  single-part path** (token minted at create). Multipart `complete` auto-binds
  against the pointer observed at completion — see **F9** (this is a defect, not
  a safe limitation).
- Breaking API changes (`feat!`): `UploadTicketDto.upload_url` optional;
  `complete` may answer `202` and returns `200` replay (not `409`) on
  re-complete; new-file default flows must stop issuing a separate `bind` and
  read `bind_state` / `X-FS-Bound`.

---

## Bottom line

The request-count reduction (single-part 3→2, multipart N+4→N+2) and all named
artifacts/endpoints are real and correct. But the branch carries real defects —
**F1 (HIGH)**, **F3 (MED–HIGH, latent)**, **F2/F9/F10 (MED)** — and F4/F5–F8
contract points. Note F1/F3 usage effects are latent (usage reporter unwired).
Resolve or explicitly triage F1, F2, F9, F10 before merge; F9 (silent content
clobber) and F2 (stranding) are the most safety-relevant.
