use super::entity;
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "orders_capability_history")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i32,
    pub old_payer_id: Uuid,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "entity::Entity",
        from = "Column::OrderId",
        to = "entity::Column::OrderId"
    )]
    Order,
}
impl Related<entity::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Order.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
