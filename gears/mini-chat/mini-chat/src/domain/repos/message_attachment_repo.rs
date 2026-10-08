use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Parameters for inserting a single `message_attachment` row.
#[domain_model]
#[allow(clippy::struct_field_names)]
pub struct InsertMessageAttachmentParams {
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub message_id: Uuid,
    pub attachment_id: Uuid,
}

/// Repository trait for `message_attachments` join-table operations.
#[async_trait]
pub trait MessageAttachmentRepository: Send + Sync {
    async fn insert_batch<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: &[InsertMessageAttachmentParams],
    ) -> Result<(), DomainError>;
    async fn copy_for_retry<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        original_message_id: Uuid,
        new_message_id: Uuid,
        chat_id: Uuid,
    ) -> Result<u64, DomainError>;
    /// Attachment IDs linked to a message, excluding soft-deleted attachments.
    async fn attachment_ids_for_message<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        message_id: Uuid,
    ) -> Result<Vec<Uuid>, DomainError>;
}
