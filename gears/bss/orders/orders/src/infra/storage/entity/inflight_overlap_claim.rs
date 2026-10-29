//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// In-flight reservation of an overlap scope by an order version.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__inflight_overlap_claim")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Claim identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub claim_id: Uuid,
    /// Billing party that owns the overlap scope.
    pub payer_tenant_id: Uuid,
    /// The order's resource recipient, copied from the order.
    pub resource_tenant_id: Uuid,
    /// Subscriptions' `catalogSubscriptionProductKey` for the line, stored as received.
    pub overlap_scope_key: String,
    /// Claiming order.
    pub order_id: Uuid,
    /// Proposed version of the reserving attempt; not proof of admission.
    pub version: i32,
    /// Server-recorded reservation instant.
    pub claimed_at: TimeDateTimeWithTimeZone,
    /// Server-recorded release instant; NULL while the claim is held.
    pub released_at: Option<TimeDateTimeWithTimeZone>,
}
/// `SeaORM` relations of this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to the claiming order in `bss_orders__order`.
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
