//! Database migration registry for the file-storage gear.

use sea_orm_migration::prelude::*;

mod m20260624_000001_p1_initial;
mod m20260701_000001_p2_initial;
mod m20260701_000002_multipart_plan_columns;

/// File-storage migrator. P1 ships the initial control-plane metadata tables;
/// P2 adds the policy store, retention rules, multipart uploads + idempotency
/// keys, and the audit + file-events transactional outboxes in one step.
/// P2-multipart-coordinator adds the plan columns (`declared_size`, `part_size`)
/// to `multipart_uploads` for the server-authoritative parts-plan model.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260624_000001_p1_initial::Migration),
            Box::new(m20260701_000001_p2_initial::Migration),
            Box::new(m20260701_000002_multipart_plan_columns::Migration),
        ]
    }
}
