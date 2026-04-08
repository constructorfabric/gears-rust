use modkit_db::secure::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "attachments")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub uploaded_by_user_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub storage_backend: String,
    pub provider_file_id: Option<String>,
    pub status: AttachmentStatus,
    pub error_code: Option<String>,
    pub attachment_kind: AttachmentKind,
    pub for_file_search: bool,
    pub for_code_interpreter: bool,
    #[sea_orm(column_type = "Text")]
    pub doc_summary: Option<String>,
    pub img_thumbnail: Option<Vec<u8>>,
    pub img_thumbnail_width: Option<i32>,
    pub img_thumbnail_height: Option<i32>,
    #[allow(clippy::struct_field_names)]
    pub summary_model: Option<String>,
    pub summary_updated_at: Option<OffsetDateTime>,
    pub cleanup_status: Option<String>,
    pub cleanup_attempts: i32,
    #[sea_orm(column_type = "Text")]
    pub last_cleanup_error: Option<String>,
    pub cleanup_updated_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
}

/// Attachment status lifecycle: pending → uploaded → ready | failed.
/// CAS guards enforce valid transitions.
#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum AttachmentStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "uploaded")]
    Uploaded,
    #[sea_orm(string_value = "ready")]
    Ready,
    #[sea_orm(string_value = "failed")]
    Failed,
}

impl std::fmt::Display for AttachmentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Uploaded => write!(f, "uploaded"),
            Self::Ready => write!(f, "ready"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

impl AttachmentStatus {
    /// Returns `true` if the status is terminal (ready or failed — no further transitions).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Ready | Self::Failed)
    }

    /// Returns `true` if the status is transient (pending or uploaded — still in progress).
    #[must_use]
    pub fn is_transient(&self) -> bool {
        !self.is_terminal()
    }
}

/// Classification of attachment content.
#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum AttachmentKind {
    #[sea_orm(string_value = "document")]
    Document,
    #[sea_orm(string_value = "image")]
    Image,
}

impl std::fmt::Display for AttachmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document => write!(f, "document"),
            Self::Image => write!(f, "image"),
        }
    }
}

impl From<crate::domain::mime_validation::AttachmentKind> for AttachmentKind {
    fn from(k: crate::domain::mime_validation::AttachmentKind) -> Self {
        match k {
            crate::domain::mime_validation::AttachmentKind::Document => Self::Document,
            crate::domain::mime_validation::AttachmentKind::Image => Self::Image,
        }
    }
}

impl From<AttachmentKind> for crate::domain::mime_validation::AttachmentKind {
    fn from(k: AttachmentKind) -> Self {
        match k {
            AttachmentKind::Document => Self::Document,
            AttachmentKind::Image => Self::Image,
        }
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
