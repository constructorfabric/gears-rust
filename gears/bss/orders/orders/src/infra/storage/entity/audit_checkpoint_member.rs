//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__audit_checkpoint_member")]
#[secure(tenant_col = "audit_tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub checkpoint_sequence: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    pub audit_sequence: i64,
    pub entry_hash: Vec<u8>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
