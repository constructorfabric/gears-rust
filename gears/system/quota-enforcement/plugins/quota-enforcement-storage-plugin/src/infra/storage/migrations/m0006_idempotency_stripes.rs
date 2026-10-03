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

use crate::infra::storage::entity::idempotency_stripe::STRIPES;

const MYSQL_NOT_SUPPORTED: &str = "quota-enforcement-storage-plugin: MySQL is not supported; \
    this migration set targets PostgreSQL and SQLite";

const TABLE: &str = "qe_idempotency_stripes";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let last = STRIPES - 1;
        let statements = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => vec![
                format!("CREATE TABLE IF NOT EXISTS {TABLE} (stripe INTEGER PRIMARY KEY);"),
                format!(
                    "INSERT INTO {TABLE} (stripe) SELECT generate_series(0, {last}) \
                     ON CONFLICT (stripe) DO NOTHING;"
                ),
            ],
            sea_orm::DatabaseBackend::Sqlite => vec![
                format!("CREATE TABLE IF NOT EXISTS {TABLE} (stripe INTEGER PRIMARY KEY);"),
                format!(
                    "WITH RECURSIVE s(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM s WHERE n < {last}) \
                     INSERT OR IGNORE INTO {TABLE} (stripe) SELECT n FROM s;"
                ),
            ],
            _ => return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned())),
        };
        let conn = manager.get_connection();
        for sql in statements {
            conn.execute_unprepared(&sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if matches!(
            manager.get_database_backend(),
            sea_orm::DatabaseBackend::MySql
        ) {
            return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
        }
        manager
            .get_connection()
            .execute_unprepared(&format!("DROP TABLE IF EXISTS {TABLE};"))
            .await?;
        Ok(())
    }
}
