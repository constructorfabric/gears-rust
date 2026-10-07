use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "durable_start_keys")]
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
    pub definition: String,
    pub key_hash: String,
    pub input_hash: String,
    pub run_id: Uuid,
    pub generation: i64,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
