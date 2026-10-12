// Created: 2026-09-24 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-table:p1
//! `audit_records` is append-only at the store, not only by convention.
//!
//! The application never issues an `UPDATE` against the table and the only
//! `DELETE` is retention pruning (DESIGN.md §4.7). This migration makes the
//! database say the same: a trigger refuses every `UPDATE`, and on `PostgreSQL`
//! also a `DELETE` while an explicit `retain_until` hold is still in force —
//! the default retention window lives in configuration, so the trigger guards
//! the explicit hold only and leaves pruning of expired rows to the sweep.
//!
//! The gear cannot grant itself a narrower database role — provisioning owns
//! roles — but it can carry a trigger in its own migration, as the ledger gear
//! does for its append-only tables. `SQLite`, the test backend, carries the
//! `UPDATE` guard; its text timestamps make the hold comparison unreliable, so
//! the `DELETE` guard is `PostgreSQL`'s alone, and the test suite exercises what
//! it can.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: Vec<&str> = if backend == DatabaseBackend::Postgres {
            vec![
                r"CREATE OR REPLACE FUNCTION settings_audit_reject_mutation() RETURNS trigger AS $$
                BEGIN
                    IF TG_OP = 'UPDATE' THEN
                        RAISE EXCEPTION 'audit_records is append-only: UPDATE is not permitted';
                    END IF;
                    IF TG_OP = 'DELETE' AND OLD.retain_until IS NOT NULL AND OLD.retain_until > now() THEN
                        RAISE EXCEPTION 'audit_records: row % is held until %', OLD.id, OLD.retain_until;
                    END IF;
                    RETURN OLD;
                END;
                $$ LANGUAGE plpgsql;",
                "DROP TRIGGER IF EXISTS trg_audit_records_append_only ON audit_records;",
                r"CREATE TRIGGER trg_audit_records_append_only
                    BEFORE UPDATE OR DELETE ON audit_records
                    FOR EACH ROW EXECUTE FUNCTION settings_audit_reject_mutation();",
            ]
        } else {
            vec![
                r"CREATE TRIGGER IF NOT EXISTS trg_audit_records_append_only
                    BEFORE UPDATE ON audit_records
                BEGIN
                    SELECT RAISE(ABORT, 'audit_records is append-only: UPDATE is not permitted');
                END;",
            ]
        };
        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: Vec<&str> = if backend == DatabaseBackend::Postgres {
            vec![
                "DROP TRIGGER IF EXISTS trg_audit_records_append_only ON audit_records;",
                "DROP FUNCTION IF EXISTS settings_audit_reject_mutation();",
            ]
        } else {
            vec!["DROP TRIGGER IF EXISTS trg_audit_records_append_only;"]
        };
        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }
}
