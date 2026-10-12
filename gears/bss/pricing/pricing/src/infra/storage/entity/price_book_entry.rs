//! Tenant-scoped storage for `pricing_price_book_entry`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_price_book_entry")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
#[allow(
    clippy::struct_field_names,
    reason = "SeaORM requires Model; the schema names the entry's pricing discriminator model (D-427)"
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub book_id: Uuid,
    pub sku_id: Uuid,
    pub charge_kind: String,
    pub period: Option<String>,
    /// The entry's pricing model (D-427): fixed for its life, part of its key; every price of
    /// the entry is money in it. `flat`, `per_unit`, `graduated`, `volume` or `package`.
    pub model: String,
    pub usage_policy_id: Option<Uuid>,
    pub usage_policy_version: Option<i64>,
    pub usage_policy_digest: Option<String>,
    /// The SKU head's `published_version` when the meter was checked. Null for a non-usage entry
    /// and for a usage entry written before D-514.
    pub usage_sku_version: Option<i64>,
    pub dimension_key: Option<String>,
    pub invoice_line_override: Option<String>,
    pub reservation_id: Uuid,
    pub reference_state: String,
    pub version: i64,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
