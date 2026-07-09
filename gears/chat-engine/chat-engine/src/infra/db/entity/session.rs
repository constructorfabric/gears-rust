// @cpt-cf-chat-engine-dbtable-sessions:p2
// @cpt-cf-chat-engine-adr-session-deletion-strategy:p1

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "sessions")]
#[secure(tenant_col = "tenant_id", owner_col = "user_id", resource_col = "session_id", no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub client_id: Option<String>,
    pub session_type_id: Option<Uuid>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub enabled_capabilities: Option<serde_json::Value>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,
    pub lifecycle_state: String,
    pub share_token: Option<String>,
    pub deleted_at: Option<OffsetDateTime>,
    pub scheduled_hard_delete_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::session_type::Entity",
        from = "Column::SessionTypeId",
        to = "super::session_type::Column::SessionTypeId",
        on_update = "NoAction",
        on_delete = "Restrict"
    )]
    SessionType,
    #[sea_orm(has_many = "super::message::Entity")]
    Message,
}

impl Related<super::session_type::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SessionType.def()
    }
}

impl Related<super::message::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Message.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
