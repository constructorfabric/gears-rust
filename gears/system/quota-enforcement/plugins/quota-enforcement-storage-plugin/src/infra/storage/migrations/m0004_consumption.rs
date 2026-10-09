//! Migration `m0004`: the counter and replay tables of the
//! consumption-operations feature (DESIGN section 3.7,
//! `quota_consumption_counters` and `idempotency_records`).
//!
//! `qe_quota_consumption_counters` holds one row per `(Quota, period)`. The
//! unique key on `(quota_id, period_start)` is both the current-period index
//! and the arbiter of concurrent materialization: two transactions opening the
//! same period race on it, and the loser reads the winner's row.
//! `period_end` is `NOT NULL`; a one-time Quota stores the open-ended sentinel,
//! so the "period closed" predicate stays `now >= period_end` everywhere
//! instead of branching on a nullable column.
//!
//! `qe_idempotency_records` is keyed by the full four-component scope, which is
//! also what serializes two writers that share a key but lock disjoint Quota
//! rows: the loser's insert violates this primary key, rolls its transaction
//! back, and resolves into a replay or a payload mismatch. There is
//! deliberately no key-only index: every lookup, rollback's original included,
//! carries the whole scope.
//!
//! `attribution_hash` is what a rollback must present to reverse a debit: the
//! scope proves the tenant and subjects, this proves the metric and resource.
//! `applied_entries` is plugin-private: the per-Quota amounts and their
//! acquisition periods, which is what rollback reverses (I5).

use sea_orm_migration::prelude::*;

use super::ensure_supported;
use super::m0002_quotas::{QeQuotaAllocationCounters, QeQuotas};

#[derive(DeriveIden)]
pub(super) enum QeQuotaConsumptionCounters {
    Table,
    PeriodId,
    QuotaId,
    TenantId,
    PeriodStart,
    PeriodEnd,
    Consumed,
    HighestCrossedThresholdPct,
    IsSettled,
    RecordVersion,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum QeIdempotencyRecords {
    Table,
    TenantId,
    SubjectKey,
    OperationType,
    IdemKey,
    PayloadHash,
    DecisionBlob,
    AppliedEntries,
    AttributionHash,
    ReversedByKey,
    EngineId,
    PolicyId,
    PolicyVersion,
    CreatedAt,
    ExpiresAt,
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
                    .table(QeQuotaConsumptionCounters::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::PeriodId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::QuotaId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::TenantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::PeriodStart)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::PeriodEnd)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::Consumed)
                            .big_integer()
                            .not_null()
                            .check(Expr::col(QeQuotaConsumptionCounters::Consumed).gte(0)),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::HighestCrossedThresholdPct)
                            .small_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::IsSettled)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::RecordVersion)
                            .integer()
                            .not_null()
                            .check(Expr::col(QeQuotaConsumptionCounters::RecordVersion).gte(1)),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeQuotaConsumptionCounters::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .index(
                        Index::create()
                            .name("uq_qe_consumption_period")
                            .col(QeQuotaConsumptionCounters::QuotaId)
                            .col(QeQuotaConsumptionCounters::PeriodStart)
                            .unique(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                QeQuotaConsumptionCounters::Table,
                                QeQuotaConsumptionCounters::QuotaId,
                            )
                            .to(QeQuotas::Table, QeQuotas::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_qe_consumption_unsettled")
                    .table(QeQuotaConsumptionCounters::Table)
                    .col(QeQuotaConsumptionCounters::QuotaId)
                    .col(QeQuotaConsumptionCounters::IsSettled)
                    .col(QeQuotaConsumptionCounters::PeriodEnd)
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(QeIdempotencyRecords::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::TenantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::SubjectKey)
                            .blob()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::OperationType)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::IdemKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::PayloadHash)
                            .blob()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::DecisionBlob)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::AppliedEntries)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::AttributionHash)
                            .blob()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::ReversedByKey)
                            .text()
                            .null(),
                    )
                    .col(ColumnDef::new(QeIdempotencyRecords::EngineId).text().null())
                    .col(ColumnDef::new(QeIdempotencyRecords::PolicyId).text().null())
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::PolicyVersion)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QeIdempotencyRecords::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(QeIdempotencyRecords::TenantId)
                            .col(QeIdempotencyRecords::SubjectKey)
                            .col(QeIdempotencyRecords::OperationType)
                            .col(QeIdempotencyRecords::IdemKey),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_qe_idempotency_expires")
                    .table(QeIdempotencyRecords::Table)
                    .col(QeIdempotencyRecords::ExpiresAt)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(QeQuotaAllocationCounters::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(QeQuotaConsumptionCounters::HighestCrossedThresholdPct)
                            .small_integer()
                            .null(),
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
                    .table(QeIdempotencyRecords::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        // The added column stays: SQLite cannot drop one before 3.35.
        manager
            .drop_table(
                Table::drop()
                    .table(QeQuotaConsumptionCounters::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
