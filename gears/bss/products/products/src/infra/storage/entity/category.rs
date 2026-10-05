//! Tenant-scoped storage model for `products_category`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_category")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    pub is_default: bool,
    pub sort_order: i32,
    pub status: String,
    pub version: i64,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
    /// Set while the retired category is archived (P-D-263): a mark, not a status.
    pub archived_at: Option<TimeDateTimeWithTimeZone>,
    /// Who archived it; set with [`Self::archived_at`].
    pub archived_by: Option<Uuid>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
