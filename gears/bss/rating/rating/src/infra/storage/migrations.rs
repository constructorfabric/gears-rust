//! The `bss_rating` migration chain. The outbox tables (prefix `bss_rating__outbox`) join it at
//! the gear's `DatabaseCapability`.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

mod m20261019_000001_create_rating_foundation;
mod m20261029_000002_create_usage_intake;

/// The statements of one migration step for each supported backend, named so the two dialects
/// cannot be swapped.
#[derive(Debug, Clone, Copy)]
pub struct Dialects<'a> {
    /// PostgreSQL's statements.
    pub postgres: &'a [&'a str],
    /// SQLite's statements.
    pub sqlite: &'a [&'a str],
}

impl<'a> Dialects<'a> {
    /// The same statements on both backends.
    #[must_use]
    pub const fn both(statements: &'a [&'a str]) -> Self {
        Self {
            postgres: statements,
            sqlite: statements,
        }
    }
}

/// Runs the backend's statements in order. PostgreSQL and SQLite are the supported profiles
/// (DESIGN §3.7); every other backend is refused.
pub async fn exec_backend(
    migration: &str,
    manager: &SchemaManager<'_>,
    dialects: Dialects<'_>,
) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    let statements: &[&str] = match backend {
        sea_orm::DatabaseBackend::Postgres => dialects.postgres,
        sea_orm::DatabaseBackend::Sqlite => dialects.sqlite,
        // `DatabaseBackend` is non-exhaustive; every backend but the two is refused.
        _ => {
            return Err(DbErr::Migration(format!(
                "{backend:?} is not a supported backend for bss-rating"
            )));
        }
    };
    let conn = manager.get_connection();
    for (index, sql) in statements.iter().enumerate() {
        conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
            .await
            .map_err(|e| {
                DbErr::Migration(format!(
                    "{migration}: statement {} of {} failed on {backend:?}: {e}",
                    index.saturating_add(1),
                    statements.len()
                ))
            })?;
    }
    Ok(())
}

/// The gear's own migrations, oldest first.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261019_000001_create_rating_foundation::Migration),
            Box::new(m20261029_000002_create_usage_intake::Migration),
        ]
    }
}
