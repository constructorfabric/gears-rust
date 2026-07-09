//! Create `bss.payer_pii_map` (the per-payer PII reference + erasure tombstone,
//! Slice 6 Phase 3 Group 3A, architecture §4.5 / AC #22). One row per
//! `(tenant_id, payer_tenant_id)` holds the opaque `pii_ref` (a pointer into the
//! external PII store — never the PII itself) and an `erased` tombstone the
//! erasure path flips.
//!
//! UNLIKE the audit / metadata-change tables this table is MUTABLE: the GDPR
//! right-to-erasure tombstone sets `erased = true` in place, so there is NO
//! `bss.reject_mutation()` append-only trigger. `SQLite` mirrors the same shape
//! with the systematic transforms (`uuid`→`text`). Tenant isolation runs through
//! the entity `#[secure(tenant_col = "tenant_id", …)]` `SecureORM` layer.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.payer_pii_map (
        tenant_id        uuid    NOT NULL,
        payer_tenant_id  uuid    NOT NULL,
        pii_ref          text    NOT NULL,
        erased           boolean NOT NULL DEFAULT false,
        PRIMARY KEY (tenant_id, payer_tenant_id)
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.payer_pii_map"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `boolean` kept; PK preserved). No append-only trigger (this table is mutable).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE payer_pii_map (
        tenant_id        text    NOT NULL,
        payer_tenant_id  text    NOT NULL,
        pii_ref          text    NOT NULL,
        erased           boolean NOT NULL DEFAULT false,
        PRIMARY KEY (tenant_id, payer_tenant_id)
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS payer_pii_map"];

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
