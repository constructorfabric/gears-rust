//! Tenant-scoped storage model for `products_approval_decision`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_approval_decision")]
#[secure(tenant_col = "tenant_id", resource_col = "unit_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub unit_id: Uuid,
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub actor: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub generation: i32,
    pub decision: String,
    pub note: Option<String>,
    pub at: TimeDateTimeWithTimeZone,
    pub stale: bool,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
