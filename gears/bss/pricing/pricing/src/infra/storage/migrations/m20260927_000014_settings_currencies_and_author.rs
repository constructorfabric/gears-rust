//! D-438: the tenant settings offer currencies and say who changed them. A FORWARD migration
//! (D-427 closed D-412: the chain is deployed and no shipped migration is edited again).
//!
//! `ADD COLUMN` only, inside the toolkit runner's transaction, on both dialects:
//! - `pricing_settings.currencies`, declared as `invoice_line_templates` is (`jsonb` on Postgres,
//!   `text` on `SQLite`; the entity's `Json`), `NOT NULL DEFAULT '[]'`: every existing row reads
//!   "any currency", so no existing book or tenant changes behaviour;
//! - `pricing_settings.updated_by`, nullable, declared as the dialect's other uuid columns are
//!   (`uuid` on Postgres, `text` on `SQLite`, where the application stores a 16-byte blob): NULL on
//!   every row written before this migration.
//!
//! No CHECK is added: the currency codes are judged by the door (`CURRENCY_INVALID`), as a book's
//! own code is; `default_rounding` gets no CHECK either (D-437; the deploy pre-flight is the gate).
//!
//! Each step is skipped when its column is already there (Postgres `IF NOT EXISTS`; on `SQLite`
//! the catalog is read first), so a replay changes nothing; `down` drops the two columns the same
//! way.
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    r"ALTER TABLE bss.pricing_settings ADD COLUMN IF NOT EXISTS currencies jsonb NOT NULL DEFAULT '[]'::jsonb",
    r"ALTER TABLE bss.pricing_settings ADD COLUMN IF NOT EXISTS updated_by uuid",
];
const PG_DOWN: &[&str] = &[
    r"ALTER TABLE bss.pricing_settings DROP COLUMN IF EXISTS updated_by",
    r"ALTER TABLE bss.pricing_settings DROP COLUMN IF EXISTS currencies",
];
/// `SQLite`: `(column, ADD COLUMN statement)`, each run only for a column the table lacks.
const SQLITE_UP: &[(&str, &str)] = &[
    (
        "currencies",
        r"ALTER TABLE pricing_settings ADD COLUMN currencies text NOT NULL DEFAULT '[]'",
    ),
    (
        "updated_by",
        r"ALTER TABLE pricing_settings ADD COLUMN updated_by text",
    ),
];

/// Whether `pricing_settings` has the column, on `SQLite`.
async fn sqlite_has(manager: &SchemaManager<'_>, column: &str) -> Result<bool, DbErr> {
    let rows = manager
        .get_connection()
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "SELECT name AS v FROM pragma_table_info('pricing_settings') WHERE name = '{column}'"
            ),
        ))
        .await?;
    Ok(!rows.is_empty())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => {
                for (column, sql) in SQLITE_UP {
                    if !sqlite_has(manager, column).await? {
                        super::exec_backend(self.name(), manager, &[], &[sql]).await?;
                    }
                }
                Ok(())
            }
            _ => super::exec_backend(self.name(), manager, PG_UP, &[]).await,
        }
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => {
                for column in ["updated_by", "currencies"] {
                    if sqlite_has(manager, column).await? {
                        let sql = format!("ALTER TABLE pricing_settings DROP COLUMN {column}");
                        super::exec_backend(self.name(), manager, &[], &[sql.as_str()]).await?;
                    }
                }
                Ok(())
            }
            _ => super::exec_backend(self.name(), manager, PG_DOWN, &[]).await,
        }
    }
}
