-- Created: 2026-04-20 by Constructor Tech

-- ═══════════════════════════════════════════════════════════════════════════
-- File Storage — shared module schema (reference DDL, P1)
-- See modules/file-storage/docs/DESIGN.md §3.7 and ./rust-traits.md
--
-- Ownership:
--   This schema is owned by the FileStorage module, not by any single
--   backend. Every `s3-compatible` backend (per ADR-0001) writes into
--   the same schema. Rows are discriminated by `backend_id`. There is
--   no per-backend database, no per-backend schema, and no operational
--   deployment that runs FileStorage without the module database.
--
-- Engine neutrality:
--   This is reference DDL in portable SQL. Deployments dialectize it
--   per target engine (SQLite for local dev/tests, any relational
--   engine in production). UUIDs are generated in the application
--   layer — no engine-specific extensions or default-value functions
--   are used. JSON is stored via the generic `json` type (or `text`
--   on engines without native JSON). Partial unique indexes are
--   supported by SQLite and most mainstream engines; engines without
--   partial-index support require an application-level uniqueness
--   check.
--
-- File lifecycle (P1):
--   pending_upload → uploaded → deleting → (purged)
--                       │
--                       └──── uploaded → uploaded   (drift resync via reconcile,
--                                                    or DB+S3 sync via PUT /meta)
--
--     pending_upload : row created by `presign-batch` upload item without
--                      `file_id`. The bytes have not been confirmed at the
--                      backend yet; the row's `etag` is the sentinel
--                      FileStorage pinned at presign time.
--     uploaded       : authoritative finalized state — set by the
--                      `reconcile` operation after HEAD against the
--                      backend confirms the object's S3 ETag and reads
--                      the authoritative metadata (Content-Type,
--                      Content-Disposition, x-amz-meta-*). Subject to
--                      the partial unique index
--                      files_tenant_backend_path_uploaded_uq. Self-resync
--                      (uploaded → uploaded) is performed by `reconcile`
--                      on a row whose backend object's S3 ETag has
--                      drifted from the row (e.g. a presigned re-upload
--                      to the same key); the row's `etag` and other
--                      mirrored metadata are pulled from S3 directly.
--                      `gts_file_type` is DB-only — never mirrored to
--                      S3, and never overwritten by reconcile.
--     deleting       : transient operational state set by Phase 1 of
--                      `delete_file`. This is NOT a soft delete or a
--                      tombstone — see
--                      cpt-cf-file-storage-constraint-no-soft-delete.
--                      Subsequent `reconcile` / `put_file_info` /
--                      `delete_file` on a `deleting` row return
--                      `delete_in_progress` (HTTP 409); reads return
--                      `NotFound`. Phase 2 best-effort deletes the
--                      backend object (with inline retries); Phase 3
--                      hard-deletes the row. A row stuck in `deleting`
--                      after persistent backend failure is reaped by a
--                      future P2 GC sweep.
--
-- Addressing:
--   `id` (uuid) is the canonical, opaque file_id. External URLs and
--   cross-module handles all key off this column (per ADR-0002).
--   Tenant scoping is enforced by always including `tenant_id` in the
--   WHERE clause; this closes the enumeration oracle without
--   requiring a separate per-tenant lookup table.
--
-- Concurrency contract (see DESIGN §2.1
-- cpt-cf-file-storage-principle-optimistic-concurrency and §3.9):
--   The schema relies on database-level primitives — no advisory or
--   pessimistic locks — to coordinate concurrent writers:
--
--     1. (etag, updated_at[, xmin]) race detection on UPDATE.
--        Every mutation that targets an existing row is a single
--        statement of the form
--          UPDATE files SET … WHERE id = ?
--                                AND etag = ?
--                                AND updated_at = ?
--                                [AND xmin = ?]            -- Postgres only
--        The number of rows affected (0 or 1) decides the outcome:
--        1 = caller wins, 0 = the row moved underneath them. The
--        write handler may retry up to 3 times before surfacing an
--        error. Engines without a transaction-id system column use
--        the (etag, updated_at) pair alone, accepting the
--        last-write-wins property documented in
--        cpt-cf-file-storage-constraint-no-meta-cas.
--     2. Optional ABA-safe content CAS. When `Backend.versioning`
--        is `true` the row's `version_id` mirrors S3's per-object
--        VersionId. The eager strong-CAS variant of `PUT /files/{id}/meta`
--        verifies (etag, version_id) against S3 before issuing
--        `CopyObject`; this closes the ABA window where two
--        re-uploads happen to land identical bytes (and therefore an
--        identical S3 ETag). When `Backend.versioning = false`,
--        ABA on content is an accepted P1 risk — see
--        cpt-cf-file-storage-constraint-versioning-aware-cas.
--     3. Partial unique index files_tenant_backend_path_uploaded_uq —
--        at most one row with a given (tenant_id, backend_id,
--        file_path) is in status='uploaded' at any instant. Concurrent
--        presign-first uploads for the same logical address race; the
--        partial predicate excludes pending_upload and deleting rows
--        so they never block each other, and only the commit step
--        (transition to 'uploaded') is serialised by the index.
--     4. Status state machine. The status column doubles as a
--        coarse-grained lock: pending_upload → uploaded → deleting.
--        A mutation declares the status it expects to find via WHERE
--        status=…; a row engaged in another transition rejects the
--        new mutation by returning 0 rows.
--
--   Last-write-wins on a logical address (per ADR-0004):
--
--     - Same file_id re-upload (presigned-first overwrite via the
--       presign-batch upload item with `file_id` set): the client PUTs
--       new bytes to the same backend key with the headers FileStorage
--       pinned from the row's current meta; the next `reconcile(file_id)`
--       call HEADs S3 and pulls the authoritative etag (and metadata).
--       If `reconcile` is never called (browser-died scenario), the row
--       stays at its prior etag until a `read_file` triggers lazy
--       in-process self-healing.
--
--     - Different file_id collision on the same logical address (two
--       distinct initial-upload presigns for the same (tenant_id,
--       backend_id, file_path)): the partial unique index admits
--       exactly one — the loser's `reconcile` rolls back on the index
--       conflict and surfaces Conflict.
-- ═══════════════════════════════════════════════════════════════════════════

