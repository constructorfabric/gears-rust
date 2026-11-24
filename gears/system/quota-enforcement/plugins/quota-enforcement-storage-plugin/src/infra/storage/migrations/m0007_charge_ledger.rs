//! Migration `m0007`: quota resolver subjects (ADR-0008), metric types
//! (ADR-0013) and the charge ledger (ADR-0011), on top of `m0001`–`m0006`.
//!
//! - `qe_quotas`: `projection_type` becomes `subject_type`, a quota resolver
//!   subject type; the constraint contract columns go; `holds_pending` marks a
//!   deactivated allocation Quota whose holding entries the sweeper has not
//!   released yet.
//! - `qe_operation_log`: the actor is one text column, its record form.
//! - `qe_idempotency_records` is keyed by `(tenant_id, operation_type,
//!   idem_key)`: the subjects are part of the request fingerprint, not of the
//!   key. A primary key cannot be altered in SQLite, so the table is created
//!   again; its rows only answer replays within their window.
//! - `qe_leases` keeps `attribution_digest`, which the charge of its commit
//!   takes, and no subject key; `qe_lease_holds` keeps each entry's chain and
//!   position, by which a commit unwinds the unused part (ADR-0010).
//! - `qe_charges` holds one row per allowed debit or lease commit, unique per
//!   `(tenant_id, operation, idem_key)`; it is that operation's idempotency
//!   record. `expires_at` is when the charge is deleted, `NULL` while an entry
//!   holds it. `qe_charge_entries` holds one row per plan entry; `holding`
//!   marks the entries that hold their charge, and `quota_id` is not a foreign
//!   key, because the check would lock the Quota row outside the lock rank.
//!   `qe_charge_returns` holds one record per applied return, unique per
//!   `(tenant_id, return_key)`. The rule the columns store is stated in
//!   [`charge_repo`](crate::infra::storage::repo::charge_repo). Partial indexes
//!   carry each reader: the sweeper's range over `expires_at`, the release of
//!   one Quota's holding entries, the work list of pending Quotas, and the
//!   gauge over held charges.
//!
//! Existing values are not converted: nothing has run on the earlier shape.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

use super::ensure_supported;

#[derive(DeriveIden)]
enum QeQuotas {
    Table,
    HoldsPending,
    ConstraintContractType,
    ConstraintContractVersion,
}

#[derive(DeriveIden)]
enum QeOperationLog {
    Table,
    Actor,
    ActorSubjectId,
    ActorSubjectType,
}

#[derive(DeriveIden)]
enum QeLeases {
    Table,
    SubjectKey,
}

