use modkit_db::secure::DBRunner;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Payload for the attachment processing outbox queue.
///
/// Serialized to JSON and stored in `modkit_outbox_events.payload`.
/// The handler deserializes this, loads the `upload_blob` from the
/// attachments table, and drives the processing pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentProcessingEvent {
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub attachment_id: Uuid,
    pub attachment_kind: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub uploaded_by_user_id: Uuid,
}

/// Domain-layer abstraction for enqueuing attachment processing events.
///
/// Analogous to `OutboxEnqueuer` for usage events. The infra layer
/// implements this by delegating to `modkit_db::outbox::Outbox::enqueue()`.
#[async_trait::async_trait]
pub trait AttachmentOutboxEnqueuer: Send + Sync {
    /// Enqueue an attachment processing event within the caller's transaction.
    async fn enqueue_attachment_processing(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: AttachmentProcessingEvent,
    ) -> Result<(), DomainError>;

    /// Notify the outbox sequencer that new events are available.
    fn flush(&self);
}
