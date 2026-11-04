//! Migration `m0005`: the lease tables of the lease-operations feature
//! (DESIGN section 3.7, `leases`, `lease_holds`, `lease_capacity_counters`).
//!
//! `qe_leases` carries the state machine and, with it, the two values a
//! settlement cannot take from its caller: `subject_key`, the idempotency
//! subject the acquisition fingerprinted, and `attribution_hash`, the
//! attribution the PDP authorized it under. Commit and release complete their
//! scope from the first and a rollback of a commit must present the second.
//! `reserved_amount` is what the acquisition asked for, which the commit's
//! share is measured against; the plan's holds need not sum to it.
//!
//! `qe_lease_holds` is one row per Quota in the plan, rather than an array on
//! the lease, so a Quota's holds are reachable by index from the Quota side:
//! the deactivation cascade and the expired-hold reconciliation both start
//! there. `returned_at` is that reconciliation's arbiter — an expired lease is
//! released the moment its TTL passes (I4), but its capacity sits in the
//! counter until a writer or the sweeper gives it back, and exactly one of them
//! may do so. The index leads with `(quota_id, period_id, returned_at)` because
//! every one of those readers asks the same question: what does this counter
//! row still owe?
//!
//! `qe_lease_capacity_counters` is the serialization point of the
//! per-`(tenant, metric)` cap (I7), not its source of truth: `active_count` is
//! maintained for diagnostics, while admission counts live leases under the
//! row lock, so an expired lease never occupies the cap. A pair's row is created
//! with its first Quota, so an acquisition only ever locks it; the backfill here
//! gives every pair that already has a Quota its row. It copies the earliest
//! `created_at` rather than taking `now()`, so the stored timestamp has exactly
//! the encoding the ORM itself writes on both backends.

use sea_orm_migration::prelude::*;

use super::ensure_supported;
use super::m0002_quotas::QeQuotas;
use super::m0004_consumption::QeQuotaConsumptionCounters;

