//! Create the Mode-B FX-revaluation completion-marker table in schema `bss`:
//! `ledger_fx_revaluation_run` (VHP-1859 review C3). One COMPLETE marker per
//! `(tenant_id, period_id)`, upserted by the revaluation job after a period-end
//! `run_period` finishes ALL scopes without error. The period-close gate, when
//! Mode-B is enabled (`fx.revaluation_enabled`), REQUIRES this marker for the
//! closing period inside the close SERIALIZABLE txn — without it (a failed/lagged
//! run) close BLOCKS and emits `FxRevaluationIncomplete`, rather than certifying a
//! period whose missing `FX_REVALUATION` entries the closed-period guard would
//! make unpostable forever. Entry-existence alone cannot distinguish "ran, nothing
//! to post" from "never ran", so a marker is required (`run_scope` legitimately
//! posts zero entries for a zero-net payer). `scope` is retained as a forward-compat
//! column (the whole-period run records `PERIOD`). Tenant scoping is via `SecureORM`.
//! `SQLite` mirrors the shape (`uuid`→`text`, `timestamptz`→`text`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.ledger_fx_revaluation_run (
        tenant_id        uuid         NOT NULL,
        period_id        varchar(64)  NOT NULL,
        scope            varchar(32)  NOT NULL,
        status           varchar(16)  NOT NULL,
        completed_at_utc timestamptz  NOT NULL,
        PRIMARY KEY (tenant_id, period_id),
        CONSTRAINT chk_fx_revaluation_run_status CHECK (status IN ('COMPLETE'))
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_fx_revaluation_run"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; the CHECK + PK preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE ledger_fx_revaluation_run (
        tenant_id        text         NOT NULL,
        period_id        varchar(64)  NOT NULL,
        scope            varchar(32)  NOT NULL,
        status           varchar(16)  NOT NULL,
        completed_at_utc text         NOT NULL,
        PRIMARY KEY (tenant_id, period_id),
        CONSTRAINT chk_fx_revaluation_run_status CHECK (status IN ('COMPLETE'))
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_fx_revaluation_run"];

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
