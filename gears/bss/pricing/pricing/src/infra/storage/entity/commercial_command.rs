//! Tenant-scoped immutable commercial command row.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_commercial_command")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    pub caller_tenant_id: Uuid,
    pub caller_id: Uuid,
    pub operation: String,
    pub idempotency_key: String,
    pub request_digest: String,
    pub receipt_kind: String,
    pub receipt_id: Uuid,
    pub acceptance_id: Option<Uuid>,
    pub hold_id: Option<Uuid>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
