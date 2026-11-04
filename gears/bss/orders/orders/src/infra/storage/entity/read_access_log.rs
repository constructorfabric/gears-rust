//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Evidence of one order read or list call, served or refused.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__read_access_log")]
#[secure(
    no_tenant,
    resource_col = "access_id",
    no_owner,
    no_type,
    pep_prop(actor = "actor")
)]
pub struct Model {
    /// Entry identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub access_id: Uuid,
    /// Resolved order; NULL for a list call or a missing order.
    pub order_id: Option<Uuid>,
    /// Order identifier the caller asked for, without an FK; NULL for a list call.
    pub requested_order_ref: Option<Uuid>,
    /// Caller subject UUID rendered as lowercase hyphenated text.
    pub actor: String,
    /// Closed actor class: `system`, `service` or `user`.
    pub actor_class: String,
    /// Read operation performed.
    pub operation: String,
    /// `served` or `refused`.
    pub outcome: String,
    /// Registered refusal reason; set exactly when refused.
    pub refusal_reason: Option<String>,
    /// Delegation-proof refusal class hidden behind `order-not-found`; never returned.
    pub internal_refusal_detail: Option<String>,
    /// Delegation proof reference, if any.
    pub delegation_proof_ref: Option<String>,
    /// Access instant.
    pub accessed_at: TimeDateTimeWithTimeZone,
}
/// `SeaORM` relations of this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to the read order in `bss_orders__order`.
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