#[derive(DeriveIden)]
enum QeLeases {
    Table,
    Token,
    TenantId,
    Metric,
    SubjectKey,
    AttributionHash,
    IdemKey,
    State,
    ReservedAmount,
    AcquiredAt,
    ExpiryAt,
    ResolvedAt,
    RecordVersion,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum QeLeaseHolds {
    Table,
    LeaseToken,
    QuotaId,
    TenantId,
    HeldAmount,
    PeriodId,
    ReturnedAt,
}

#[derive(DeriveIden)]
enum QeLeaseCapacityCounters {
    Table,
    TenantId,
    Metric,
    ActiveCount,
    RecordVersion,
    UpdatedAt,
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        create_leases(manager).await?;
        create_lease_holds(manager).await?;
        create_capacity_counters(manager).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeLeaseHolds::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(QeLeases::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeLeaseCapacityCounters::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

async fn create_leases(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(QeLeases::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(QeLeases::Token)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(ColumnDef::new(QeLeases::TenantId).uuid().not_null())
                .col(ColumnDef::new(QeLeases::Metric).text().not_null())
                .col(ColumnDef::new(QeLeases::SubjectKey).blob().not_null())
                .col(ColumnDef::new(QeLeases::AttributionHash).blob().not_null())
                .col(ColumnDef::new(QeLeases::IdemKey).text().not_null())
                .col(ColumnDef::new(QeLeases::State).text().not_null().check(
                    Expr::col(QeLeases::State).is_in([
                        "active",
                        "committed",
                        "released",
                        "auto_released",
                        "resolved_by_deactivation",
                    ]),
                ))
                .col(
                    ColumnDef::new(QeLeases::ReservedAmount)
                        .big_integer()
                        .not_null()
                        .check(Expr::col(QeLeases::ReservedAmount).gte(0)),
                )
                .col(
                    ColumnDef::new(QeLeases::AcquiredAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeLeases::ExpiryAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeLeases::ResolvedAt)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .col(
                    ColumnDef::new(QeLeases::RecordVersion)
                        .integer()
                        .not_null()
                        .check(Expr::col(QeLeases::RecordVersion).gte(1)),
                )
                .col(
                    ColumnDef::new(QeLeases::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeLeases::UpdatedAt)
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
                .name("idx_qe_leases_cap")
                .table(QeLeases::Table)
                .col(QeLeases::TenantId)
                .col(QeLeases::Metric)
                .col(QeLeases::State)
                .col(QeLeases::ExpiryAt)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_qe_leases_expiry")
                .table(QeLeases::Table)
                .col(QeLeases::State)
                .col(QeLeases::ExpiryAt)
                .to_owned(),
        )
        .await?;
    Ok(())
}

async fn create_lease_holds(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(QeLeaseHolds::Table)
                .if_not_exists()
                .col(ColumnDef::new(QeLeaseHolds::LeaseToken).uuid().not_null())
                .col(ColumnDef::new(QeLeaseHolds::QuotaId).uuid().not_null())
                .col(ColumnDef::new(QeLeaseHolds::TenantId).uuid().not_null())
                .col(
                    ColumnDef::new(QeLeaseHolds::HeldAmount)
                        .big_integer()
                        .not_null()
                        .check(Expr::col(QeLeaseHolds::HeldAmount).gte(0)),
                )
                .col(ColumnDef::new(QeLeaseHolds::PeriodId).uuid().null())
                .col(
                    ColumnDef::new(QeLeaseHolds::ReturnedAt)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .primary_key(
                    Index::create()
                        .col(QeLeaseHolds::LeaseToken)
                        .col(QeLeaseHolds::QuotaId),
                )
                .foreign_key(
                    ForeignKey::create()
                        .from(QeLeaseHolds::Table, QeLeaseHolds::LeaseToken)
                        .to(QeLeases::Table, QeLeases::Token)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .foreign_key(
                    ForeignKey::create()
                        .from(QeLeaseHolds::Table, QeLeaseHolds::QuotaId)
                        .to(QeQuotas::Table, QeQuotas::Id)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .from(QeLeaseHolds::Table, QeLeaseHolds::PeriodId)
                        .to(
                            QeQuotaConsumptionCounters::Table,
                            QeQuotaConsumptionCounters::PeriodId,
                        )
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_qe_lease_holds_counter")
                .table(QeLeaseHolds::Table)
                .col(QeLeaseHolds::QuotaId)
                .col(QeLeaseHolds::PeriodId)
                .col(QeLeaseHolds::ReturnedAt)
                .to_owned(),
        )
        .await?;
    Ok(())
}

async fn create_capacity_counters(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(QeLeaseCapacityCounters::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(QeLeaseCapacityCounters::TenantId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeLeaseCapacityCounters::Metric)
                        .text()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeLeaseCapacityCounters::ActiveCount)
                        .integer()
                        .not_null()
                        .check(Expr::col(QeLeaseCapacityCounters::ActiveCount).gte(0)),
                )
                .col(
                    ColumnDef::new(QeLeaseCapacityCounters::RecordVersion)
                        .integer()
                        .not_null()
                        .check(Expr::col(QeLeaseCapacityCounters::RecordVersion).gte(1)),
                )
                .col(
                    ColumnDef::new(QeLeaseCapacityCounters::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .primary_key(
                    Index::create()
                        .col(QeLeaseCapacityCounters::TenantId)
                        .col(QeLeaseCapacityCounters::Metric),
                )
                .to_owned(),
        )
        .await?;
    let backfill = Query::insert()
        .into_table(QeLeaseCapacityCounters::Table)
        .columns([
            QeLeaseCapacityCounters::TenantId,
            QeLeaseCapacityCounters::Metric,
            QeLeaseCapacityCounters::ActiveCount,
            QeLeaseCapacityCounters::RecordVersion,
            QeLeaseCapacityCounters::UpdatedAt,
        ])
        .select_from(
            Query::select()
                .column(QeQuotas::TenantId)
                .column(QeQuotas::Metric)
                .expr(Expr::val(0))
                .expr(Expr::val(1))
                .expr(Func::min(Expr::col(QeQuotas::CreatedAt)))
                .from(QeQuotas::Table)
                .group_by_col(QeQuotas::TenantId)
                .group_by_col(QeQuotas::Metric)
                .to_owned(),
        )
        .map_err(|error| DbErr::Custom(error.to_string()))?
        .on_conflict(
            OnConflict::columns([
                QeLeaseCapacityCounters::TenantId,
                QeLeaseCapacityCounters::Metric,
            ])
            .do_nothing()
            .to_owned(),
        )
        .to_owned();
    manager.exec_stmt(backfill).await?;

    Ok(())
}
