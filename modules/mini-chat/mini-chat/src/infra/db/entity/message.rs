use modkit_db::secure::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "messages")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
#[allow(clippy::struct_field_names)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub request_id: Option<Uuid>,
    pub role: MessageRole,
    #[sea_orm(column_type = "Text")]
    pub content: String,
    pub content_type: String,
    pub token_estimate: i32,
    pub provider_response_id: Option<String>,
    pub request_kind: Option<String>,
    #[sea_orm(column_type = "JsonBinary")]
    pub features_used: serde_json::Value,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model: Option<String>,
    pub is_compressed: bool,
    pub created_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum MessageRole {
    #[sea_orm(string_value = "user")]
    User,
    #[sea_orm(string_value = "assistant")]
    Assistant,
    #[sea_orm(string_value = "system")]
    System,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
