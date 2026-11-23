//! Migration `m0009` — `pending_user_deprovisions`, the record of a user
//! deletion that has started but not yet finished.
//!
//! `UserService::delete_user` writes a row once it has confirmed that the
//! user belongs to the tenant named in the request, and before it asks the
//! `IdP` to delete the user. It removes the row once the `IdP` delete, the
//! user-group membership cleanup and the principal-lifecycle hooks have all
//! succeeded. While the row exists, a retried DELETE finishes the deletion
//! even though the `IdP` no longer has the user; without one, a DELETE for a
//! user the tenant does not have does nothing.
//!
//! Schema rationale:
//!
//! * `(tenant_id, user_id)` is the PRIMARY KEY: one deletion in progress per
//!   user and tenant, and an idempotent insert on retry.
//! * `tenant_id` references `tenants(id)` with `ON DELETE CASCADE` on
//!   Postgres. `toolkit-db` does not enable `PRAGMA foreign_keys` on
//!   `SQLite`, so the tenant hard-delete also removes the rows explicitly.
//! * `created_at` is forensic only: how long a deletion has been left
//!   unfinished.
//!
//! The row is an operation marker, not identity data: it holds only the two
//! identifiers the request carried, and only while the deletion is in
//! flight.

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
                r"
CREATE TABLE IF NOT EXISTS pending_user_deprovisions (
    tenant_id UUID NOT NULL,
    user_id UUID NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (tenant_id, user_id),
    CONSTRAINT fk_pending_user_deprovisions_tenant
        FOREIGN KEY (tenant_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE
);
                ",
            ],
            sea_orm::DatabaseBackend::Sqlite => vec![
                // SQLite has no native UUID or TIMESTAMPTZ types; sea_orm
                // serialises both to TEXT.
                r"
CREATE TABLE IF NOT EXISTS pending_user_deprovisions (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (tenant_id, user_id)
);
                ",
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
            .execute_unprepared("DROP TABLE IF EXISTS pending_user_deprovisions;")
            .await?;
        Ok(())
    }
}
