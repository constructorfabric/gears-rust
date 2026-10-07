//! Tenant-scoped storage model for `products_derived_usage_type` (P-D-231): one row per derived
//! usage type, its identity and its name. No door writes it after the create.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_derived_usage_type")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// `^[a-z0-9][a-z0-9._-]{0,63}$`, unique per tenant: the `<code>` of the meter id.
    pub code: String,
    pub name: String,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
