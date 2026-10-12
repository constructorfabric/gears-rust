//! Database migration registry for the file-storage gear.

use sea_orm_migration::prelude::*;

mod m20260624_000001_p1_initial;
mod m20260701_000001_p2_initial;
mod m20260701_000002_multipart_plan_columns;
mod m20260706_000001_idempotency_subject_id;
mod m20260706_000002_idempotency_request_hash;
mod m20260706_000003_policies_unique_scope;
mod m20260707_000001_content_hash_modes;

/// File-storage migrator: control-plane tables, policy/retention/multipart/idempotency/outbox
/// tables, multipart plan columns, idempotency `subject_id` and `request_hash`, unique
/// policy-scope indexes, and content-hash modes (`hash_mode`, `version_hash_manifest`).
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260624_000001_p1_initial::Migration),
            Box::new(m20260701_000001_p2_initial::Migration),
            Box::new(m20260701_000002_multipart_plan_columns::Migration),
            Box::new(m20260706_000001_idempotency_subject_id::Migration),
            Box::new(m20260706_000002_idempotency_request_hash::Migration),
            Box::new(m20260706_000003_policies_unique_scope::Migration),
            Box::new(m20260707_000001_content_hash_modes::Migration),
        ]
    }
}
