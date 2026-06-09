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

use super::m20260417_000001_create_session_tables::Sessions;

pub const UQ_VARIANT_INDEX: &str = "uq_messages_session_parent_variant";

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
pub(crate) enum Messages {
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
