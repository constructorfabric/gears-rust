//! Migrations of the storage plugin. Later features append migrations for
//! their tables; the foundation creates the schema metadata and the three
//! configuration tables.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::prelude::{DbErr, SchemaManager};

mod m0001_foundation;

const MYSQL_NOT_SUPPORTED: &str = "quota-enforcement-storage-plugin: MySQL is not supported; \
    this migration set targets PostgreSQL and SQLite";

/// Refuse a backend the migrations do not target.
fn ensure_supported(manager: &SchemaManager) -> Result<(), DbErr> {
    if matches!(
        manager.get_database_backend(),
        sea_orm::DatabaseBackend::MySql
    ) {
        return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
    }
    Ok(())
}

/// Migrator for the storage plugin schema.
pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![Box::new(m0001_foundation::Migration)]
    }
}
