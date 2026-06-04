// @cpt-cf-chat-engine-dbtable-plugin-configs:p1

use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

// Plugin configs are global lookup rows keyed by `(plugin_instance_id,
// session_type_id)` with no per-tenant column. Mark `unrestricted`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "plugin_configs")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub plugin_instance_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_type_id: Uuid,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub config: Option<serde_json::Value>,
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
        on_delete = "Cascade"
    )]
    SessionType,
}

impl Related<super::session_type::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SessionType.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
