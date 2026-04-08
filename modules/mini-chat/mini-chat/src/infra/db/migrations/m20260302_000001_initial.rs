use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration("MySQL not supported for mini-chat".into()));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(DOWN).await?;
        Ok(())
    }
}

const DOWN: &str = r"
DROP TABLE IF EXISTS message_reactions;
DROP TABLE IF EXISTS quota_usage;
DROP TABLE IF EXISTS chat_vector_stores;
DROP TABLE IF EXISTS thread_summaries;
DROP TABLE IF EXISTS message_attachments;
DROP TABLE IF EXISTS attachments;
DROP TABLE IF EXISTS chat_turns;
DROP TABLE IF EXISTS messages;
DROP TABLE IF EXISTS chats;
";

const POSTGRES_UP: &str = r"
-- 1. chats
CREATE TABLE IF NOT EXISTS chats (
    id          UUID PRIMARY KEY NOT NULL,
    tenant_id   UUID NOT NULL,
    user_id     UUID NOT NULL,
    model       TEXT NOT NULL,
    title       VARCHAR(255),
    is_temporary BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL,
    deleted_at  TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_chats_tenant_user_updated
    ON chats (tenant_id, user_id, updated_at DESC)
    WHERE deleted_at IS NULL;

-- 2. messages
CREATE TABLE IF NOT EXISTS messages (
    id                  UUID PRIMARY KEY NOT NULL,
    tenant_id           UUID NOT NULL,
    chat_id             UUID NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    request_id          UUID,
    role                VARCHAR(16) NOT NULL,
    content             TEXT NOT NULL DEFAULT '',
    content_type        VARCHAR(32) NOT NULL DEFAULT 'text',
    token_estimate      INT NOT NULL DEFAULT 0 CHECK (token_estimate >= 0),
    provider_response_id VARCHAR(128),
    request_kind        VARCHAR(16),
    features_used       JSONB NOT NULL DEFAULT '[]',
    input_tokens        BIGINT NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens       BIGINT NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    model               TEXT,
    is_compressed       BOOLEAN NOT NULL DEFAULT FALSE,
    created_at          TIMESTAMPTZ NOT NULL,
    deleted_at          TIMESTAMPTZ
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_chat_request_role
    ON messages (chat_id, request_id, role)
    WHERE request_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_messages_chat_created
    ON messages (chat_id, created_at)
    WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_id_chat_id
    ON messages (id, chat_id);

-- 3. chat_turns
CREATE TABLE IF NOT EXISTS chat_turns (
    id                          UUID PRIMARY KEY NOT NULL,
    tenant_id                   UUID NOT NULL,
    chat_id                     UUID NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    request_id                  UUID NOT NULL,
    requester_type              VARCHAR(16) NOT NULL,
    requester_user_id           UUID,
    state                       VARCHAR(16) NOT NULL,
    provider_name               VARCHAR(128),
    provider_response_id        VARCHAR(128),
    assistant_message_id        UUID REFERENCES messages(id) ON DELETE SET NULL,
    error_code                  VARCHAR(64),
    error_detail                TEXT,
    reserve_tokens              BIGINT,
    max_output_tokens_applied   INT,
    reserved_credits_micro      BIGINT,
    policy_version_applied      BIGINT,
    effective_model             TEXT,
    minimal_generation_floor_applied INT,
    deleted_at                  TIMESTAMPTZ,
    replaced_by_request_id      UUID,
    started_at                  TIMESTAMPTZ NOT NULL,
    completed_at                TIMESTAMPTZ,
    updated_at                  TIMESTAMPTZ NOT NULL,
    UNIQUE (chat_id, request_id),
    CHECK (requester_type IN ('user', 'system')),
    CHECK (state IN ('running', 'completed', 'failed', 'cancelled'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_chat_turns_running
    ON chat_turns (chat_id)
    WHERE state = 'running' AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_chat_turns_chat_started
    ON chat_turns (chat_id, started_at DESC)
    WHERE deleted_at IS NULL;

-- 4. attachments
CREATE TABLE IF NOT EXISTS attachments (
    id                      UUID PRIMARY KEY NOT NULL,
    tenant_id               UUID NOT NULL,
    chat_id                 UUID NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    uploaded_by_user_id     UUID NOT NULL,
    filename                VARCHAR(255) NOT NULL,
    content_type            VARCHAR(128) NOT NULL,
    size_bytes              BIGINT NOT NULL CHECK (size_bytes >= 0),
    storage_backend         VARCHAR(32) NOT NULL DEFAULT 'azure',
    provider_file_id        VARCHAR(128),
    status                  VARCHAR(16) NOT NULL,
    error_code              VARCHAR(64),
    attachment_kind         VARCHAR(16) NOT NULL,
    doc_summary             TEXT,
    img_thumbnail           BYTEA,
    img_thumbnail_width     INT CHECK (img_thumbnail_width >= 0),
    img_thumbnail_height    INT CHECK (img_thumbnail_height >= 0),
    summary_model           TEXT,
    summary_updated_at      TIMESTAMPTZ,
    cleanup_status          VARCHAR(16),
    cleanup_attempts        INT NOT NULL DEFAULT 0 CHECK (cleanup_attempts >= 0),
    last_cleanup_error      TEXT,
    cleanup_updated_at      TIMESTAMPTZ,
    created_at              TIMESTAMPTZ NOT NULL,
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    for_file_search         BOOLEAN NOT NULL DEFAULT false,
    for_code_interpreter    BOOLEAN NOT NULL DEFAULT false,
    deleted_at              TIMESTAMPTZ,
    CHECK (attachment_kind IN ('document', 'image')),
    CHECK (status IN ('pending', 'uploaded', 'ready', 'failed'))
);
CREATE INDEX IF NOT EXISTS idx_attachments_tenant_chat
    ON attachments (tenant_id, chat_id)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_attachments_cleanup
    ON attachments (cleanup_status)
    WHERE cleanup_status IS NOT NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_attachments_id_chat_id
    ON attachments (id, chat_id);

-- 4a. message_attachments
CREATE TABLE IF NOT EXISTS message_attachments (
    tenant_id       UUID NOT NULL,
    chat_id         UUID NOT NULL,
    message_id      UUID NOT NULL,
    attachment_id   UUID NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (chat_id, message_id, attachment_id),
    FOREIGN KEY (message_id, chat_id) REFERENCES messages(id, chat_id) ON DELETE CASCADE,
    FOREIGN KEY (attachment_id, chat_id) REFERENCES attachments(id, chat_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_message_attachments_tenant_chat
    ON message_attachments (tenant_id, chat_id);
CREATE INDEX IF NOT EXISTS idx_message_attachments_attachment_chat
    ON message_attachments (attachment_id, chat_id);

-- 5. thread_summaries
CREATE TABLE IF NOT EXISTS thread_summaries (
    id                  UUID PRIMARY KEY NOT NULL,
    tenant_id           UUID NOT NULL,
    chat_id             UUID NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    summary_text        TEXT NOT NULL,
    summarized_up_to    UUID NOT NULL,
    token_estimate      INT NOT NULL DEFAULT 0 CHECK (token_estimate >= 0),
    created_at          TIMESTAMPTZ NOT NULL,
    updated_at          TIMESTAMPTZ NOT NULL,
    UNIQUE (chat_id)
);

-- 6. chat_vector_stores
CREATE TABLE IF NOT EXISTS chat_vector_stores (
    id              UUID PRIMARY KEY NOT NULL,
    tenant_id       UUID NOT NULL,
    chat_id         UUID NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    vector_store_id VARCHAR(128),
    provider        VARCHAR(128) NOT NULL,
    file_count      INT NOT NULL DEFAULT 0 CHECK (file_count >= 0),
    created_at      TIMESTAMPTZ NOT NULL,
    UNIQUE (tenant_id, chat_id)
);

-- 7. quota_usage
CREATE TABLE IF NOT EXISTS quota_usage (
    id                      UUID PRIMARY KEY NOT NULL,
    tenant_id               UUID NOT NULL,
    user_id                 UUID NOT NULL,
    period_type             VARCHAR(16) NOT NULL,
    period_start            DATE NOT NULL,
    bucket                  VARCHAR(32) NOT NULL,
    spent_credits_micro     BIGINT NOT NULL DEFAULT 0 CHECK (spent_credits_micro >= 0),
    reserved_credits_micro  BIGINT NOT NULL DEFAULT 0 CHECK (reserved_credits_micro >= 0),
    calls                   INT NOT NULL DEFAULT 0 CHECK (calls >= 0),
    input_tokens            BIGINT NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens           BIGINT NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    file_search_calls       INT NOT NULL DEFAULT 0 CHECK (file_search_calls >= 0),
    web_search_calls        INT NOT NULL DEFAULT 0 CHECK (web_search_calls >= 0),
    rag_retrieval_calls     INT NOT NULL DEFAULT 0 CHECK (rag_retrieval_calls >= 0),
    image_inputs            INT NOT NULL DEFAULT 0 CHECK (image_inputs >= 0),
    image_upload_bytes      BIGINT NOT NULL DEFAULT 0 CHECK (image_upload_bytes >= 0),
    updated_at              TIMESTAMPTZ NOT NULL,
    UNIQUE (tenant_id, user_id, period_type, period_start, bucket)
);

-- 8. message_reactions
CREATE TABLE IF NOT EXISTS message_reactions (
    id          UUID PRIMARY KEY NOT NULL,
    tenant_id   UUID NOT NULL,
    message_id  UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id     UUID NOT NULL,
    reaction    VARCHAR(16) NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    UNIQUE (message_id, user_id),
    CHECK (reaction IN ('like', 'dislike'))
);
";

const SQLITE_UP: &str = r"
-- 1. chats
CREATE TABLE IF NOT EXISTS chats (
    id          TEXT PRIMARY KEY NOT NULL,
    tenant_id   TEXT NOT NULL,
    user_id     TEXT NOT NULL,
    model       TEXT NOT NULL,
    title       TEXT,
    is_temporary INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    deleted_at  TEXT
);
CREATE INDEX IF NOT EXISTS idx_chats_tenant_user_updated
    ON chats (tenant_id, user_id, updated_at DESC)
    WHERE deleted_at IS NULL;

-- 2. messages
CREATE TABLE IF NOT EXISTS messages (
    id                  TEXT PRIMARY KEY NOT NULL,
    tenant_id           TEXT NOT NULL,
    chat_id             TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    request_id          TEXT,
    role                TEXT NOT NULL,
    content             TEXT NOT NULL DEFAULT '',
    content_type        TEXT NOT NULL DEFAULT 'text',
    token_estimate      INTEGER NOT NULL DEFAULT 0 CHECK (token_estimate >= 0),
    provider_response_id TEXT,
    request_kind        TEXT,
    features_used       TEXT NOT NULL DEFAULT '[]',
    input_tokens        INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens       INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    model               TEXT,
    is_compressed       INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    deleted_at          TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_chat_request_role
    ON messages (chat_id, request_id, role)
    WHERE request_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_messages_chat_created
    ON messages (chat_id, created_at)
    WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_id_chat_id
    ON messages (id, chat_id);

-- 3. chat_turns
CREATE TABLE IF NOT EXISTS chat_turns (
    id                          TEXT PRIMARY KEY NOT NULL,
    tenant_id                   TEXT NOT NULL,
    chat_id                     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    request_id                  TEXT NOT NULL,
    requester_type              TEXT NOT NULL,
    requester_user_id           TEXT,
    state                       TEXT NOT NULL,
    provider_name               TEXT,
    provider_response_id        TEXT,
    assistant_message_id        TEXT REFERENCES messages(id) ON DELETE SET NULL,
    error_code                  TEXT,
    error_detail                TEXT,
    reserve_tokens              INTEGER,
    max_output_tokens_applied   INTEGER,
    reserved_credits_micro      INTEGER,
    policy_version_applied      INTEGER,
    effective_model             TEXT,
    minimal_generation_floor_applied INTEGER,
    deleted_at                  TEXT,
    replaced_by_request_id      TEXT,
    started_at                  TEXT NOT NULL,
    completed_at                TEXT,
    updated_at                  TEXT NOT NULL,
    UNIQUE (chat_id, request_id),
    CHECK (requester_type IN ('user', 'system')),
    CHECK (state IN ('running', 'completed', 'failed', 'cancelled'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_chat_turns_running
    ON chat_turns (chat_id)
    WHERE state = 'running' AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_chat_turns_chat_started
    ON chat_turns (chat_id, started_at DESC)
    WHERE deleted_at IS NULL;

-- 4. attachments
CREATE TABLE IF NOT EXISTS attachments (
    id                      TEXT PRIMARY KEY NOT NULL,
    tenant_id               TEXT NOT NULL,
    chat_id                 TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    uploaded_by_user_id     TEXT NOT NULL,
    filename                TEXT NOT NULL,
    content_type            TEXT NOT NULL,
    size_bytes              INTEGER NOT NULL CHECK (size_bytes >= 0),
    storage_backend         TEXT NOT NULL DEFAULT 'azure',
    provider_file_id        TEXT,
    status                  TEXT NOT NULL,
    error_code              TEXT,
    attachment_kind         TEXT NOT NULL,
    doc_summary             TEXT,
    img_thumbnail           BLOB,
    img_thumbnail_width     INTEGER CHECK (img_thumbnail_width >= 0),
    img_thumbnail_height    INTEGER CHECK (img_thumbnail_height >= 0),
    summary_model           TEXT,
    summary_updated_at      TEXT,
    cleanup_status          TEXT,
    cleanup_attempts        INTEGER NOT NULL DEFAULT 0 CHECK (cleanup_attempts >= 0),
    last_cleanup_error      TEXT,
    cleanup_updated_at      TEXT,
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL DEFAULT (datetime('now')),
    for_file_search         INTEGER NOT NULL DEFAULT 0,
    for_code_interpreter    INTEGER NOT NULL DEFAULT 0,
    deleted_at              TEXT,
    CHECK (attachment_kind IN ('document', 'image')),
    CHECK (status IN ('pending', 'uploaded', 'ready', 'failed'))
);
CREATE INDEX IF NOT EXISTS idx_attachments_tenant_chat
    ON attachments (tenant_id, chat_id)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_attachments_cleanup
    ON attachments (cleanup_status)
    WHERE cleanup_status IS NOT NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_attachments_id_chat_id
    ON attachments (id, chat_id);

-- 4a. message_attachments
CREATE TABLE IF NOT EXISTS message_attachments (
    tenant_id       TEXT NOT NULL,
    chat_id         TEXT NOT NULL,
    message_id      TEXT NOT NULL,
    attachment_id   TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    PRIMARY KEY (chat_id, message_id, attachment_id),
    FOREIGN KEY (message_id, chat_id) REFERENCES messages(id, chat_id) ON DELETE CASCADE,
    FOREIGN KEY (attachment_id, chat_id) REFERENCES attachments(id, chat_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_message_attachments_tenant_chat
    ON message_attachments (tenant_id, chat_id);
CREATE INDEX IF NOT EXISTS idx_message_attachments_attachment_chat
    ON message_attachments (attachment_id, chat_id);

-- 5. thread_summaries
CREATE TABLE IF NOT EXISTS thread_summaries (
    id                  TEXT PRIMARY KEY NOT NULL,
    tenant_id           TEXT NOT NULL,
    chat_id             TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    summary_text        TEXT NOT NULL,
    summarized_up_to    TEXT NOT NULL,
    token_estimate      INTEGER NOT NULL DEFAULT 0 CHECK (token_estimate >= 0),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE (chat_id)
);

-- 6. chat_vector_stores
CREATE TABLE IF NOT EXISTS chat_vector_stores (
    id              TEXT PRIMARY KEY NOT NULL,
    tenant_id       TEXT NOT NULL,
    chat_id         TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    vector_store_id TEXT,
    provider        TEXT NOT NULL,
    file_count      INTEGER NOT NULL DEFAULT 0 CHECK (file_count >= 0),
    created_at      TEXT NOT NULL,
    UNIQUE (tenant_id, chat_id)
);

-- 7. quota_usage
CREATE TABLE IF NOT EXISTS quota_usage (
    id                      TEXT PRIMARY KEY NOT NULL,
    tenant_id               TEXT NOT NULL,
    user_id                 TEXT NOT NULL,
    period_type             TEXT NOT NULL,
    period_start            TEXT NOT NULL,
    bucket                  TEXT NOT NULL,
    spent_credits_micro     INTEGER NOT NULL DEFAULT 0 CHECK (spent_credits_micro >= 0),
    reserved_credits_micro  INTEGER NOT NULL DEFAULT 0 CHECK (reserved_credits_micro >= 0),
    calls                   INTEGER NOT NULL DEFAULT 0 CHECK (calls >= 0),
    input_tokens            INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens           INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    file_search_calls       INTEGER NOT NULL DEFAULT 0 CHECK (file_search_calls >= 0),
    web_search_calls        INTEGER NOT NULL DEFAULT 0 CHECK (web_search_calls >= 0),
    rag_retrieval_calls     INTEGER NOT NULL DEFAULT 0 CHECK (rag_retrieval_calls >= 0),
    image_inputs            INTEGER NOT NULL DEFAULT 0 CHECK (image_inputs >= 0),
    image_upload_bytes      INTEGER NOT NULL DEFAULT 0 CHECK (image_upload_bytes >= 0),
    updated_at              TEXT NOT NULL,
    UNIQUE (tenant_id, user_id, period_type, period_start, bucket)
);

-- 8. message_reactions
CREATE TABLE IF NOT EXISTS message_reactions (
    id          TEXT PRIMARY KEY NOT NULL,
    tenant_id   TEXT NOT NULL,
    message_id  TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL,
    reaction    TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (message_id, user_id),
    CHECK (reaction IN ('like', 'dislike'))
);
";
