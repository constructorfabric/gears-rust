use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "durable_runs")]
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
    pub revision: i64,
    pub registration_generation: i64,
    pub status: String,
    pub journal: Vec<u8>,
    pub storage_version: i32,
    pub activity_count: i32,
    pub lease_until: Option<DateTimeUtc>,
    pub due_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