#[derive(DeriveIden)]
enum QeLeaseHolds {
    Table,
    Chain,
    Position,
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

#[derive(DeriveIden)]
enum QeCharges {
    Table,
    Id,
    TenantId,
    Operation,
    IdemKey,
    PayloadHash,
    AttributionDigest,
    DecisionBlob,
    EngineId,
    PolicyId,
    PolicyVersion,
    Metric,
    Amount,
    CreatedAt,
    ReturnWindowEnd,
    KeptUntil,
    ExpiresAt,
}

#[derive(DeriveIden)]
enum QeChargeEntries {
    Table,
    ChargeId,
    Chain,
    Position,
    TenantId,
    QuotaId,
    PeriodId,
    Charged,
    Returned,
    Holding,
}

#[derive(DeriveIden)]
enum QeChargeReturns {
    Table,
    TenantId,
    ReturnKey,
    ChargeId,
    Amount,
    CreatedAt,
}

const CHARGE_INDEXES: [&str; 4] = [
    "CREATE INDEX idx_qe_charges_expires_at \
     ON qe_charges (expires_at) WHERE expires_at IS NOT NULL",
    // Everything the held-charge gauge reads, so its aggregate stays on the index.
    "CREATE INDEX idx_qe_charges_held \
     ON qe_charges (metric, return_window_end, created_at) WHERE expires_at IS NULL",
    "CREATE INDEX idx_qe_charge_entries_holding \
     ON qe_charge_entries (quota_id) WHERE holding",
    "CREATE INDEX idx_qe_charge_returns_charge ON qe_charge_returns (charge_id)",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        let conn = manager.get_connection();
        // One change per statement: SQLite alters one column at a time, and
        // drops a column only when no index names it.
        for sql in [
            "DROP INDEX IF EXISTS idx_qe_quotas_status_metric_projection",
            "ALTER TABLE qe_quotas RENAME COLUMN projection_type TO subject_type",
            "ALTER TABLE qe_quotas DROP COLUMN constraint_contract_type",
            "ALTER TABLE qe_quotas DROP COLUMN constraint_contract_version",
            "CREATE INDEX idx_qe_quotas_status_metric_subject_type \
             ON qe_quotas (status, metric, subject_type, cap)",
            "ALTER TABLE qe_operation_log DROP COLUMN actor_subject_id",
            "ALTER TABLE qe_operation_log DROP COLUMN actor_subject_type",
            "ALTER TABLE qe_leases DROP COLUMN subject_key",
            "ALTER TABLE qe_leases RENAME COLUMN attribution_hash TO attribution_digest",
        ] {
            conn.execute_unprepared(sql).await?;
        }
        add_column(
            manager,
            QeQuotas::Table,
            ColumnDef::new(QeQuotas::HoldsPending)
                .boolean()
                .not_null()
                .default(false),
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX idx_qe_quotas_holds_pending ON qe_quotas (id) WHERE holds_pending",
        )
        .await?;
        add_column(
            manager,
            QeOperationLog::Table,
            ColumnDef::new(QeOperationLog::Actor)
                .text()
                .not_null()
                .default(""),
        )
        .await?;
        for column in [QeLeaseHolds::Chain, QeLeaseHolds::Position] {
            add_column(
                manager,
                QeLeaseHolds::Table,
                ColumnDef::new(column).integer().not_null().default(0),
            )
            .await?;
        }
        recreate_idempotency_records(manager, false).await?;
        create_charges(manager).await?;
        for sql in CHARGE_INDEXES {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        for table in [
            QeChargeReturns::Table.into_iden(),
            QeChargeEntries::Table.into_iden(),
            QeCharges::Table.into_iden(),
        ] {
            manager
                .drop_table(Table::drop().table(table).if_exists().to_owned())
                .await?;
        }
        recreate_idempotency_records(manager, true).await?;
        let conn = manager.get_connection();
        for sql in [
            "ALTER TABLE qe_lease_holds DROP COLUMN chain",
            "ALTER TABLE qe_lease_holds DROP COLUMN position",
            "ALTER TABLE qe_leases RENAME COLUMN attribution_digest TO attribution_hash",
            "ALTER TABLE qe_operation_log DROP COLUMN actor",
            "DROP INDEX IF EXISTS idx_qe_quotas_holds_pending",
            "ALTER TABLE qe_quotas DROP COLUMN holds_pending",
            "DROP INDEX IF EXISTS idx_qe_quotas_status_metric_subject_type",
            "ALTER TABLE qe_quotas RENAME COLUMN subject_type TO projection_type",
            "CREATE INDEX idx_qe_quotas_status_metric_projection \
             ON qe_quotas (status, metric, projection_type, cap)",
        ] {
            conn.execute_unprepared(sql).await?;
        }
        add_column(
            manager,
            QeLeases::Table,
            ColumnDef::new(QeLeases::SubjectKey)
                .blob()
                .not_null()
                .default(Vec::<u8>::new()),
        )
        .await?;
        add_column(
            manager,
            QeOperationLog::Table,
            ColumnDef::new(QeOperationLog::ActorSubjectId)
                .uuid()
                .not_null()
                .default(uuid::Uuid::nil()),
        )
        .await?;
        add_column(
            manager,
            QeOperationLog::Table,
            ColumnDef::new(QeOperationLog::ActorSubjectType)
                .text()
                .null(),
        )
        .await?;
        add_column(
            manager,
            QeQuotas::Table,
            ColumnDef::new(QeQuotas::ConstraintContractType)
                .text()
                .not_null()
                .default(""),
        )
        .await?;
        add_column(
            manager,
            QeQuotas::Table,
            ColumnDef::new(QeQuotas::ConstraintContractVersion)
                .integer()
                .not_null()
                .default(0),
        )
        .await
    }
}

async fn add_column(
    manager: &SchemaManager<'_>,
    table: impl IntoIden,
    column: &mut ColumnDef,
) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(table)
                .add_column(column.to_owned())
                .to_owned(),
        )
        .await
}

/// `qe_idempotency_records` as it is after this migration, or, with
/// `with_subject_key`, as `m0004` created it.
async fn recreate_idempotency_records(
    manager: &SchemaManager<'_>,
    with_subject_key: bool,
) -> Result<(), DbErr> {
    manager
        .drop_table(
            Table::drop()
                .table(QeIdempotencyRecords::Table)
                .if_exists()
                .to_owned(),
        )
        .await?;
    let mut table = Table::create();
    table.table(QeIdempotencyRecords::Table).col(
        ColumnDef::new(QeIdempotencyRecords::TenantId)
            .uuid()
            .not_null(),
    );
    if with_subject_key {
        table.col(
            ColumnDef::new(QeIdempotencyRecords::SubjectKey)
                .blob()
                .not_null(),
        );
    }
    table
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
        );
    if with_subject_key {
        table
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
            );
    }
    table
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
        );
    let mut key = Index::create();
    key.col(QeIdempotencyRecords::TenantId);
    if with_subject_key {
        key.col(QeIdempotencyRecords::SubjectKey);
    }
    key.col(QeIdempotencyRecords::OperationType)
        .col(QeIdempotencyRecords::IdemKey);
    table.primary_key(&mut key);
    manager.create_table(table).await?;
    manager
        .create_index(
            Index::create()
                .name("idx_qe_idempotency_expires")
                .table(QeIdempotencyRecords::Table)
                .col(QeIdempotencyRecords::ExpiresAt)
                .to_owned(),
        )
        .await
}

