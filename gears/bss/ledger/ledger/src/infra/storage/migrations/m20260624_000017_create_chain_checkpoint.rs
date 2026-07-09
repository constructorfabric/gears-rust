//! Create the `bss.chain_checkpoint` table: a per-tenant retention checkpoint
//! that records a contiguous range of the tamper-evidence hash chain (Slice 6
//! design §4.8/E-5). One row pins a `from_row_hash` .. `to_row_hash` range plus
//! the number of journal entries it covers, so a future partition-rotation pass
//! can prove a detached partition is anchored by a signed checkpoint before it
//! retires the underlying rows.
//!
//! **Dormant seam (Variant 2).** Partitioning / rotation is Foundation
//! (Slice-1) debt: nothing in the MVP writes a checkpoint on a schedule yet.
//! This table ships as the interface Foundation's rotation will call. The
//! `signature` column is nullable on purpose — signing / `WORM` storage is
//! post-MVP (Bucket A); in the MVP a checkpoint just records a range, unsigned.
//!
//! `SQLite` (the non-production test backend) mirrors the shape with the
//! systematic transforms (drop the `bss.` prefix; `uuid` → `text`;
//! `bytea` → `blob`; `timestamptz NOT NULL DEFAULT now()` →
//! `text NOT NULL DEFAULT (CURRENT_TIMESTAMP)`). The `checkpoint_id` PK is
//! preserved on both backends.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.chain_checkpoint (
        checkpoint_id        uuid         NOT NULL,
        tenant_id            uuid         NOT NULL,
        from_row_hash        bytea        NOT NULL,
        to_row_hash          bytea        NOT NULL,
        covered_entry_count  bigint       NOT NULL,
        signature            bytea,
        created_at_utc       timestamptz  NOT NULL DEFAULT now(),
        PRIMARY KEY (checkpoint_id)
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.chain_checkpoint"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------
//
// Systematic transforms from the Postgres variant:
// * schema prefix `bss.` dropped (single namespace);
// * `uuid` → `text`;
// * `bytea` → `blob` (both the NOT-NULL range hashes and the nullable signature);
// * `timestamptz NOT NULL DEFAULT now()` → `text NOT NULL DEFAULT (CURRENT_TIMESTAMP)`
//   (mirroring how the P1 journal-tables migration maps `posted_at_utc`).
// The `checkpoint_id` PK is preserved.

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE chain_checkpoint (
        checkpoint_id        text         NOT NULL,
        tenant_id            text         NOT NULL,
        from_row_hash        blob         NOT NULL,
        to_row_hash          blob         NOT NULL,
        covered_entry_count  bigint       NOT NULL,
        signature            blob,
        created_at_utc       text         NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        PRIMARY KEY (checkpoint_id)
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS chain_checkpoint"];

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
