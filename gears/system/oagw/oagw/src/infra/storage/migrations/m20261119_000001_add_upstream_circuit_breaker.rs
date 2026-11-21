//! `oagw_upstream.circuit_breaker` and `circuit_breaker_sharing`
//! (ADR-0005, ADR-0009): the upstream's circuit breaker as a JSON document
//! without `sharing`, and its sharing mode as a column, like the rate limit.
//!
//! One column per statement: `SQLite` adds one per `ALTER TABLE`. Rows
//! written before have no breaker: `NULL` and `private`, what the mapper
//! writes for an absent one.

use sea_orm_migration::prelude::*;

use super::m20261005_000001_create_upstreams_and_routes::{ENUM_LEN, json};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use OagwUpstream as T;
        manager
            .alter_table(
                Table::alter()
                    .table(T::Table)
                    .add_column(json(T::CircuitBreaker).null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(T::Table)
                    .add_column(
                        ColumnDef::new(T::CircuitBreakerSharing)
                            .string_len(ENUM_LEN)
                            .not_null()
                            .default("private"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use OagwUpstream as T;
        for column in [T::CircuitBreakerSharing, T::CircuitBreaker] {
            manager
                .alter_table(
                    Table::alter()
                        .table(T::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwUpstream {
    Table,
    CircuitBreaker,
    CircuitBreakerSharing,
}
