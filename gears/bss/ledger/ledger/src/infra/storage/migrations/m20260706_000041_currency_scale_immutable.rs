//! Enforce currency-scale immutability at the DB layer (design §3.7 /
//! MoneyModule): `minor_units` for a `(tenant, currency)` is **immutable once
//! any posting exists** (`CURRENCY_SCALE_LOCKED`). The application path already
//! refuses a scale change once a `journal_line` exists for the currency
//! (`ReferenceRepo::upsert_currency_scale` → `RepoError::CurrencyScaleLocked`),
//! but a direct SQL `UPDATE` bypassing the gear would silently re-denominate
//! history. This adds the defense-in-depth trigger the design calls for — the
//! sibling of the journal / fx-snapshot append-only guards, which likewise pair
//! an app-level assert with a DB trigger.
//!
//! The guard fires ONLY when `minor_units` actually changes AND a posting for
//! the `(tenant, currency)` exists — a same-value upsert (the gear's
//! `ON CONFLICT DO UPDATE` rewrites the column to its current value) and a
//! pre-posting scale correction both pass. `SQLite` (non-production test
//! backend) carries no triggers; immutability is re-asserted in application code.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE OR REPLACE FUNCTION bss.reject_currency_scale_change() RETURNS trigger AS $$
        BEGIN
            IF NEW.minor_units IS DISTINCT FROM OLD.minor_units
               AND EXISTS (
                   SELECT 1 FROM bss.ledger_journal_line
                   WHERE tenant_id = OLD.tenant_id AND currency = OLD.currency
               )
            THEN
                RAISE EXCEPTION 'currency scale locked: minor_units for (%, %) is immutable once a posting exists',
                    OLD.tenant_id, OLD.currency;
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_currency_scale_immutable
        BEFORE UPDATE ON bss.ledger_currency_scale_registry
        FOR EACH ROW EXECUTE FUNCTION bss.reject_currency_scale_change()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TRIGGER IF EXISTS trg_currency_scale_immutable ON bss.ledger_currency_scale_registry",
    "DROP FUNCTION IF EXISTS bss.reject_currency_scale_change()",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema: no triggers (immutability re-asserted
// in application code, as with the journal / fx-snapshot append-only guards).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[];
const SQLITE_DOWN_STATEMENTS: &[&str] = &[];

// ---------------------------------------------------------------------------
// Migration dispatch.
// ---------------------------------------------------------------------------

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
