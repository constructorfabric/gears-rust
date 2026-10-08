//! Tenant-scoped storage for `pricing_reference_op`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_reference_op")]
#[secure(tenant_col = "tenant_id", resource_col = "op_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_id: Uuid,
    pub tenant_id: Uuid,
    pub kind: String,
    /// `price_book_entry` or `plan_item`; no foreign key, so an op outlives its reference.
    pub ref_kind: String,
    pub ref_id: Uuid,
    pub sku_id: Uuid,
    pub reservation_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub state: String,
    pub outcome: Option<String>,
    pub attempts: i32,
    pub next_attempt_at: TimeDateTimeWithTimeZone,
    pub last_error: Option<String>,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
