use crate::domain::models::Chat;
use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "chats")]
#[secure(
    tenant_col = "tenant_id",
    owner_col = "user_id",
    resource_col = "id",
    no_type
)]
#[allow(clippy::struct_field_names)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub model: String,
    #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
    pub title: Option<String>,
    pub is_temporary: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for Chat {
    fn from(m: Model) -> Self {
        Self {
            id: m.id,
            tenant_id: m.tenant_id,
            user_id: m.user_id,
            model: m.model,
            title: m.title,
            is_temporary: m.is_temporary,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}
