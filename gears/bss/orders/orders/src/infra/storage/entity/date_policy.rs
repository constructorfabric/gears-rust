//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__date_policy")]
// `resource_tenant_id` addresses a tenant's override without knowing its fresh `policy_id`
// (DESIGN 02 §3.7); the platform default is addressed by its immutable identity.
#[secure(
    no_tenant,
    resource_col = "policy_id",
    no_owner,
    no_type,
    pep_prop(resource_tenant_id = "resource_tenant_id")
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub policy_id: Uuid,
    pub resource_tenant_id: Option<Uuid>,
    pub service_activation_required: bool,
    pub acceptance_due_required: bool,
    pub revision: i64,
    pub updated_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
