//! Migration `m0002`: the Quota tables of the quota-lifecycle feature (DESIGN
//! section 3.7, `qe_quotas`, `qe_quota_allocation_counters`, and the minimal
//! `qe_operation_log`).
//!
//! `qe_quotas` holds one row per Quota; `id` is a `UUIDv7`, so ascending id is
//! creation order and the list cursor. Caps live in `0..=i64::MAX` under a
//! check constraint; JSON columns (`notification_thresholds`, `metadata`) are
//! text the plugin serializes canonically. `qe_quota_allocation_counters` is
//! the in-flight counter of allocation Quotas, one row per allocation Quota,
//! created with it; consumption counters arrive with consumption-operations.
//! `qe_operation_log` records who did what to which Quota.

use sea_orm_migration::prelude::*;

use super::ensure_supported;

#[derive(DeriveIden)]
pub(super) enum QeQuotas {
    Table,
    Id,
    TenantId,
    ProjectionType,
    SubjectId,
    Metric,
    QuotaType,
    Period,
    EnforcementMode,
    Cap,
    NotificationThresholds,
    ValidityStart,
    ValidityEnd,
    FailOpenHint,
    Metadata,
    Source,
    Status,
    ConstraintContractType,
    ConstraintContractVersion,
    RecordVersion,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(super) enum QeQuotaAllocationCounters {
    Table,
    QuotaId,
    TenantId,
    InFlight,
    RecordVersion,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum QeOperationLog {
    Table,
    Id,
    TenantId,
    QuotaId,
    Operation,
    ActorSubjectId,
    ActorSubjectType,
    RecordVersion,
    Detail,
    OccurredAt,
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        Box::pin(create_quotas(manager)).await?;
        create_allocation_counters(manager).await?;
        create_operation_log(manager).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeQuotaAllocationCounters::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeOperationLog::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(QeQuotas::Table).if_exists().to_owned())
            .await
    }
}

async fn create_quotas(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(QeQuotas::Table)
                .if_not_exists()
                .col(ColumnDef::new(QeQuotas::Id).uuid().not_null().primary_key())
                .col(ColumnDef::new(QeQuotas::TenantId).uuid().not_null())
                .col(ColumnDef::new(QeQuotas::ProjectionType).text().not_null())
                .col(ColumnDef::new(QeQuotas::SubjectId).text().not_null())
                .col(ColumnDef::new(QeQuotas::Metric).text().not_null())
                .col(ColumnDef::new(QeQuotas::QuotaType).text().not_null())
                .col(ColumnDef::new(QeQuotas::Period).text().null())
                .col(ColumnDef::new(QeQuotas::EnforcementMode).text().not_null())
                .col(
                    ColumnDef::new(QeQuotas::Cap).big_integer().null().check(
                        Expr::col(QeQuotas::Cap)
                            .is_null()
                            .or(Expr::col(QeQuotas::Cap).gte(0)),
                    ),
                )
                .col(
                    ColumnDef::new(QeQuotas::NotificationThresholds)
                        .text()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeQuotas::ValidityStart)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .col(
                    ColumnDef::new(QeQuotas::ValidityEnd)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .col(ColumnDef::new(QeQuotas::FailOpenHint).boolean().not_null())
                .col(ColumnDef::new(QeQuotas::Metadata).text().not_null())
                .col(ColumnDef::new(QeQuotas::Source).text().not_null())
                .col(
                    ColumnDef::new(QeQuotas::Status)
                        .text()
                        .not_null()
                        .check(Expr::col(QeQuotas::Status).is_in(["active", "deactivated"])),
                )
                .col(
                    ColumnDef::new(QeQuotas::ConstraintContractType)
                        .text()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeQuotas::ConstraintContractVersion)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeQuotas::RecordVersion)
                        .integer()
                        .not_null()
                        .check(Expr::col(QeQuotas::RecordVersion).gte(1)),
                )
                .col(
                    ColumnDef::new(QeQuotas::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeQuotas::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_qe_quotas_tenant_status_metric")
                .table(QeQuotas::Table)
                .col(QeQuotas::TenantId)
                .col(QeQuotas::Status)
                .col(QeQuotas::Metric)
                .col(QeQuotas::Id)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_qe_quotas_tenant_subject")
                .table(QeQuotas::Table)
                .col(QeQuotas::TenantId)
                .col(QeQuotas::ProjectionType)
                .col(QeQuotas::SubjectId)
                .col(QeQuotas::Id)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_qe_quotas_status_metric_projection")
                .table(QeQuotas::Table)
                .col(QeQuotas::Status)
                .col(QeQuotas::Metric)
                .col(QeQuotas::ProjectionType)
                .col(QeQuotas::Cap)
                .to_owned(),
        )
        .await?;
    Ok(())
}

async fn create_allocation_counters(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(QeQuotaAllocationCounters::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(QeQuotaAllocationCounters::QuotaId)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(QeQuotaAllocationCounters::TenantId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeQuotaAllocationCounters::InFlight)
                        .big_integer()
                        .not_null()
                        .check(Expr::col(QeQuotaAllocationCounters::InFlight).gte(0)),
                )
                .col(
                    ColumnDef::new(QeQuotaAllocationCounters::RecordVersion)
                        .integer()
                        .not_null()
                        .check(Expr::col(QeQuotaAllocationCounters::RecordVersion).gte(1)),
                )
                .col(
                    ColumnDef::new(QeQuotaAllocationCounters::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .foreign_key(
                    ForeignKey::create()
                        .from(
                            QeQuotaAllocationCounters::Table,
                            QeQuotaAllocationCounters::QuotaId,
                        )
                        .to(QeQuotas::Table, QeQuotas::Id)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .to_owned(),
        )
        .await?;
    Ok(())
}

async fn create_operation_log(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(QeOperationLog::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(QeOperationLog::Id)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(ColumnDef::new(QeOperationLog::TenantId).uuid().not_null())
                .col(ColumnDef::new(QeOperationLog::QuotaId).uuid().null())
                .col(ColumnDef::new(QeOperationLog::Operation).text().not_null())
                .col(
                    ColumnDef::new(QeOperationLog::ActorSubjectId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeOperationLog::ActorSubjectType)
                        .text()
                        .null(),
                )
                .col(
                    ColumnDef::new(QeOperationLog::RecordVersion)
                        .integer()
                        .null(),
                )
                .col(ColumnDef::new(QeOperationLog::Detail).text().not_null())
                .col(
                    ColumnDef::new(QeOperationLog::OccurredAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_qe_operation_log_occurred")
                .table(QeOperationLog::Table)
                .col(QeOperationLog::OccurredAt)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_qe_operation_log_tenant_quota")
                .table(QeOperationLog::Table)
                .col(QeOperationLog::TenantId)
                .col(QeOperationLog::QuotaId)
                .col(QeOperationLog::OccurredAt)
                .to_owned(),
        )
        .await?;

    Ok(())
}
