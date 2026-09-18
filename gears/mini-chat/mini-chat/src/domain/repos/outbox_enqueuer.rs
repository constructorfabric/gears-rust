use mini_chat_sdk::UsageEvent;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::audit_envelope::AuditEnvelope;

/// Coordinates needed to issue a provider-specific `DELETE` against a
/// secondary upload.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecondaryCleanupRef {
    /// Provider-side file id stored in `attachments.secondary_file_id`.
    pub file_id: String,
    /// Which provider's id is in `file_id` (e.g. `"anthropic"`).
    pub provider_kind: String,
    /// OAGW upstream alias for the secondary provider.
    pub upstream_alias: String,
}

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
    /// `Some` when the chat performed a secondary upload that succeeded
    /// (`secondary_status = uploaded`). The cleanup worker uses this to
    /// issue a provider-specific `DELETE` after the primary delete succeeds.
    #[serde(default)]
    pub secondary_ref: Option<SecondaryCleanupRef>,
}

/// Why provider cleanup was triggered.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CleanupReason {
    /// Chat was explicitly soft-deleted by the user.
    ChatSoftDelete,
}

/// Outcome after recording a cleanup attempt (returned by `record_cleanup_attempt`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// Attachment remains `pending` — retry later.
    StillPending,
    /// Max attempts reached — attachment transitioned to terminal `failed`.
    TerminalFailure,
    /// Attachment was already in a terminal state (`done` or `failed`) — stale
    /// redelivery or concurrent worker already handled it. Not a real failure.
    AlreadyTerminal,
}

/// Payload for chat-level cleanup outbox events.
///
/// Enqueued atomically with the chat soft-delete. The handler iterates
/// pending attachments, deletes provider files, then deletes the vector store.
/// Per DESIGN.md (line 1758) the payload MUST contain at minimum:
/// `tenant_id`, `chat_id`, `system_request_id`, `reason`, `chat_deleted_at`.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCleanupEvent {
    pub reason: CleanupReason,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub system_request_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub chat_deleted_at: OffsetDateTime,
    /// OAGW upstream alias for the chat's secondary-upload provider, when
    /// the chat is backed by one (e.g. Anthropic) and at least one
    /// attachment may have a secondary file id to clean up. `None` for
    /// chats without a secondary provider. Resolved at chat-delete time so
    /// the handler doesn't need a `ProviderResolver` dependency.
    #[serde(default)]
    pub secondary_upstream_alias: Option<String>,
}

/// Durable outbox payload for thread summary generation.
///
/// Persisted at enqueue time in the finalization transaction. The handler
/// reads this payload to know exactly which message range to summarize.
/// `system_request_id` is generated once and reused across retries.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummaryTaskPayload {
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    /// Stable system-task identity -- generated at enqueue, reused across retries.
    pub system_request_id: Uuid,
    pub system_task_type: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub base_frontier_created_at: Option<OffsetDateTime>,
    pub base_frontier_message_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub frozen_target_created_at: OffsetDateTime,
    pub frozen_target_message_id: Uuid,
}

/// Domain-layer abstraction for enqueuing outbox events within a transaction.
///
/// The finalization service calls this trait to insert outbox rows atomically
/// alongside the CAS state transition and quota settlement. The infra layer
/// implements it by delegating to `toolkit_db::outbox::Outbox::enqueue()`.
///
/// # Why a trait?
///
/// The `toolkit_db::outbox::Outbox` API is partition-based and accepts raw
/// `Vec<u8>` payloads. Mini-Chat needs a domain-oriented interface that:
/// - Accepts typed events (from `mini-chat-sdk`; serialized by the implementation)
/// - Resolves the queue name and partition from tenant context
/// - Participates in the caller's transaction via `&dyn DBRunner`
/// - Returns domain errors, not infra-level `OutboxError`
///
/// # Implementation note
///
/// The infra implementation (`InfraOutboxEnqueuer`) holds an
/// `Arc<toolkit_db::outbox::Outbox>` and calls `outbox.enqueue(runner, ...)`
/// within the finalization transaction. Each enqueue returns a
/// [`toolkit_db::outbox::FlushHandle`]; the caller accumulates the handles of a
/// unit of work (with `+=`) and calls `.flush()` on the combined handle *after*
/// the transaction commits, which marks the written partitions dirty and wakes
/// the sequencers.
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
    /// Returns a [`toolkit_db::outbox::FlushHandle`] the caller flushes after
    /// the transaction commits. Returns `Err` on database error.
    async fn enqueue_usage_event(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: UsageEvent,
    ) -> Result<toolkit_db::outbox::FlushHandle, DomainError>;

    /// Enqueue an attachment cleanup event within the caller's transaction.
    ///
    /// Called during the delete-attachment transaction to schedule async
    /// cleanup of provider-side resources (file deletion, vector store removal).
    async fn enqueue_attachment_cleanup(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: AttachmentCleanupEvent,
    ) -> Result<toolkit_db::outbox::FlushHandle, DomainError>;

    /// Enqueue a chat-deletion cleanup event within the caller's transaction.
    ///
    /// Called during the delete-chat transaction to schedule async cleanup
    /// of all provider-side resources (files + vector store) for the soft-deleted chat.
    /// Partitioned by `chat_id` so all cleanup for one chat is serialized.
    async fn enqueue_chat_cleanup(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: ChatCleanupEvent,
    ) -> Result<toolkit_db::outbox::FlushHandle, DomainError>;

    /// Enqueue an audit event within the caller's transaction.
    ///
    /// The implementation MUST:
    /// - Serialize `event` to `Vec<u8>` (JSON wire format)
    /// - Insert into the outbox table using the provided `runner` (transaction)
    /// - Use `queue = "mini-chat.audit"`
    /// - Derive the partition from the envelope's `tenant_id`
    ///
    /// Returns a [`toolkit_db::outbox::FlushHandle`] the caller flushes after
    /// the transaction commits. Returns `Err` on database error.
    async fn enqueue_audit_event(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: AuditEnvelope,
    ) -> Result<toolkit_db::outbox::FlushHandle, DomainError>;

    /// Enqueue a thread summary task within the caller's transaction.
    ///
    /// Partitioned by `chat_id` so all summary events for one chat are
    /// processed sequentially within the same partition.
    async fn enqueue_thread_summary(
        &self,
        runner: &(dyn DBRunner + Sync),
        payload: ThreadSummaryTaskPayload,
    ) -> Result<toolkit_db::outbox::FlushHandle, DomainError>;
}
