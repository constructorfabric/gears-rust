//! Tenant-scoped repositories (`SecureORM`) for the control-plane metadata.
//!
//! All access goes through the `toolkit_db::secure` extension API, which takes a
//! `DBRunner` connection and an `AccessScope`. Tenant isolation is enforced
//! on the `files` table (`cpt-cf-file-storage-fr-tenant-boundary`); version and
//! custom-metadata rows are reached only after the parent file is authorized, so
//! they use an unconstrained scope on their `file_id`-keyed queries.
//!
//! P2-M1 adds `PolicyRepo` and `RetentionRuleRepo`.
//! P2-M3 adds `MultipartRepo` and `IdempotencyRepo`.
//! P2-M4 adds `AuditRepo`.

mod audit_repo;
mod events_outbox_repo;
mod file_repo;
mod idempotency_repo;
mod metadata_repo;
mod multipart_repo;
mod policy_repo;
mod retention_rule_repo;
mod version_repo;

pub use audit_repo::AuditRepo;
pub use events_outbox_repo::{EventsOutboxRepo, FileEvent};
pub use file_repo::FileRepo;
pub use idempotency_repo::IdempotencyRepo;
pub use metadata_repo::MetadataRepo;
pub use multipart_repo::MultipartRepo;
pub use policy_repo::PolicyRepo;
pub use retention_rule_repo::{InsertRetentionRule, RetentionRuleRepo};
pub use version_repo::VersionRepo;
