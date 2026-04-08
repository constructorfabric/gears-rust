use std::collections::HashMap;

use async_trait::async_trait;
use modkit_db::secure::DBRunner;
use modkit_macros::domain_model;
use modkit_security::AccessScope;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::AttachmentSummary;
use crate::infra::db::entity::message::Model as MessageModel;

/// Snapshot boundary for deterministic context assembly.
///
/// Computed once before the current user message is persisted.
/// Context queries MUST include only messages where
/// `(created_at, id) <= (boundary_created_at, boundary_id)`.
///
/// See DESIGN.md `§ContextPlan` Determinism and Snapshot Boundary (P1).
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct SnapshotBoundary {
    pub created_at: OffsetDateTime,
    pub id: Uuid,
}

/// Parameters for inserting a user message.
#[domain_model]
pub struct InsertUserMessageParams {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub request_id: Uuid,
    pub content: String,
}

/// Parameters for inserting an assistant message.
#[domain_model]
pub struct InsertAssistantMessageParams {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub request_id: Uuid,
    pub content: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub model: Option<String>,
    pub provider_response_id: Option<String>,
}

/// Repository trait for message persistence operations.
#[async_trait]
#[allow(dead_code, clippy::too_many_arguments)]
pub trait MessageRepository: Send + Sync {
    /// INSERT a user message linked to a turn's `request_id`.
    async fn insert_user_message<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: InsertUserMessageParams,
    ) -> Result<MessageModel, DomainError>;

    /// INSERT an assistant message with usage data. Returns the message model
    /// (caller uses `model.id` to set `chat_turns.assistant_message_id`).
    async fn insert_assistant_message<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: InsertAssistantMessageParams,
    ) -> Result<MessageModel, DomainError>;

    /// SELECT the user-role message for a given `(chat_id, request_id)`.
    /// Used by retry/edit to retrieve the original user message content.
    async fn find_user_message_by_request_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        request_id: Uuid,
    ) -> Result<Option<MessageModel>, DomainError>;

    /// SELECT messages for a turn by `(chat_id, request_id)` where not deleted.
    async fn find_by_chat_and_request_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        request_id: Uuid,
    ) -> Result<Vec<MessageModel>, DomainError>;

    /// SELECT a single message by `(id, chat_id)` where not deleted.
    async fn get_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        msg_id: Uuid,
        chat_id: Uuid,
    ) -> Result<Option<MessageModel>, DomainError>;

    /// List messages for a chat with cursor pagination + `OData` filter/sort.
    /// Only returns messages with `request_id` IS NOT NULL and `deleted_at` IS NULL.
    async fn list_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        query: &modkit_odata::ODataQuery,
    ) -> Result<modkit_odata::Page<MessageModel>, DomainError>;

    /// Batch-fetch attachment summaries for the given message IDs (single query).
    /// Returns a map from `message_id` to its `AttachmentSummary` list.
    async fn batch_attachment_summaries<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        message_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<AttachmentSummary>>, DomainError>;

    /// Fetch the snapshot boundary: the latest message's `(created_at, id)` in the chat.
    ///
    /// Returns `None` if the chat has no messages yet. Must be called BEFORE
    /// persisting the current user message to establish the deterministic boundary.
    async fn snapshot_boundary<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<SnapshotBoundary>, DomainError>;

    /// Fetch the most recent K messages for context assembly.
    ///
    /// Returns messages where `(created_at, id) <= snapshot_boundary` (if provided),
    /// `deleted_at IS NULL`, and `request_id IS NOT NULL`,
    /// ordered chronologically (oldest first). The query fetches DESC then reverses.
    async fn recent_for_context<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        limit: u32,
        boundary: Option<SnapshotBoundary>,
    ) -> Result<Vec<MessageModel>, DomainError>;

    /// Soft-delete all messages belonging to a turn identified by `(chat_id, request_id)`.
    ///
    /// Sets `deleted_at = now()` on every message where `chat_id` and `request_id` match
    /// and `deleted_at IS NULL`. Used during retry, edit, and delete mutations to ensure
    /// old messages disappear from active conversation history.
    async fn soft_delete_by_request_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        request_id: Uuid,
    ) -> Result<u64, DomainError>;

    /// Fetch recent messages after a thread summary boundary for context assembly.
    ///
    /// Same as [`recent_for_context`] but only returns messages with
    /// `(created_at, id) > (lower_created_at, lower_id)` AND
    /// `(created_at, id) <= snapshot_boundary` (if provided).
    async fn recent_after_boundary<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        lower_created_at: time::OffsetDateTime,
        lower_id: Uuid,
        limit: u32,
        boundary: Option<SnapshotBoundary>,
    ) -> Result<Vec<MessageModel>, DomainError>;
}
