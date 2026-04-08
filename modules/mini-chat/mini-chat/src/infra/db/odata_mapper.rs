use modkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};
use modkit_odata::filter::{FieldKind, FilterField};

use crate::infra::db::entity::chat::{Column, Entity, Model};
use crate::infra::db::entity::message::{
    Column as MsgColumn, Entity as MsgEntity, Model as MsgModel,
};

/// Cursor/sort/filter field enum for chat pagination.
///
/// Pagination uses `updated_at DESC` + `id` tiebreaker.
/// Filtering supports `contains(title, '...')` for chat title search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatCursorField {
    UpdatedAt,
    Id,
    Title,
}

impl FilterField for ChatCursorField {
    const FIELDS: &'static [Self] = &[Self::UpdatedAt, Self::Id, Self::Title];

    fn name(&self) -> &'static str {
        match self {
            Self::UpdatedAt => "updated_at",
            Self::Id => "id",
            Self::Title => "title",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::UpdatedAt => FieldKind::DateTimeUtc,
            Self::Id => FieldKind::Uuid,
            Self::Title => FieldKind::String,
        }
    }
}

pub struct ChatODataMapper;

impl FieldToColumn<ChatCursorField> for ChatODataMapper {
    type Column = Column;

    fn map_field(field: ChatCursorField) -> Column {
        match field {
            ChatCursorField::UpdatedAt => Column::UpdatedAt,
            ChatCursorField::Id => Column::Id,
            ChatCursorField::Title => Column::Title,
        }
    }
}

impl ODataFieldMapping<ChatCursorField> for ChatODataMapper {
    type Entity = Entity;

    fn extract_cursor_value(model: &Model, field: ChatCursorField) -> sea_orm::Value {
        match field {
            ChatCursorField::UpdatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(Box::new(model.updated_at)))
            }
            ChatCursorField::Id => sea_orm::Value::Uuid(Some(Box::new(model.id))),
            ChatCursorField::Title => {
                sea_orm::Value::String(model.title.as_ref().map(|s| Box::new(s.clone())))
            }
        }
    }
}

/// Cursor/sort field enum for message pagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageField {
    CreatedAt,
    Id,
    Role,
}

impl FilterField for MessageField {
    const FIELDS: &'static [Self] = &[Self::CreatedAt, Self::Id, Self::Role];

    fn name(&self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at",
            Self::Id => "id",
            Self::Role => "role",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::CreatedAt => FieldKind::DateTimeUtc,
            Self::Id => FieldKind::Uuid,
            Self::Role => FieldKind::String,
        }
    }
}

pub struct MessageODataMapper;

impl FieldToColumn<MessageField> for MessageODataMapper {
    type Column = MsgColumn;

    fn map_field(field: MessageField) -> MsgColumn {
        match field {
            MessageField::CreatedAt => MsgColumn::CreatedAt,
            MessageField::Id => MsgColumn::Id,
            MessageField::Role => MsgColumn::Role,
        }
    }
}

impl ODataFieldMapping<MessageField> for MessageODataMapper {
    type Entity = MsgEntity;

    fn extract_cursor_value(model: &MsgModel, field: MessageField) -> sea_orm::Value {
        match field {
            MessageField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(Box::new(model.created_at)))
            }
            MessageField::Id => sea_orm::Value::Uuid(Some(Box::new(model.id))),
            MessageField::Role => {
                let s = match model.role {
                    crate::infra::db::entity::message::MessageRole::User => "user",
                    crate::infra::db::entity::message::MessageRole::Assistant => "assistant",
                    crate::infra::db::entity::message::MessageRole::System => "system",
                };
                sea_orm::Value::String(Some(Box::new(s.to_owned())))
            }
        }
    }
}
