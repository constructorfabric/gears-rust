//! Migration `m0006`: the idempotency stripes (invariant I8).
//!
//! Two writers of one idempotency scope whose Quota rows do not overlap meet
//! only on the record's primary key, where an insert or delete waits for the
//! other transaction with no `NOWAIT` to bound it. Each scope therefore maps to
//! one of a fixed set of rows, which a writer locks `NOWAIT` inside its
//! transaction before it reads or writes the record. The rows are created here,
//! all of them, so no writer ever has to insert one.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

use super::ensure_supported;
use crate::infra::storage::entity::idempotency_stripe::STRIPES;

#[derive(DeriveIden)]
enum QeIdempotencyStripes {
    Table,
    Stripe,
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
                    .table(QeIdempotencyStripes::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QeIdempotencyStripes::Stripe)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .to_owned(),
            )
            .await?;
        // The one backend-specific step: generating the rows.
        let last = STRIPES - 1;
        let seed = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => format!(
                "INSERT INTO qe_idempotency_stripes (stripe) SELECT generate_series(0, {last}) \
                 ON CONFLICT (stripe) DO NOTHING;"
            ),
            _ => format!(
                "WITH RECURSIVE s(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM s WHERE n < {last}) \
                 INSERT OR IGNORE INTO qe_idempotency_stripes (stripe) SELECT n FROM s;"
            ),
        };
        manager.get_connection().execute_unprepared(&seed).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        manager
            .drop_table(
                Table::drop()
                    .table(QeIdempotencyStripes::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
