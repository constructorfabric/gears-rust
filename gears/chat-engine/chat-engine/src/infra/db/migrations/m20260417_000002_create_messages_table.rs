// @cpt-cf-chat-engine-dbtable-messages:p1
// @cpt-cf-chat-engine-adr-message-tree-structure:p1
//
// Creates `messages` per ADR-0001 (immutable message tree). Self-FK on
// `parent_message_id` has NO cascade — parents are immutable by design and
// hard-delete cascades start from `sessions`. The named UNIQUE constraint
// `uq_messages_session_parent_variant` is what the variant-index retry
// loops in `infra::db::repo::message_repo` / `variant_repo` match against
// (via `infra::db::is_variant_unique_violation`).
//
// `file_ids` is stored as JSONB (an array of UUID strings) for backend
// portability: SeaORM does not currently expose a dialect-portable
// `UUID[]` column builder. Phase 11 (Message Search) is responsible for the
// GIN FTS index on `content` — it is intentionally omitted here because
// `sea_orm_migration::IndexCreateStatement` does not yet support
// `USING gin (to_tsvector(...))` portably.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

use super::m20260417_000001_create_session_tables::Sessions;

pub const UQ_VARIANT_INDEX: &str = "uq_messages_session_parent_variant";

/// Partial UNIQUE index guaranteeing root-message variant uniqueness.
///
/// `UQ_VARIANT_INDEX` covers `(session_id, parent_message_id, variant_index)`,
/// but `parent_message_id` is NULL for roots and SQL treats NULLs as distinct
/// in a multi-column UNIQUE index — so it does NOT prevent two roots in the
/// same session sharing a `variant_index`. This partial index closes that gap
/// for `parent_message_id IS NULL`. Matched alongside `UQ_VARIANT_INDEX` by
/// `infra::db::is_variant_unique_violation` so root collisions hit the retry
/// path too.
pub const UQ_VARIANT_INDEX_ROOT: &str = "uq_messages_session_root_variant";

/// Partial btree index on the denormalized `tenant_id`, supporting
/// message-scoped queries (cross-session search, retention) and future
/// sharding without joining `sessions`. Partial (`WHERE tenant_id IS NOT NULL`)
/// because un-backfilled legacy rows hold NULL and need not be indexed.
pub const IDX_MESSAGES_TENANT: &str = "idx_messages_tenant";

