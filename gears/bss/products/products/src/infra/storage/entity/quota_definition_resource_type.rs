//! Tenant-scoped storage model for `products_quota_definition_resource_type` (P-D-270, P-D-274):
//! one resource type a definition version counts, by its deterministic UUID. The table's triggers
//! refuse every `UPDATE` and `DELETE`; this entity carries no write besides the insert.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_quota_definition_resource_type")]
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
    #[sea_orm(primary_key, auto_increment = false)]
    pub resource_type_uuid: Uuid,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
