//! Database migration registry for the file-storage gear.

use sea_orm_migration::prelude::*;

mod m20260624_000001_p1_initial;
mod m20260701_000001_p2_initial;
mod m20260701_000002_multipart_plan_columns;
mod m20260706_000001_idempotency_subject_id;
mod m20260706_000002_idempotency_request_hash;
mod m20260706_000003_policies_unique_scope;
mod m20260707_000001_content_hash_modes;
mod m20260722_000001_multipart_auto_bind;

/// File-storage migrator. P1 ships the initial control-plane metadata tables;
/// P2 adds the policy store, retention rules, multipart uploads + idempotency
/// keys, and the audit + file-events transactional outboxes in one step.
/// P2-multipart-coordinator adds the plan columns (`declared_size`, `part_size`)
/// to `multipart_uploads` for the server-authoritative parts-plan model.
/// P2-remediation-0.10 adds `subject_id` to `idempotency_keys` so a replay can
/// be bound to the authenticated caller, not just the request-body owner.
/// P2-remediation-2.1 adds `request_hash` to `idempotency_keys` so a replay
/// can be bound to the request body that created it, not just the caller.
/// P2-remediation-2.4 adds two partial unique indexes on `policies` so at
/// most one row can exist per `(tenant_id, scope, scope_owner_id)`, closing
/// the upsert delete-then-insert race.
/// ADR-0006 adds `hash_mode`/`part_count` to `file_versions` and the new
/// `version_hash_manifest` table for the multipart offset-manifest composite
/// content-hash mode.
/// The upload-flow redesign adds `auto_bind` to `multipart_uploads` so
/// `complete` knows whether to bind the finalized version itself.
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
            Box::new(m20260722_000001_multipart_auto_bind::Migration),
        ]
    }
}
