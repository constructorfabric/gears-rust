// Created: 2026-08-12 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-persistence:p1
//! The migration harness.
//!
//! `ToolKit` collects this list through
//! [`DatabaseCapability::migrations`](toolkit::DatabaseCapability::migrations)
//! and runs whatever is outstanding before the gear serves, aborting startup if
//! one fails. That is what keeps a partially migrated schema unreachable: there
//! is no path where a request observes a half-applied migration, because no
//! request is accepted until every migration has succeeded.
//!
//! Order is the vector's order, so append — never insert.

use sea_orm_migration::prelude::*;

mod m20260812_000001_initial;
mod m20260813_000001_categories;

/// The gear's migrations, oldest first.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260812_000001_initial::Migration),
            Box::new(m20260813_000001_categories::Migration),
        ]
    }
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod migrations_tests;
