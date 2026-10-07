use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "durable_epochs")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "run_id",
    owner_col = "owner_id",
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
    pub run_id: Uuid,
    pub epoch: i64,
    pub state: Vec<u8>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
