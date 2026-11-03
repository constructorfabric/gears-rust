//! `Storage`'s durable namespaces (eb-single-process-implementation D2):
//! `consumer_group`, `cursor`, `producer`, `producer_sequence`. Built with the
//! schema builder, so one definition serves `SQLite`, `PostgreSQL` and
//! `MySQL`/`MariaDB` ([`super::columns`]); the builder quotes `offset` and
//! `partition` per engine.

use sea_orm_migration::prelude::*;

use super::columns::{create_index, gts_id, timestamp};

/// `anonymous` or `named`.
const KIND_LEN: u32 = 16;
/// `chained` or `monotonic`.
const MODE_LEN: u32 = 32;
/// `validate::client_agent`: at most 256 bytes of printable ASCII.
const CLIENT_AGENT_LEN: u32 = 256;
/// `validate::description`: at most 1024 bytes of printable ASCII.
const DESCRIPTION_LEN: u32 = 1024;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        create_consumer_group(manager).await?;
        create_cursor(manager).await?;
        create_producer(manager).await?;
        create_producer_sequence(manager).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let tables: [DynIden; 4] = [
            EventBrokerProducerSequence::Table.into_iden(),
            EventBrokerProducer::Table.into_iden(),
            EventBrokerCursor::Table.into_iden(),
            EventBrokerConsumerGroup::Table.into_iden(),
        ];
        for table in tables {
            manager
                .drop_table(Table::drop().table(table).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}

async fn create_consumer_group(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use EventBrokerConsumerGroup as T;
    manager
        .create_table(
            Table::create()
                .table(T::Table)
                .if_not_exists()
                .col(gts_id(manager, T::Id).primary_key())
                .col(ColumnDef::new(T::Kind).string_len(KIND_LEN).not_null())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::OwnerPrincipalId).uuid().not_null())
                .col(
                    ColumnDef::new(T::Description)
                        .string_len(DESCRIPTION_LEN)
                        .null(),
                )
                .col(timestamp(manager, T::CreatedAt))
                .to_owned(),
        )
        .await?;
    create_index(
        manager,
        "event_broker_consumer_group_tenant_idx",
        T::Table,
        &[T::TenantId],
        false,
    )
    .await
}

async fn create_cursor(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use EventBrokerCursor as T;
    manager
        .create_table(
            Table::create()
                .table(T::Table)
                .if_not_exists()
                .col(gts_id(manager, T::ConsumerGroup))
                .col(ColumnDef::new(T::TopicId).big_integer().not_null())
                .col(ColumnDef::new(T::Partition).integer().not_null())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::Offset).big_integer().not_null())
                .col(timestamp(manager, T::UpdatedAt))
                .primary_key(
                    Index::create()
                        .col(T::ConsumerGroup)
                        .col(T::TopicId)
                        .col(T::Partition),
                )
                .to_owned(),
        )
        .await?;
    create_index(
        manager,
        "event_broker_cursor_tenant_idx",
        T::Table,
        &[T::TenantId],
        false,
    )
    .await
}

async fn create_producer(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use EventBrokerProducer as T;
    manager
        .create_table(
            Table::create()
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::Id).uuid().not_null().primary_key())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::OwnerId).uuid().not_null())
                .col(ColumnDef::new(T::Mode).string_len(MODE_LEN).not_null())
                .col(
                    ColumnDef::new(T::ClientAgent)
                        .string_len(CLIENT_AGENT_LEN)
                        .not_null(),
                )
                .col(timestamp(manager, T::CreatedAt))
                .to_owned(),
        )
        .await?;
    create_index(
        manager,
        "event_broker_producer_tenant_idx",
        T::Table,
        &[T::TenantId],
        false,
    )
    .await
}

async fn create_producer_sequence(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use EventBrokerProducerSequence as T;
    manager
        .create_table(
            Table::create()
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::ProducerId).uuid().not_null())
                .col(gts_id(manager, T::Topic))
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::LastSequence).big_integer().not_null())
                .col(timestamp(manager, T::UpdatedAt))
                .primary_key(Index::create().col(T::ProducerId).col(T::Topic))
                .to_owned(),
        )
        .await?;
    create_index(
        manager,
        "event_broker_producer_sequence_tenant_idx",
        T::Table,
        &[T::TenantId],
        false,
    )
    .await
}

#[derive(DeriveIden, Clone, Copy)]
enum EventBrokerConsumerGroup {
    Table,
    Id,
    Kind,
    TenantId,
    OwnerPrincipalId,
    Description,
    CreatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum EventBrokerCursor {
    Table,
    ConsumerGroup,
    TopicId,
    Partition,
    TenantId,
    Offset,
    UpdatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum EventBrokerProducer {
    Table,
    Id,
    TenantId,
    OwnerId,
    Mode,
    ClientAgent,
    CreatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum EventBrokerProducerSequence {
    Table,
    ProducerId,
    Topic,
    TenantId,
    LastSequence,
    UpdatedAt,
}
