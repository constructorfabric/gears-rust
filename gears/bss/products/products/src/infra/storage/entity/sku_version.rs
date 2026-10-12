//! Tenant-scoped storage model for `products_sku_version`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_sku_version")]
#[secure(tenant_col = "tenant_id", resource_col = "sku_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub sku_id: Uuid,
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub published_version: i64,
    pub effective_from: TimeDate,
    pub content: Json,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