BEGIN;

-- Schema namespacing. Engines without CREATE SCHEMA (e.g. SQLite)
-- should omit this statement; table names remain unqualified on such
-- engines.
CREATE SCHEMA IF NOT EXISTS file_storage;

-- ── Files ────────────────────────────────────────────────────────────────────
-- Realizes cpt-cf-file-storage-dbtable-files
-- See DESIGN.md §3.7

CREATE TABLE IF NOT EXISTS file_storage.files (
    id                            UUID PRIMARY KEY,
    tenant_id                     UUID NOT NULL,
    backend_id                    UUID NOT NULL,
    file_path                     TEXT NOT NULL,
    owner_id                      UUID NOT NULL,
    name                          VARCHAR(512) NOT NULL,
    gts_file_type                 VARCHAR(256) NOT NULL,
    mime_type                     VARCHAR(256) NOT NULL,
    size_bytes                    BIGINT NOT NULL DEFAULT 0
                                  CHECK (size_bytes >= 0),
    etag                          VARCHAR(128) NOT NULL,
    version_id                    VARCHAR(1024),
    status                        VARCHAR(16) NOT NULL DEFAULT 'pending_upload',
    custom_metadata               JSON NOT NULL DEFAULT '{}',
    upload_expires_at             TIMESTAMP,
    created_at                    TIMESTAMP NOT NULL,
    updated_at                    TIMESTAMP NOT NULL
);

