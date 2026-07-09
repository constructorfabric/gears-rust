// @cpt-cf-chat-engine-dbtable-link-citations:p2
// @cpt-cf-chat-engine-design-entity-link-citation:p2
//
// Web-page citations attached to a `text` message_part. The full plugin-
// supplied `LinkCitation` payload is stored verbatim in `content` (JSONB);
// CASCADE FK so deleting a part removes its citations.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "link_citations")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub message_part_id: Uuid,
    #[sea_orm(column_type = "JsonBinary")]
    pub content: serde_json::Value,
    pub number: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::message_part::Entity",
        from = "Column::MessagePartId",
        to = "super::message_part::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Part,
}

impl Related<super::message_part::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Part.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
