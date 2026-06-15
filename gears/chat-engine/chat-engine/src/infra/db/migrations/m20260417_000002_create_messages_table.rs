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
                    .col(ColumnDef::new(Messages::ParentMessageId).uuid().null())
                    .col(ColumnDef::new(Messages::Role).string().not_null())
                    .col(ColumnDef::new(Messages::Content).json_binary().not_null())
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

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
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
    ParentMessageId,
    Role,
    Content,
    FileIds,
    VariantIndex,
    IsActive,
    IsComplete,
    IsHiddenFromUser,
    IsHiddenFromBackend,
    Metadata,
    CreatedAt,
}
