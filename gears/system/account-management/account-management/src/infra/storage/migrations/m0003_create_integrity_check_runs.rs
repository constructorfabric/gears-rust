//! Migration `m0003` — single-flight gate for the hierarchy-integrity check.
//!
//! Both backends (`Postgres` + `SQLite`) coordinate via a primary-key
//! INSERT into the `integrity_check_runs` table. The PK is a synthetic
//! singleton (`id = 1`, enforced by a `CHECK` constraint) so the
//! table holds at most one row at a time: a second worker attempting
//! to insert receives a unique-violation, which the runtime lock
//! gear maps to
//! [`crate::domain::error::DomainError::IntegrityCheckInProgress`].
//!
//! The gate row is committed by a short `acquire` transaction
//! independent of the snapshot/work transaction (see
//! `crate::infra::storage::integrity::lock::acquire_committed`), so
//! contenders see it immediately and surface
//! `IntegrityCheckInProgress` instead of queueing on an uncommitted
//! PK and waiting for the original transaction to finish. Release
//! is a separate committed `DELETE`; a stale-lock sweep on the next
//! `acquire` reclaims any row left behind by a crashed worker
//! (`MAX_LOCK_AGE`).

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
                "CREATE TABLE integrity_check_runs ( \
                    id INTEGER PRIMARY KEY CHECK (id = 1), \
                    worker_id UUID NOT NULL, \
                    started_at TIMESTAMPTZ NOT NULL \
                );",
            ],
            sea_orm::DatabaseBackend::Sqlite => vec![
                // SQLite has no native UUID type; sea_orm serialises
                // `Uuid` to canonical TEXT, so the column type stays
                // `TEXT` and storage is hand-shaken through the
                // entity's `Uuid` field.
                "CREATE TABLE integrity_check_runs ( \
                    id INTEGER PRIMARY KEY CHECK (id = 1), \
                    worker_id TEXT NOT NULL, \
                    started_at TIMESTAMP NOT NULL \
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
            .execute_unprepared("DROP TABLE IF EXISTS integrity_check_runs;")
            .await?;
        Ok(())
    }
}
