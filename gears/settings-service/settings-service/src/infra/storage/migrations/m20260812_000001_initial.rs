// Created: 2026-08-12 by Constructor Tech
//! Shared schema prerequisites.
//!
//! Deliberately creates **no tables**. Every settings table belongs to the
//! feature that owns it — `categories` to category management,
//! `setting_declarations` to declaration authoring, `setting_values` to value
//! resolution — so that a table and the code that reads it arrive together and
//! neither can drift ahead of the other.
//!
//! What is left is what no single feature owns: the Postgres extensions their
//! DDL will assume. An extension is cheap, idempotent, and shared, and a feature
//! migration that had to create one would be racing every sibling that needed
//! the same.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

#[derive(DeriveMigrationName)]
pub struct Migration;

// `MigrationTrait` elides the `SchemaManager` lifetime and `async_trait`'s
// desugaring rejects an explicit `<'_>` here as a signature mismatch, so the
// crate-wide `rust_2018_idioms` deny is relaxed for this impl alone.
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();

        // SQLite backs the unit and integration suites and has neither
        // extensions nor a need for them: it supplies UUIDs from the
        // application layer and has no trigram index. Nothing to do.
        if backend != DatabaseBackend::Postgres {
            return Ok(());
        }

        let conn = manager.get_connection();
        for sql in [
            // `gen_random_uuid()` for UUID primary keys, so an id is assigned by
            // the database rather than by whichever caller got there first.
            "CREATE EXTENSION IF NOT EXISTS pgcrypto;",
            // Trigram search over category and setting names. DESIGN.md §4.7
            // specifies `idx_categories_name_trgm` as a GIN `pg_trgm` index; the
            // migration that creates it cannot also create the extension without
            // conflicting with every other feature that needs it.
            "CREATE EXTENSION IF NOT EXISTS pg_trgm;",
        ] {
            conn.execute_unprepared(sql).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Extensions are not dropped. They are database-wide and may be in use
        // by another schema in the same database; dropping one here to undo a
        // settings migration could break an unrelated gear.
        let _ = manager;
        Ok(())
    }
}
