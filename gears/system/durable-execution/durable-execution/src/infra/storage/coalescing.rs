use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "durable_coalescing")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
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
    pub revision: i64,
    pub active_run_id: Option<Uuid>,
    pub successor_run_id: Option<Uuid>,
    pub requested_generation: i64,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
