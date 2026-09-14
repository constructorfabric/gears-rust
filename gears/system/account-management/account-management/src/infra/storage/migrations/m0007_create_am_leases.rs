//! Migration `m0007` — `am_leases` distributed-lease table backing
//! the hierarchy-integrity coordinator (see `infra::lease`).
//!
//! Schema rationale:
//!
//! * `key` is the PRIMARY KEY (no synthetic id) — saves an index and
//!   makes adding a second coordination domain a zero-schema change.
//! * `locked_until` defaults to the Unix epoch so the steal-filter
//!   `WHERE locked_until < NOW()` works uniformly regardless of
//!   whether the row was ever held.
//! * `attempts` defaults to `0`; the acquire path bumps it on every
//!   steal so a flapping cluster surfaces as a high-water value
//!   (alerting hook on `attempts >= 3`).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str = "account-management migrations: MySQL is not supported \
    (this migration set targets PostgreSQL/SQLite); add a dedicated MySQL migration set \
    before running against MySQL";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: Vec<&str> = match backend {
            sea_orm::DatabaseBackend::Postgres => vec![
                "CREATE TABLE am_leases ( \
                    key TEXT PRIMARY KEY, \
                    locked_by UUID NULL, \
                    locked_until TIMESTAMPTZ NOT NULL DEFAULT 'epoch', \
                    attempts INTEGER NOT NULL DEFAULT 0 \
                );",
            ],
            sea_orm::DatabaseBackend::Sqlite => vec![
                // SQLite has no native UUID or TIMESTAMPTZ types;
                // sea_orm serialises `Uuid` to canonical TEXT and
                // `OffsetDateTime` to ISO-8601 TEXT, so both columns
                // are declared `TEXT`. The epoch default below uses
                // the SQLite literal form the `OffsetDateTime`
                // mapper accepts on read.
                "CREATE TABLE am_leases ( \
                    key TEXT PRIMARY KEY, \
                    locked_by TEXT NULL, \
                    locked_until TEXT NOT NULL DEFAULT '1970-01-01 00:00:00+00:00', \
                    attempts INTEGER NOT NULL DEFAULT 0 \
                );",
            ],
            _ => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        if matches!(backend, sea_orm::DatabaseBackend::MySql) {
            return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
        }
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS am_leases;")
            .await?;
        Ok(())
    }
}
