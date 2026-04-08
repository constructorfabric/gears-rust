use modkit_db::secure::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

use super::attachment::Entity as AttachmentEntity;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "message_attachments")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub chat_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub message_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub attachment_id: Uuid,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "AttachmentEntity",
        from = "Column::AttachmentId",
        to = "super::attachment::Column::Id"
    )]
    Attachment,
}

impl Related<AttachmentEntity> for Entity {
    fn to() -> RelationDef {
        Relation::Attachment.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
