// Created: 2026-09-07 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-table:p1
//! `audit_records` — the gear-local audit store (DESIGN.md §4.7).
//!
//! Append-only: the mutation's transaction inserts one row as its last step,
//! nothing ever updates a row, and the only delete is retention pruning.
//! `idx_audit_scoped` serves the per-(setting, scope) history read as an index
//! lookup on `(declaration_key, tenant_id)` newest first; the partial
//! `idx_audit_retention` serves pruning of rows with an explicit horizon.
//!
//! The `SQLite` branch mirrors the `PostgreSQL` DDL with the types that engine
//! has — text for uuid and timestamps, text for JSON — so the same
//! migration runs the in-memory tests.

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
                r"CREATE TABLE IF NOT EXISTS audit_records (
                    id                    uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
                    resource              text         NOT NULL,
                    declaration_key       text         NOT NULL,
                    tenant_id             uuid         NULL,
                    operation             text         NOT NULL
                                          CHECK (operation IN ('create', 'change', 'revert',
                                                               'remove', 'clone', 'secret_use')),
                    actor                 text         NOT NULL,
                    actor_classification  text         NOT NULL
                                          CHECK (actor_classification IN ('public', 'pii')),
                    pre_value             jsonb,
                    post_value            jsonb,
                    outcome               text         NOT NULL
                                          CHECK (outcome IN ('success', 'failure')),
                    request_id            text         NOT NULL,
                    change_set_id         uuid,
                    occurred_at           timestamptz  NOT NULL DEFAULT now(),
                    retain_until          timestamptz
                );",
                "CREATE INDEX IF NOT EXISTS idx_audit_scoped
                     ON audit_records (declaration_key, tenant_id, occurred_at DESC);",
                "CREATE INDEX IF NOT EXISTS idx_audit_retention
                     ON audit_records (retain_until)
                     WHERE retain_until IS NOT NULL;",
            ]
        } else {
            vec![
                r"CREATE TABLE IF NOT EXISTS audit_records (
                    id                    text     PRIMARY KEY,
                    resource              text     NOT NULL,
                    declaration_key       text     NOT NULL,
                    tenant_id             text     NULL,
                    operation             text     NOT NULL
                                          CHECK (operation IN ('create', 'change', 'revert',
                                                               'remove', 'clone', 'secret_use')),
                    actor                 text     NOT NULL,
                    actor_classification  text     NOT NULL
                                          CHECK (actor_classification IN ('public', 'pii')),
                    pre_value             text,
                    post_value            text,
                    outcome               text     NOT NULL
                                          CHECK (outcome IN ('success', 'failure')),
                    request_id            text     NOT NULL,
                    change_set_id         text,
                    occurred_at           text     NOT NULL,
                    retain_until          text
                );",
                "CREATE INDEX IF NOT EXISTS idx_audit_scoped
                     ON audit_records (declaration_key, tenant_id, occurred_at DESC);",
                "CREATE INDEX IF NOT EXISTS idx_audit_retention
                     ON audit_records (retain_until)
                     WHERE retain_until IS NOT NULL;",
            ]
        };
        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS audit_records;")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260907_000001_audit_records_tests.rs"]
mod m20260907_000001_audit_records_tests;
