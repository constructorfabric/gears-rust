//! Relax the `journal_entry` append-only trigger from a blanket
//! reject-all-mutations guard (P1's `bss.reject_mutation`) into an
//! append-only *seal* guard: a single in-place `UPDATE` is permitted iff it
//! only sets the tamper-evidence chain columns (`row_hash` / `prev_hash` /
//! the prev pointers) on a not-yet-sealed row, and every business column is
//! left byte-for-byte unchanged. `DELETE` stays forbidden, and a second seal
//! of an already-sealed row is rejected. Postgres-only: `SQLite` (the
//! non-production test backend) carries no triggers — those invariants are
//! re-asserted in application code — so its up/down statement arrays are empty.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE OR REPLACE FUNCTION bss.ledger_journal_entry_append_guard() RETURNS trigger AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'append-only: DELETE not permitted on journal_entry';
  END IF;
  IF OLD.row_hash IS NOT NULL THEN
    RAISE EXCEPTION 'append-only: chain already sealed (entry=%)', OLD.entry_id;
  END IF;
  IF NEW.row_hash IS NULL OR NEW.prev_hash IS NULL THEN
    RAISE EXCEPTION 'append-only: seal must set row_hash and prev_hash';
  END IF;
  IF ROW(NEW.entry_id,NEW.tenant_id,NEW.period_id,NEW.legal_entity_id,NEW.entry_currency,
         NEW.source_doc_type,NEW.source_business_id,NEW.reverses_entry_id,NEW.reverses_period_id,
         NEW.posted_at_utc,NEW.effective_at,NEW.origin,NEW.posted_by_actor_id,NEW.correlation_id,
         NEW.rounding_evidence,NEW.created_seq)
     IS DISTINCT FROM
     ROW(OLD.entry_id,OLD.tenant_id,OLD.period_id,OLD.legal_entity_id,OLD.entry_currency,
         OLD.source_doc_type,OLD.source_business_id,OLD.reverses_entry_id,OLD.reverses_period_id,
         OLD.posted_at_utc,OLD.effective_at,OLD.origin,OLD.posted_by_actor_id,OLD.correlation_id,
         OLD.rounding_evidence,OLD.created_seq) THEN
    RAISE EXCEPTION 'append-only: only chain columns may be sealed (entry=%)', OLD.entry_id;
  END IF;
  RETURN NEW;
END; $$ LANGUAGE plpgsql",
    "DROP TRIGGER trg_journal_entry_append_only ON bss.ledger_journal_entry",
    "CREATE TRIGGER trg_journal_entry_append_guard
        BEFORE UPDATE OR DELETE ON bss.ledger_journal_entry
        FOR EACH ROW EXECUTE FUNCTION bss.ledger_journal_entry_append_guard()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TRIGGER trg_journal_entry_append_guard ON bss.ledger_journal_entry",
    "CREATE TRIGGER trg_journal_entry_append_only
        BEFORE UPDATE OR DELETE ON bss.ledger_journal_entry
        FOR EACH ROW EXECUTE FUNCTION bss.reject_mutation()",
    "DROP FUNCTION IF EXISTS bss.ledger_journal_entry_append_guard()",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------
//
// SQLite omits all triggers and PL/pgSQL (see the P1 journal-tables
// migration): the append-only / seal invariants are re-asserted in
// application code, so there is nothing to relax here. Both arrays are empty.

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
