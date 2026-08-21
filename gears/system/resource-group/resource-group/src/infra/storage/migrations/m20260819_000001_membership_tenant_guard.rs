// Created: 2026-08-19 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
/// Migration: add `resource_membership_tenant` guard table for
/// membership-race prevention (RG-01).
///
/// This table serializes the first membership of a resource: PK
/// `(gts_type_id, resource_id)` ensures that only one concurrent writer can
/// insert the guard row, which also records the resource's tenant.
/// Subsequent `add_membership` calls read back the tenant and reject the
/// insertion if the group belongs to a different tenant.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                let sql = r"
CREATE TABLE IF NOT EXISTS resource_membership_tenant (
    gts_type_id SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE RESTRICT,
    resource_id TEXT NOT NULL,
    tenant_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (gts_type_id, resource_id)
);
";
                conn.execute_unprepared(sql).await?;
                Ok(())
            }
            sea_orm::DatabaseBackend::Sqlite => {
                let sql = r"
CREATE TABLE IF NOT EXISTS resource_membership_tenant (
    gts_type_id SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE RESTRICT,
    resource_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    created_at DATETIME NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    PRIMARY KEY (gts_type_id, resource_id)
);
";
                conn.execute_unprepared(sql).await?;
                Ok(())
            }
            _ => Err(DbErr::Migration(format!(
                "Unsupported backend: {backend:?}"
            ))),
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        match backend {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                conn.execute_unprepared("DROP TABLE IF EXISTS resource_membership_tenant")
                    .await?;
                Ok(())
            }
            _ => Err(DbErr::Migration(format!(
                "Unsupported backend: {backend:?}"
            ))),
        }
    }
}
