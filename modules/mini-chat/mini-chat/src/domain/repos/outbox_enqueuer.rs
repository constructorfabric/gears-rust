use mini_chat_sdk::UsageEvent;
use modkit_db::secure::DBRunner;
use modkit_macros::domain_model;
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::audit_envelope::AuditEnvelope;

/// Payload for attachment cleanup outbox events.
///
/// Enqueued within the delete transaction so cleanup workers can
/// remove provider-side files and vector store entries asynchronously.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct AttachmentCleanupEvent {
    pub event_type: String,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub attachment_id: Uuid,
    pub provider_file_id: Option<String>,
    pub vector_store_id: Option<String>,
    pub storage_backend: String,
    pub attachment_kind: String,
    pub deleted_at: OffsetDateTime,
}

/// Domain-layer abstraction for enqueuing outbox events within a transaction.
///
/// The finalization service calls this trait to insert outbox rows atomically
/// alongside the CAS state transition and quota settlement. The infra layer
/// implements it by delegating to `modkit_db::outbox::Outbox::enqueue()`.
///
/// # Why a trait?
///
/// The `modkit_db::outbox::Outbox` API is partition-based and accepts raw
/// `Vec<u8>` payloads. Mini-Chat needs a domain-oriented interface that:
/// - Accepts typed events (from `mini-chat-sdk`; serialized by the implementation)
/// - Resolves the queue name and partition from tenant context
/// - Participates in the caller's transaction via `&dyn DBRunner`
/// - Returns domain errors, not infra-level `OutboxError`
///
/// # Implementation note
///
/// The infra implementation (`InfraOutboxEnqueuer`) holds an
/// `Arc<modkit_db::outbox::Outbox>` and calls `outbox.enqueue(runner, ...)`
/// within the finalization transaction. The `Outbox::flush()` notification
/// is sent after the transaction commits (by the finalization service).
#[async_trait::async_trait]
pub trait OutboxEnqueuer: Send + Sync {
    /// Enqueue a usage event within the caller's transaction.
    ///
    /// The implementation MUST:
    /// - Serialize `event` to `Vec<u8>` (JSON wire format)
    /// - Insert into the outbox table using the provided `runner` (transaction)
    /// - Use `queue = "mini-chat.usage_snapshot"` (or equivalent registered name)
    /// - Derive the partition from `event.tenant_id`
    ///
    /// Duplicate prevention is handled by the CAS guard in the finalization
    /// transaction — the outbox enqueue is only reached by the CAS winner.
    ///
    /// Returns `Ok(())` on success. Returns `Err` on database error.
    async fn enqueue_usage_event(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: UsageEvent,
    ) -> Result<(), DomainError>;

    /// Enqueue an attachment cleanup event within the caller's transaction.
    ///
    /// Called during the delete-attachment transaction to schedule async
    /// cleanup of provider-side resources (file deletion, vector store removal).
    async fn enqueue_attachment_cleanup(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: AttachmentCleanupEvent,
    ) -> Result<(), DomainError>;

    /// Enqueue an audit event within the caller's transaction.
    ///
    /// The implementation MUST:
    /// - Serialize `event` to `Vec<u8>` (JSON wire format)
    /// - Insert into the outbox table using the provided `runner` (transaction)
    /// - Use `queue = "mini-chat.audit"`
    /// - Derive the partition from the envelope's `tenant_id`
    ///
    /// Returns `Ok(())` on success. Returns `Err` on database error.
    async fn enqueue_audit_event(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: AuditEnvelope,
    ) -> Result<(), DomainError>;

    /// Notify the outbox sequencer that new events are available.
    ///
    /// Called after the transaction that contains enqueue calls commits.
    /// Multiple flush calls coalesce — calling flush 10 times results in at most
    /// one sequencer wakeup.
    ///
    /// This is outbox-wide: it wakes the sequencer for ALL registered queues,
    /// so a single flush call suffices regardless of which queue was written to.
    fn flush(&self);
}
