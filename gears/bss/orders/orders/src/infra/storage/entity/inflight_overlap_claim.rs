//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__inflight_overlap_claim")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub claim_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub resource_tenant_id: Uuid,
    pub overlap_scope_key: String,
    pub order_id: Uuid,
    pub version: i32,
    pub claimed_at: TimeDateTimeWithTimeZone,
    pub released_at: Option<TimeDateTimeWithTimeZone>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
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