/// Citation / reference child tables of `message_parts` (FR-023). All share
/// the shape `(id, message_part_id, content JSONB, number)`.
pub const CITATION_TABLES: [&str; 3] = ["file_citations", "link_citations", "link_references"];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Messages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Messages::MessageId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Messages::SessionId).uuid().not_null())
                    .col(ColumnDef::new(Messages::TenantId).string().null())
                    .col(ColumnDef::new(Messages::UserId).string().null())
                    .col(ColumnDef::new(Messages::ParentMessageId).uuid().null())
                    .col(ColumnDef::new(Messages::Role).string().not_null())
                    .col(ColumnDef::new(Messages::FileIds).json_binary().null())
                    .col(
                        ColumnDef::new(Messages::VariantIndex)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Messages::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Messages::IsComplete)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Messages::IsHiddenFromUser)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Messages::IsHiddenFromBackend)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Messages::Metadata).json_binary().null())
                    .col(
                        ColumnDef::new(Messages::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_messages_session")
                            .from(Messages::Table, Messages::SessionId)
                            .to(Sessions::Table, Sessions::SessionId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_messages_parent")
                            .from(Messages::Table, Messages::ParentMessageId)
                            .to(Messages::Table, Messages::MessageId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(UQ_VARIANT_INDEX)
                    .table(Messages::Table)
                    .col(Messages::SessionId)
                    .col(Messages::ParentMessageId)
                    .col(Messages::VariantIndex)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Root-message variant uniqueness. The composite UNIQUE above cannot
        // enforce it (NULL `parent_message_id` ⇒ NULLs distinct), so add a
        // partial UNIQUE index over roots. Postgres and SQLite both support
        // partial indexes; Chat Engine targets PG + SQLite only (ADR-0019),
        // so MySQL is a no-op rather than a hard failure. Raw SQL because the
        // SeaORM index builder does not express a partial `WHERE` portably.
        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                manager
                    .get_connection()
                    .execute_unprepared(&format!(
                        "CREATE UNIQUE INDEX IF NOT EXISTS {UQ_VARIANT_INDEX_ROOT} \
                         ON messages (session_id, variant_index) \
                         WHERE parent_message_id IS NULL"
                    ))
                    .await?;
            }
            sea_orm::DatabaseBackend::MySql => {}
        }

        manager
            .create_index(
                Index::create()
                    .name("idx_messages_session_parent")
                    .table(Messages::Table)
                    .col(Messages::SessionId)
                    .col(Messages::ParentMessageId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_messages_session_created")
                    .table(Messages::Table)
                    .col(Messages::SessionId)
                    .col(Messages::CreatedAt)
                    .to_owned(),
            )
            .await?;

        // Partial index on the denormalized `tenant_id`. Raw SQL because the
        // SeaORM index builder does not express a partial `WHERE` portably;
        // PG + SQLite support it (ADR-0019), MySQL is a no-op.
        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                manager
                    .get_connection()
                    .execute_unprepared(&format!(
                        "CREATE INDEX IF NOT EXISTS {IDX_MESSAGES_TENANT} \
                         ON messages (tenant_id) WHERE tenant_id IS NOT NULL"
                    ))
                    .await?;
            }
            sea_orm::DatabaseBackend::MySql => {}
        }

        // Message body lives in `message_parts` (ordered typed parts) rather
        // than a `messages.content` blob — see DESIGN
        // `cpt-cf-chat-engine-dbtable-message-parts`. CASCADE FK so a hard
        // message delete removes its parts; UNIQUE(message_id, number) backs
        // the `compute_next_part_number` allocation.
        manager
            .create_table(
                Table::create()
                    .table(MessageParts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MessageParts::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(MessageParts::MessageId).uuid().not_null())
                    .col(ColumnDef::new(MessageParts::Type).string().not_null())
                    .col(
                        ColumnDef::new(MessageParts::Content)
                            .json_binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(MessageParts::Number).integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_parts_message")
                            .from(MessageParts::Table, MessageParts::MessageId)
                            .to(Messages::Table, Messages::MessageId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("uq_message_parts_message_number")
                    .table(MessageParts::Table)
                    .col(MessageParts::MessageId)
                    .col(MessageParts::Number)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Citation / reference child tables (FR-023). Each row carries the full
        // plugin-supplied payload as `content` JSONB plus the structural columns
        // the engine uses; CASCADE FK to `message_parts` so a part (or message)
        // delete removes attached citations. The three tables are structurally
        // identical, so they are built in a loop via `Alias`.
        for table in CITATION_TABLES {
            manager
                .create_table(
                    Table::create()
                        .table(Alias::new(table))
                        .if_not_exists()
                        .col(
                            ColumnDef::new(Alias::new("id"))
                                .uuid()
                                .not_null()
                                .primary_key(),
                        )
                        .col(
                            ColumnDef::new(Alias::new("message_part_id"))
                                .uuid()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(Alias::new("content"))
                                .json_binary()
                                .not_null(),
                        )
                        .col(ColumnDef::new(Alias::new("number")).integer().not_null())
                        .foreign_key(
                            ForeignKey::create()
                                .name(format!("fk_{table}_part"))
                                .from(Alias::new(table), Alias::new("message_part_id"))
                                .to(MessageParts::Table, MessageParts::Id)
                                .on_delete(ForeignKeyAction::Cascade),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name(format!("idx_{table}_part"))
                        .table(Alias::new(table))
                        .col(Alias::new("message_part_id"))
                        .to_owned(),
                )
                .await?;
        }

        // Resume buffer for the SSE delta stream (FR-024). Append-only,
        // short-TTL; no FK to `messages` (ephemeral infra, reclaimed by TTL).
        // See `cpt-cf-chat-engine-dbtable-stream-events`.
        manager
            .create_table(
                Table::create()
                    .table(StreamEvents::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(StreamEvents::MessageId).uuid().not_null())
                    .col(ColumnDef::new(StreamEvents::Seq).big_integer().not_null())
                    .col(ColumnDef::new(StreamEvents::Event).json_binary().not_null())
                    .col(
                        ColumnDef::new(StreamEvents::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(StreamEvents::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(StreamEvents::MessageId)
                            .col(StreamEvents::Seq),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_stream_events_expiry")
                    .table(StreamEvents::Table)
                    .col(StreamEvents::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(StreamEvents::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        // Drop citation children before their parent `message_parts`.
        for table in CITATION_TABLES {
            manager
                .drop_table(
                    Table::drop()
                        .table(Alias::new(table))
                        .if_exists()
                        .to_owned(),
                )
                .await?;
        }

        manager
            .drop_table(
                Table::drop()
                    .table(MessageParts::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                manager
                    .get_connection()
                    .execute_unprepared(&format!("DROP INDEX IF EXISTS {IDX_MESSAGES_TENANT}"))
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(&format!("DROP INDEX IF EXISTS {UQ_VARIANT_INDEX_ROOT}"))
                    .await?;
            }
            sea_orm::DatabaseBackend::MySql => {}
        }

        manager
            .drop_index(
                Index::drop()
                    .name("idx_messages_session_created")
                    .table(Messages::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_messages_session_parent")
                    .table(Messages::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(UQ_VARIANT_INDEX)
                    .table(Messages::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Messages::Table).if_exists().to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum Messages {
    Table,
    MessageId,
    SessionId,
    TenantId,
    UserId,
    ParentMessageId,
    Role,
    FileIds,
    VariantIndex,
    IsActive,
    IsComplete,
    IsHiddenFromUser,
    IsHiddenFromBackend,
    Metadata,
    CreatedAt,
}

#[derive(DeriveIden)]
pub enum MessageParts {
    Table,
    Id,
    MessageId,
    Type,
    Content,
    Number,
}

#[derive(DeriveIden)]
pub enum StreamEvents {
    Table,
    MessageId,
    Seq,
    Event,
    CreatedAt,
    ExpiresAt,
}
