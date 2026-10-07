//! Tenant-scoped storage model for `products_sku_reference`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_sku_reference")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub sku_id: Uuid,
    pub owner_gear: String,
    pub ref_kind: String,
    pub ref_id: Uuid,
    pub state: String,
    pub reserved_by: Uuid,
    pub reserved_at: TimeDateTimeWithTimeZone,
    pub confirmed_at: Option<TimeDateTimeWithTimeZone>,
    pub released_at: Option<TimeDateTimeWithTimeZone>,
    pub released_by: Option<Uuid>,
    pub release_reason: Option<String>,
    pub forced: bool,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
