//! Isolated probe schema, not the S2-02 Orders migrations.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "orders_capability_order")]
#[secure(
    no_tenant,
    resource_col = "order_id",
    no_owner,
    no_type,
    pep_prop(
        resource_tenant_id = "resource_tenant_id",
        seller_tenant_id = "seller_tenant_id",
        payer_tenant_id = "payer_tenant_id"
    )
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    pub resource_tenant_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub version: i32,
    pub state: String,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
