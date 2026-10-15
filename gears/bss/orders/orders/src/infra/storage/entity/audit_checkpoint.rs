//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__audit_checkpoint")]
#[secure(tenant_col = "audit_tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub checkpoint_sequence: i64,
    pub format_version: i16,
    pub captured_at: TimeDateTimeWithTimeZone,
    pub member_count: i64,
    pub prev_checkpoint_hash: Vec<u8>,
    pub checkpoint_hash: Vec<u8>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
