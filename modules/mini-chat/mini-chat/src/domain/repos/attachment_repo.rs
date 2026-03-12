use async_trait::async_trait;
use mini_chat_sdk::models::{Attachment, AttachmentStatus};
use modkit_db::secure::DBRunner;
use modkit_security::AccessScope;
use uuid::Uuid;

use modkit_macros::domain_model;

use crate::domain::error::DomainError;

/// Extended attachment that includes `provider_file_id` for internal use only
/// (used by `validate_attachments` and other mini-chat internals; never serialized to API).
#[allow(dead_code)]
#[domain_model]
#[derive(Debug, Clone)]
pub struct AttachmentWithProvider {
    pub attachment: Attachment,
    pub provider_file_id: String,
}

/// Input for inserting a new attachment row.
#[domain_model]
pub struct NewAttachmentEntity {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub uploaded_by_user_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub storage_backend: String,
    pub attachment_kind: String,
    /// File content bytes for crash-safe outbox processing.
    /// Cleared after successful provider upload.
    pub upload_blob: Option<Vec<u8>>,
}

/// Repository trait for attachment persistence operations.
///
/// Implementations are stateless; the caller provides a `DBRunner`
/// (connection or transaction handle) per call.
#[async_trait]
pub trait AttachmentRepository: Send + Sync {
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        entity: NewAttachmentEntity,
    ) -> Result<Attachment, DomainError>;

    async fn find_by_id<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Attachment>, DomainError>;

    async fn update_status<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        status: AttachmentStatus,
        provider_file_id: Option<String>,
        error_code: Option<String>,
    ) -> Result<(), DomainError>;

    async fn update_thumbnail<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        thumbnail: Vec<u8>,
        width: i32,
        height: i32,
    ) -> Result<(), DomainError>;

    #[allow(dead_code)]
    async fn update_doc_summary<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        summary: String,
        model: String,
    ) -> Result<(), DomainError>;

    #[allow(dead_code)]
    async fn mark_for_cleanup<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<u64, DomainError>;

    #[allow(dead_code)]
    async fn find_ready_by_ids<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<AttachmentWithProvider>, DomainError>;

    /// Find all ready document attachments for a chat (for `AllChat` retrieval scope).
    /// Returns documents only (not images), with `provider_file_id` for internal use.
    async fn find_ready_documents_by_chat<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Vec<AttachmentWithProvider>, DomainError>;

    /// Count uploads by a user in the last 24 hours (for daily per-user quota).
    async fn count_uploads_by_user_today<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        user_id: Uuid,
    ) -> Result<u64, DomainError>;

    /// Count document attachments in a chat (for `max_documents_per_chat` enforcement).
    async fn count_documents_by_chat<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<u64, DomainError>;

    /// Sum of `size_bytes` for all document attachments in a chat (for `max_total_upload_mb_per_chat` enforcement).
    async fn total_document_bytes_by_chat<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<u64, DomainError>;

    /// Load the `upload_blob` for an attachment (for outbox-driven processing).
    /// Returns `None` if the blob has already been cleared (idempotent).
    async fn load_upload_blob<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        attachment_id: Uuid,
    ) -> Result<Option<Vec<u8>>, DomainError>;

    /// Clear the `upload_blob` after successful provider upload.
    async fn clear_upload_blob<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        attachment_id: Uuid,
    ) -> Result<(), DomainError>;
}
