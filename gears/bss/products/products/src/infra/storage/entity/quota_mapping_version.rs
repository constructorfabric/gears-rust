//! Tenant-scoped storage model for `products_quota_mapping_version` (P-D-270, P-D-274): one
//! immutable mapping version with its qualified profile version. The table's triggers refuse every
//! `UPDATE` and `DELETE`; this entity carries no write besides the insert.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_quota_mapping_version")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "mapping_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub mapping_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i64,
    pub target_id: Uuid,
    pub capability_key: String,
    pub plugin_instance_id: String,
    pub target_key: String,
    pub mapping_key: String,
    pub mapping_json: Json,
    pub digest: String,
    pub profile_id: Uuid,
    pub profile_version: i64,
    pub profile_json: Json,
    pub profile_digest: String,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
