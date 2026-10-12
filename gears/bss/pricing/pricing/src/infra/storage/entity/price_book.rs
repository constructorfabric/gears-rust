//! Tenant-scoped storage for `pricing_price_book`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_price_book")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    pub currency: String,
    pub valid_from: Option<TimeDate>,
    pub valid_until: Option<TimeDate>,
    /// Free text, at most 2000 characters (D-444); NULL for a book without one.
    pub description: Option<String>,
    pub version: i64,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
    /// Set while the book is archived (D-522): a mark, not a state. Its entries' references are
    /// released and its entries and prices are read-only.
    pub archived_at: Option<TimeDateTimeWithTimeZone>,
    /// Who archived it; set with [`Self::archived_at`].
    pub archived_by: Option<Uuid>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
