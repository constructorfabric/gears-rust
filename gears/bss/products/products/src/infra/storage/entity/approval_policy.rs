//! Tenant-scoped storage model for `products_approval_policy`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_approval_policy")]
#[secure(tenant_col = "tenant_id", resource_col = "kind", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub kind: String,
    pub quorum: i32,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
