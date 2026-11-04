//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Idempotency record of one operation key within an authorized principal scope.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__idempotency")]
#[secure(
    no_tenant,
    resource_col = "execution_id",
    no_owner,
    no_type,
    pep_prop(
        principal_scope = "principal_scope",
        operation = "operation",
        idempotency_key = "idempotency_key"
    )
)]
pub struct Model {
    /// Operation the key scopes.
    #[sea_orm(primary_key, auto_increment = false)]
    pub operation: String,
    /// Authorized principal the key is bound to, taken from the security context.
    #[sea_orm(primary_key, auto_increment = false)]
    pub principal_scope: String,
    /// Caller-supplied key, unique within `(operation, principal_scope)`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub idempotency_key: String,
    /// Target order; NULL for an in-flight or refused create.
    pub order_id: Option<Uuid>,
    /// Hash of the request inputs; detects a same-key different-request replay.
    pub request_fingerprint: String,
    /// `in_flight` or `settled`.
    pub status: String,
    /// Fresh execution generation identity; never derived from the client key.
    pub execution_id: Uuid,
    /// Linked commercial attempt, once a candidate version is reserved.
    pub attempt_id: Option<Uuid>,
    /// Linked fulfillment control; mutually exclusive with `attempt_id`.
    pub fulfillment_control_id: Option<Uuid>,
    /// Token of the live executor; replaced on reclaim.
    pub owner_token: Option<Uuid>,
    /// Monotonic fencing generation, incremented on every ownership reclaim.
    pub fencing_generation: i64,
    /// Executor lease expiry while in flight; NULL once settled.
    pub lease_expires_at: Option<TimeDateTimeWithTimeZone>,
    /// `success` or `refused` once settled.
    pub outcome: Option<String>,
    /// Registered reason on a refusal.
    pub outcome_reason: Option<String>,
    /// Transition audit entry the settled record produced.
    pub audit_id: Option<Uuid>,
    /// Immutable versioned response snapshot written at settlement; NULL while in flight.
    pub settled_response: Option<Json>,
    /// Creation instant.
    pub created_at: TimeDateTimeWithTimeZone,
    /// Instant after which the record may be purged.
    pub expires_at: TimeDateTimeWithTimeZone,
}
/// `SeaORM` relations of this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to the target order in `bss_orders__order`.
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
