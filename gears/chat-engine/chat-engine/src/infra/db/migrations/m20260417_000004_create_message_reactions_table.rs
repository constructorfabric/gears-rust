// @cpt-cf-chat-engine-dbtable-reactions:p2
//
// Creates `message_reactions` with composite PK `(message_id, user_id)`. FK
// `message_id → messages` is `ON DELETE CASCADE` so a hard-deleted message
// leaves no orphan reactions (per ADR-0021).

use sea_orm_migration::prelude::*;

use super::m20260417_000002_create_messages_table::Messages;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MessageReactions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MessageReactions::MessageId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageReactions::UserId)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageReactions::ReactionType)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageReactions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageReactions::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .name("pk_message_reactions")
                            .col(MessageReactions::MessageId)
                            .col(MessageReactions::UserId)
                            .primary(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_reactions_message")
                            .from(MessageReactions::Table, MessageReactions::MessageId)
                            .to(Messages::Table, Messages::MessageId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_reactions_message")
                    .table(MessageReactions::Table)
                    .col(MessageReactions::MessageId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_reactions_message")
                    .table(MessageReactions::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(MessageReactions::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub(crate) enum MessageReactions {
    Table,
    MessageId,
    UserId,
    ReactionType,
    CreatedAt,
    UpdatedAt,
}
