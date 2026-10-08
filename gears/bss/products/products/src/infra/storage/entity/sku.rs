//! Tenant-scoped storage model for `products_sku`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_sku")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "sellable, type_change_pending and retire_pending are three independent flags (P-D-248)"
)]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    #[sea_orm(column_name = "type")]
    pub r#type: String,
    /// Nullable since `m20260925_000007` (P-D-196).
    pub category_id: Option<Uuid>,
    pub description: String,
    pub sellable: bool,
    pub lifecycle: String,
    pub fenced_at: Option<TimeDateTimeWithTimeZone>,
    pub fence_op_id: Option<Uuid>,
    pub revision: i64,
    pub published_version: i64,
    pub gl_code: Option<String>,
    pub tax_category: Option<String>,
    pub invoice_line_template: Option<String>,
    pub billing_timing: Option<String>,
    pub usage_type_ref: Option<String>,
    pub unit: Option<String>,
    pub type_change_pending: bool,
    /// Set while a retire is in review (P-D-248). The lifecycle column is unchanged.
    pub retire_pending: bool,
    /// The lifecycle a dated change will install, with [`Self::lifecycle_next_from`] (P-D-249).
    pub lifecycle_next: Option<String>,
    pub lifecycle_next_from: Option<TimeDate>,
    pub pending_unit_id: Option<Uuid>,
    pub approved_by_unit_id: Option<Uuid>,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
    /// Set while the retired SKU is archived (P-D-263): a mark, not a lifecycle.
    pub archived_at: Option<TimeDateTimeWithTimeZone>,
    /// Who archived it; set with [`Self::archived_at`].
    pub archived_by: Option<Uuid>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
