//! Database migration registry for the `event-broker` gear
//! (eb-single-process-implementation). No versioned migration *chain* by
//! design (ongoing implementation with no existing deployment to protect) -
//! but schema setup still goes through the platform-mandated
//! `DatabaseCapability` seam, one idempotent
//! `CREATE TABLE IF NOT EXISTS`-shaped migration per table, never a raw
//! connection (`libs/toolkit/src/contracts.rs`'s `DatabaseCapability` rule).
//! The tables are written with the schema builder, so the database may be
//! `SQLite`, `PostgreSQL` or `MySQL`/`MariaDB` (DESIGN
//! `cpt-cf-evbk-constraint-multi-sql`).
//!
//! This database holds ingest and delivery metadata only: the specification
//! cache, the `Storage` facade's tables, and the ingest outbox. The event log
//! is not among them - a storage backend opens the database its own options
//! name and applies its own tables to it, so a topic's events can live
//! somewhere this gear's metadata does not.

use sea_orm_migration::prelude::*;

mod columns;
mod m20260819_000001_spec_cache;
mod m20260819_000002_storage_facade;
mod m20260819_000003_backend_binding;
mod refused;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        let mut migrations: Vec<Box<dyn MigrationTrait>> = vec![
            Box::new(m20260819_000001_spec_cache::Migration),
            Box::new(m20260819_000002_storage_facade::Migration),
            Box::new(m20260819_000003_backend_binding::Migration),
        ];
        // The `default` outbox group's own table family under its fixed
        // prefix (feature 0005 §2.5), in the `Vec<Box<dyn MigrationTrait>>`
        // shape `DatabaseCapability::migrations()` expects (design.md D5),
        // applied the same idempotent way as every other table this gear
        // owns. Fixed because this runs before configuration is read: a
        // prefix minted at runtime would need the gear to run DDL itself.
        match toolkit_db::outbox::outbox_migrations_with_prefix(
            crate::domain::outbox::OUTBOX_TABLE_PREFIX,
        ) {
            Ok(outbox) => migrations.extend(outbox),
            // Not reached for the constant prefix, which the tests prove
            // valid; if it were, the migration phase stops the process with
            // the reason instead of starting it without the outbox tables.
            Err(err) => migrations.push(Box::new(refused::Refused {
                reason: format!("the ingest outbox table prefix is not usable: {err}"),
            })),
        }
        migrations
    }
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod migrations_tests;
