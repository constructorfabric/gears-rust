//! Tenant-scoped storage for `pricing_plan_item`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_plan_item")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub revision_id: Uuid,
    pub sku_id: Uuid,
    pub price_book_entry_id: Option<Uuid>,
    pub treatment: String,
    /// Canonical decimal text, exact on both dialects (as `min_fee`).
    pub included_qty: Option<String>,
    pub qty_min: Option<i32>,
    pub reservation_id: Option<Uuid>,
    pub reference_state: String,
    pub version: i64,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
