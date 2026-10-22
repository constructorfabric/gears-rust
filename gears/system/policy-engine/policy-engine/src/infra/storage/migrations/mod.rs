//! Policy-engine migrations.
//!
//! [`Migrator`] holds the gear's schema.
//!
//! Supported backends: Postgres and `SQLite`. Every migration refuses `MySQL`
//! with a descriptive error, because this gear does not support it.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::prelude::{DbErr, MigrationTrait};
use sea_orm_migration::sea_orm::DatabaseBackend;

mod m20260923_000001_initial;

/// Migrator for the gear's schema.
pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260923_000001_initial::Migration)]
    }
}

/// Rejects every backend except Postgres and `SQLite`.
pub(crate) fn ensure_supported_backend(backend: DatabaseBackend) -> Result<(), DbErr> {
    match backend {
        DatabaseBackend::Postgres | DatabaseBackend::Sqlite => Ok(()),
        other => Err(DbErr::Migration(format!(
            "policy-engine supports only Postgres and SQLite; database backend {other:?} is not supported"
        ))),
    }
}

/// Every table the gear's migrations create, in creation order.
#[cfg(test)]
pub(crate) fn own_tables() -> Vec<&'static str> {
    m20260923_000001_initial::TABLES.to_vec()
}

/// Every explicitly named index the gear's migrations create.
#[cfg(test)]
pub(crate) fn own_indexes() -> Vec<&'static str> {
    m20260923_000001_initial::INDEXES.to_vec()
}

/// Postgres DDL of the initial migration, rendered without a database.
#[cfg(test)]
pub(crate) fn postgres_initial_ddl() -> Vec<String> {
    m20260923_000001_initial::postgres_statements()
}
