//! Migration `m0001`: schema metadata and the three configuration tables
//! (DESIGN section 3.7, "Bootstrap seeded state").
//!
//! Configuration rows use the sentinel key `*` for the platform default, so
//! every table has a real primary key and `NULL` never enters a key.

use sea_orm_migration::prelude::*;

use super::ensure_supported;

#[derive(DeriveIden)]
enum QeSchemaMeta {
    Table,
    ContractMajor,
    AppliedAt,
}

#[derive(DeriveIden)]
enum QeContentionTimeoutConfig {
    Table,
    MetricKey,
    TimeoutMs,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum QeLeaseCapacityConfig {
    Table,
    TenantKey,
    MetricKey,
    MaxActiveLeases,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum QeIdempotencyRetentionConfig {
    Table,
    TenantKey,
    MetricKey,
    RetentionSeconds,
    UpdatedAt,
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        manager
            .create_table(
                Table::create()
                    .table(QeSchemaMeta::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QeSchemaMeta::ContractMajor)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(QeSchemaMeta::AppliedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(QeContentionTimeoutConfig::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QeContentionTimeoutConfig::MetricKey)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(QeContentionTimeoutConfig::TimeoutMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeContentionTimeoutConfig::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(QeLeaseCapacityConfig::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QeLeaseCapacityConfig::TenantKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeLeaseCapacityConfig::MetricKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeLeaseCapacityConfig::MaxActiveLeases)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeLeaseCapacityConfig::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(QeLeaseCapacityConfig::TenantKey)
                            .col(QeLeaseCapacityConfig::MetricKey),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(QeIdempotencyRetentionConfig::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QeIdempotencyRetentionConfig::TenantKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRetentionConfig::MetricKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRetentionConfig::RetentionSeconds)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRetentionConfig::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(QeIdempotencyRetentionConfig::TenantKey)
                            .col(QeIdempotencyRetentionConfig::MetricKey),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeIdempotencyRetentionConfig::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeLeaseCapacityConfig::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeContentionTimeoutConfig::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeSchemaMeta::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
