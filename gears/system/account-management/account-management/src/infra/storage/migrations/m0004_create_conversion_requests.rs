//! Migration `m0004` — create the `conversion_requests` and
//! `tenant_idp_metadata` tables that round out the AM schema for this
//! PR.
//!
//! Two tables land in the same migration because they belong to the
//! same feature slice (managed/self-managed conversion + IdP-metadata
//! isolation). A future PR that needs to alter either table opens a
//! new migration file rather than mutating this one.
//!
//! # `conversion_requests`
//!
//! Records the durable state machine for post-creation mode changes
//! (`pending` -> `approved` | `cancelled` | `rejected` | `expired`)
//! and is wired into the rest of the schema via `FOREIGN KEY`
//! constraints on `tenants.id` for both the converting tenant
//! (`tenant_id`) and the parent linkage (`parent_id`). Both FKs
//! declare `ON DELETE CASCADE` on Postgres, but `toolkit-db` does not
//! enable `PRAGMA foreign_keys` on `SQLite`, so the cascade is a
//! silent no-op there. `TenantRepoImpl::hard_delete_one` therefore
//! issues an explicit DELETE on `conversion_requests` keyed on both
//! `tenant_id` and `parent_id` inside the tenant-delete transaction
//! to keep dialect parity — mirroring the explicit cleanups already
//! used for `tenant_closure`, `tenant_metadata`, and
//! `tenant_idp_metadata`.
//!
//! # `tenant_idp_metadata`
//!
//! Plugin-private per-tenant state separated from the public
//! `tenant_metadata` table. AM persists the opaque JSON blob returned
//! by the `IdP` plugin from `IdpProvisionResult::metadata` and replays
//! it via `TenantContext::metadata` on every subsequent `IdP` call
//! for that tenant. AM does not interpret the JSON; the plugin owns
//! the shape end-to-end.
//!
//! Backend coverage: per-backend raw DDL preserves the FK +
//! `ON DELETE CASCADE` on Postgres while `SQLite` omits FK clauses
//! (FK enforcement is off by `toolkit-db` convention) and relies on
//! `TenantRepoImpl::hard_delete_one` to explicit-delete
//! `tenant_idp_metadata` rows in the same transaction as the tenant
//! delete.
//!
//! No `plugin_id` column today: AM resolves at most one
//! `IdpPluginClient` from `ClientHub` per deployment, and adding the
//! column before a multi-plugin contract exists would persist a value
//! no caller actually owns.
//!
//! # Dialects + down
//!
//! Per-backend raw DDL is used so the `CHECK` invariants and the
//! partial unique index that enforces the at-most-one-pending
//! invariant on `conversion_requests` survive byte-for-byte on
//! Postgres, with a dialect-equivalent expression on `SQLite`.
//! `MySQL` is unsupported and returns
//! `DbErr::Custom(MYSQL_NOT_SUPPORTED)`.
//!
//! `down` drops both tables in reverse-creation order — FK
//! constraints (Postgres) and indexes drop together with the table on
//! each supported dialect.

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
                r"
