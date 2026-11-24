//! `qe_charges`: the charge of an allowed debit or a lease commit (ADR-0011),
//! which is that operation's idempotency record for as long as it lives.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_charges")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// Names this charge, apart from any later charge under the same key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// `debit` or `commit`.
    pub operation: String,
    /// The idempotency key of the charged operation.
    pub idem_key: String,
    /// The request fingerprint a replay is compared against.
    pub payload_hash: Vec<u8>,
    /// The attribution digest; a charge holds no subject identifier.
    pub attribution_digest: Vec<u8>,
    /// The first result, with its top-level schema version.
    pub decision_blob: String,
    /// Engine that produced the decision, when one was invoked.
    pub engine_id: Option<String>,
    /// Policy that produced it, when one was selected.
    pub policy_id: Option<String>,
    /// Version of that policy.
    pub policy_version: Option<i32>,
    /// The metric type id.
    pub metric: String,
    /// The charged amount.
    pub amount: i64,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// End of the return window, fixed at creation.
    pub return_window_end: OffsetDateTime,
    /// The charge is kept at least until then, set by the last return or
    /// rollback applied to it.
    pub kept_until: Option<OffsetDateTime>,
    /// When the charge is deleted: the later of `return_window_end` and
    /// `kept_until`, or `NULL` while one of its entries is holding.
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