async fn create_charges(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(QeCharges::Table)
                .col(
                    ColumnDef::new(QeCharges::Id)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(ColumnDef::new(QeCharges::TenantId).uuid().not_null())
                .col(ColumnDef::new(QeCharges::Operation).text().not_null())
                .col(ColumnDef::new(QeCharges::IdemKey).text().not_null())
                .col(ColumnDef::new(QeCharges::PayloadHash).blob().not_null())
                .col(
                    ColumnDef::new(QeCharges::AttributionDigest)
                        .blob()
                        .not_null(),
                )
                .col(ColumnDef::new(QeCharges::DecisionBlob).text().not_null())
                .col(ColumnDef::new(QeCharges::EngineId).text().null())
                .col(ColumnDef::new(QeCharges::PolicyId).text().null())
                .col(ColumnDef::new(QeCharges::PolicyVersion).integer().null())
                .col(ColumnDef::new(QeCharges::Metric).text().not_null())
                .col(
                    ColumnDef::new(QeCharges::Amount)
                        .big_integer()
                        .not_null()
                        .check(Expr::col(QeCharges::Amount).gte(0)),
                )
                .col(
                    ColumnDef::new(QeCharges::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeCharges::ReturnWindowEnd)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(QeCharges::KeptUntil)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .col(
                    ColumnDef::new(QeCharges::ExpiresAt)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .index(
                    Index::create()
                        .name("uq_qe_charges_scope")
                        .col(QeCharges::TenantId)
                        .col(QeCharges::Operation)
                        .col(QeCharges::IdemKey)
                        .unique(),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_table(
            Table::create()
                .table(QeChargeEntries::Table)
                .col(ColumnDef::new(QeChargeEntries::ChargeId).uuid().not_null())
                .col(ColumnDef::new(QeChargeEntries::Chain).integer().not_null())
                .col(
                    ColumnDef::new(QeChargeEntries::Position)
                        .integer()
                        .not_null(),
                )
                .col(ColumnDef::new(QeChargeEntries::TenantId).uuid().not_null())
                .col(ColumnDef::new(QeChargeEntries::QuotaId).uuid().not_null())
                .col(ColumnDef::new(QeChargeEntries::PeriodId).uuid().null())
                .col(
                    ColumnDef::new(QeChargeEntries::Charged)
                        .big_integer()
                        .not_null()
                        .check(Expr::col(QeChargeEntries::Charged).gte(0)),
                )
                .col(
                    ColumnDef::new(QeChargeEntries::Returned)
                        .big_integer()
                        .not_null()
                        .check(
                            Expr::col(QeChargeEntries::Returned).gte(0).and(
                                Expr::col(QeChargeEntries::Returned)
                                    .lte(Expr::col(QeChargeEntries::Charged)),
                            ),
                        ),
                )
                .col(
                    ColumnDef::new(QeChargeEntries::Holding)
                        .boolean()
                        .not_null()
                        .default(false),
                )
                .primary_key(
                    Index::create()
                        .col(QeChargeEntries::ChargeId)
                        .col(QeChargeEntries::Chain)
                        .col(QeChargeEntries::Position),
                )
                .foreign_key(
                    ForeignKey::create()
                        .from(QeChargeEntries::Table, QeChargeEntries::ChargeId)
                        .to(QeCharges::Table, QeCharges::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_table(
            Table::create()
                .table(QeChargeReturns::Table)
                .col(ColumnDef::new(QeChargeReturns::TenantId).uuid().not_null())
                .col(ColumnDef::new(QeChargeReturns::ReturnKey).text().not_null())
                .col(ColumnDef::new(QeChargeReturns::ChargeId).uuid().not_null())
                .col(
                    ColumnDef::new(QeChargeReturns::Amount)
                        .big_integer()
                        .not_null()
                        .check(Expr::col(QeChargeReturns::Amount).gt(0)),
                )
                .col(
                    ColumnDef::new(QeChargeReturns::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .primary_key(
                    Index::create()
                        .col(QeChargeReturns::TenantId)
                        .col(QeChargeReturns::ReturnKey),
                )
                .foreign_key(
                    ForeignKey::create()
                        .from(QeChargeReturns::Table, QeChargeReturns::ChargeId)
                        .to(QeCharges::Table, QeCharges::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await
}
