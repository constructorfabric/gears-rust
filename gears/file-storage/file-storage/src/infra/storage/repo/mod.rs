//! Tenant-scoped repositories (`SecureORM`) for the control-plane metadata.
//!
//! All access goes through the `toolkit_db::secure` extension API, which takes a
//! `DBRunner` connection and an `AccessScope`. Tenant isolation is enforced
//! on the `files` table (`cpt-cf-file-storage-fr-tenant-boundary`); version and
//! custom-metadata rows are reached only after the parent file is authorized, so
//! they use an unconstrained scope on their `file_id`-keyed queries.
//!
//! `PolicyRepo` and `RetentionRuleRepo` cover the policy engine;
//! `MultipartRepo` and `IdempotencyRepo` cover multipart uploads; `AuditRepo`
//! covers audit recording.

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

// Keyset predicates are written as row-value comparisons because Postgres
// uses `(a, b) < (x, y)` as an index range bound, while the equivalent
// `a < x OR (a = x AND b < y)` expansion only filters after the scan.
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

/// Parameters for [`RetentionRuleRepo::list_page`](retention_rule_repo::RetentionRuleRepo::list_page)
///. Defined here for the same reason as
/// [`InsertRetentionRule`] above.
pub struct RetentionRuleListParams<'a> {
    pub tenant_id: uuid::Uuid,
    /// Skips the non-admin visibility filter entirely -- an admin sees
    /// every rule in the tenant.
    pub admin: bool,
    /// The caller's own `(owner_kind, owner_id)` pair -- `"user"`/`"app"` --
    /// used to resolve which `File`-scope rules are visible to a non-admin
    /// caller (ignored when `admin` is `true`).
    pub subject_kind: &'a str,
    pub subject_id: uuid::Uuid,
    /// Already `limit + 1` (or whatever the caller wants fetched); this
    /// method does not itself know about the `Page` `has_more` convention.
    pub limit: u64,
    pub after: Option<crate::domain::pagination::Seek>,
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
    pub audit: AuditRepo,
    pub events_outbox: EventsOutboxRepo,
}
