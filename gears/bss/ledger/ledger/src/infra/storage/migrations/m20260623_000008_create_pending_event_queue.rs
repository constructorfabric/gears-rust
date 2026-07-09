//! Create the durable pending-event queue table in schema `bss`:
//! `pending_event_queue` (one row per in-flight cross-flow work item, keyed by
//! `(tenant_id, flow, business_id)`). A row is a queued/quarantined unit of
//! deferred ledger work: an event whose financial effect cannot be applied
//! inline (out-of-order arrival, an `apply_after` embargo, or a transient
//! apply failure awaiting retry). Owned by Slice 2 and shared by Slices 2+3 —
//! Slice 2 (intake) durably enqueues the PII-free `payload` in its own
//! transaction; Slice 3 (apply) drains it in a *second*, separate transaction,
//! flipping `status` `QUEUED`→`APPLIED` (or `CANCELLED`) and bumping `attempts`
//! on a retry. The two-transaction intake/apply split keeps the durable queue
//! the system-of-record for deferred work, so a crash between intake and apply
//! never loses (nor double-applies) an event. `payload` is JSON and PII-free by
//! construction — it carries only the financial keys the apply path needs.
//!
//! `apply_after` (nullable) embargoes a row until an instant; the drain index
//! `(tenant_id, flow, status, queued_at)` lets the apply path scan a tenant's
//! oldest still-`QUEUED` rows per flow without a full-table sweep. Tenant
//! scoping is via `SecureORM` at query time (no PG RLS policy block — same as
//! the payment / precedence-policy tables). `SQLite` mirrors the same shape
//! with the systematic transforms (`uuid`→`text`, `timestamptz`→`text`,
//! `jsonb`→`text`); the status CHECK + PK + drain index are preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_pending_event_queue (
        tenant_id    uuid          NOT NULL,
        flow         varchar(64)   NOT NULL,
        business_id  varchar(255)  NOT NULL,
        payload      jsonb         NOT NULL,
        queued_at    timestamptz   NOT NULL,
        apply_after  timestamptz,
        status       varchar(16)   NOT NULL
            CONSTRAINT chk_pending_event_queue_status
            CHECK (status IN ('QUEUED','APPLIED','CANCELLED')),
        attempts     integer       NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, flow, business_id)
    )",
    "CREATE INDEX ix_pending_event_queue_drain
        ON bss.ledger_pending_event_queue (tenant_id, flow, status, queued_at)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_pending_event_queue"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`, `jsonb`→`text`; the CHECK + PK + drain index preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_pending_event_queue (
        tenant_id    text          NOT NULL,
        flow         varchar(64)   NOT NULL,
        business_id  varchar(255)  NOT NULL,
        payload      text          NOT NULL,
        queued_at    text          NOT NULL,
        apply_after  text,
        status       varchar(16)   NOT NULL
            CONSTRAINT chk_pending_event_queue_status
            CHECK (status IN ('QUEUED','APPLIED','CANCELLED')),
        attempts     integer       NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, flow, business_id)
    )",
    "CREATE INDEX ix_pending_event_queue_drain
        ON ledger_pending_event_queue (tenant_id, flow, status, queued_at)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_pending_event_queue"];

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
