//! The coordination and `PriceBook` registry migration chain.
//! @cpt-dod:cpt-cf-bss-products-dod-tables-two-backends:p1

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

/// Run the backend statements in order, preserving migration context.
pub async fn exec_backend(
    migration: &str,
    manager: &SchemaManager<'_>,
    postgres: &[&str],
    sqlite: &[&str],
) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    let conn = manager.get_connection();
    let statements: &[&str] = match backend {
        sea_orm::DatabaseBackend::Postgres => postgres,
        sea_orm::DatabaseBackend::Sqlite => sqlite,
        // A catch-all rather than naming MySQL: `DatabaseBackend` is
        // `#[non_exhaustive]`, so naming the one unsupported variant would stop
        // covering the match the moment sea-orm adds another. The catch-all is
        // what this arm always meant — this gear ships two dialects and refuses
        // every other one.
        _ => {
            return Err(DbErr::Migration(format!(
                "{backend:?} is not a supported backend for bss-products"
            )));
        }
    };
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

pub mod m0000_products_refuse_a_legacy_or_stale_schema;
mod m20260925_000001_create_products_category;
mod m20260925_000002_create_products_sku;
mod m20260925_000003_create_products_approvals;
mod m20260925_000004_create_products_audit_log;
mod m20260925_000005_create_products_idempotency;
mod m20260925_000006_create_products_sku_reference;
mod m20260925_000007_sku_category_optional;
mod m20260927_000008_audit_lifecycle_move;
mod m20260928_000009_unit_submit_note;
mod m20260928_000010_clear_retired_defaults;
mod m20261001_000011_sku_lifecycle_honesty;
mod m20261001_000012_derived_usage_type;
mod m20261002_000013_derived_sku_unit;
mod m20261003_000014_archive_mark;
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0000_products_refuse_a_legacy_or_stale_schema::Migration),
            Box::new(coord::migration::Migration::in_schema("bss")),
            Box::new(m20260925_000001_create_products_category::Migration),
            Box::new(m20260925_000002_create_products_sku::Migration),
            Box::new(m20260925_000003_create_products_approvals::Migration),
            Box::new(m20260925_000004_create_products_audit_log::Migration),
            Box::new(m20260925_000005_create_products_idempotency::Migration),
            Box::new(m20260925_000006_create_products_sku_reference::Migration),
            Box::new(m20260925_000007_sku_category_optional::Migration),
            Box::new(m20260927_000008_audit_lifecycle_move::Migration),
            Box::new(m20260928_000009_unit_submit_note::Migration),
            Box::new(m20260928_000010_clear_retired_defaults::Migration),
            Box::new(m20261001_000011_sku_lifecycle_honesty::Migration),
            Box::new(m20261001_000012_derived_usage_type::Migration),
            Box::new(m20261002_000013_derived_sku_unit::Migration),
            Box::new(m20261003_000014_archive_mark::Migration),
        ]
    }
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod migrations_tests;