CREATE TABLE IF NOT EXISTS conversion_requests (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    parent_id UUID NULL,
    child_tenant_name TEXT NOT NULL CHECK (length(child_tenant_name) BETWEEN 1 AND 255),
    initiator_side SMALLINT NOT NULL CHECK (initiator_side IN (0, 1)),
    target_mode SMALLINT NOT NULL CHECK (target_mode IN (0, 1)),
    status SMALLINT NOT NULL CHECK (status IN (0, 1, 2, 3, 4)),
    requested_by UUID NOT NULL,
    approved_by UUID NULL,
    cancelled_by UUID NULL,
    rejected_by UUID NULL,
    requested_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    resolved_at TIMESTAMP WITH TIME ZONE NULL,
    expires_at TIMESTAMP WITH TIME ZONE NOT NULL,
    deleted_at TIMESTAMP WITH TIME ZONE NULL,
    CONSTRAINT fk_conversion_requests_tenant
        FOREIGN KEY (tenant_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE,
    CONSTRAINT fk_conversion_requests_parent
        FOREIGN KEY (parent_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE,
    -- Per-status actor / resolution invariants. Pinned at the DB layer
    -- so a buggy service write cannot persist
    -- `status=approved AND approved_by=NULL`, multiple `*_by` columns
    -- populated at once, or a terminal row with `resolved_at=NULL`.
    -- Maps 1:1 to the FEATURE state-machine table:
    --   * status=0 (pending):   all *_by NULL,            resolved_at NULL,  deleted_at NULL
    --   * status=1 (approved):  approved_by  NOT NULL,    resolved_at NOT NULL
    --   * status=2 (cancelled): cancelled_by NOT NULL,    resolved_at NOT NULL
    --   * status=3 (rejected):  rejected_by  NOT NULL,    resolved_at NOT NULL
    --   * status=4 (expired):   all *_by NULL (system),   resolved_at NOT NULL
    --
    -- The pending arm pins `deleted_at IS NULL` because soft-delete is
    -- a retention-side operation owned by `soft_delete_resolved_older_than`,
    -- which only touches resolved rows (statuses 1..4). Without this
    -- predicate, an out-of-band UPDATE that stamps `deleted_at` on a
    -- pending row would still satisfy the CHECK while the
    -- `ux_conversion_requests_pending` partial unique index would
    -- silently exclude the row from the uniqueness guarantee -- the
    -- writer could then insert a fresh pending row beside the
    -- soft-deleted one and create two coexisting pending claims.
    -- Stamping `deleted_at` on pending becomes a CHECK violation
    -- here, closing that gap defensively.
    CONSTRAINT ck_conversion_requests_actor_invariant
        CHECK (
            (status = 0 AND approved_by IS NULL AND cancelled_by IS NULL
                AND rejected_by IS NULL AND resolved_at IS NULL
                AND deleted_at IS NULL)
            OR (status = 1 AND approved_by IS NOT NULL AND cancelled_by IS NULL
                AND rejected_by IS NULL AND resolved_at IS NOT NULL)
            OR (status = 2 AND approved_by IS NULL AND cancelled_by IS NOT NULL
                AND rejected_by IS NULL AND resolved_at IS NOT NULL)
            OR (status = 3 AND approved_by IS NULL AND cancelled_by IS NULL
                AND rejected_by IS NOT NULL AND resolved_at IS NOT NULL)
            OR (status = 4 AND approved_by IS NULL AND cancelled_by IS NULL
                AND rejected_by IS NULL AND resolved_at IS NOT NULL)
        )
);
                ",
                "CREATE UNIQUE INDEX IF NOT EXISTS ux_conversion_requests_pending ON conversion_requests (tenant_id) WHERE status = 0 AND deleted_at IS NULL;",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_tenant_status ON conversion_requests (tenant_id, status);",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_parent_status ON conversion_requests (parent_id, status) WHERE parent_id IS NOT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_expiry_sweep ON conversion_requests (expires_at) WHERE status = 0 AND deleted_at IS NULL;",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_retention_scan ON conversion_requests (resolved_at) WHERE status IN (1, 2, 3, 4) AND deleted_at IS NULL;",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_deleted_at ON conversion_requests (deleted_at) WHERE deleted_at IS NOT NULL;",
                r"
CREATE TABLE IF NOT EXISTS tenant_idp_metadata (
    tenant_id UUID PRIMARY KEY,
    metadata JSONB NULL,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_tenant_idp_metadata_tenant
        FOREIGN KEY (tenant_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE
);
                ",
            ],
            sea_orm::DatabaseBackend::Sqlite => vec![
                r"
CREATE TABLE IF NOT EXISTS conversion_requests (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    parent_id TEXT NULL,
    child_tenant_name TEXT NOT NULL CHECK (length(child_tenant_name) BETWEEN 1 AND 255),
    initiator_side SMALLINT NOT NULL CHECK (initiator_side IN (0, 1)),
    target_mode SMALLINT NOT NULL CHECK (target_mode IN (0, 1)),
    status SMALLINT NOT NULL CHECK (status IN (0, 1, 2, 3, 4)),
    requested_by TEXT NOT NULL,
    approved_by TEXT NULL,
    cancelled_by TEXT NULL,
    rejected_by TEXT NULL,
    requested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    resolved_at TEXT NULL,
    expires_at TEXT NOT NULL,
    deleted_at TEXT NULL,
    -- Mirror of the Postgres-side
    -- `ck_conversion_requests_actor_invariant`. The same five-arm
    -- predicate over `(status, *_by, resolved_at, deleted_at)` works
    -- on `SQLite` because every column referenced is portable across
    -- both backends (`SMALLINT`/`TEXT IS [NOT] NULL`). The pending
    -- arm's `deleted_at IS NULL` predicate mirrors the Postgres
    -- rationale documented above.
    CONSTRAINT ck_conversion_requests_actor_invariant
        CHECK (
            (status = 0 AND approved_by IS NULL AND cancelled_by IS NULL
                AND rejected_by IS NULL AND resolved_at IS NULL
                AND deleted_at IS NULL)
            OR (status = 1 AND approved_by IS NOT NULL AND cancelled_by IS NULL
                AND rejected_by IS NULL AND resolved_at IS NOT NULL)
            OR (status = 2 AND approved_by IS NULL AND cancelled_by IS NOT NULL
                AND rejected_by IS NULL AND resolved_at IS NOT NULL)
            OR (status = 3 AND approved_by IS NULL AND cancelled_by IS NULL
                AND rejected_by IS NOT NULL AND resolved_at IS NOT NULL)
            OR (status = 4 AND approved_by IS NULL AND cancelled_by IS NULL
                AND rejected_by IS NULL AND resolved_at IS NOT NULL)
        ),
    FOREIGN KEY (tenant_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE,
    FOREIGN KEY (parent_id) REFERENCES tenants(id)
        ON UPDATE CASCADE ON DELETE CASCADE
);
                ",
                // Partial unique index — keyed only on `tenant_id`,
                // identical to the Postgres definition above. The
                // `WHERE status = 0 AND deleted_at IS NULL` clause
                // already excludes any row with a non-null
                // `deleted_at`, so the indexed rows have `deleted_at
                // IS NULL` by construction; folding it into the key
                // via `COALESCE` would be a constant `''` for every
                // such row and add nothing. `tenant_id` is `NOT
                // NULL`, so the SQLite "NULLs are distinct" rule is
                // not in play here.
                "CREATE UNIQUE INDEX IF NOT EXISTS ux_conversion_requests_pending ON conversion_requests (tenant_id) WHERE status = 0 AND deleted_at IS NULL;",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_tenant_status ON conversion_requests (tenant_id, status);",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_parent_status ON conversion_requests (parent_id, status) WHERE parent_id IS NOT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_expiry_sweep ON conversion_requests (expires_at) WHERE status = 0 AND deleted_at IS NULL;",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_retention_scan ON conversion_requests (resolved_at) WHERE status IN (1, 2, 3, 4) AND deleted_at IS NULL;",
                "CREATE INDEX IF NOT EXISTS idx_conversion_requests_deleted_at ON conversion_requests (deleted_at) WHERE deleted_at IS NOT NULL;",
                r"
CREATE TABLE IF NOT EXISTS tenant_idp_metadata (
    tenant_id TEXT PRIMARY KEY NOT NULL,
    metadata TEXT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
                ",
            ],
            _ => {
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
        // Reverse-creation order so any cross-table FK introduced by a
        // later evolution drops cleanly before its referent.
        conn.execute_unprepared("DROP TABLE IF EXISTS tenant_idp_metadata;")
            .await?;
        conn.execute_unprepared("DROP TABLE IF EXISTS conversion_requests;")
            .await?;
        Ok(())
    }
}
