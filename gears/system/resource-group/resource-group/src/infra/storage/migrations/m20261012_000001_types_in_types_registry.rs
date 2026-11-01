// Created: 2026-10-12 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
//! Resource-group types move to the Types Registry.
//!
//! Group and membership rows reference a type by its Registry Reference
//! (UUID), with no foreign key: the type tables (`gts_type*`) and the
//! `gts_type_path` domain are dropped. `resource_group` gains `is_tenant`,
//! and a partial unique index allows at most one tenant root.
//!
//! No data is carried over: the tables are dropped and recreated.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        // The names `fk_resource_group_parent`, `fk_rgm_group_id` and
        // `uq_rg_tenant_root` are mirrored as constants in
        // `crate::infra::storage`: the repositories classify a violation by
        // matching the constraint name in the driver's message. Rename one
        // here and the classification silently stops firing.
        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                r"
DROP TABLE IF EXISTS resource_group_membership;
DROP TABLE IF EXISTS resource_group_closure;
DROP TABLE IF EXISTS resource_group;
DROP TABLE IF EXISTS gts_type_allowed_membership;
DROP TABLE IF EXISTS gts_type_allowed_parent;
DROP TABLE IF EXISTS gts_type;
DROP DOMAIN IF EXISTS gts_type_path;

CREATE TABLE IF NOT EXISTS resource_group (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id UUID,
    gts_type_id UUID NOT NULL,
    is_tenant BOOLEAN NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    metadata JSONB,
    tenant_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ DEFAULT NULL,
    CONSTRAINT fk_resource_group_parent
        FOREIGN KEY (parent_id)
        REFERENCES resource_group(id)
        ON UPDATE CASCADE
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_rg_parent_id ON resource_group (parent_id);
CREATE INDEX IF NOT EXISTS idx_rg_name ON resource_group (name);
CREATE INDEX IF NOT EXISTS idx_rg_gts_type_id ON resource_group (gts_type_id, id);
CREATE INDEX IF NOT EXISTS idx_rg_tenant_id ON resource_group (tenant_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_rg_tenant_root
    ON resource_group (is_tenant) WHERE parent_id IS NULL AND is_tenant;

CREATE TABLE IF NOT EXISTS resource_group_closure (
    ancestor_id UUID NOT NULL,
    descendant_id UUID NOT NULL,
    depth INTEGER NOT NULL CHECK (depth >= 0),
    PRIMARY KEY (ancestor_id, descendant_id),
    CONSTRAINT fk_closure_ancestor
        FOREIGN KEY (ancestor_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    CONSTRAINT fk_closure_descendant
        FOREIGN KEY (descendant_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_rgc_descendant_id ON resource_group_closure (descendant_id);
CREATE INDEX IF NOT EXISTS idx_rgc_ancestor_depth ON resource_group_closure (ancestor_id, depth);

CREATE TABLE IF NOT EXISTS resource_group_membership (
    group_id UUID NOT NULL,
    gts_type_id UUID NOT NULL,
    resource_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    PRIMARY KEY (group_id, gts_type_id, resource_id)
);

CREATE INDEX IF NOT EXISTS idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
DROP TABLE IF EXISTS resource_group_membership;
DROP TABLE IF EXISTS resource_group_closure;
DROP TABLE IF EXISTS resource_group;
DROP TABLE IF EXISTS gts_type_allowed_membership;
DROP TABLE IF EXISTS gts_type_allowed_parent;
DROP TABLE IF EXISTS gts_type;

CREATE TABLE IF NOT EXISTS resource_group (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    gts_type_id TEXT NOT NULL,
    is_tenant BOOLEAN NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    metadata TEXT,
    tenant_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT NULL,
    CONSTRAINT fk_resource_group_parent
        FOREIGN KEY (parent_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_rg_parent_id ON resource_group (parent_id);
CREATE INDEX IF NOT EXISTS idx_rg_name ON resource_group (name);
CREATE INDEX IF NOT EXISTS idx_rg_gts_type_id ON resource_group (gts_type_id, id);
CREATE INDEX IF NOT EXISTS idx_rg_tenant_id ON resource_group (tenant_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_rg_tenant_root
    ON resource_group (is_tenant) WHERE parent_id IS NULL AND is_tenant;

CREATE TABLE IF NOT EXISTS resource_group_closure (
    ancestor_id TEXT NOT NULL,
    descendant_id TEXT NOT NULL,
    depth INTEGER NOT NULL CHECK (depth >= 0),
    PRIMARY KEY (ancestor_id, descendant_id),
    CONSTRAINT fk_closure_ancestor
        FOREIGN KEY (ancestor_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    CONSTRAINT fk_closure_descendant
        FOREIGN KEY (descendant_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_rgc_descendant_id ON resource_group_closure (descendant_id);
CREATE INDEX IF NOT EXISTS idx_rgc_ancestor_depth ON resource_group_closure (ancestor_id, depth);

CREATE TABLE IF NOT EXISTS resource_group_membership (
    group_id TEXT NOT NULL,
    gts_type_id TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    PRIMARY KEY (group_id, gts_type_id, resource_id)
);

CREATE INDEX IF NOT EXISTS idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);
                "
            }
            _ => {
                return Err(DbErr::Migration(
                    "Only PostgreSQL and SQLite are supported".to_owned(),
                ));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            r"
DROP TABLE IF EXISTS resource_group_membership;
DROP TABLE IF EXISTS resource_group_closure;
DROP TABLE IF EXISTS resource_group;
            ",
        )
        .await?;
        Ok(())
    }
}
