// Created: 2026-08-12 by Virtuozzo International GmbH
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
mod m20260825_000001_setting_declarations;
mod m20260906_000001_setting_values;
mod m20260907_000001_audit_records;
mod m20260907_000002_tenant_permissions;
mod m20260915_000001_pending_secrets;
mod m20260917_000001_value_type_namespace;
mod m20260924_000001_audit_records_append_only;
mod m20260924_000002_audit_records_retention_floor;
mod m20260925_000001_audit_records_default_horizon_index;
mod m20260925_000002_audit_retention_policy;

/// The gear's migrations, oldest first.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260812_000001_initial::Migration),
            Box::new(m20260813_000001_categories::Migration),
            Box::new(m20260825_000001_setting_declarations::Migration),
            Box::new(m20260906_000001_setting_values::Migration),
            Box::new(m20260907_000001_audit_records::Migration),
            Box::new(m20260907_000002_tenant_permissions::Migration),
            Box::new(m20260915_000001_pending_secrets::Migration),
            Box::new(m20260917_000001_value_type_namespace::Migration),
            Box::new(m20260924_000001_audit_records_append_only::Migration),
            Box::new(m20260924_000002_audit_records_retention_floor::Migration),
            Box::new(m20260925_000001_audit_records_default_horizon_index::Migration),
            Box::new(m20260925_000002_audit_retention_policy::Migration),
        ]
    }
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod migrations_tests;