-- file_storage.files: one row per logical file managed by any backend
-- in the FileStorage module (discriminated by backend_id). Realizes
-- cpt-cf-file-storage-dbtable-files. Finalized rows (status=uploaded)
-- are uniquely addressable by (tenant_id, backend_id, file_path)
-- through the partial unique index files_tenant_backend_path_uploaded_uq;
-- transient rows (pending_upload, deleting) may coexist for the same
-- logical path.
--
-- Column notes:
--   id                    — opaque, app-generated file_id (UUID v7).
--                           Per ADR-0002 this is the canonical external
--                           handle; URLs and cross-module references
--                           all key off it. The S3 object key is
--                           derived deterministically from `id` at the
--                           adapter boundary.
--   tenant_id             — owning tenant UUID. Always present in the
--                           WHERE clause of every read/mutation so
--                           cross-tenant enumeration is impossible.
--   backend_id            — UUID of the backend instance hosting this
--                           file's bytes. Stable across config reloads;
--                           operators assign it once in the static
--                           TOML roster (cpt-cf-file-storage-principle-
--                           modular-backend-roster).
--   file_path             — logical path captured at upload-presign
--                           time. Used for filtering / listing; not
--                           part of the URL surface.
--   owner_id              — UUID of the principal that owns this file
--                           (a user or an app — FileStorage does not
--                           distinguish; the kind is tracked in the
--                           identity / authz subsystem).
--   name                  — display name (the file's human filename).
--                           Updatable via PUT /files/{file_id}/meta;
--                           used in Content-Disposition on download
--                           (always set via response-content-disposition
--                           query params on presigned downloads — see
--                           cpt-cf-file-storage-constraint-presigned-
--                           download-headers-from-db).
--   gts_file_type         — GTS file type
--                           (gts.cf.fstorage.file.type.v1~...) —
--                           mandatory at creation, immutable.
--                           Structurally immutable: not present in
--                           FileMetaUpdate. Stored in DB only — NEVER
--                           mirrored to S3 (specific exception to
--                           cpt-cf-file-storage-constraint-meta-mirrored-
--                           via-put-meta). Reconcile does not pull this
--                           column from S3.
--   mime_type             — declared MIME, pinned in the SigV4
--                           SignedHeaders of the presigned PUT and in
--                           the response-content-type query param of
--                           presigned GETs. Mutable via PUT /meta
--                           (DB+S3 atomic sync via CopyObject REPLACE).
--   size_bytes            — final file size; 0 while pending_upload.
--                           Pulled from S3 Content-Length on every
--                           reconcile.
--   etag                  — raw S3 ETag (sans surrounding quotes).
--                           CONTENT FINGERPRINT ONLY — does NOT track
--                           metadata changes. See
--                           cpt-cf-file-storage-constraint-etag-content-
--                           only. Rotated only by content writes and
--                           by `CopyObject` self-copy on PUT /meta
--                           (which is a content rewrite at the S3
--                           level, even when the bytes are bit-identical).
--                           For S3 multipart uploads (deferred to P2)
--                           the ETag has the form `<hex>-<N>`; the
--                           single-PUT format is `<hex>` of length 32.
--   version_id            — raw S3 VersionId for the current object
--                           generation. NULL when `Backend.versioning`
--                           is `false`. Used as the ABA-safe extension
--                           to etag-CAS on PUT /meta — see ADR-0005
--                           and cpt-cf-file-storage-constraint-versioning-
--                           aware-cas. Also honoured by presigned
--                           download items that request a historical
--                           version: when the caller passes
--                           `PresignDownloadItem.version_id` and the
--                           backend has versioning enabled, the server
--                           includes `versionId=<vid>` in the signed
--                           URL. Sized as VARCHAR(1024) per the AWS S3
--                           User Guide ("Version IDs are Unicode, UTF-8
--                           encoded, URL-ready, opaque strings that are
--                           no more than 1,024 bytes long"); FileStorage
--                           treats the value as opaque — no parsing,
--                           sorting, or monotonicity assumptions.
--   status                — file lifecycle: pending_upload → uploaded
--                           → deleting. No engine-level CHECK so the
--                           value space remains extensible by backend
--                           adapters. `deleting` is a transient
--                           operational state, NOT a soft-delete
--                           tombstone.
--   custom_metadata       — user-defined string key/value pairs
--                           (cpt-cf-file-storage-fr-metadata-storage).
--                           Aggregated user-metadata size (Content-Type
--                           + Content-Disposition + every
--                           x-amz-meta-<k>=<v>) is capped at 2 KB by
--                           AWS S3; FileStorage enforces the same cap
--                           at presign and at PUT /meta.
--                           gts_file_type does NOT count toward this
--                           budget (not mirrored).
--   upload_expires_at     — expiration captured at upload-presign time.
--                           For variant B re-upload presigns issued
--                           against an existing `file_id`, the field
--                           is updated to MAX(coalesce(current, ε),
--                           NOW + TTL) so multiple outstanding URLs do
--                           not shorten the existing window.
--                           `reconcile` rejects `pending_upload` rows
--                           past this deadline with `UploadExpired`.
--                           NULL once status='uploaded'.
--   created_at            — row insertion timestamp (UTC), set to
--                           NOW() at INSERT and immutable thereafter.
--                           DB-managed: this column tracks when the
--                           DB ROW was created in the FileStorage
--                           database, NOT when the underlying S3
--                           object was created. The S3 object's
--                           `Last-Modified` header (or any other
--                           S3-side timestamp) MUST NEVER be written
--                           into this column.
--   updated_at            — refreshed to NOW() on every successful
--                           UPDATE that touches the row (UTC).
--                           Serves both roles: (a) the user-visible
--                           "last modified" timestamp returned in
--                           FileInfo, and (b) the race-detection
--                           token used together with `etag` (and
--                           optional `xmin` on Postgres) in the
--                           WHERE clause of every conditional UPDATE.
--                           DB-managed: this column tracks when the
--                           DB ROW was last touched by FileStorage,
--                           NOT when the underlying S3 object was
--                           last modified. The S3 object's
--                           `Last-Modified` header (or any other
--                           S3-side timestamp) MUST NEVER be written
--                           into this column. Follows the workspace
--                           convention (resource-group,
--                           account-management, mini-chat, oagw —
--                           all use `updated_at`).

-- Partial unique index upholds last-write-wins for finalized rows
-- while tolerating concurrent in-flight uploads for the same logical
-- path. Keyed on (tenant_id, backend_id, file_path) so two different
-- backends may legitimately host the same file_path for the same
-- tenant. Engines without partial-index support must emulate this via
-- an application-level uniqueness check on the write handler path.
CREATE UNIQUE INDEX IF NOT EXISTS files_tenant_backend_path_uploaded_uq
    ON file_storage.files (tenant_id, backend_id, file_path)
    WHERE status = 'uploaded';

-- Supports list_files by owner across every backend the caller can
-- see (the only listing filter exposed in P1 — see DESIGN §3.3).
CREATE INDEX IF NOT EXISTS files_owner_lookup_idx
    ON file_storage.files (tenant_id, owner_id);

-- Supports list_files by recency (default and only ordering in P1).
-- The trailing `id` column is the stable cursor-pagination tiebreaker:
-- two rows with the same created_at are deterministically ordered by
-- their UUID, so cursor decoding can resume from the exact (created_at,
-- id) pair without overlap or gaps.
CREATE INDEX IF NOT EXISTS files_created_idx
    ON file_storage.files (tenant_id, created_at DESC, id);

COMMIT;
