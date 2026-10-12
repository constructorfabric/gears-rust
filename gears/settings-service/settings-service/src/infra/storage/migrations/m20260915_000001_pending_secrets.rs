// Created: 2026-09-15 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-secret-values-stage:p1
//! `pending_secrets` — a secret staged ahead of the batch (DESIGN.md §4.7).
//!
//! One row per stage, minted by the gear and single-use: the batch that names
//! its `pending_id` deletes it, and the sweep deletes what nobody claimed once
//! `expires_at` has passed. `idx_pending_secrets_expires` is the sweep's index.

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
                r"CREATE TABLE IF NOT EXISTS pending_secrets (
                    id              uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
                    declaration_id  uuid         NOT NULL
                                    REFERENCES setting_declarations (id) ON DELETE CASCADE,
                    tenant_id       uuid         NOT NULL,
                    subject_id      text         NOT NULL,
                    secret_ref      text         NOT NULL,
                    created_at      timestamptz  NOT NULL DEFAULT now(),
                    expires_at      timestamptz  NOT NULL
                );",
                "CREATE INDEX IF NOT EXISTS idx_pending_secrets_expires
                     ON pending_secrets (expires_at);",
            ]
        } else {
            vec![
                r"CREATE TABLE IF NOT EXISTS pending_secrets (
                    id              text     PRIMARY KEY,
                    declaration_id  text     NOT NULL
                                    REFERENCES setting_declarations (id) ON DELETE CASCADE,
                    tenant_id       text     NOT NULL,
                    subject_id      text     NOT NULL,
                    secret_ref      text     NOT NULL,
                    created_at      text     NOT NULL,
                    expires_at      text     NOT NULL
                );",
                "CREATE INDEX IF NOT EXISTS idx_pending_secrets_expires
                     ON pending_secrets (expires_at);",
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
            .execute_unprepared("DROP TABLE IF EXISTS pending_secrets;")
            .await?;
        Ok(())
    }
}
