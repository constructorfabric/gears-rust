//! Initial Account Management schema — `tenants`, `tenant_closure`, and
//! `tenant_metadata` tables with all supporting indexes.
//!
//! Per-backend raw SQL is used (not `SeaORM`'s schema-builder) so the
//! `CHECK` / partial unique / barrier invariants are preserved
//! byte-for-byte on Postgres. `SQLite` receives a dialect-adjusted
//! variant used by the in-tree integration tests.
//!
//! `MySQL` is intentionally **not** supported — the Postgres DDL relies
//! on partial unique indexes (`WHERE parent_id IS NULL`), `CHECK`
//! constraints, and `FOREIGN KEY ... ON DELETE` modes that need a
//! `MySQL`-specific design pass before they can be reproduced safely.
//! A `MySQL` backend MUST ship its own migration set; running the AM
//! migrator against `MySQL` fails fast with an explicit `DbErr::Custom`.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str = "account-management migrations: MySQL is not supported \
    (this migration set targets PostgreSQL/SQLite); add a dedicated MySQL migration set \
    before running against MySQL";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: Vec<&str> = match backend {
            sea_orm::DatabaseBackend::Postgres => vec![
                "CREATE EXTENSION IF NOT EXISTS pgcrypto;",
                r"
CREATE TABLE IF NOT EXISTS tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id UUID NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    status SMALLINT NOT NULL CHECK (status IN (0, 1, 2, 3)),
    self_managed BOOLEAN NOT NULL DEFAULT FALSE,
    tenant_type_uuid UUID NOT NULL,
    depth INTEGER NOT NULL CHECK (depth >= 0),
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at TIMESTAMP WITH TIME ZONE NULL,
    retention_window_secs BIGINT NULL,
    claimed_by UUID NULL,
    claimed_at TIMESTAMPTZ NULL,
    CONSTRAINT fk_tenants_parent
        FOREIGN KEY (parent_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    CONSTRAINT ck_tenants_root_depth
        CHECK ((parent_id IS NULL AND depth = 0) OR (parent_id IS NOT NULL AND depth > 0))
);
                ",
                "CREATE UNIQUE INDEX IF NOT EXISTS ux_tenants_single_root ON tenants ((1)) WHERE parent_id IS NULL;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_parent_status ON tenants (parent_id, status);",
                "CREATE INDEX IF NOT EXISTS idx_tenants_status ON tenants (status);",
                "CREATE INDEX IF NOT EXISTS idx_tenants_type ON tenants (tenant_type_uuid);",
                "CREATE INDEX IF NOT EXISTS idx_tenants_deleted_at ON tenants (deleted_at) WHERE deleted_at IS NOT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_retention_scan ON tenants (deleted_at, depth DESC) WHERE status = 3 AND deleted_at IS NOT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_provisioning_stuck ON tenants (created_at) WHERE status = 0;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_retention_claim ON tenants (claimed_by) WHERE claimed_by IS NOT NULL;",
                r"
CREATE TABLE IF NOT EXISTS tenant_closure (
    ancestor_id UUID NOT NULL,
    descendant_id UUID NOT NULL,
    barrier SMALLINT NOT NULL DEFAULT 0,
    descendant_status SMALLINT NOT NULL CHECK (descendant_status IN (1, 2, 3)),
    CONSTRAINT pk_tenant_closure PRIMARY KEY (ancestor_id, descendant_id),
    CONSTRAINT fk_tenant_closure_ancestor
        FOREIGN KEY (ancestor_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE,
    CONSTRAINT fk_tenant_closure_descendant
        FOREIGN KEY (descendant_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE,
    CONSTRAINT ck_tenant_closure_self_row_barrier
        CHECK (ancestor_id <> descendant_id OR barrier = 0),
    CONSTRAINT ck_tenant_closure_barrier_nonnegative
        CHECK (barrier >= 0)
);
                ",
                "CREATE INDEX IF NOT EXISTS idx_tenant_closure_ancestor_barrier_status ON tenant_closure (ancestor_id, barrier, descendant_status);",
                "CREATE INDEX IF NOT EXISTS idx_tenant_closure_descendant ON tenant_closure (descendant_id);",
                r"
CREATE TABLE IF NOT EXISTS tenant_metadata (
    tenant_id UUID NOT NULL,
    schema_uuid UUID NOT NULL,
    value JSONB NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version BIGINT NOT NULL DEFAULT 1,
    CONSTRAINT pk_tenant_metadata PRIMARY KEY (tenant_id, schema_uuid),
    CONSTRAINT fk_tenant_metadata_tenant
        FOREIGN KEY (tenant_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE
);
                ",
                "CREATE INDEX IF NOT EXISTS idx_tenant_metadata_tenant ON tenant_metadata (tenant_id);",
            ],
            sea_orm::DatabaseBackend::Sqlite => vec![
                r"
CREATE TABLE IF NOT EXISTS tenants (
    id TEXT PRIMARY KEY NOT NULL,
    parent_id TEXT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    status SMALLINT NOT NULL CHECK (status IN (0, 1, 2, 3)),
    self_managed INTEGER NOT NULL DEFAULT 0,
    tenant_type_uuid TEXT NOT NULL,
    depth INTEGER NOT NULL CHECK (depth >= 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at TEXT NULL,
    retention_window_secs INTEGER NULL,
    claimed_by TEXT NULL,
    claimed_at TIMESTAMP NULL,
    CONSTRAINT ck_tenants_root_depth
        CHECK ((parent_id IS NULL AND depth = 0) OR (parent_id IS NOT NULL AND depth > 0))
);
                ",
                // The PG version uses `ON tenants ((1))` — a constant
                // expression — so every root row collapses to the same
                // index key and the UNIQUE constraint catches a second
                // root. SQLite (per SQL standard) treats NULLs in a
                // unique index as distinct, so the partial filter
                // alone won't enforce single-root; `COALESCE(parent_id,
                // '')` collapses the indexed value to a fixed sentinel
                // for every row caught by the partial filter, restoring
                // the single-root invariant.
                "CREATE UNIQUE INDEX IF NOT EXISTS ux_tenants_single_root ON tenants (COALESCE(parent_id, '')) WHERE parent_id IS NULL;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_parent_status ON tenants (parent_id, status);",
                "CREATE INDEX IF NOT EXISTS idx_tenants_status ON tenants (status);",
                "CREATE INDEX IF NOT EXISTS idx_tenants_type ON tenants (tenant_type_uuid);",
                "CREATE INDEX IF NOT EXISTS idx_tenants_retention_scan ON tenants (deleted_at, depth DESC) WHERE status = 3 AND deleted_at IS NOT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_provisioning_stuck ON tenants (created_at) WHERE status = 0;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_retention_claim ON tenants (claimed_by) WHERE claimed_by IS NOT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_deleted_at ON tenants (deleted_at) WHERE deleted_at IS NOT NULL;",
                r"
CREATE TABLE IF NOT EXISTS tenant_closure (
    ancestor_id TEXT NOT NULL,
    descendant_id TEXT NOT NULL,
    barrier SMALLINT NOT NULL DEFAULT 0,
    descendant_status SMALLINT NOT NULL CHECK (descendant_status IN (1, 2, 3)),
    PRIMARY KEY (ancestor_id, descendant_id),
    CONSTRAINT ck_tenant_closure_self_row_barrier
        CHECK (ancestor_id <> descendant_id OR barrier = 0),
    CONSTRAINT ck_tenant_closure_barrier_nonnegative
        CHECK (barrier >= 0)
);
                ",
                "CREATE INDEX IF NOT EXISTS idx_tenant_closure_ancestor_barrier_status ON tenant_closure (ancestor_id, barrier, descendant_status);",
                "CREATE INDEX IF NOT EXISTS idx_tenant_closure_descendant ON tenant_closure (descendant_id);",
                r"
CREATE TABLE IF NOT EXISTS tenant_metadata (
    tenant_id TEXT NOT NULL,
    schema_uuid TEXT NOT NULL,
    value TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (tenant_id, schema_uuid),
    FOREIGN KEY (tenant_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE
);
                ",
                "CREATE INDEX IF NOT EXISTS idx_tenant_metadata_tenant ON tenant_metadata (tenant_id);",
            ],
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        if matches!(backend, sea_orm::DatabaseBackend::MySql) {
            return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
        }
        let conn = manager.get_connection();
        // Drop in reverse FK-dependency order — metadata + closure
        // first, then tenants. Only Postgres enforces FK cascades by
        // default; SQLite's `PRAGMA foreign_keys` is not enabled by
        // `modkit-db`, so SQLite cascade behaviour cannot be assumed
        // and the explicit reverse-order drop is what guarantees a
        // clean teardown on both backends.
        for sql in [
            "DROP TABLE IF EXISTS tenant_metadata;",
            "DROP TABLE IF EXISTS tenant_closure;",
            "DROP TABLE IF EXISTS tenants;",
        ] {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }
}
