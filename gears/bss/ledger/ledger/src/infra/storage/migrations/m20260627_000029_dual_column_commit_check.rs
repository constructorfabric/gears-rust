//! Extend the `check_entry_balanced` constraint-trigger function with the FX
//! dual-column functional-balance assertion (Slice 5, spec §2). The existing
//! transaction-balance checks (empty / mixed-payer / currency-mismatch /
//! per-(currency,scale) zero-sum) are reproduced verbatim from the P1
//! journal-tables migration; only the functional block is added. `CREATE OR
//! REPLACE FUNCTION` updates the body in place — the existing deferred
//! constraint trigger `trg_journal_entry_balanced` keeps pointing at it.
//!
//! NULL-aware so existing single-currency posts (every line's
//! `functional_amount_minor` NULL) stay byte-green: let
//! `f = count(functional NOT NULL)`. `f = 0` → skip (single-currency).
//! `f = line_count` → enforce `SUM(DR.functional) = SUM(CR.functional)`.
//! `0 < f < line_count` → RAISE (a partial-functional entry is a posting bug —
//! fail loud, never silently imbalance). Postgres-only: `SQLite` carries no
//! triggers, so the application-level `validate_balanced_entry` re-asserts the
//! same NULL-aware rule (Phase 1, group B).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — replace the function body (trigger unchanged).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE OR REPLACE FUNCTION bss.check_entry_balanced() RETURNS trigger AS $$
        DECLARE
          line_count        int;
          payer_count       int;
          currency_mismatch int;
          func_count        int;
          unbalanced        int;
          func_imbalance    bigint;
        BEGIN
          SELECT count(*),
                 count(DISTINCT l.payer_tenant_id),
                 count(*) FILTER (WHERE l.currency <> NEW.entry_currency
                                  AND NOT (l.amount_minor = 0
                                           AND l.functional_amount_minor IS NOT NULL)),
                 count(*) FILTER (WHERE l.functional_amount_minor IS NOT NULL)
            INTO line_count, payer_count, currency_mismatch, func_count
            FROM bss.ledger_journal_line l
           WHERE (l.tenant_id, l.period_id, l.entry_id)
                 = (NEW.tenant_id, NEW.period_id, NEW.entry_id);

          IF line_count < 1 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_EMPTY entry=%', NEW.entry_id;
          END IF;
          IF payer_count > 1 THEN
            RAISE EXCEPTION 'MIXED_PAYER_TENANT entry=%', NEW.entry_id;
          END IF;
          IF currency_mismatch > 0 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_CURRENCY_MISMATCH entry=%', NEW.entry_id;
          END IF;

          -- zero-sum per (currency, currency_scale), exact (zero tolerance)
          SELECT count(*) INTO unbalanced FROM (
            SELECT 1
            FROM bss.ledger_journal_line l
            WHERE (l.tenant_id, l.period_id, l.entry_id)
                  = (NEW.tenant_id, NEW.period_id, NEW.entry_id)
            GROUP BY l.currency, l.currency_scale
            HAVING sum(CASE WHEN l.side = 'DR' THEN l.amount_minor
                            ELSE -l.amount_minor END) <> 0
          ) u;
          IF unbalanced > 0 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_UNBALANCED entry=%', NEW.entry_id;
          END IF;

          -- FX dual-column functional balance (NULL-aware; Slice 5 decision 8).
          -- func_count = 0   -> single-currency entry: skip.
          -- 0 < func_count < line_count -> partial-functional posting bug: reject.
          -- func_count = line_count -> enforce SUM(DR.functional) = SUM(CR.functional).
          IF func_count > 0 AND func_count < line_count THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_FUNCTIONAL_PARTIAL entry=%', NEW.entry_id;
          END IF;
          IF func_count = line_count THEN
            SELECT coalesce(sum(CASE WHEN l.side = 'DR' THEN l.functional_amount_minor
                                     ELSE -l.functional_amount_minor END), 0)
              INTO func_imbalance
              FROM bss.ledger_journal_line l
             WHERE (l.tenant_id, l.period_id, l.entry_id)
                   = (NEW.tenant_id, NEW.period_id, NEW.entry_id);
            IF func_imbalance <> 0 THEN
              RAISE EXCEPTION 'LEDGER_ENTRY_FUNCTIONAL_UNBALANCED entry=%', NEW.entry_id;
            END IF;
          END IF;

          RETURN NULL;
        END;
     $$ LANGUAGE plpgsql",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    // Restore the P1 transaction-only body (no functional block).
    "CREATE OR REPLACE FUNCTION bss.check_entry_balanced() RETURNS trigger AS $$
        DECLARE
          line_count        int;
          payer_count       int;
          currency_mismatch int;
          unbalanced        int;
        BEGIN
          SELECT count(*),
                 count(DISTINCT l.payer_tenant_id),
                 count(*) FILTER (WHERE l.currency <> NEW.entry_currency
                                  AND NOT (l.amount_minor = 0
                                           AND l.functional_amount_minor IS NOT NULL))
            INTO line_count, payer_count, currency_mismatch
            FROM bss.ledger_journal_line l
           WHERE (l.tenant_id, l.period_id, l.entry_id)
                 = (NEW.tenant_id, NEW.period_id, NEW.entry_id);

          IF line_count < 1 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_EMPTY entry=%', NEW.entry_id;
          END IF;
          IF payer_count > 1 THEN
            RAISE EXCEPTION 'MIXED_PAYER_TENANT entry=%', NEW.entry_id;
          END IF;
          IF currency_mismatch > 0 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_CURRENCY_MISMATCH entry=%', NEW.entry_id;
          END IF;

          SELECT count(*) INTO unbalanced FROM (
            SELECT 1
            FROM bss.ledger_journal_line l
            WHERE (l.tenant_id, l.period_id, l.entry_id)
                  = (NEW.tenant_id, NEW.period_id, NEW.entry_id)
            GROUP BY l.currency, l.currency_scale
            HAVING sum(CASE WHEN l.side = 'DR' THEN l.amount_minor
                            ELSE -l.amount_minor END) <> 0
          ) u;
          IF unbalanced > 0 THEN
            RAISE EXCEPTION 'LEDGER_ENTRY_UNBALANCED entry=%', NEW.entry_id;
          END IF;

          RETURN NULL;
        END;
     $$ LANGUAGE plpgsql",
];

// ---------------------------------------------------------------------------
// SQLite variant — no triggers (the app-level validate_balanced_entry covers it).
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
