-- =============================================================================
-- FileStorage — database migrations
-- =============================================================================
-- All FileStorage state lives in the `file_storage` schema of the shared
-- Cyber Ware Postgres cluster. Migrations are applied through `db-runner`
-- (see docs/modkit_unified_system/11_database_patterns.md) at module startup
-- by one elected replica.
--
-- The file is split into three phase sections. Each section is intended to
-- be run as a single migration unit when its phase ships:
--
--   * P1 — initial release; everything required for the P1 scope in PRD.md
--          and DESIGN.md (auth-required CRUD, public-access flag, content
--          and metadata revisions, SHA-256 hash, content-state machine,
--          backend pointer; one-table custom metadata)
--
--   * P2 — multipart upload, versioning, idempotency, audit and event
--          outboxes, policies, retention rules
--
--   * P3 — runtime backend configuration (supersedes the P1 static TOML)
--
-- Naming convention for migration files when split per phase by the runner:
--   202xxxxxxxxx_file_storage_p1_initial.sql
--   202xxxxxxxxx_file_storage_p2_multipart.sql
--   202xxxxxxxxx_file_storage_p2_versioning.sql
--   ... etc
-- This combined file lists the DDL in dependency order within each phase.
-- =============================================================================


-- =============================================================================
-- P1 — Initial Release
-- =============================================================================

-- Schema and extensions ------------------------------------------------------

CREATE SCHEMA IF NOT EXISTS file_storage;

-- gen_random_uuid() is used for server-side ID generation where the
-- application does not supply one. Provided by the pgcrypto extension on
-- Postgres < 13 and as a built-in from 13 onwards. The shared platform
-- runtime guarantees Postgres >= 14; this is a no-op on those versions.
CREATE EXTENSION IF NOT EXISTS pgcrypto;


-- Table: file_storage.files --------------------------------------------------
-- @cpt-cf-file-storage-dbtable-files

CREATE TABLE file_storage.files (
    file_id                 uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),

    -- Tenant boundary. Immutable after creation. Enforced at the service
    -- layer; the DB-level immutability is a CHECK on UPDATE handled in the
    -- application-side trigger or repository code.
    tenant_id               uuid         NOT NULL,

    -- Ownership principal.
    owner_kind              text         NOT NULL
                                         CHECK (owner_kind IN ('user', 'app')),
    owner_id                uuid         NOT NULL,

    -- Display + classification.
    name                    text         NOT NULL,
    mime_type               text         NOT NULL,
    gts_file_type           text         NOT NULL,

    -- Size in bytes. 0 is permitted (empty file).
    size                    bigint       NOT NULL  CHECK (size >= 0),

    -- Revision counters.
    --   content_revision   bumped only on content writes (P1: single-shot
    --                      PATCH with content; P2: multipart complete and
    --                      versioning writes). ETag is derived from
    --                      (file_id, content_revision).
    --   metadata_revision  bumped on every successful write — content or
    --                      metadata-only.
    content_revision        bigint       NOT NULL  DEFAULT 0
                                         CHECK (content_revision >= 0),
    metadata_revision       bigint       NOT NULL  DEFAULT 0
                                         CHECK (metadata_revision >= 0),

    -- Content hash. P1 allow-list is locked to SHA-256 per ADR-0002. The
    -- CHECK is widened in the P2 hash-policy migration.
    hash_algorithm          text         NOT NULL  DEFAULT 'SHA-256'
                                         CHECK (hash_algorithm = 'SHA-256'),
    -- 32 bytes for SHA-256; widened to up to 64 in P2 when BLAKE3 lands.
    hash_value              bytea        NOT NULL  CHECK (octet_length(hash_value) = 32),

    -- Content lifecycle state. In P1 every file lands directly in
    -- 'available'; 'pending' exists for forward-compatibility with the
    -- P2 multipart flow where files are created without content.
    content_state           text         NOT NULL  DEFAULT 'available'
                                         CHECK (content_state IN ('pending', 'available')),

    -- Backend pointer. `backend_id` references the BackendConfig loaded from
    -- TOML in P1 (or from `storage_backends_runtime` in P3). `backend_path`
    -- is an opaque per-driver path; format is not parsed by FileStorage.
    backend_id              text         NOT NULL,
    backend_path            text         NOT NULL,

    -- Audit timestamps.
    created_at              timestamptz  NOT NULL  DEFAULT now(),
    last_modified_at        timestamptz  NOT NULL  DEFAULT now()
);

