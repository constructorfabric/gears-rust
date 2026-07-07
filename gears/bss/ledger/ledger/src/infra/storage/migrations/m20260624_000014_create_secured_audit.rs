//! Create the secured audit store: the append-only `bss.secured_audit_record`
//! and its own per-tenant tamper-evidence chain tip `bss.audit_chain_state`
//! (Slice 6 Phase 2 Group 2A). Each audit row is born sealed (`row_hash` /
//! `prev_hash` non-NULL) and is never updated; Postgres carries the
//! append-only `bss.reject_mutation()` trigger (reused from migration 000002)
//! to forbid any later UPDATE/DELETE. `SQLite` (non-production test backend)
//! mirrors the shape with the systematic transforms (`uuid`→`text`,
//! `jsonb`→`text`, `bytea`→`blob`, `timestamptz`→`text`, `bigint`→`integer`,
//! no `bss.` prefix) and omits the trigger (`SQLite` has none — mirror 000002).
//! Every CHECK, index, PK, and both tables are preserved on both backends.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.secured_audit_record (
        audit_id       uuid        NOT NULL PRIMARY KEY,
        tenant_id      uuid        NOT NULL,
        event_type     text        NOT NULL,
        actor_ref      text,
        reason_code    text,
        before_after   jsonb       NOT NULL DEFAULT '{}'::jsonb,
        correlation_id uuid,
        row_hash       bytea       NOT NULL,
        prev_hash      bytea       NOT NULL,
        at_utc         timestamptz NOT NULL DEFAULT now(),
        retain_until   timestamptz,
        CONSTRAINT chk_secured_audit_event_type CHECK (event_type IN (
          'conflict-capture','metadata-change','cross-tenant-access','manual-adjustment',
          'erasure','re-identification','account-lifecycle-change','exception-resolution',
          'freeze-set-clear','config-change','restore-event','period-reopen'))
    )",
    "CREATE INDEX idx_secured_audit_correlation
        ON bss.secured_audit_record (tenant_id, correlation_id)",
    "CREATE INDEX idx_secured_audit_event
        ON bss.secured_audit_record (tenant_id, event_type, at_utc)",
    "CREATE TRIGGER trg_secured_audit_append_only
        BEFORE UPDATE OR DELETE ON bss.secured_audit_record
        FOR EACH ROW EXECUTE FUNCTION bss.reject_mutation()",
    "CREATE TABLE bss.audit_chain_state (
        tenant_id     uuid   NOT NULL PRIMARY KEY,
        last_row_hash bytea  NOT NULL,
        last_audit_id uuid   NOT NULL,
        last_seq      bigint NOT NULL
    )",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TRIGGER IF EXISTS trg_secured_audit_append_only ON bss.secured_audit_record",
    "DROP TABLE IF EXISTS bss.audit_chain_state",
    "DROP TABLE IF EXISTS bss.secured_audit_record",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------
//
// Systematic transforms from the Postgres variant:
// * schema prefix `bss.` dropped (single namespace);
// * `uuid` → `text`; `jsonb` → `text` (JSON default `'{}'::jsonb` → `'{}'`);
// * `bytea` → `blob`; `bigint` → `integer`;
// * `timestamptz NOT NULL DEFAULT now()` → `text NOT NULL DEFAULT (CURRENT_TIMESTAMP)`,
//   nullable `timestamptz` → nullable `text`;
// * the append-only trigger and `bss.reject_mutation()` are DROPPED — SQLite
//   has neither; that invariant is re-asserted in application code / tests.
// Every CHECK, index, PK, and both tables are preserved.

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE secured_audit_record (
        audit_id       text    NOT NULL PRIMARY KEY,
        tenant_id      text    NOT NULL,
        event_type     text    NOT NULL,
        actor_ref      text,
        reason_code    text,
        before_after   text    NOT NULL DEFAULT '{}',
        correlation_id text,
        row_hash       blob    NOT NULL,
        prev_hash      blob    NOT NULL,
        at_utc         text    NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        retain_until   text,
        CONSTRAINT chk_secured_audit_event_type CHECK (event_type IN (
          'conflict-capture','metadata-change','cross-tenant-access','manual-adjustment',
          'erasure','re-identification','account-lifecycle-change','exception-resolution',
          'freeze-set-clear','config-change','restore-event','period-reopen'))
    )",
    "CREATE INDEX idx_secured_audit_correlation
        ON secured_audit_record (tenant_id, correlation_id)",
    "CREATE INDEX idx_secured_audit_event
        ON secured_audit_record (tenant_id, event_type, at_utc)",
    "CREATE TABLE audit_chain_state (
        tenant_id     text    NOT NULL PRIMARY KEY,
        last_row_hash blob    NOT NULL,
        last_audit_id text    NOT NULL,
        last_seq      integer NOT NULL
    )",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS audit_chain_state",
    "DROP TABLE IF EXISTS secured_audit_record",
];

#[cfg(test)]
#[path = "m20260624_000014_create_secured_audit_tests.rs"]
mod check_drift_tests;

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
