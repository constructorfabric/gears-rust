//! Tenant-scoped storage model for `products_quota_snapshot` (P-D-268, P-D-274): one verified
//! capability snapshot per target and revision. The table's triggers refuse every `UPDATE` and
//! `DELETE`; this entity carries no write besides the insert.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_quota_snapshot")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub target_id: Uuid,
    pub snapshot_id: Uuid,
    /// `^(0|[1-9][0-9]{0,38})$`, unique per tenant and target.
    pub source_revision: String,
    pub observed_at: TimeDateTimeWithTimeZone,
    pub item_count: i64,
    /// 64 lowercase hex digits.
    pub content_digest: String,
    pub snapshot_json: Json,
    pub evidence_json: Json,
    pub review_json: Json,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