COMMENT ON TABLE  file_storage.files                          IS 'FileStorage primary file row. One row per logical file (independent of backend versions).';
COMMENT ON COLUMN file_storage.files.tenant_id                IS 'Tenant boundary; immutable after creation.';
COMMENT ON COLUMN file_storage.files.owner_kind               IS 'Owner principal kind: user (platform user) or app (Cyber Ware module).';
COMMENT ON COLUMN file_storage.files.content_revision         IS 'Monotonic counter; bumped only on content writes. Backs the ETag derivation.';
COMMENT ON COLUMN file_storage.files.metadata_revision        IS 'Monotonic counter; bumped on every successful write (content or metadata).';
COMMENT ON COLUMN file_storage.files.content_state            IS 'pending = created without content (P2 multipart pre-completion); available = content present.';

-- Indexes on files -----------------------------------------------------------

-- Covers the primary `GET /files` listing query: tenant + owner_kind + owner_id
-- with created_at descending for stable cursor pagination.
CREATE INDEX files_owner_listing_idx
    ON file_storage.files (tenant_id, owner_kind, owner_id, created_at DESC);

-- Per-tenant per-type queries (used by authorization audit, P2 policy checks).
CREATE INDEX files_tenant_gts_idx
    ON file_storage.files (tenant_id, gts_file_type);

