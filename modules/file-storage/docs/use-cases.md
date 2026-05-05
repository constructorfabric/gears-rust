<!-- Created: 2026-04-20 by Constructor Tech -->

## 11.5 What is the file lifecycle?

1. **Authorized Client — intent to upload a file.**

2. **Chat Frontend — user clicks the "Upload" button.**

3. **Chat Frontend → `POST /api/chat/v1/threads/{thread_id}/upload-file`.**
   Passes the file's main parameters (name, size, type), but not its content.

4. **Chat Backend → `FileStorageClient.create_presigned_upload(ctx, backend_id?, owner: OwnerRef { tenant_id, owner_id }, meta: FileMeta, capability: "upload.s3.multipart.sigv4.v1", part_count: N, params: UrlParams)`.**
   Performs all the application's business validations, checks the limits of its own service, of the given thread, of the user, etc. The `ctx: SecurityContext` carries the caller's tenant; `backend_id` is optional — when omitted, FileStorage falls back to the tenant's `default_private` backend. FileStorage:
   - Mints a fresh opaque `file_id`, derives the backend object key (`file_path`) deterministically from it.
   - INSERTs the row in `file_storage.files` with `status = 'pending_upload'`, sentinel `etag`, `version_id = NULL`, and `upload_expires_at` derived from `params.expires_in_seconds` (capped by the backend's max URL TTL).
   - Calls `CreateMultipartUpload` on the storage backend; captures the backend-supplied `upload_id` (NOT persisted in the FileStorage DB — round-trips through the caller).
   - Presigns N `UploadPart` PUT URLs (SigV4) with `Content-Type` / `Content-Disposition` / `x-amz-meta-<k>` pinned via SignedHeaders. **`x-amz-meta-gts-file-type` is NOT pinned** — that field is DB-only.
   - Returns `PresignedUploadHandle { file_id, upload_id, part_urls[], expires_at }`.
   In the future a Garbage Collector can be run that finds expired `pending_upload` rows for which the file was never actually uploaded. The handler also persists the link to the new `file_id` in its own `threads` table.

5. **Chat Frontend ← `PresignedUploadHandle { file_id, upload_id, part_urls[], expires_at }`.**
   The client cannot modify the parameters in the part URLs because they are SigV4-signed (hash of canonical params + backend secret); any change makes the URL invalid. Each part URL pins the same `Content-Type` / `Content-Disposition` / `x-amz-meta-<k>` headers via SignedHeaders.

6. **Chat Frontend → S3 directly: `PUT` each part to its corresponding presigned URL.**
   The frontend uploads parts in any order (concurrency is the client's choice), collects `(part_number, etag)` pairs from each part's response, and never goes through FileStorage on the data plane. Even single-byte files use a one-part session (last-part rule lets `part_size` be arbitrarily small). P1 ships SigV4 PUT without backend-side preconditions on the upload presign — correctness is upheld entirely by FileStorage's own primitives (the 3-phase commit at step 8, `(etag, updated_at, version_id[, xmin])` race-detection on conditional UPDATEs — `version_id` participates always, null-safe via `IS NOT DISTINCT FROM`, closing the ABA window when S3 versioning is enabled; `xmin` adds Postgres-only transaction-id race detection — the status state machine, in-band recovery for stuck transient states on the next SDK call). Logical-address uniqueness is structural — `file_path` is derived from `file_id`, so collisions are impossible by construction.

7. **Chat Frontend → `POST /api/chat/v1/threads/{thread_id}/files/{file_id}/status`** (with the collected parts list).
   Signals that all parts uploaded successfully. The body carries `{ parts: [{part_number, etag}] }` collected at step 6 — FileStorage forwards these etags to S3's `CompleteMultipartUpload` (it does not trust them for the FINAL object etag, which it captures from S3's response).

8. **Chat Backend → `FileStorageClient.complete_upload(ctx, file_id, upload_id, parts)`** (in-process SDK in P1; in P2 also: `POST /api/file-storage/v1/files/{file_id}/upload/{upload_id}` with body `{ parts: [{part_number, etag}] }`).
   Commits the upload via a 3-phase commit. **There is no separate `reconcile` primitive** — atomicity of the DB↔S3 commit lives entirely inside `complete_upload`:
   - **Phase 1 (DB).** Conditional UPDATE flips `pending_upload → completing`, capturing the row's pre-existing `(etag, updated_at, version_id[, xmin])` for race detection. `0` rows → row was concurrently aborted/deleted → return the appropriate error.
   - **Phase 2 (S3).** Calls `CompleteMultipartUpload` on the storage backend with the supplied `(part_number, etag)` list; captures the finalized object's etag and `version_id` from S3's response.
   - **Phase 3 (DB).** Conditional UPDATE flips `completing → uploaded` AND writes the new `(etag, version_id, size_bytes)` along with `upload_expires_at = NULL` in one statement. The row never claims durability before S3 has acknowledged the multipart finalize.
   Returns `FileInfo`. After the commit, chat performs downstream business operations — for example, sends the `file_id` to antivirus or LLM analysis. From then on, everywhere in the system, all services interact only with `(file_id, etag, version_id?)` and never with the real backend object key.

9. **Downstream Modules (e.g. antivirus) → FS Rust SDK (`get_file_info` / `read_file` / `put_file_info` / `delete_file` / `reconcile`).**
   Having received the `file_id`, other services call the FS Rust SDK on their own:
   - `FileStorageClient.get_file_info(ctx, file_id, etag: Option<&Etag>, version_id: Option<&VersionId>)` — get all the information about the file (directly from the FileStorage database, without touching the real storage backend such as S3). Both pins are CAS — mismatch returns `EtagMismatch`.
   - `FileStorageClient.read_file(ctx, file_id, etag: Option<&Etag>, version_id: Option<&VersionId>, range: Option<ByteRange>) -> FileReadHandle { info, bytes: Stream<Bytes>, range: Option<ResolvedByteRange> }` — stream the file content in the idiomatic Rust async byte-stream mode. `etag` is a CAS pin; `version_id` is a historical selector (when S3 versioning is enabled the underlying `GetObject` embeds `versionId=<vid>`) that doubles as ABA-safe CAS when paired with `etag`. `range` is an optional partial-read selector mapped to HTTP `Range: bytes=...` on the backend `GetObject` (`Inclusive { start, end }` / `From(start)` / `Suffix(n)` for last-N bytes); `info` continues to reflect the FULL object metadata, while `bytes` carries only the requested diapason and `FileReadHandle.range` mirrors the backend's `Content-Range`. Range is constitutive on every S3-class backend (no capability tag needed); typical use cases are video/audio streaming with seek, parallel multi-range downloads, and footer-only inspection of large files (Parquet / ZIP / MP4 moov-atom). `read_file` is also the lazy in-process self-healing trigger that reconciles the row's etag against the backend's `ETag` header (per ADR-0004) — the ETag returned on a range response is the FULL object ETag, so reconciliation works unchanged with or without `range`.
   - `FileStorageClient.put_file_info(ctx, file_id, update: FileMetaUpdate, etag?, version_id?)` — atomic DB+S3 metadata sync via `CopyObject` self-copy with `MetadataDirective: REPLACE`. The body declares `name`, `mime_type`, `custom_metadata` only — `gts_file_type` is structurally immutable. Both `etag` and `version_id` are optional CAS pins; together they make the strong-CAS path ABA-safe even on bit-identical re-uploads.
   - `FileStorageClient.delete_file(ctx, file_id, etag?, version_id?)` — 2-phase hard delete (Phase 1 `uploaded → deleting`, Phase 2 backend DELETE with retries, Phase 3 hard-DELETE the row). Both `etag` and `version_id` are optional CAS pins.
   - `FileStorageClient.reconcile(ctx, file_id)` — explicit reconciliation of the row against the backend (e.g. after an out-of-band overwrite the caller knows about). Always safe to invoke.

   **Re-uploading file content** is a single flow — variant B, preserving `file_id`:
   - The application backend calls `FileStorageClient.create_presigned_url(ctx, file_id = Some(id), params)`. The server pins the row's CURRENT metadata into the presigned PUT (the caller MUST NOT supply `meta` on this variant); the end-client `PUT`s new bytes to the same backend object key (deterministically derived from `file_id`); the application calls `complete_upload` (or `reconcile` on the legacy single-PUT path) to refresh the row's `etag` and `version_id`. The `file_id` is preserved; consumers holding the `file_id` see the new bytes after the next finalize (or via the lazy self-heal on `read_file`).
   - There is **no fresh-`file_id` supersession flow**: minting a new `file_id` always creates an independent file with its own backend object key (`file_path` is derived from `file_id`), not an "overwrite" of any existing file. To replace a file in place, use variant B above; to discard the old file and create a new one, call `delete_file` followed by a fresh `create_presigned_url`.

10. **Chat Backend → `FileStorageClient.presign_urls(ctx, items: Vec<PresignDownloadItem { file_id, params: UrlParams, etag: Option<Etag>, version_id: Option<String> }>)`.**
    Obtains URLs to give the frontend for display/download. The method is batch-first by design (one DB SELECT for the whole list, one RTT in a future remote topology), and a single-URL caller simply passes a one-element vector. Per item: when `etag` is present, the server verifies the row's current `etag` matches (DB-only; no HEAD against S3) before signing — mismatch returns `EtagMismatch` for that item. When `version_id` is present and the chosen capability is a `*.versioned.*` variant, the signed URL embeds `versionId=<vid>` so the URL resolves to that historical generation. Every issued URL sets `response-content-type` and `response-content-disposition` query params from the DB row's metadata, so the user-visible download experience tracks DB.meta independent of any S3-side drift. Backends declaring the `download.s3.public.v1` capability tag (typically paired with `default_public = true`) issue bare-HTTPS URLs with no expiry (`is_public = true` on the outcome) when the per-item `capability` is `download.s3.public.v1`.

11. **Authorized Owner — direct FileStorage access (P2 only).**
    **In P1 there is no FileStorage-owned REST endpoint** — authorized owners reach FileStorage exclusively through their application backend (chat backend, etc.), which calls the SDK on their behalf. The application can already implement an owner-self-service surface today by exposing its own routes (e.g. `GET /api/chat/v1/threads/{tid}/files/{file_id}/meta` → chat-backend → `FileStorageClient.get_file_info(...)`).

    In **P2**, when modules are separately deployable, FileStorage publishes its own REST surface. From that point on, owners may also call FileStorage directly:
    - `GET /api/file-storage/v1/files/{file_id}` — authoritative `FileInfo`.
    - `POST /api/file-storage/v1/presign-batch` with `{ "items": [{ "kind": "download", "file_id": "...", "params": {...} }] }` — presigned download URL; the user then `GET`s bytes directly from the storage backend (S3) via that URL. FileStorage never proxies file content over its REST surface in any phase — every byte download goes client ↔ S3 directly through a presigned URL (or bare HTTPS for public-read backends).

    The underlying SDK mechanism (`get_file_info` + `presign_urls` plus `gts.cf.fstorage.file.type.v1~{type}` authz) exists in P1; what P2 adds is the externally-callable HTTP transport.

12. **External Consumer → Application Backend → `FileStorageClient.presign_urls(...)`.**
    To access another user's files you must go through the specific application's API — for example, the chat backend itself must check whether specific users have access rights to specific files, and if access is granted, it must itself call `presign_urls` for the required files and return them to the user. In that case the user will be able to download files uploaded by another user, but only because the chat application allows it.

### Sequence diagram

```mermaid
sequenceDiagram
    autonumber
    actor User as Authorized Client
    participant FE as Chat Frontend
    participant BE as Chat Backend
    participant FS as FS SDK / Service
    participant DB as FileStorage DB
    participant S3 as Storage Backend (S3)
    participant Mod as Downstream Module<br/>(e.g. antivirus, llm-gateway)

    Note over User,FE: Steps 1–2. User initiates upload

    FE->>BE: 3. POST /threads/{tid}/upload-file<br/>(name, size, type)
    BE->>BE: 4a. business validations
    BE->>FS: 4b. create_presigned_upload(ctx, backend_id?, owner,<br/>meta, capability="upload.s3.multipart.sigv4.v1", part_count=N, params)
    FS->>DB: INSERT files (status='pending_upload',<br/>etag=sentinel, version_id=NULL, upload_expires_at)
    FS->>S3: CreateMultipartUpload → upload_id
    FS-->>BE: PresignedUploadHandle<br/>(file_id, upload_id, part_urls[], expires_at)
    BE-->>FE: 5. (file_id, upload_id, part_urls[], expires_at)

    loop for each part_number in 1..=N
      FE->>S3: 6. PUT part_urls[i] (bytes + pinned headers)
      S3-->>FE: 200 OK + part ETag
    end

    FE->>BE: 7. POST /threads/{tid}/files/{file_id}/status<br/>{parts: [{part_number, etag}]}
    BE->>FS: 8. complete_upload(ctx, file_id, upload_id, parts)
    Note right of FS: Phase 1 — DB
    FS->>DB: UPDATE files SET status='completing'<br/>WHERE id=file_id AND status='pending_upload'<br/>AND etag=$captured AND updated_at=$captured<br/>AND version_id IS NOT DISTINCT FROM $captured_version_id
    Note right of FS: Phase 2 — S3
    FS->>S3: CompleteMultipartUpload(upload_id, parts)
    S3-->>FS: ETag, VersionId
    Note right of FS: Phase 3 — DB
    FS->>DB: UPDATE files SET status='uploaded',<br/>etag=$s3_etag, version_id=$s3_version_id,<br/>size_bytes, updated_at=NOW, upload_expires_at=NULL<br/>WHERE id=file_id AND status='completing'
    FS-->>BE: FileInfo

    Note over BE,Mod: Step 8′. Chat triggers downstream business operations

    Mod->>FS: 9a. get_file_info(ctx, file_id, etag?)
    FS->>DB: SELECT
    FS-->>Mod: FileInfo

    Mod->>FS: 9b. read_file(ctx, file_id, etag?)
    FS->>DB: SELECT row
    FS->>S3: GET object
    S3-->>FS: bytes stream + ETag
    FS-->>Mod: FileReadHandle { info, bytes }

    Mod->>FS: 9c. put_file_info / delete_file<br/>(etag optional)
    FS->>DB: UPDATE / DELETE WHERE etag, updated_at, version_id [, xmin], …

    BE->>FS: 10. presign_urls(ctx,<br/>[{file_id, params, etag?, version_id?}])
    FS->>DB: SELECT batch
    FS-->>BE: [PresignedDownload { url, expires_at, is_public }]
    BE-->>FE: download / display URLs

    User->>FS: 11a. GET /files/{file_id}/meta<br/>or POST /presign-batch (kind=download)
    FS-->>User: FileInfo / signed URL
    User->>S3: 11b. GET <signed_url> (bytes)<br/>(or bare HTTPS for public-read backends)
    S3-->>User: bytes stream

    Note over User,Mod: Step 12. Cross-owner access goes through<br/>the application backend, which calls presign_urls<br/>after its own access checks.
```

## 11.6 Desync recovery: self-healing

### 11.6.1 Desync scenario

Initial in-sync state:

- DB: `row(file_id=f1, status=uploaded, etag=e_old, version_id=v_old)`
- S3: `derive(f1)` holds `content_old`, S3-ETag = `e_old`, S3-VersionId = `v_old`.
- DB and S3 fully consistent.

Steps that produce a desync:

1. The frontend asks chat-backend for permission to overwrite `f1` in place.
2. Chat-backend calls `FileStorageClient.create_presigned_url(ctx, file_id = Some(f1), params)` (variant-B re-upload). The row is unaffected at this stage; the server pins the row's current metadata into the presigned PUT.
3. The frontend successfully `PUT`s `content_new` directly to S3 against the same backend object key. S3 acks with `ETag = e_new` (and assigns `VersionId = v_new` when S3 versioning is enabled).
4. **The browser dies / the connection drops / the user closes the tab.**
5. `reconcile(...)` never reaches FileStorage.

Resulting state:

- DB: `row(f1, uploaded, etag=e_old, version_id=v_old)` — DB still believes the key holds `content_old`.
- S3: the key holds `content_new`, S3-ETag = `e_new`, S3-VersionId = `v_new`.
- DB and S3 are out of sync on the `(content, etag, version_id)` axis. Other mirrored fields (`name`, `mime_type`, `custom_metadata`) are untouched (variant B re-upload pins them from the row's current state, so the new bytes carry the same metadata).

### 11.6.2 Self-healing primitive

Since the desync can manifest only on a bounded axis (etag, version_id, possibly other S3-mirrored fields if the operator's bucket policy permits out-of-band header injection), and S3 always provides the authoritative answer through HEAD/GET, FileStorage reconciles the DB against S3 through one of two triggers — explicit `reconcile` (eager) or in-process `read_file` (lazy). No separate infrastructure (sweeper, S3 events, webhooks) is required for correctness.

Base operation:

```text
reconcile_primitive(file_id):
    loop up to 3 times:
        (etag_db, version_id_db, updated_at_db, status_db, meta_db)
            = SELECT * FROM files WHERE id = file_id AND status IN ('pending_upload', 'uploaded')
        if not found: return Err(NotFound)
        if status_db == 'deleting': return Err(DeleteInProgress)

        s3 = HEAD derive(file_id)
        if s3.404: return Err(BackendFailure)
        new_meta = build_from(s3)            # name from Content-Disposition, mime from Content-Type,
                                              # custom_metadata from x-amz-meta-*, size from Content-Length
                                              # gts_file_type kept from meta_db (DB-only)

        UPDATE files
           SET status = 'uploaded',
               etag = s3.etag,
               version_id = s3.version_id,
               name = new_meta.name,
               mime_type = new_meta.mime_type,
               custom_metadata = new_meta.custom_metadata,
               size_bytes = s3.content_length,
               upload_expires_at = NULL,
               updated_at = NOW()
         WHERE id = file_id
           AND etag = etag_db
           AND updated_at = updated_at_db
           AND version_id IS NOT DISTINCT FROM version_id_db   -- null-safe; participates always
           [AND xmin = xmin_db]      -- on Postgres
        if 1 row: return Ok(refreshed_FileInfo)
        # 0 rows: race detected, retry
    return Err(Conflict)  # 3 retries exhausted
```

This primitive runs in two places: triggers A (`reconcile`) and B (`read_file`).

### 11.6.3 Trigger A — `reconcile(file_id)` (explicit reconciliation, REST + SDK)

**Goal**: caller wants the row to converge on whatever is actually at the backend. This is the explicit reconciliation primitive.

In P1 the eager reconciliation primitive is the SDK method (and the in-band recovery on every other SDK call); in P2 the same operation is also reachable as `POST /files/{file_id}/meta/reconcile`, which rejects `If-Match` with `400`. Algorithm — see DESIGN §3.9 step 8 for the full step-by-step.

**Concurrent `reconcile` calls converge by construction.** Two callers both HEAD S3, observe the same state, attempt the same UPDATE — the loser sees `0` rows but retries from a fresh SELECT, observes the row already converged, and either succeeds as a no-op or retries until convergence within 3 attempts. There is no `EtagMismatch` outcome under pure `reconcile ⇄ reconcile` racing.

### 11.6.4 Trigger B — `read_file(file_id, etag = Some(e_pinned))` / `etag = None`

**Goal**: caller is fetching bytes; if the row's etag has drifted from S3, the SDK reconciles it before returning the stream.

Algorithm:

1. SELECT row → `(etag_db, version_id_db, updated_at_db, …)`.
2. If caller pinned `etag = Some(e_pinned)` and `e_pinned != etag_db` → return `EtagMismatch{ current: etag_db }`. Likewise if caller pinned `version_id = Some(v_pinned)` and `v_pinned IS DISTINCT FROM version_id_db` (null-safe; when S3 versioning is not enabled `version_id_db` is `None` and any `Some(v_pinned)` is a mismatch).
3. Open backend GET on `derive(file_id)`. When the caller pinned `version_id = Some(v)` and the chosen capability is a `*.versioned.*` variant, the GET embeds `versionId=v` so the bytes correspond to that exact historical generation; otherwise the GET resolves to the current generation. S3 returns ETag, VersionId, body stream.
4. If `s3.etag != etag_db` OR (when S3 versioning is enabled) `s3.version_id != version_id_db` → desync detected on the CURRENT generation. (Note: when the caller fetched a historical generation by passing `version_id`, this check is skipped — the historical etag legitimately differs from the row's etag.) Run system-context UPDATE pulling the same fields the eager `reconcile` pulls (etag, version_id, mirrored metadata, size). The UPDATE WHERE clause carries the captured `(etag_db, updated_at_db, version_id_db)` for race detection (`version_id` null-safe).
5. Branch:
   - If caller pinned `etag = Some(e_pinned)` (or `version_id = Some(v_pinned)`): return `Err(EtagMismatch{ current: s3.etag })` — DB is now repaired; caller's retry will succeed.
   - If both pins are `None`: re-SELECT, return `Ok(FileReadHandle { info: refreshed, bytes: stream })` transparently — the caller never learns of the repair.
6. If `s3.etag == etag_db` (in sync): return `Ok(FileReadHandle { info: row, bytes: stream })`.

The repair UPDATE on this trigger runs without a `SecurityContext` (system-context maintenance — `cpt-cf-file-storage-constraint-system-context-maintenance`).

## 11.7 2-Phase Delete

`delete_file(ctx, file_id, etag?, version_id?)` (in-process SDK in P1; in P2 also: `DELETE /api/file-storage/v1/files/{file_id}` with optional `If-Match` and `If-Match-VersionId`) runs a 3-step flow that gives the caller a strong guarantee about row state and a graceful degradation path under backend transient failure.

### 11.7.1 Phase 1 — claim the row

Conditional UPDATE:

```sql
UPDATE files
   SET status = 'deleting', updated_at = NOW()
 WHERE id = $file_id
   AND status = 'uploaded'
   [AND etag = $If-Match]
   [AND version_id IS NOT DISTINCT FROM $If-Match-VersionId]   -- null-safe; participates only when supplied
```

- `0` rows affected → with either pin: the row's etag or version_id rotated under the caller (`EtagMismatch`, HTTP 412) or the row is absent (`NotFound`, HTTP 404). Without either pin: `NotFound`. A row already in `deleting` is reported as `DeleteInProgress` (HTTP 409) — another caller's Phase 1 won. Both pins compose; passing both gives ABA-safe CAS.
- `1` row affected → the caller owns the delete. The row is now invisible to readers (`get_file_info`, `read_file`, `presign_urls`, `list_files` filter on `status = 'uploaded'`); concurrent `reconcile` and `put_file_info` against this row return `DeleteInProgress`.

### 11.7.2 Phase 2 — backend cleanup

Adapter `delete_object(derive(file_id))`. S3 `DeleteObject` is idempotent (backends with S3 versioning enabled create a delete marker).

On transient failure (5xx, network, throttle): inline retry up to 3 attempts with exponential backoff (e.g. 100 ms, 500 ms, 2 s). On persistent failure: leave the row in `deleting`, return `BackendFailure` (HTTP 502). Subsequent reads on the row return `NotFound`; the P2 GC sweep retries.

P1 deliberately accepts that a persistent backend outage can leave the row stuck in `deleting`. This is a bounded incident class — the row is invisible to callers, and when the backend recovers, the GC sweep (P2) reaps it. P1 ships without the GC sweep; until P2 lands, an operator can manually re-drive the delete by calling `delete_file` again with the row's current etag (the Phase 1 status check will accept the `deleting` row only if the operator passes through a special maintenance path — also a P2 candidate).

### 11.7.3 Phase 3 — purge the row

```sql
DELETE FROM files
 WHERE id = $file_id
   AND status = 'deleting';
```

No etag check — Phase 1 owns the row in `deleting`, so the DELETE is unconditional on etag. Phase 3 returns `204 No Content` (REST) / `Ok(())` (SDK) on success.

### 11.7.4 Concurrency analysis

**Concurrent `delete_file` and `read_file`** — `delete_file` Phase 1 returns success; the in-flight reader continues to receive bytes from its open S3 GET (in-flight snapshot semantics) until Phase 2 actually deletes the backend object. New readers after Phase 1 see `NotFound`.

**Concurrent `delete_file` and `put_file_info`** — first to land wins. A `put_file_info` arriving after Phase 1 sees `status='deleting'` and returns `DeleteInProgress`. A `delete_file` arriving after a successful `put_file_info` (with a stale `If-Match`) fails with `EtagMismatch`; without `If-Match` it succeeds.

**Concurrent `delete_file` and `reconcile`** — `reconcile` against a `deleting` row returns `DeleteInProgress`.

**Concurrent `delete_file` and `delete_file`** — Phase 1 admits exactly one. The other sees `EtagMismatch` (their `If-Match` did not match) or `DeleteInProgress` (their `If-Match` was correct but they arrived after the first Phase 1 commit) or `NotFound` (no `If-Match`, but Phase 3 already ran).

### 11.7.5 Sequence diagram

```mermaid
sequenceDiagram
    autonumber
    participant App as Caller (chat backend / module)
    participant FS as FS SDK / Service
    participant DB as FileStorage DB
    participant S3 as Storage Backend (S3)

    App->>FS: delete_file(ctx, file_id, etag?)
    FS->>DB: Phase 1: UPDATE files SET status='deleting'<br/>WHERE id=? AND status='uploaded' [AND etag=?]
    alt 1 row affected (caller owns the delete)
        FS->>S3: Phase 2: DeleteObject(derive(file_id))<br/>(retry up to 3× on transient failure)
        alt backend DELETE succeeded
            FS->>DB: Phase 3: DELETE FROM files<br/>WHERE id=? AND status='deleting'
            FS-->>App: Ok(()) — 204 No Content
        else backend DELETE persistently failed
            FS-->>App: Err(BackendFailure) — 502<br/>row left in 'deleting'<br/>P2 GC sweep will retry
        end
    else 0 rows affected
        FS->>DB: SELECT row to disambiguate
        alt row.status == 'deleting'
            FS-->>App: Err(DeleteInProgress) — 409
        else row absent
            FS-->>App: Err(NotFound) — 404
        else row.etag != caller_etag
            FS-->>App: Err(EtagMismatch) — 412
        end
    end
```

## 11.8 Variant-B re-upload

Re-uploading bytes to an existing `file_id` preserves the file's identity and reuses the backend object key. The flow:

1. Application backend calls `FileStorageClient.create_presigned_url(ctx, file_id = Some(file_id), params, etag?, version_id?)`. **No `meta` argument** on this variant. The server SELECTs the row, optionally verifies caller-supplied `etag` and/or `version_id` pins (both null-safe — mismatch fails the call with `EtagMismatch` *before* `CreateMultipartUpload` runs), pins the current `name`, `mime_type`, `custom_metadata` into a fresh presigned PUT URL, and updates `upload_expires_at` to `MAX(coalesce(current, ε), NOW + TTL)` so multiple outstanding URLs do not shorten the existing window. Passing both pins makes the variant-B initiation ABA-safe even when bytes are bit-identical.
2. The end-client `PUT`s the new bytes to the same backend object key with the SigV4-pinned headers.
3. After the PUT completes (or the application backend polls / receives a notification), the application calls `FileStorageClient.reconcile(ctx, file_id)` to refresh the row's `etag` and `version_id` from S3.

**Race window.** Between the client's PUT acknowledgment and `reconcile`, the row's `etag` lags S3 (DB still says `e_old`, S3 holds `e_new`). A reader during this window:

- Observes `DB.meta` as unchanged (variant-B does not change metadata) and `DB.etag = e_old`.
- A presigned download URL the reader is handed targets the same backend object key, so it resolves to the new bytes (`e_new`) — the user-visible content is fresh, but the row's etag claim is stale.

The window closes when `reconcile` lands. A `read_file` against the row during the window also closes the gap (lazy self-heal trigger).

The re-upload presign-batch item with `file_id` rejects the `meta` field with `400 bad_request`. Callers who need to change metadata before re-uploading content do so via `PUT /files/{file_id}/meta` first; the new variant-B presign then pins the updated metadata.

## 11.9 PUT /meta — DB+S3 atomic metadata sync

`put_file_info(ctx, file_id, FileMetaUpdate, etag?, version_id?)` (in-process SDK in P1; in P2 also: `PUT /api/file-storage/v1/files/{file_id}/meta` with optional `If-Match` and `If-Match-VersionId`) keeps DB.meta and S3 user-metadata in sync atomically.

The body declares `name?`, `mime_type?`, `custom_metadata?` only. **`gts_file_type` is structurally absent** — `FileMetaUpdate` does not declare this field, so `PUT /meta` cannot change it. No runtime field-validation needed — the type system catches it.

The flow:

1. SELECT row → capture `(etag_db, version_id_db, updated_at_db, meta_db)`. Reject `Deleting` rows with `DeleteInProgress`.
2. **If `If-Match` supplied** (strong CAS path):
   - Verify `etag_db == If-Match` (DB check). Mismatch → `412`.
   - HEAD S3 → capture `(s3_etag, s3_version_id)`. Verify `s3_etag == If-Match`. When S3 versioning is enabled, also verify `s3_version_id == version_id_db`. Mismatch → `412`.
   **If `If-Match-VersionId` supplied** (independent CAS pin on `version_id`):
   - Verify `version_id_db IS NOT DISTINCT FROM If-Match-VersionId` (DB check; null-safe). Mismatch → `412`.
   - When S3 versioning is enabled, the HEAD response above is reused: verify `s3_version_id == If-Match-VersionId`. Mismatch → `412`.
   - On backends without S3 versioning, `If-Match-VersionId = Some(_)` is a null-safe mismatch against the row's `None` and surfaces `412` immediately.
   Both pins compose. Passing both gives ABA-safe CAS even on bit-identical re-uploads.
3. Compute `new_meta = merge(meta_db, body)`. Validate aggregate user-metadata size ≤ 2 KB; oversize → `413 payload_too_large` with the `max_metadata_bytes` extension.
4. Issue `CopyObject` self-copy on the file's backend object:
   - `CopySource: derive(file_id)`
   - `MetadataDirective: REPLACE`
   - `Content-Type: new_meta.mime_type`
   - `Content-Disposition: attachment; filename="<URL-encoded new_meta.name>"`
   - `x-amz-meta-<k>: <v>` for each entry in `new_meta.custom_metadata`
   - **NO `x-amz-meta-gts-file-type`** (specific exception to the meta-mirror rule)
   - When `If-Match` was supplied: `x-amz-copy-source-if-match: <If-Match>`. (`If-Match-VersionId` has no S3-side header equivalent — its check happens at step 2 via HEAD; the application contract is that the HEAD-then-CopyObject window is short and the conditional UPDATE in step 5 catches any race.)
   - Returns `(new_etag, new_version_id)`. `412` from the precondition → propagate as `412 etag_mismatch`. Other failure → `502 backend_failure`.
5. Conditional UPDATE on the row:
   ```sql
   UPDATE files
      SET name = $new.name, mime_type = $new.mime_type,
          custom_metadata = $new.custom_metadata,
          etag = $new_etag, version_id = $new_version_id,
          updated_at = NOW()
    WHERE id = $file_id
      AND etag = $etag_db
      AND updated_at = $updated_at_db
      AND version_id IS NOT DISTINCT FROM $version_id_db   -- null-safe; participates always
      [AND xmin = $xmin_db]
   ```
   `0` rows → race detected, retry from step 1 up to 3 times. After 3 unsuccessful attempts → `Conflict`.
6. Return updated `FileInfo`.

The strong-CAS variant (with `If-Match`) closes the ABA window when S3 versioning is enabled because the HEAD also verifies `s3_version_id`. On backends without S3 versioning, ABA on content is an accepted P1 risk (`cpt-cf-file-storage-constraint-versioning-aware-cas`).

The without-`If-Match` variant is best-effort last-write-wins on metadata (`cpt-cf-file-storage-constraint-no-meta-cas`); race detection on the conditional UPDATE plus the 3-attempt retry loop bounds the window.

## 11.10 Historical version GET (backends with S3 versioning enabled)

When the bucket has S3 versioning enabled and the caller chooses a `*.versioned.*` capability tag, callers can fetch historical generations by passing `version_id` on a presign-download item:

```text
files.presign_urls(&ctx, vec![PresignDownloadItem {
    file_id,
    params: UrlParams { expires_in_seconds: 300, … },
    etag: None,
    version_id: Some("v_old".to_string()),
}]).await?;
```

The server includes `versionId=v_old` in the SigV4-signed GET URL. The browser following the URL fetches that exact historical generation, even if newer generations or delete markers exist.

Retention of historical generations is operator-controlled via S3 lifecycle rules. FileStorage does not track historical-version retention; if the operator's lifecycle has expired generation `v_old`, a presigned URL embedding `versionId=v_old` resolves to `404 NotFound` from S3.

When the chosen capability is not a `*.versioned.*` variant, passing `version_id` on the request item is `bad_request`. Callers that require strict historical-fetch semantics inspect the declared `capabilities` from `list_backends` to choose between `download.s3.sigv4.v1` and `download.s3.sigv4.versioned.v1` (or the public counterparts).

## 11.11 Component diagram

Two client surfaces (in-process Rust SDK, REST) front the same `File Storage Service`. The service owns its SQL metadata DB and exposes its byte plane through an `S3 Adapter` that talks to one of N **physical S3-compatible endpoints** (the roster of which is loaded from a static TOML configuration at boot — implementation detail, not shown as a separate node). Bytes never proxy through FS — external clients PUT/GET directly against the S3 endpoint using SigV4 presigned URLs (or bare HTTPS for `download.s3.public.v1`-capable endpoints); in-process streaming readers (`read_file`) consume bytes through the adapter without going through a presigned URL.

```mermaid
flowchart TB
    subgraph SDK_W["Rust SDK Writers (in-process)"]
        SDKChat["chat-backend<br/>writer / orchestrator"]
    end

    subgraph SDK_R["Rust SDK Streaming Readers (in-process)"]
        direction LR
        SDKAv["antivirus"]
        SDKLlm["llm-gateway"]
        SDKFp["file-parser"]
    end

    subgraph HTTP["HTTP Clients (REST)"]
        direction LR
        H1[application backends]
        H2[owner self-service]
        H3[browsers / scripts]
    end

    subgraph FS["File Storage Service"]
        direction TB
        REST["REST adapter (axum)<br/>• race-detection conditional UPDATE<br/>• partial-unique-index guard<br/>• reconcile (HEAD-and-pull)<br/>• PUT /meta DB+S3 sync (CopyObject)<br/>• 2-phase delete<br/>• status state machine<br/>• POST/DELETE multipart (P2)"]
        SDKImpl["FileStorageClient impl<br/>(SDK trait)"]
        AUTHZ["AuthZ integration<br/>gts.cf.fstorage.file.type.v1~…"]
        BR["Backend Router<br/>backend_id → Arc&lt;S3Backend&gt;<br/>(roster loaded from TOML at boot)"]
        ADAPTER["S3Backend (pub(crate) struct, no trait)<br/>• create_multipart_and_presign_parts<br/>• complete_multipart / abort_multipart<br/>• issue_presigned_gets<br/>• head_object<br/>• copy_object_self<br/>• delete_object<br/>• open_read (streaming)"]
        REST --> AUTHZ
        REST --> BR
        SDKImpl --> AUTHZ
        SDKImpl --> BR
        BR --> ADAPTER
    end

    DB[("FileStorage DB (SQL)<br/>schema: file_storage<br/>files + indexes<br/>partial-unique on uploaded")]

    subgraph BACKENDS["Storage Backends (S3-compatible endpoints)"]
        direction TB
        B1["AWS S3 / MinIO / Ceph / Wasabi / s3s-fs<br/>(also: WebDAV / FTP / custom backends behind an S3 gateway)<br/>capabilities=[upload.s3.multipart.sigv4.v1,<br/>download.s3.sigv4.v1]"]
        B2["S3 with public-read ACL / origin behind CDN<br/>capabilities=[upload.s3.multipart.sigv4.v1,<br/>download.s3.sigv4.v1,<br/>download.s3.public.v1]"]
    end

    EC["External clients<br/>(browsers, mobile, application<br/>backends, external services)"]

    SDK_W ==>|"ClientHub.get::&lt;dyn FileStorageClient&gt;()"| SDKImpl
    SDK_R ==>|"ClientHub.get::&lt;dyn FileStorageClient&gt;()"| SDKImpl
    HTTP ==>|"/api/file-storage/v1"| REST
    REST --> DB
    SDKImpl --> DB
    ADAPTER -->|"server-side S3 API<br/>(presign / HEAD / CopyObject /<br/>DeleteObject / GetObject stream /<br/>CreateMultipartUpload /<br/>CompleteMultipartUpload /<br/>AbortMultipartUpload)"| BACKENDS
    EC <-->|"Direct PUT / GET via SigV4 presigned URLs<br/>(or bare HTTPS for download.s3.public.v1)"| BACKENDS
```

**Reading the diagram.**

- **Control plane** — every metadata mutation (presign issuance, reconcile, PUT /meta CAS, delete, multipart complete/abort) flows through `REST` or `SDKImpl` → `AUTHZ` + `BR` → `ADAPTER` → `BACKENDS`, with concurrent SQL writes/reads against `DB`.
- **External byte plane** — once `REST` returns a `PresignedUploadHandle` / `PresignedDownload` / `PresignedMultipartHandle`, the URL travels back to whichever HTTP client requested it. The actual bytes flow **directly between the client and the S3 endpoint** (the `EC ↔ BACKENDS` arrow), bypassing the FileStorage service entirely. `download.s3.public.v1` URLs require no signing and have no expiry; everything else is SigV4-signed and TTL-capped.
- **In-process byte plane** — `read_file` for SDK streaming readers (antivirus / llm-gateway / file-parser) does NOT hand back a presigned URL; the byte stream is opened by the adapter (`open_read`) and forwarded back through `SDKImpl` to the caller as a `Stream<Bytes>`. The path is `SDK_R → SDKImpl → BR → ADAPTER → BACKENDS` and bytes return along the same path.
- **`ROSTER`** — not drawn as a separate node. It is a static TOML configuration loaded once at boot; the `BR` reads it to resolve `backend_id` to an `S3 Adapter` instance and to enforce per-backend tenant access lists. The architectural invariant (§1.1) is that every entry declares only versioned capability tags — the SDK's `KNOWN_CAPABILITIES` whitelist validates them at init and fails-fast on unknown tags.

### 11.11.1 `file-storage-sdk` public surface

Consumer-facing trait `FileStorageClient` (registered in `ClientHub`). 11 P1 methods + 3 P2-reserved multipart methods (`unimplemented!()` stubs in P1) = 14 total surface.

**Streaming read**

- `read_file(ctx, file_id, etag?, version_id?) -> FileReadHandle { info, bytes: Stream<Bytes> }` — lazy in-process self-healing trigger. `etag` is a CAS pin; `version_id` is a historical selector (backends with S3 versioning enabled; `GetObject?versionId=<vid>`) and doubles as ABA-safe CAS when paired with `etag`.

**Streaming write** (P1 — Rust SDK only; not exposed via HTTP API in P1)

- `put_file(ctx, file_id?, backend_id?, owner, meta, bytes: Stream<Bytes>, etag?, version_id?) -> FileInfo` — single-call in-process upload. Compresses the canonical multipart lifecycle into one async call without the presign roundtrip — the SDK drives the same `S3Backend` adapter methods that the external presigned path uses (`create_multipart_and_presign_parts` to start the session, then a single `UploadPart` call against the just-issued part URL for each chunk it pulls off the stream, then `complete_multipart`). There is **no single-shot `PutObject` path** anywhere in the design — the in-process variant uses a 1-part multipart session for small payloads (one part of arbitrary size, last-part rule) and as many parts as needed for larger ones. Internally: INSERT `PendingUpload` row (or SELECT existing on variant-B re-upload) → start multipart session against the deterministic `derive(file_id)` key with pinned `Content-Type` / `Content-Disposition` / `x-amz-meta-<k>` (never `gts_file_type`) → stream bytes part-by-part → 3-phase commit (`pending_upload → completing → uploaded`) inside the same call; the row is finalised against the etag/version_id S3 returns from `CompleteMultipartUpload`. On any error between the INSERT and the final UPDATE the SDK best-effort calls `abort_multipart` and runs `DELETE FROM files WHERE id = $file_id AND status = 'pending_upload'` so the call leaves no `PendingUpload` row behind; if the failure occurred after the multipart finalize, the orphan backend object is reclaimed by the P2 GC inverse sweep. `file_id = None` is initial upload (fresh server-minted `file_id`); `file_id = Some(id)` with optional `etag` / `version_id` pins is variant-B re-upload (same `file_id` and backend object key, mismatch on either pin → `EtagMismatch`). There is no bytes-through-FileStorage REST proxy upload in any phase — external clients always go presign-first.

**Presign-first write (bytes flow client ↔ S3 directly, no SDK byte stream)**

- `create_presigned_url(ctx, file_id?, backend_id?, owner, meta, params, etag?, version_id?) -> PresignedUploadHandle` — initial upload (`file_id = None`, `etag`/`version_id` ignored) or variant-B re-upload (`file_id = Some(id)`, `meta` MUST be omitted; `etag` and `version_id` act as optional CAS pins).
- `reconcile(ctx, file_id) -> ReconcileResult { info, s3_etag, s3_version_id }` — explicit reconciliation primitive (HEAD + conditional UPDATE)
- `delete_file(ctx, file_id, etag?, version_id?) -> ()` — 2-phase hard delete; both pins optional.

**Presigned downloads (batch)**

- `presign_urls(ctx, items: Vec<PresignDownloadItem>) -> Vec<PresignDownloadOutcome>` — each item carries optional `etag` (CAS pin, DB-only check) and optional `version_id` (historical selector when S3 versioning is enabled; combined with `etag` becomes ABA-safe CAS).

**Metadata**

- `get_file_info(ctx, file_id, etag?, version_id?) -> FileInfo`
- `put_file_info(ctx, file_id, update, etag?, version_id?) -> FileInfo` — DB+S3 atomic sync via CopyObject; both pins optional.
- `list_files(ctx, query) -> FileList`

**Backends**

- `list_backends(ctx) -> Vec<Backend>`

**Public types**

- IDs: `FileId`, `BackendId`, `Etag`, `VersionId`
- File: `FileInfo`, `FileMeta`, `FileMetaUpdate`, `FileStatus` (`PendingUpload | Uploaded | Deleting`), `CustomMetadata`, `FileList`, `ListFilesQuery`, `OwnerRef`
- Backends: `Backend` (with `default_private`, `default_public`, `capabilities: Vec<CapabilityTag>`, `max_file_size_bytes`, `max_metadata_bytes`, `max_presign_ttl_seconds`). `CapabilityTag` is a flat string `<operation>.<protocol>.<algorithm>.<variant>?.v<n>` (P1 ships 5: `upload.s3.multipart.sigv4.v1`, `download.s3.sigv4.v1`, `download.s3.sigv4.versioned.v1`, `download.s3.public.v1`, `download.s3.public.versioned.v1` — the same set covers non-S3 endpoints reached through S3-compat gateways for WebDAV / FTP / custom backends). Versioning support is declared via the `*.versioned.*` capability tags rather than a separate flag. No P2 or P3 additions are planned; cloud-native upload tags such as `upload.gcs.resumable.v1` or `upload.azure.blocks.sas_user.v1` are illustrative examples only and may be appended at any time without schema migration. Validated at boot against the SDK's `KNOWN_CAPABILITIES` whitelist — unknown tag → fail-fast init. Presigned URL support is constitutive; `kind` and `transport` discriminators are not represented (only one possible value each, by architectural decision).
- Presign: `UrlParams`, `PresignedUploadHandle`, `PresignedDownload` (with `is_public`), `PresignDownloadItem` (with optional `version_id`), `PresignDownloadOutcome`
- Reconcile: `ReconcileResult`
- Streaming: `FileByteStream`, `FileReadHandle`
- Errors: `FileStorageError` (`NotFound | AccessDenied | BadRequest | EtagMismatch | DeleteInProgress | CapabilityUnavailable | PayloadTooLarge | UploadExpired | BackendFailure | Internal`)
