//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__fulfillment_grant")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub grant_id: Uuid,
    pub order_id: Uuid,
    pub order_version: i32,
    pub fulfillment_attempt_id: Uuid,
    pub generation: i64,
    pub roster_receipt_digest: String,
    pub roster: Json,
    pub authority_fact_digest: String,
    pub predecessor_grant_id: Option<Uuid>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub execution_id: Uuid,
    pub audit_id: Uuid,
    pub audit_outcome: String,
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
