//! Database migrations for the Types Registry gear.
//!
//! One initial migration creates the P0 subset of `docs/database.sql`: 9 of its
//! 11 tables, omitting `source_claim` and `routing_config` (federation, out of
//! P0 scope — SPEC §9).
//!
//! Outbox tables are **not** created here. They come from
//! `toolkit_db::outbox::outbox_migrations_with_prefix("types_registry_outbox")`,
//! which the gear's `DatabaseCapability::migrations()` appends.

use sea_orm_migration::MigratorTrait;

mod m20260817_000001_initial;

/// Migrator for the Types Registry managed-state schema.
pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![Box::new(m20260817_000001_initial::Migration)]
    }
}
