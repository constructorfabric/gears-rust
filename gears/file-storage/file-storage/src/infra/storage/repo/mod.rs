//! Tenant-scoped repositories (`SecureORM`) for the control-plane metadata.
//!
//! All access goes through `toolkit_db::secure` with a `DBRunner` and an
//! `AccessScope`. Tenant isolation is enforced on the `files` table; version and
//! custom-metadata rows are reached only after the parent file is authorized, so
//! their `file_id`-keyed queries use an unconstrained scope.

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

use sea_orm::ExprTrait;
use sea_orm::sea_query::{Expr, IntoColumnRef};

use crate::domain::policy::{RetentionRuleBody, RetentionScope};

// Keyset predicates use row-value comparisons: Postgres uses `(a, b) < (x, y)` as an index
// range bound, whereas the `a < x OR (a = x AND b < y)` expansion only filters after the scan.
fn keyset_pair(
    a: impl IntoColumnRef,
    b: impl IntoColumnRef,
    va: impl Into<sea_orm::Value>,
    vb: impl Into<sea_orm::Value>,
) -> (Expr, Expr) {
    (
        Expr::tuple([Expr::col(a), Expr::col(b)]),
        Expr::tuple([Expr::val(va), Expr::val(vb)]),
    )
}

fn tuple_lt(
    a: impl IntoColumnRef,
    b: impl IntoColumnRef,
    va: impl Into<sea_orm::Value>,
    vb: impl Into<sea_orm::Value>,
) -> Expr {
    let (cols, vals) = keyset_pair(a, b, va, vb);
    cols.lt(vals)
}

fn tuple_gt(
    a: impl IntoColumnRef,
    b: impl IntoColumnRef,
    va: impl Into<sea_orm::Value>,
    vb: impl Into<sea_orm::Value>,
) -> Expr {
    let (cols, vals) = keyset_pair(a, b, va, vb);
    cols.gt(vals)
}

/// Row types returned by the audit / file-event outbox repositories (defined here so
/// callers do not reach into `entity::*`).
pub type AuditRow = crate::infra::storage::entity::audit_outbox::Model;
/// See [`AuditRow`].
pub type FileEventRow = crate::infra::storage::entity::events_outbox::Model;

/// Parameters for inserting a new retention rule.
pub struct InsertRetentionRule<'a> {
    pub tenant_id: uuid::Uuid,
    pub retention_scope: &'a RetentionScope,
    pub scope_target_id: Option<uuid::Uuid>,
    pub body: &'a RetentionRuleBody,
    pub now: time::OffsetDateTime,
}

/// Parameters for `RetentionRuleRepo::list_page`.
pub(crate) struct RetentionRuleListParams {
    pub tenant_id: uuid::Uuid,
    /// Already `limit + 1`; this method does not know the `Page` `has_more` convention.
    pub limit: u64,
    pub after: Option<crate::domain::pagination::Seek>,
}

/// All repositories, bundled so `Store` depends on one collaborator.
/// Every field is a unit struct, so `Repos` is trivially `Clone`.
#[derive(Clone, Default)]
pub struct Repos {
    pub files: FileRepo,
    pub versions: VersionRepo,
    pub metadata: MetadataRepo,
    pub policies: PolicyRepo,
    pub retention_rules: RetentionRuleRepo,
    pub multipart: MultipartRepo,
    pub idempotency_keys: IdempotencyRepo,
    pub audit: AuditRepo,
    pub events_outbox: EventsOutboxRepo,
}
