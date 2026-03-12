use modkit_db::secure::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "attachments")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
#[allow(clippy::struct_field_names)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub uploaded_by_user_id: Uuid,
    #[sea_orm(column_type = "String(StringLen::N(255))")]
    pub filename: String,
    #[sea_orm(column_type = "String(StringLen::N(128))")]
    pub content_type: String,
    pub size_bytes: i64,
    #[sea_orm(column_type = "String(StringLen::N(32))")]
    pub storage_backend: String,
    #[sea_orm(column_type = "String(StringLen::N(128))", nullable)]
    pub provider_file_id: Option<String>,
    #[sea_orm(column_type = "String(StringLen::N(16))")]
    pub status: String,
    #[sea_orm(column_type = "String(StringLen::N(16))")]
    pub attachment_kind: String,
    #[sea_orm(column_type = "String(StringLen::N(64))", nullable)]
    pub error_code: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub doc_summary: Option<String>,
    #[sea_orm(nullable)]
    pub img_thumbnail: Option<Vec<u8>>,
    pub img_thumbnail_width: Option<i32>,
    pub img_thumbnail_height: Option<i32>,
    #[sea_orm(column_type = "String(StringLen::N(64))", nullable)]
    pub summary_model: Option<String>,
    pub summary_updated_at: Option<OffsetDateTime>,
    /// File content bytes stored temporarily for crash-safe outbox processing.
    /// Cleared (set to NULL) after successful provider upload.
    #[sea_orm(nullable)]
    pub upload_blob: Option<Vec<u8>>,
    #[sea_orm(column_type = "String(StringLen::N(16))", nullable)]
    pub cleanup_status: Option<String>,
    pub cleanup_attempts: i32,
    #[sea_orm(column_type = "Text", nullable)]
    pub last_cleanup_error: Option<String>,
    pub cleanup_updated_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
