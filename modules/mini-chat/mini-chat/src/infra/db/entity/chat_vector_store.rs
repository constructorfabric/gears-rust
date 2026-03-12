use modkit_db_macros::Scopable;
#[allow(clippy::wildcard_imports)]
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "chat_vector_stores")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    #[sea_orm(column_type = "String(StringLen::N(128))", nullable)]
    pub vector_store_id: Option<String>,
    #[sea_orm(column_type = "String(StringLen::N(128))")]
    pub provider: String,
    pub file_count: i32,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
