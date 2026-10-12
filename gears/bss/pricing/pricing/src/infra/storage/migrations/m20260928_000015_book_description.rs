//! D-444: a price book has an optional description. A FORWARD migration (D-427 closed D-412: the
//! chain is deployed and no shipped migration is edited again).
//!
//! `ADD COLUMN` only, inside the toolkit runner's transaction, on both dialects:
//! `pricing_price_book.description`, nullable `text`: NULL on every book written before this
//! migration, which reads as a book without a description. Its length (at most 2000 characters) is
//! judged by the door (`BOOK_DESCRIPTION_TOO_LONG`), as a book's name is; no CHECK is added.
//!
//! The step is skipped when the column is already there (Postgres `IF NOT EXISTS`; on `SQLite` the
//! catalog is read first), so a replay changes nothing; `down` drops the column the same way.
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] =
    &[r"ALTER TABLE bss.pricing_price_book ADD COLUMN IF NOT EXISTS description text"];
const PG_DOWN: &[&str] = &[r"ALTER TABLE bss.pricing_price_book DROP COLUMN IF EXISTS description"];
const SQLITE_UP: &str = r"ALTER TABLE pricing_price_book ADD COLUMN description text";
const SQLITE_DOWN: &str = r"ALTER TABLE pricing_price_book DROP COLUMN description";

/// Whether `pricing_price_book` has the column, on `SQLite`.
async fn sqlite_has_description(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    let rows = manager
        .get_connection()
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name AS v FROM pragma_table_info('pricing_price_book') \
             WHERE name = 'description'"
                .to_owned(),
        ))
        .await?;
    Ok(!rows.is_empty())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => {
                if !sqlite_has_description(manager).await? {
                    super::exec_backend(self.name(), manager, &[], &[SQLITE_UP]).await?;
                }
                Ok(())
            }
            _ => super::exec_backend(self.name(), manager, PG_UP, &[]).await,
        }
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => {
                if sqlite_has_description(manager).await? {
                    super::exec_backend(self.name(), manager, &[], &[SQLITE_DOWN]).await?;
                }
                Ok(())
            }
            _ => super::exec_backend(self.name(), manager, PG_DOWN, &[]).await,
        }
    }
}
