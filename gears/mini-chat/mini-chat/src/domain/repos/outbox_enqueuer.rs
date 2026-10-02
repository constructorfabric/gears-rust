use mini_chat_sdk::UsageEvent;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::model::audit_envelope::AuditEnvelope;

/// Why an outbox enqueue failed.
///
/// The port's own typed error, so it names no `toolkit_db` error and no
/// catch-all `DomainError`; the transport maps the underlying failure into one
/// of these, and `DomainError: From<OutboxError>` lets a service surface it with
/// `?` (payload-too-large as a client validation error, the rest as internal).
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    /// The event could not be serialized to its JSON wire form.
    #[error("failed to serialize {event}")]
    Serialize {
        event: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// The event payload exceeds the outbox column limit — caller-driven, so a
    /// client (4xx) fault rather than a server one.
    #[error("outbox payload is {size} bytes, over the {max}-byte limit")]
    PayloadTooLarge { size: usize, max: usize },
    /// The outbox operation (record build or the transactional insert) failed.
    #[error("outbox enqueue failed: {message}")]
    Enqueue { message: String },
}

impl OutboxError {
    /// Build an [`Enqueue`](Self::Enqueue) from anything printable.
    pub fn enqueue(message: impl Into<String>) -> Self {
        Self::Enqueue {
            message: message.into(),
        }
    }
}

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

/// Domain-owned handle to the outbox wake performed *after* the enclosing
/// transaction commits.
///
/// A thin passthrough over the infra wake token: the caller accumulates the
/// handles of a unit of work with `+=`, then calls [`fire`](Self::fire) once
/// the transaction has committed (or drops it on rollback). Keeping this type
/// domain-owned lets the [`OutboxEnqueuer`] port
/// speak only domain types; the actual wake protocol lives in `toolkit-db`,
/// behind this newtype.
#[derive(Debug)]
#[must_use = "a Wake wakes no sequencer until fired; call .fire() after the transaction commits"]
pub struct Wake(toolkit_db::outbox::Wake);

impl Wake {
    /// A handle that carries no work: the accumulation seed and the value a
    /// zero-enqueue path returns. [`fire`](Self::fire) is then a no-op.
    pub fn empty() -> Self {
        Self(toolkit_db::outbox::Wake::empty())
    }

    /// Wrap a freshly produced infra wake token. Crate-private: only the infra
    /// implementation of [`OutboxEnqueuer`] builds one from a real handle.
    pub(crate) fn from_wake(handle: toolkit_db::outbox::Wake) -> Self {
        Self(handle)
    }

    /// Wake the sequencers for the accumulated work. Call only after the
    /// transaction that produced this handle has committed. On rollback, drop
    /// the handle (or return [`empty`](Self::empty)) instead.
    pub fn fire(self) {
        self.0.fire();
    }
}

impl std::ops::AddAssign for Wake {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl std::ops::Add for Wake {
    type Output = Self;

    fn add(mut self, rhs: Self) -> Self {
        self += rhs;
        self
    }
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
/// - Returns the domain-owned [`OutboxError`] (not `toolkit_db`'s), and a
///   domain-owned [`Wake`] rather than the infra wake token, so the port names
///   no `toolkit_db` error type
///
/// # Implementation note
///
/// The infra implementation (`InfraOutboxEnqueuer`) holds an
/// `Arc<toolkit_db::outbox::Outbox>` and calls `outbox.enqueue(runner, ...)`
/// within the finalization transaction. Each enqueue returns a [`Wake`];
/// the caller accumulates the handles of a unit of work (with `+=`) and calls
/// `.fire()` on the combined handle *after* the transaction commits, which
/// marks the written partitions dirty and wakes the sequencers.
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
    /// Returns a [`Wake`] the caller fires after
    /// the transaction commits. Returns `Err` on database error.
    async fn enqueue_usage_event(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: UsageEvent,
    ) -> Result<Wake, OutboxError>;

    /// Enqueue an attachment cleanup event within the caller's transaction.
    ///
    /// Called during the delete-attachment transaction to schedule async
    /// cleanup of provider-side resources (file deletion, vector store removal).
    async fn enqueue_attachment_cleanup(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: AttachmentCleanupEvent,
    ) -> Result<Wake, OutboxError>;

    /// Enqueue a chat-deletion cleanup event within the caller's transaction.
    ///
    /// Called during the delete-chat transaction to schedule async cleanup
    /// of all provider-side resources (files + vector store) for the soft-deleted chat.
    /// Partitioned by `chat_id` so all cleanup for one chat is serialized.
    async fn enqueue_chat_cleanup(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: ChatCleanupEvent,
    ) -> Result<Wake, OutboxError>;

    /// Enqueue an audit event within the caller's transaction.
    ///
    /// The implementation MUST:
    /// - Serialize `event` to `Vec<u8>` (JSON wire format)
    /// - Insert into the outbox table using the provided `runner` (transaction)
    /// - Use `queue = "mini-chat.audit"`
    /// - Derive the partition from the envelope's `tenant_id`
    ///
    /// Returns a [`Wake`] the caller fires after
    /// the transaction commits. Returns `Err` on database error.
    async fn enqueue_audit_event(
        &self,
        runner: &(dyn DBRunner + Sync),
        event: AuditEnvelope,
    ) -> Result<Wake, OutboxError>;

    /// Enqueue a thread summary task within the caller's transaction.
    ///
    /// Partitioned by `chat_id` so all summary events for one chat are
    /// processed sequentially within the same partition.
    async fn enqueue_thread_summary(
        &self,
        runner: &(dyn DBRunner + Sync),
        payload: ThreadSummaryTaskPayload,
    ) -> Result<Wake, OutboxError>;
}
