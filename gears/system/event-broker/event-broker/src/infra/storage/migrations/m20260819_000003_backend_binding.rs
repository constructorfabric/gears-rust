//! The ingest side's bookkeeping toward its storage backends: the identity
//! every ingest outbox group fences its appends with (feature 0005 §2.5) and each
//! topic's provisioning record (feature 0006 §2.2). Built with the schema
//! builder like the rest of this database ([`super::columns`]).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        create_outbox_identity(manager).await?;
        create_topic_binding(manager).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let tables: [DynIden; 2] = [
            EventBrokerTopicBinding::Table.into_iden(),
            EventBrokerOutboxIdentity::Table.into_iden(),
        ];
        for table in tables {
            manager
                .drop_table(Table::drop().table(table).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}

async fn create_outbox_identity(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use EventBrokerOutboxIdentity as T;
    manager
        .create_table(
            Table::create()
                .table(T::Table)
                .if_not_exists()
                // Outbox group name, its toolkit-db queue.
                .col(
                    ColumnDef::new(T::Name)
                        .string_len(63)
                        .not_null()
                        .primary_key(),
                )
                // Random 64 bits, 16 lowercase hex.
                .col(ColumnDef::new(T::OutboxId).string_len(16).not_null())
                // RFC 3339 UTC.
                .col(ColumnDef::new(T::CreatedAt).string_len(32).not_null())
                .to_owned(),
        )
        .await
}

async fn create_topic_binding(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use EventBrokerTopicBinding as T;
    manager
        .create_table(
            Table::create()
                .table(T::Table)
                .if_not_exists()
                // `GtsId::to_uuid(topic_id)`.
                .col(
                    ColumnDef::new(T::TopicUuid)
                        .string_len(36)
                        .not_null()
                        .primary_key(),
                )
                // For operators, not indexed.
                .col(ColumnDef::new(T::TopicId).string_len(1024).not_null())
                // The current attempt's, then the log's.
                .col(ColumnDef::new(T::LogId).string_len(36).not_null())
                // `provisioning` | `provisioned`.
                .col(ColumnDef::new(T::State).string_len(16).not_null())
                // RFC 3339 UTC, database time, compared as text.
                .col(ColumnDef::new(T::AttemptAt).string_len(32).not_null())
                // The count the backend holds, once provisioned.
                .col(ColumnDef::new(T::Partitions).big_integer().null())
                .to_owned(),
        )
        .await
}

#[derive(DeriveIden, Clone, Copy)]
enum EventBrokerOutboxIdentity {
    #[sea_orm(iden = "event_broker__outbox_identity")]
    Table,
    Name,
    OutboxId,
    CreatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum EventBrokerTopicBinding {
    #[sea_orm(iden = "event_broker__topic_binding")]
    Table,
    TopicUuid,
    TopicId,
    LogId,
    State,
    AttemptAt,
    Partitions,
}
