// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                r"
CREATE DOMAIN gts_type_path AS TEXT
    CHECK (
        LENGTH(VALUE) <= 1024
    );

CREATE TABLE IF NOT EXISTS gts_type (
    id SMALLINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    schema_id gts_type_path NOT NULL UNIQUE,
    metadata_schema JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ DEFAULT NULL
);

CREATE TABLE IF NOT EXISTS gts_type_allowed_parent (
    type_id        SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    parent_type_id SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    PRIMARY KEY (type_id, parent_type_id)
);

CREATE TABLE IF NOT EXISTS gts_type_allowed_membership (
    type_id            SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    membership_type_id SMALLINT NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    PRIMARY KEY (type_id, membership_type_id)
);

CREATE TABLE IF NOT EXISTS resource_group (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id UUID,
    gts_type_id SMALLINT NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    metadata JSONB,
    tenant_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ DEFAULT NULL,
    CONSTRAINT fk_rg_gts_type
        FOREIGN KEY (gts_type_id)
        REFERENCES gts_type(id)
        ON DELETE RESTRICT,
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
    gts_type_id SMALLINT NOT NULL,
    resource_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    CONSTRAINT fk_rgm_gts_type
        FOREIGN KEY (gts_type_id) REFERENCES gts_type(id)
        ON DELETE RESTRICT,
    CONSTRAINT uq_resource_group_membership_unique
        UNIQUE (group_id, gts_type_id, resource_id)
);

CREATE INDEX IF NOT EXISTS idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gts_type (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    schema_id TEXT NOT NULL UNIQUE,
    metadata_schema TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT NULL
);

CREATE TABLE IF NOT EXISTS gts_type_allowed_parent (
    type_id        INTEGER NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    parent_type_id INTEGER NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    PRIMARY KEY (type_id, parent_type_id)
);

CREATE TABLE IF NOT EXISTS gts_type_allowed_membership (
    type_id            INTEGER NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    membership_type_id INTEGER NOT NULL REFERENCES gts_type(id) ON DELETE CASCADE,
    PRIMARY KEY (type_id, membership_type_id)
);

CREATE TABLE IF NOT EXISTS resource_group (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    gts_type_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    metadata TEXT,
    tenant_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT NULL,
    CONSTRAINT fk_rg_gts_type
        FOREIGN KEY (gts_type_id) REFERENCES gts_type(id) ON DELETE RESTRICT,
    CONSTRAINT fk_resource_group_parent
        FOREIGN KEY (parent_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_rg_parent_id ON resource_group (parent_id);
CREATE INDEX IF NOT EXISTS idx_rg_name ON resource_group (name);
CREATE INDEX IF NOT EXISTS idx_rg_gts_type_id ON resource_group (gts_type_id, id);
CREATE INDEX IF NOT EXISTS idx_rg_tenant_id ON resource_group (tenant_id);

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
    gts_type_id INTEGER NOT NULL,
    resource_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    CONSTRAINT fk_rgm_gts_type
        FOREIGN KEY (gts_type_id) REFERENCES gts_type(id) ON DELETE RESTRICT,
    CONSTRAINT uq_resource_group_membership_unique
        UNIQUE (group_id, gts_type_id, resource_id)
);

CREATE INDEX IF NOT EXISTS idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
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
DROP TABLE IF EXISTS gts_type_allowed_membership;
DROP TABLE IF EXISTS gts_type_allowed_parent;
DROP TABLE IF EXISTS gts_type;
DROP DOMAIN IF EXISTS gts_type_path;
            ",
        )
        .await?;
        Ok(())
    }
}
