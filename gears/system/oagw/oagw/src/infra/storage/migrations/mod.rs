//! OAGW schema migrations.
//!
//! `ToolKit` collects this list through `DatabaseCapability::migrations` and
//! runs whatever is outstanding before the gear serves, aborting startup if one
//! fails. Without a configured database no migration runs.
//!
//! The runner applies them in name order, so a new migration takes a later
//! `mYYYYMMDD_NNNNNN_` prefix than every existing one.

use sea_orm_migration::prelude::*;

mod m20261005_000001_create_upstreams_and_routes;

/// The gear's migrations, oldest first.
pub(crate) struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            m20261005_000001_create_upstreams_and_routes::Migration,
        )]
    }
}

#[cfg(test)]
#[path = "migration_tests.rs"]
mod migration_tests;
