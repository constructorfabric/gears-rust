//! `SeaORM` migrations for the Account Management module.
//!
//! * `m0001_initial_schema` — `tenants`, `tenant_closure`,
//!   `tenant_metadata` tables with every column and index needed by
//!   the storage-floor repository.
//! * `m0002_add_terminal_failure_columns` — `tenants.terminal_failure_at`
//!   column for provisioning-reaper terminal-failure handling
//!   (operator-action-required state that keeps the row out of the
//!   automatic reaper retry loop).
//! * `m0003_create_integrity_check_runs` —
//!   `integrity_check_runs` single-flight gate table for the
//!   Rust-side hierarchy-integrity check (singleton-row table with a
//!   `CHECK (id = 1)` PK so concurrent acquires collide on
//!   unique-violation, lifecycle-bound to the integrity-check
//!   transaction).
//! * `m0004_create_conversion_requests` — `conversion_requests` and
//!   `tenant_idp_metadata` tables. The former backs the
//!   managed/self-managed dual-consent conversion state machine (with
//!   the partial unique index that enforces the at-most-one-pending
//!   invariant per tenant); the latter holds the plugin-private
//!   per-tenant state AM echoes through `TenantContext::metadata` on
//!   every `IdP` call, separated from public `tenant_metadata` so
//!   plugin internals do not leak through the public metadata REST
//!   surface.
//! * `m0005_tenant_metadata_indexes` — adds
//!   `idx_tenant_metadata_schema ON tenant_metadata(schema_uuid)`
//!   for the FEATURE 2.7 (Tenant Metadata) walk-up resolver and
//!   future per-schema cross-tenant scans. The `tenant_metadata`
//!   table itself is owned by `m0001`; this migration only adds the
//!   secondary index.

use sea_orm_migration::prelude::*;

pub mod m0001_initial_schema;
pub mod m0002_add_terminal_failure_columns;
pub mod m0003_create_integrity_check_runs;
pub mod m0004_create_conversion_requests;
pub mod m0005_tenant_metadata_indexes;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_initial_schema::Migration),
            Box::new(m0002_add_terminal_failure_columns::Migration),
            Box::new(m0003_create_integrity_check_runs::Migration),
            Box::new(m0004_create_conversion_requests::Migration),
            Box::new(m0005_tenant_metadata_indexes::Migration),
        ]
    }
}
