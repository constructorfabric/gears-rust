// @cpt-cf-chat-engine-dbtable-session-types:p1

use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

// Session types are global lookup rows (no per-tenant column); the
// `unrestricted` marker advertises that intent to the secure layer.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "session_types")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_type_id: Uuid,
    pub name: String,
    pub plugin_instance_id: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::session::Entity")]
    Session,
    #[sea_orm(has_many = "super::plugin_config::Entity")]
    PluginConfig,
}

impl Related<super::session::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Session.def()
    }
}

impl Related<super::plugin_config::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PluginConfig.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
