//! Create the `bss.audit_pack_export` table: one materialized audit-pack export
//! job (Slice 6 §5/§10). `POST …/audit/packs` inserts a row and returns
//! `202 Accepted` + a `Location` to `GET …/audit/packs/{exportId}`, which polls
//! the row for `status` and the materialized CSV.
//!
//! The row is owned by the requester's home tenant (`tenant_id`);
//! `target_tenant_id` records whose ledger was opened. The `status` CHECK pins
//! the four job states; MVP rows are born `succeeded` (synchronous build), the
//! `accepted` / `processing` states being reserved for a future worker path (no
//! migration needed to activate them). An index on `(tenant_id, created_at_utc)`
//! serves the requester's recent-exports listing.
//!
//! `SQLite` (the non-production test backend) mirrors the shape with the
//! systematic transforms (drop the `bss.` prefix; `uuid` → `text`;
//! `bytea` → `blob`; `timestamptz NOT NULL DEFAULT now()` →
//! `text NOT NULL DEFAULT (CURRENT_TIMESTAMP)`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.audit_pack_export (
        export_id         uuid         NOT NULL,
        tenant_id         uuid         NOT NULL,
        target_tenant_id  uuid         NOT NULL,
        status            text         NOT NULL,
        reason_code       text,
        actor_ref         text         NOT NULL,
        csv               bytea,
        row_count         bigint       NOT NULL DEFAULT 0,
        error_detail      text,
        created_at_utc    timestamptz  NOT NULL DEFAULT now(),
        completed_at_utc  timestamptz,
        PRIMARY KEY (export_id),
        CONSTRAINT chk_audit_pack_export_status
            CHECK (status IN ('accepted', 'processing', 'succeeded', 'failed'))
    )",
    "CREATE INDEX idx_audit_pack_export_tenant_created
        ON bss.audit_pack_export (tenant_id, created_at_utc)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.audit_pack_export"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE audit_pack_export (
        export_id         text         NOT NULL,
        tenant_id         text         NOT NULL,
        target_tenant_id  text         NOT NULL,
        status            text         NOT NULL,
        reason_code       text,
        actor_ref         text         NOT NULL,
        csv               blob,
        row_count         bigint       NOT NULL DEFAULT 0,
        error_detail      text,
        created_at_utc    text         NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        completed_at_utc  text,
        PRIMARY KEY (export_id),
        CONSTRAINT chk_audit_pack_export_status
            CHECK (status IN ('accepted', 'processing', 'succeeded', 'failed'))
    )",
    "CREATE INDEX idx_audit_pack_export_tenant_created
        ON audit_pack_export (tenant_id, created_at_utc)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS audit_pack_export"];

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
