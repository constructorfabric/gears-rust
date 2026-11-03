//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Engine-owned staged fulfillment control (hold or terminal coordination) for an order (D-198).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__fulfillment_control")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Control identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub control_id: Uuid,
    /// Owning order.
    pub order_id: Uuid,
    /// Execution generation of the owning idempotency record; unique.
    pub idempotency_execution_id: Uuid,
    /// `hold`, `cancel-workflow-mediated`, `acknowledge-failed` or `force-fail-unreconciled`.
    pub operation: String,
    /// Fingerprint of the originating request.
    pub request_fingerprint: String,
    /// Committed order version the control was requested against.
    pub expected_version: i32,
    /// Fulfillment attempt the control coordinates.
    pub fulfillment_attempt_id: Uuid,
    /// Fulfillment control generation allocated from the order.
    pub generation: i64,
    /// Original actor that requested the control.
    pub original_actor: Json,
    /// Delegation proof reference supplied with the request, if any.
    pub proof_reference: Option<String>,
    /// Fingerprint of the authorization facts the request was decided on.
    pub authorization_fact_fingerprint: String,
    /// Exact receiver, line and create-key roster to coordinate.
    pub roster: Json,
    /// Digest of `roster`.
    pub roster_digest: String,
    /// Creation instant.
    pub created_at: TimeDateTimeWithTimeZone,
    /// `prepared`, `awaiting`, `barrier_ready`, `settled`, `refused` or `abandoned`.
    pub status: String,
    /// Token of the live executor; replaced on reclaim.
    pub owner_token: Uuid,
    /// Monotonic fencing generation, incremented on every ownership reclaim.
    pub fencing_generation: i64,
    /// Executor lease expiry while nonterminal; NULL once terminal.
    pub lease_until: Option<TimeDateTimeWithTimeZone>,
    /// Per-receiver command identifiers.
    pub receiver_commands: Json,
    /// Immutable first outcome and evidence per receiver.
    pub receiver_evidence: Json,
    /// Classification of the error behind the outcome, if any.
    pub error_classification: Option<String>,
    /// When the control reached a terminal status; NULL while unresolved.
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
