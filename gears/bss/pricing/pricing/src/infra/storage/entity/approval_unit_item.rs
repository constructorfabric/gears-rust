//! Scoped persistence model, following Products.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_approval_unit_item")]
#[secure(tenant_col = "tenant_id", resource_col = "unit_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub unit_id: Uuid,
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub item_type: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub item_id: Uuid,
    pub created_by: Uuid,
    pub before_json: Option<Json>,
    pub after_json: Json,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
