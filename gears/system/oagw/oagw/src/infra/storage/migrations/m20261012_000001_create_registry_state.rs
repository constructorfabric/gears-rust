//! `oagw_registry_state`: the one row that gates the registry reconcile on a
//! database (ADR-0009, ADR-0018 *Provisioning from the Types Registry*): the highest
//! deployment generation that claimed the registry, the content hash of the
//! last clean reconcile, and the reconcile lease.
//!
//! No `tenant_id`: the row is installation-wide. The seed row is inserted
//! here, idempotently, so every statement can address `id = 1` and treat a
//! miss as a missing seed. `lease_expires_at` and `applied_at` are written by
//! the database clock only.

use sea_orm_migration::prelude::*;

use super::m20261005_000001_create_upstreams_and_routes::{new_table, timestamp_type};

/// Hex SHA-256.
const HASH_LEN: u32 = 64;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use OagwRegistryState as T;
        manager
            .create_table(
                new_table(manager)
                    .table(T::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(T::Id)
                            .small_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(T::Generation).big_integer().null())
                    .col(ColumnDef::new(T::ContentHash).string_len(HASH_LEN).null())
                    .col(timestamp_type(manager, T::AppliedAt).null())
                    .col(ColumnDef::new(T::LeaseOwner).uuid().null())
                    .col(timestamp_type(manager, T::LeaseExpiresAt).null())
                    .to_owned(),
            )
            .await?;
        // Idempotent on every backend: a row already there is left as it is.
        manager
            .get_connection()
            .execute_unprepared(
                "INSERT INTO oagw_registry_state (id) SELECT 1 \
                 WHERE NOT EXISTS (SELECT 1 FROM oagw_registry_state WHERE id = 1)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(OagwRegistryState::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwRegistryState {
    Table,
    Id,
    Generation,
    ContentHash,
    AppliedAt,
    LeaseOwner,
    LeaseExpiresAt,
}
