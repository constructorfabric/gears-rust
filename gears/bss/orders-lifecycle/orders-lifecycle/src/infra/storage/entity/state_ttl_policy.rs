//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__state_ttl_policy")]
#[secure(no_tenant, resource_col = "policy_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub policy_id: Uuid,
    pub scope: String,
    pub seller_tenant_id: Option<Uuid>,
    pub state: String,
    #[sea_orm(
        column_type = "custom(\"interval\")",
        select_as = "text",
        save_as = "interval"
    )]
    pub ttl_duration: Option<String>,
    pub provisional: bool,
    pub policy_revision: i64,
    pub updated_by: String,
    pub updated_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
