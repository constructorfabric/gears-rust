//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Engine-owned submit or amendment attempt for a reserved candidate version (D-188).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__commercial_attempt")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Attempt identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub attempt_id: Uuid,
    /// Owning order.
    pub order_id: Uuid,
    /// Reserved candidate version this attempt would commit; unique per order.
    pub candidate_version: i32,
    /// Committed version the candidate supersedes.
    pub previous_committed_version: i32,
    /// Execution generation of the owning idempotency record; unique.
    pub idempotency_execution_id: Uuid,
    /// `submit` or `amendment`.
    pub operation: String,
    /// Authorized principal scope of the owning idempotency record.
    pub principal_scope: String,
    /// Fingerprint of the originating request.
    pub request_fingerprint: String,
    /// Draft revision the submit was prepared from; set only for `submit`.
    pub prepared_draft_revision: Option<i64>,
    /// Complete proposed tenant arrangement captured at reservation.
    pub proposed_arrangement: Json,
    /// Fingerprint of the authorization facts the request was decided on.
    pub authorization_fact_fingerprint: String,
    /// Original business principal that requested the operation.
    pub original_principal: Json,
    /// Delegation proof reference supplied with the request, if any.
    pub proof_reference: Option<String>,
    /// Canonical per-line Pricing requests with their hashes and stable command keys.
    pub line_requests: Json,
    /// Captured date-policy basis for the candidate's lines.
    pub date_policy_basis: Json,
    /// Authenticated seller service subject running the commercial commands (D-194).
    pub commercial_subject_id: Uuid,
    /// Type of the authenticated commercial service subject.
    pub commercial_subject_type: String,
    /// Tenant of the authenticated commercial service subject.
    pub commercial_subject_tenant_id: Uuid,
    /// Operational status: `prepared`, `running`, `committed`, `refused` or `abandoned`.
    pub status: String,
    /// Token of the live executor; replaced on reclaim.
    pub owner_token: Uuid,
    /// Monotonic fencing generation, incremented on every ownership reclaim.
    pub fencing_generation: i64,
    /// Executor lease expiry while `prepared` or `running`; NULL once terminal.
    pub lease_until: Option<TimeDateTimeWithTimeZone>,
    /// Per-line Pricing receipt IDs, digests and results.
    pub receipt_results: Json,
    /// Creation instant.
    pub created_at: TimeDateTimeWithTimeZone,
    /// When the attempt reached a terminal status; NULL while unresolved.
    pub terminal_at: Option<TimeDateTimeWithTimeZone>,
}
/// `SeaORM` relations of this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to the owning order in `bss_orders__order`.
    #[sea_orm(
        belongs_to = "super::order::Entity",
        from = "Column::OrderId",
        to = "super::order::Column::OrderId"
    )]
    Order,
}
impl Related<super::order::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Order.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
