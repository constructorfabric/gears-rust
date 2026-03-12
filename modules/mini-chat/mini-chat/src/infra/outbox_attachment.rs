//! Outbox handler for attachment processing.
//!
//! Replaces the mpsc-channel-based `AttachmentWorker` with a crash-safe
//! outbox pattern. The handler is registered as a `MessageHandler` on the
//! `attachment_processing` outbox queue and performs the same work:
//! provider upload, thumbnail generation, vector store add, doc summary.

use std::sync::Arc;

use async_trait::async_trait;
use modkit_db::outbox::Outbox;
use modkit_db::outbox::{HandlerResult, MessageHandler, OutboxMessage};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::domain::error::DomainError;
use crate::domain::repos::attachment_outbox::AttachmentProcessingEvent;
use crate::domain::repos::{AttachmentRepository, VectorStoreRepository};
use crate::infra::workers::AttachmentWorker;

/// Outbox handler that processes attachment uploads.
///
/// Deserializes `AttachmentProcessingEvent` from the outbox payload,
/// loads the `upload_blob` from the DB, and delegates to the existing
/// `AttachmentWorker` processing logic.
pub struct AttachmentProcessingHandler<
    A: AttachmentRepository + 'static,
    V: VectorStoreRepository + 'static,
> {
    pub(crate) worker: Arc<AttachmentWorker<A, V>>,
}

#[async_trait]
impl<A: AttachmentRepository + 'static, V: VectorStoreRepository + 'static> MessageHandler
    for AttachmentProcessingHandler<A, V>
{
    async fn handle(&self, msg: &OutboxMessage, _cancel: CancellationToken) -> HandlerResult {
        let event = match serde_json::from_slice::<AttachmentProcessingEvent>(&msg.payload) {
            Ok(e) => e,
            Err(e) => {
                error!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    payload_len = msg.payload.len(),
                    "attachment processing event deserialization failed: {e}"
                );
                return HandlerResult::Reject {
                    reason: format!("deserialization failed: {e}"),
                };
            }
        };

        info!(
            attachment_id = %event.attachment_id,
            chat_id = %event.chat_id,
            kind = %event.attachment_kind,
            filename = %event.filename,
            partition_id = msg.partition_id,
            seq = msg.seq,
            "processing attachment via outbox"
        );

        match self.worker.process_from_outbox(&event).await {
            Ok(()) => {
                info!(
                    attachment_id = %event.attachment_id,
                    "attachment outbox processing completed"
                );
                HandlerResult::Success
            }
            Err(e) => {
                // Transient errors get retried; the attachment stays in its current state
                // until the outbox retries. The worker already marks the attachment as
                // failed in the DB for permanent failures (e.g., provider upload rejected).
                warn!(
                    attachment_id = %event.attachment_id,
                    error = %e,
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    "attachment outbox processing failed - will retry"
                );
                HandlerResult::Retry {
                    reason: format!("processing failed: {e}"),
                }
            }
        }
    }
}

/// Infrastructure implementation of `AttachmentOutboxEnqueuer`.
///
/// Serializes `AttachmentProcessingEvent` to JSON and inserts into the
/// outbox table within the caller's transaction.
pub struct InfraAttachmentOutboxEnqueuer {
    outbox: Arc<Outbox>,
    queue_name: String,
    num_partitions: u32,
}

impl InfraAttachmentOutboxEnqueuer {
    pub(crate) fn new(outbox: Arc<Outbox>, queue_name: String, num_partitions: u32) -> Self {
        Self {
            outbox,
            queue_name,
            num_partitions,
        }
    }

    fn partition_for(&self, tenant_id: uuid::Uuid) -> u32 {
        let hash = tenant_id.as_u128();
        #[allow(clippy::cast_possible_truncation)]
        {
            (hash % u128::from(self.num_partitions)) as u32
        }
    }
}

#[async_trait]
impl crate::domain::repos::attachment_outbox::AttachmentOutboxEnqueuer
    for InfraAttachmentOutboxEnqueuer
{
    async fn enqueue_attachment_processing(
        &self,
        runner: &(dyn modkit_db::secure::DBRunner + Sync),
        event: AttachmentProcessingEvent,
    ) -> Result<(), DomainError> {
        let partition = self.partition_for(event.tenant_id);
        let payload = serde_json::to_vec(&event).map_err(|e| {
            DomainError::internal(format!("serialize AttachmentProcessingEvent: {e}"))
        })?;

        self.outbox
            .enqueue(
                runner,
                &self.queue_name,
                partition,
                payload,
                "application/json",
            )
            .await
            .map_err(|e| DomainError::internal(format!("outbox enqueue: {e}")))?;

        info!(
            queue = %self.queue_name,
            partition,
            attachment_id = %event.attachment_id,
            tenant_id = %event.tenant_id,
            "attachment processing event enqueued"
        );

        Ok(())
    }

    fn flush(&self) {
        self.outbox.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modkit_db::outbox::{HandlerResult, MessageHandler, OutboxMessage};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    fn make_outbox_message(payload: Vec<u8>) -> OutboxMessage {
        OutboxMessage {
            partition_id: 0,
            seq: 1,
            payload,
            payload_type: "application/json".to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0,
        }
    }

    fn make_valid_event() -> AttachmentProcessingEvent {
        AttachmentProcessingEvent {
            tenant_id: Uuid::new_v4(),
            chat_id: Uuid::new_v4(),
            attachment_id: Uuid::new_v4(),
            attachment_kind: "document".to_owned(),
            filename: "test.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            size_bytes: 1024,
            uploaded_by_user_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn event_serialization_roundtrip() {
        let event = make_valid_event();
        let bytes = serde_json::to_vec(&event).unwrap();
        let decoded: AttachmentProcessingEvent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.attachment_id, event.attachment_id);
        assert_eq!(decoded.tenant_id, event.tenant_id);
        assert_eq!(decoded.attachment_kind, "document");
    }

    #[test]
    fn event_deserialization_rejects_invalid_json() {
        let result = serde_json::from_slice::<AttachmentProcessingEvent>(b"not json");
        assert!(result.is_err());
    }

    /// Test partition computation directly (same hash function as `InfraAttachmentOutboxEnqueuer`).
    fn compute_partition(tenant_id: Uuid, num_partitions: u32) -> u32 {
        let hash = tenant_id.as_u128();
        #[allow(clippy::cast_possible_truncation)]
        {
            (hash % u128::from(num_partitions)) as u32
        }
    }

    #[test]
    fn partition_deterministic() {
        let tenant_id = Uuid::new_v4();
        let p1 = compute_partition(tenant_id, 4);
        let p2 = compute_partition(tenant_id, 4);
        assert_eq!(p1, p2);
    }

    #[test]
    fn partition_in_range() {
        for num_partitions in [1, 2, 4, 8, 16] {
            for _ in 0..50 {
                let tenant_id = Uuid::new_v4();
                let p = compute_partition(tenant_id, num_partitions);
                assert!(
                    p < num_partitions,
                    "partition {p} >= num_partitions {num_partitions}"
                );
            }
        }
    }
}
