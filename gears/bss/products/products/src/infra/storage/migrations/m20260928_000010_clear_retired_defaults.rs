//! P-D-220: a retired category is never the default, stored rows included.
//!
//! The doors refuse to make a retired category the default and clear the default they retire,
//! but a category retired while it was the default before P-D-220 stayed the default. This
//! forward migration clears every such row, on both dialects, with one statement:
//!
//! `UPDATE products_category SET is_default = false, version = version + 1, updated_at = <now>
//! WHERE is_default AND status = 'retired'` (`bss.products_category` on Postgres).
//!
//! The clear is written as a category write writes it (P-D-218's cleared holder): `version` + 1,
//! so a client holding the old tag meets `STALE_REVISION`, and `updated_at` the migration's
//! instant, bound from the process clock in UTC as the doors bind theirs (never an SQL-written
//! timestamp). It writes **no audit row**: a migration is not an act of a user, and the audit log
//! records acts (P-D-220 says so). A tenant whose default was retired holds no default after it,
//! which P-D-196 allows. The schema does not change. A replay matches no row.
//!
//! `down()` changes nothing: the schema is the same on both sides of this migration, a category
//! that is not the default is valid before it too, and restoring a retired default would restore
//! the state P-D-220 forbids.
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &str = "UPDATE bss.products_category SET is_default = false, version = version + 1, \
     updated_at = $1 WHERE is_default AND status = 'retired'";
const SQLITE_UP: &str = "UPDATE products_category SET is_default = false, version = version + 1, \
     updated_at = ? WHERE is_default AND status = 'retired'";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let sql = match backend {
            DatabaseBackend::Postgres => PG_UP,
            DatabaseBackend::Sqlite => SQLITE_UP,
            // `DatabaseBackend` is `#[non_exhaustive]`: every other backend is refused, as
            // `exec_backend` refuses it.
            _ => {
                return Err(DbErr::Migration(format!(
                    "{}: {backend:?} is not a supported backend for bss-products",
                    self.name()
                )));
            }
        };
        let now = time::OffsetDateTime::now_utc();
        manager
            .get_connection()
            .execute_raw(Statement::from_sql_and_values(backend, sql, [now.into()]))
            .await
            .map(|_| ())
            .map_err(|e| DbErr::Migration(format!("{}: {e}", self.name())))
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260928_000010_clear_retired_defaults_tests.rs"]
mod tests;
