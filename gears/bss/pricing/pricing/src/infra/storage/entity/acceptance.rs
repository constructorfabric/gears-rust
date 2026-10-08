//! Tenant-scoped immutable commercial acceptance row.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_acceptance")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    pub order_id: Uuid,
    pub order_version: String,
    pub line_id: Uuid,
    pub request_digest: String,
    pub terms_digest: String,
    pub receipt_json: String,
    pub accepted_at: String,
    pub hold_until: String,
    pub created_by: Uuid,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
