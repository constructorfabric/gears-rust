//! `PriceBook` migration chain; toolkit delivery tables join it at the capability.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-tables-two-backends:p1

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
                "{backend:?} is not a supported backend for bss-pricing"
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

pub mod m0000_pricing_refuse_a_legacy_or_stale_schema;
mod m20260926_000001_create_pricing_settings;
mod m20260926_000002_create_pricing_approvals;
mod m20260926_000003_create_pricing_dimension_key;
mod m20260926_000004_create_pricing_price_book;
mod m20260926_000005_create_pricing_price_book_entry;
mod m20260926_000006_create_pricing_reference_op;
mod m20260926_000007_create_pricing_price;
mod m20260926_000008_create_pricing_audit;
mod m20260926_000009_create_pricing_idempotency;
mod m20260926_000010_create_pricing_plan;
mod m20260926_000011_create_pricing_plan_revision;
mod m20260926_000012_create_pricing_plan_item;
mod m20260926_000013_model_on_the_entry;
mod m20260927_000014_settings_currencies_and_author;
mod m20260928_000015_book_description;
mod m20260928_000016_unit_submit_note;
mod m20260929_000017_revision_scheduled;
mod m20260930_000018_usage_rating_policy;

mod m20260930_000019_commercial_receipts;
mod m20261002_000020_plan_summary;
mod m20261002_000021_policy_references_sku;
mod m20261003_000022_price_cancel_and_end;
mod m20261003_000023_book_archive;

/// Coordination followed by pricing-owned migrations.
pub struct Migrator;
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0000_pricing_refuse_a_legacy_or_stale_schema::Migration),
            Box::new(coord::migration::Migration::in_schema("bss")),
            Box::new(m20260926_000001_create_pricing_settings::Migration),
            Box::new(m20260926_000002_create_pricing_approvals::Migration),
            Box::new(m20260926_000003_create_pricing_dimension_key::Migration),
            Box::new(m20260926_000004_create_pricing_price_book::Migration),
            Box::new(m20260926_000005_create_pricing_price_book_entry::Migration),
            Box::new(m20260926_000006_create_pricing_reference_op::Migration),
            Box::new(m20260926_000007_create_pricing_price::Migration),
            Box::new(m20260926_000008_create_pricing_audit::Migration),
            Box::new(m20260926_000009_create_pricing_idempotency::Migration),
            Box::new(m20260926_000010_create_pricing_plan::Migration),
            Box::new(m20260926_000011_create_pricing_plan_revision::Migration),
            Box::new(m20260926_000012_create_pricing_plan_item::Migration),
            Box::new(m20260926_000013_model_on_the_entry::Migration),
            Box::new(m20260927_000014_settings_currencies_and_author::Migration),
            Box::new(m20260928_000015_book_description::Migration),
            Box::new(m20260928_000016_unit_submit_note::Migration),
            Box::new(m20260929_000017_revision_scheduled::Migration),
            Box::new(m20260930_000018_usage_rating_policy::Migration),
            Box::new(m20260930_000019_commercial_receipts::Migration),
            Box::new(m20261002_000020_plan_summary::Migration),
            Box::new(m20261002_000021_policy_references_sku::Migration),
            Box::new(m20261003_000022_price_cancel_and_end::Migration),
            Box::new(m20261003_000023_book_archive::Migration),
        ]
    }
}