-- Recovery / debugging index on backend pointer (e.g., "which files live on
-- backend X?"). Not on the hot path.
CREATE INDEX files_backend_idx
    ON file_storage.files (backend_id);


-- Table: file_storage.files_custom_metadata ----------------------------------
-- @cpt-cf-file-storage-dbtable-files-custom-metadata

CREATE TABLE file_storage.files_custom_metadata (
    file_id   uuid         NOT NULL
                           REFERENCES file_storage.files (file_id) ON DELETE CASCADE,
    key       text         NOT NULL,
    value     text         NOT NULL,
    set_at    timestamptz  NOT NULL  DEFAULT now(),

    PRIMARY KEY (file_id, key)
);

COMMENT ON TABLE file_storage.files_custom_metadata IS
    'User-defined key-value pairs attached to a file. JSON Merge Patch semantics on PATCH /files/{id}: keys present overwrite, keys set to null delete, keys absent are unchanged.';


-- =============================================================================
-- P2 — Multipart Upload, Versioning, Idempotency, Outboxes, Policies, Retention
-- =============================================================================

-- P2 hash-policy widening ----------------------------------------------------
-- Drops the P1 lock on SHA-256 and widens the allow-list to BLAKE3 + XXH3
-- per ADR-0002. The hash_value length CHECK is also widened to admit
-- algorithm-appropriate digest sizes.

ALTER TABLE file_storage.files
    DROP CONSTRAINT files_hash_algorithm_check;

ALTER TABLE file_storage.files
    ADD CONSTRAINT files_hash_algorithm_check
        CHECK (hash_algorithm IN ('SHA-256', 'BLAKE3', 'XXH3'));

ALTER TABLE file_storage.files
    DROP CONSTRAINT files_hash_value_check;

ALTER TABLE file_storage.files
    ADD CONSTRAINT files_hash_value_check
        CHECK (
            (hash_algorithm = 'SHA-256' AND octet_length(hash_value) = 32)
         OR (hash_algorithm = 'BLAKE3'  AND octet_length(hash_value) = 32)
         OR (hash_algorithm = 'XXH3'    AND octet_length(hash_value) = 8)
        );


-- Table: file_storage.multipart_uploads --------------------------------------
-- In-flight multipart upload sessions. Created on POST /files/{id}/multipart,
-- one row per upload session. Parts go into multipart_upload_parts.

CREATE TABLE file_storage.multipart_uploads (
    upload_id        uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    file_id          uuid         NOT NULL
                                  REFERENCES file_storage.files (file_id) ON DELETE CASCADE,

    -- Backend-side handle (e.g., S3 UploadId) — opaque to FileStorage.
    backend_upload_handle  text   NOT NULL,

    -- Lifecycle state.
    state            text         NOT NULL  DEFAULT 'in_progress'
                                  CHECK (state IN ('in_progress', 'completed', 'aborted')),

    -- Validation state for content-type magic-bytes check (recorded after
    -- the first uploaded part).
    declared_mime    text         NOT NULL,
    mime_validated   boolean      NOT NULL  DEFAULT false,

    -- TTL for abandoned uploads. The reaper marks expired in-flight uploads
    -- as 'aborted' and asks the backend to abort, freeing storage.
    created_at       timestamptz  NOT NULL  DEFAULT now(),
    expires_at       timestamptz  NOT NULL
);

CREATE INDEX multipart_uploads_file_idx ON file_storage.multipart_uploads (file_id);
CREATE INDEX multipart_uploads_expired_idx
    ON file_storage.multipart_uploads (expires_at)
    WHERE state = 'in_progress';


-- Table: file_storage.multipart_upload_parts ---------------------------------
-- One row per uploaded part.

CREATE TABLE file_storage.multipart_upload_parts (
    upload_id        uuid         NOT NULL
                                  REFERENCES file_storage.multipart_uploads (upload_id) ON DELETE CASCADE,
    part_number      int          NOT NULL  CHECK (part_number > 0),
    -- ETag-shaped per-part identifier returned by the backend on PutPart.
    backend_etag     text         NOT NULL,
    -- Per-part hash (intermediate; needed for BLAKE3 tree-mode finalization
    -- and for SHA-256 / XXH3 streaming-pass).
    part_hash        bytea        NOT NULL,
    size             bigint       NOT NULL  CHECK (size >= 0),
    uploaded_at      timestamptz  NOT NULL  DEFAULT now(),

    PRIMARY KEY (upload_id, part_number)
);


-- Table: file_storage.file_versions ------------------------------------------
-- Per-file backend version pointers. Populated only on backends that declare
-- versioning_native = true. Soft-delete is a row with soft_deleted_at set
-- (the previous current row remains accessible by version_id).

CREATE TABLE file_storage.file_versions (
    file_id           uuid         NOT NULL
                                   REFERENCES file_storage.files (file_id) ON DELETE CASCADE,
    -- Opaque, backend-assigned. Format MUST NOT be parsed.
    version_id        text         NOT NULL,

    -- Snapshot of file properties at version creation time.
    size              bigint       NOT NULL  CHECK (size >= 0),
    hash_algorithm    text         NOT NULL,
    hash_value        bytea        NOT NULL,
    content_revision  bigint       NOT NULL,

    -- True when this is the file's current version.
    is_current        boolean      NOT NULL  DEFAULT false,
    -- Set to the soft-delete time when this version is logically deleted but
    -- still recoverable via restore. Permanent delete removes the row.
    soft_deleted_at   timestamptz,

    created_at        timestamptz  NOT NULL  DEFAULT now(),

    PRIMARY KEY (file_id, version_id)
);

-- One current version per file.
CREATE UNIQUE INDEX file_versions_current_idx
    ON file_storage.file_versions (file_id)
    WHERE is_current = true;

CREATE INDEX file_versions_soft_deleted_idx
    ON file_storage.file_versions (file_id)
    WHERE soft_deleted_at IS NOT NULL;


-- Table: file_storage.idempotency_keys ---------------------------------------
-- Owner-scoped POST /files idempotency. A retried request with the same key
-- by the same owner returns the original response without creating a duplicate
-- file. Keys are isolated per (tenant_id, owner_kind, owner_id) to avoid
-- cross-owner leaks.

CREATE TABLE file_storage.idempotency_keys (
    tenant_id      uuid         NOT NULL,
    owner_kind     text         NOT NULL  CHECK (owner_kind IN ('user', 'app')),
    owner_id       uuid         NOT NULL,
    idempotency_key text        NOT NULL,

    -- Result snapshot: which file was produced.
    file_id        uuid         NOT NULL
                                REFERENCES file_storage.files (file_id) ON DELETE CASCADE,

    -- Stored response envelope so retries return the original 201 body.
    response_status smallint    NOT NULL,
    response_body   jsonb       NOT NULL,
    response_etag   text        NOT NULL,

    created_at     timestamptz  NOT NULL  DEFAULT now(),
    expires_at     timestamptz  NOT NULL,

    PRIMARY KEY (tenant_id, owner_kind, owner_id, idempotency_key)
);

CREATE INDEX idempotency_keys_expired_idx ON file_storage.idempotency_keys (expires_at);


-- Table: file_storage.audit_outbox -------------------------------------------
-- Transactional outbox for the audit-publisher. Rows are inserted in the
-- same DB transaction as the writes they describe, then drained by a worker
-- and forwarded to the platform audit sink. Provides 100% coverage with no
-- silent drops (NFR cpt-cf-file-storage-nfr-audit-completeness).

CREATE TABLE file_storage.audit_outbox (
    event_id        uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id       uuid         NOT NULL,
    actor_kind      text         NOT NULL,
    actor_id        uuid         NOT NULL,
    file_id         uuid,
    operation       text         NOT NULL,        -- 'create' | 'patch_content' | 'patch_metadata' | 'delete' | etc.
    outcome         text         NOT NULL,        -- 'success' | 'failure'
    detail          jsonb        NOT NULL,        -- arbitrary structured detail
    occurred_at     timestamptz  NOT NULL  DEFAULT now(),
    published_at    timestamptz                   -- NULL until drained
);

CREATE INDEX audit_outbox_unpublished_idx
    ON file_storage.audit_outbox (occurred_at)
    WHERE published_at IS NULL;


-- Table: file_storage.events_outbox ------------------------------------------
-- Outbox for EventBroker file-event publication. Same pattern as audit_outbox
-- but targets the platform EventBroker (policy-gated, per
-- cpt-cf-file-storage-fr-file-events).

CREATE TABLE file_storage.events_outbox (
    event_id        uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id       uuid         NOT NULL,
    file_id         uuid         NOT NULL,
    event_type      text         NOT NULL,        -- 'file.created' | 'file.content_replaced' | 'file.metadata_updated' | 'file.deleted'
    payload         jsonb        NOT NULL,
    occurred_at     timestamptz  NOT NULL  DEFAULT now(),
    published_at    timestamptz
);

CREATE INDEX events_outbox_unpublished_idx
    ON file_storage.events_outbox (occurred_at)
    WHERE published_at IS NULL;


-- Table: file_storage.policies -----------------------------------------------
-- Tenant and user policies (allowed types, size limits, public-access
-- restriction, sharing model controls). Effective policy is the most
-- restrictive across applicable rows (per PRD §5.4).

CREATE TABLE file_storage.policies (
    policy_id        uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id        uuid         NOT NULL,
    -- Scope of the policy. user-level policies match against the file's
    -- owner_id when owner_kind = 'user'; tenant-level policies match
    -- against the file's tenant.
    scope            text         NOT NULL  CHECK (scope IN ('tenant', 'user')),
    scope_owner_id   uuid,                       -- NULL when scope='tenant'

    -- Policy body. Structure documented in P2 FEATURE artifacts.
    body             jsonb        NOT NULL,

    created_at       timestamptz  NOT NULL  DEFAULT now(),
    updated_at       timestamptz  NOT NULL  DEFAULT now(),

    CHECK ((scope = 'user' AND scope_owner_id IS NOT NULL) OR
           (scope = 'tenant' AND scope_owner_id IS NULL))
);

CREATE INDEX policies_scope_idx
    ON file_storage.policies (tenant_id, scope, scope_owner_id);


-- Table: file_storage.retention_rules ----------------------------------------
-- Tenant/user retention rules. Background worker evaluates against
-- file metadata and deletes when criteria are met.

CREATE TABLE file_storage.retention_rules (
    rule_id          uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id        uuid         NOT NULL,
    scope            text         NOT NULL  CHECK (scope IN ('tenant', 'user', 'file')),
    scope_target_id  uuid,                       -- user_id when scope='user'; file_id when scope='file'; NULL when scope='tenant'

    -- Rule body: age-based, inactivity-based, custom-metadata-based.
    body             jsonb        NOT NULL,

    created_at       timestamptz  NOT NULL  DEFAULT now()
);

CREATE INDEX retention_rules_scope_idx
    ON file_storage.retention_rules (tenant_id, scope, scope_target_id);


-- =============================================================================
-- P3 — Runtime Backend Configuration, Encryption metadata
-- =============================================================================

-- Table: file_storage.storage_backends_runtime ------------------------------
-- DB-resident replacement for the P1 TOML configuration file. When this
-- table is populated, the BackendRegistry switches its source from TOML to
-- DB on module startup. Credentials are stored encrypted at rest; the
-- envelope encryption is managed by the platform secret store
-- (PRD `cpt-cf-file-storage-fr-runtime-backends`).

CREATE TABLE file_storage.storage_backends_runtime (
    backend_id       text         PRIMARY KEY,
    kind             text         NOT NULL,         -- 'local-filesystem' | 's3-compatible' | ...
    endpoint         text,                          -- nullable for local-filesystem
    region           text,                          -- nullable for non-cloud backends

    -- Credentials encrypted via the platform secret store. The column is
    -- an opaque blob; FileStorage never reads or writes the plaintext
    -- credentials directly — the secret-store SDK does that on every load.
    credentials_blob bytea,
    credentials_kms_key_id text,

    -- Capabilities (versioning_native, multipart_native, encryption_native,
    -- range_native) serialized as JSON. Loaded into BackendCapabilities
    -- struct at registry build time.
    capabilities     jsonb        NOT NULL,
    hash_policy      jsonb        NOT NULL,        -- HashPolicy (default_algorithm, allowed_algorithms, selection_rules)

    -- Soft-disable without removing the row (e.g., during scheduled
    -- maintenance). When false, the registry skips this backend; pre-existing
    -- file rows pointing at it return 503 on content access.
    enabled          boolean      NOT NULL  DEFAULT true,

    created_at       timestamptz  NOT NULL  DEFAULT now(),
    updated_at       timestamptz  NOT NULL  DEFAULT now()
);

CREATE INDEX storage_backends_runtime_enabled_idx
    ON file_storage.storage_backends_runtime (enabled)
    WHERE enabled = true;


-- P3 file-row extensions for encryption --------------------------------------
-- Per-file encryption metadata for server-side encryption with backend-managed
-- or customer-provided keys. Populated only when the writing backend
-- declares encryption_native = true and the operative policy enables
-- encryption.

ALTER TABLE file_storage.files
    ADD COLUMN encryption_scheme  text,
    ADD COLUMN encryption_kms_key_id text,
    ADD COLUMN encryption_metadata jsonb;

COMMENT ON COLUMN file_storage.files.encryption_scheme IS
    'P3: name of the server-side encryption scheme applied (e.g., AES256-GCM-SSE-S3, AES256-GCM-SSE-KMS). NULL when the backend did not encrypt.';
COMMENT ON COLUMN file_storage.files.encryption_kms_key_id IS
    'P3: key identifier in the platform KMS / secret store, when SSE-KMS is used.';
