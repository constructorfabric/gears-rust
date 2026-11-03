//! `event_broker_spec_cache` table - `SpecificationManager`'s local
//! topic/event-type cache (eb-single-process-implementation D1). Built with
//! the schema builder, so one definition serves `SQLite`, `PostgreSQL` and
//! `MySQL`/`MariaDB` ([`super::columns`]).

use sea_orm_migration::prelude::*;

use super::columns::{create_index, document, gts_id};

/// `SpecKind::as_str`: `topic` or `event_type`.
const KIND_LEN: u32 = 16;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use EventBrokerSpecCache as T;
        manager
            .create_table(
                Table::create()
                    .table(T::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(T::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(gts_id(manager, T::GtsId))
                    .col(ColumnDef::new(T::Kind).string_len(KIND_LEN).not_null())
                    .col(document(manager, T::Payload))
                    .to_owned(),
            )
            .await?;
        create_index(
            manager,
            "event_broker_spec_cache_gts_id_idx",
            T::Table,
            &[T::GtsId],
            true,
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(EventBrokerSpecCache::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden, Clone, Copy)]
enum EventBrokerSpecCache {
    Table,
    Id,
    GtsId,
    Kind,
    Payload,
}
