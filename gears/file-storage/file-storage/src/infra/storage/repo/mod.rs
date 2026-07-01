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
pub use events_outbox_repo::EventsOutboxRepo;
pub use file_repo::FileRepo;
pub use idempotency_repo::IdempotencyRepo;
pub use metadata_repo::MetadataRepo;
pub use multipart_repo::MultipartRepo;
pub use policy_repo::PolicyRepo;
pub use retention_rule_repo::RetentionRuleRepo;
pub use version_repo::VersionRepo;

use crate::domain::policy::{RetentionRuleBody, RetentionScope};

/// Row types returned by the audit / file-event outbox repositories.
///
/// Defined on the repo-layer facade (rather than re-exported from the ORM
/// `entity` modules) so callers such as [`Store`](crate::infra::storage::store)
/// depend on this one module for the row types instead of reaching directly
/// into `entity::*` — keeping the store's fan-out on the repo layer it already
/// talks to.
pub type AuditRow = crate::infra::storage::entity::audit_outbox::Model;
/// See [`AuditRow`].
pub type FileEventRow = crate::infra::storage::entity::events_outbox::Model;

/// Parameters for inserting a new retention rule.
///
/// Defined on the repo-layer facade (like [`AuditRow`] / [`FileEventRow`]) so
/// [`Store`](crate::infra::storage::store) depends on this one module for the
/// type instead of reaching into the `retention_rule_repo` submodule directly.
pub struct InsertRetentionRule<'a> {
    pub tenant_id: uuid::Uuid,
    pub retention_scope: &'a RetentionScope,
    pub scope_target_id: Option<uuid::Uuid>,
    pub body: &'a RetentionRuleBody,
    pub now: time::OffsetDateTime,
}

/// The full set of tenant-scoped repositories, owned by the persistence
/// [`Store`](crate::infra::storage::store::Store).
///
/// Bundling the nine repositories into one aggregate lets `Store` depend on a
/// single collaborator instead of naming each repo type directly — the repo
/// membership (and its coupling to the nine repo modules) lives here, on a node
/// that nothing else routes through, rather than on the `Store` crossroads.
/// Every field is a cheap unit struct, so `Repos` is trivially `Clone`.
#[derive(Clone, Default)]
pub struct Repos {
    pub files: FileRepo,
    pub versions: VersionRepo,
    pub metadata: MetadataRepo,
    pub policies: PolicyRepo,
    pub retention_rules: RetentionRuleRepo,
    pub multipart: MultipartRepo,
    pub idempotency_keys: IdempotencyRepo,
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub audit: AuditRepo,
    /// @cpt-cf-file-storage-fr-file-events
    pub events_outbox: EventsOutboxRepo,
}
