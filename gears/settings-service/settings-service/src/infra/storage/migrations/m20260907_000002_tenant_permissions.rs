// Created: 2026-09-07 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-tenant-access-table:p1
//! `tenant_permissions` — sparse per-tenant restrictions (DESIGN.md §4.7).
//!
//! `overridable` is no row, and the root tenant never has one. Rows survive a
//! declaration's soft-retire — the cascade fires only on a hard delete of the
//! declaration — and `uq_tenant_permission` is the upsert target of a set.

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
                r"CREATE TABLE IF NOT EXISTS tenant_permissions (
                    id              uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
                    declaration_id  uuid         NOT NULL
                                    REFERENCES setting_declarations (id) ON DELETE CASCADE,
                    tenant_id       uuid         NOT NULL,
                    access          text         NOT NULL
                                    CHECK (access IN ('read_only', 'hidden')),
                    set_by          text         NOT NULL,
                    created_at      timestamptz  NOT NULL DEFAULT now(),
                    updated_at      timestamptz  NOT NULL DEFAULT now()
                );",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_tenant_permission
                     ON tenant_permissions (declaration_id, tenant_id);",
                "CREATE INDEX IF NOT EXISTS idx_tenant_permission_tenant
                     ON tenant_permissions (tenant_id);",
            ]
        } else {
            vec![
                r"CREATE TABLE IF NOT EXISTS tenant_permissions (
                    id              text     PRIMARY KEY,
                    declaration_id  text     NOT NULL
                                    REFERENCES setting_declarations (id) ON DELETE CASCADE,
                    tenant_id       text     NOT NULL,
                    access          text     NOT NULL
                                    CHECK (access IN ('read_only', 'hidden')),
                    set_by          text     NOT NULL,
                    created_at      text     NOT NULL,
                    updated_at      text     NOT NULL
                );",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_tenant_permission
                     ON tenant_permissions (declaration_id, tenant_id);",
                "CREATE INDEX IF NOT EXISTS idx_tenant_permission_tenant
                     ON tenant_permissions (tenant_id);",
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
            .execute_unprepared("DROP TABLE IF EXISTS tenant_permissions;")
            .await?;
        Ok(())
    }
}
