//! Tenant-scoped storage model for `products_quota_adapter_target` (P-D-268, P-D-274): one binding
//! of a discovery plugin instance and a target key. The table's triggers refuse every `UPDATE` and
//! `DELETE`; this entity carries no write besides the insert.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_quota_adapter_target")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub plugin_instance_id: String,
    pub target_key: String,
    pub name: String,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
