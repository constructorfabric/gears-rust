//! Tenant-scoped storage model for `products_quota_definition_version` (P-D-270, P-D-274): one
//! immutable definition version. The table's triggers refuse every `UPDATE` and `DELETE`; this
//! entity carries no write besides the insert.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_quota_definition_version")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "definition_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub definition_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i64,
    pub candidate_id: Uuid,
    pub sku_id: Uuid,
    pub sku_version: i64,
    pub metric_uuid: Uuid,
    pub subject_type_uuid: Uuid,
    pub profile_id: Uuid,
    pub mapping_id: Uuid,
    pub mapping_version: i64,
    pub content_json: Json,
    pub digest: String,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
