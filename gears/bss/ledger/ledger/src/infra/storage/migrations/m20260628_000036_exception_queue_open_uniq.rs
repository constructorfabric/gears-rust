//! m036: partial UNIQUE index on `ledger_exception_queue` enforcing AT-MOST-ONE OPEN
//! row per `(tenant_id, exception_type, business_ref)`. Backs the fire-and-forget dedup
//! in `ExceptionRouter` (the `exists_open_for_ref` check + insert is racy under default
//! isolation: two concurrent routes could both insert). With the index, the loser's
//! insert fails on the unique constraint and the fire-and-forget route swallows it, so
//! exactly one OPEN row survives.
//!
//! Dual-backend (`PG` `bss`-qualified / `SQLite` bare). The table is new in m034 and carries
//! no production data yet, so the unique index cannot fail on existing duplicates at
//! creation.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE UNIQUE INDEX ledger_exception_queue_open_uniq
        ON bss.ledger_exception_queue (tenant_id, exception_type, business_ref) WHERE status = 'OPEN'",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP INDEX IF EXISTS bss.ledger_exception_queue_open_uniq"];

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE UNIQUE INDEX ledger_exception_queue_open_uniq
        ON ledger_exception_queue (tenant_id, exception_type, business_ref) WHERE status = 'OPEN'"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP INDEX IF EXISTS ledger_exception_queue_open_uniq"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_UP_STATEMENTS,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP_STATEMENTS,
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_DOWN_STATEMENTS,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN_STATEMENTS,
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
