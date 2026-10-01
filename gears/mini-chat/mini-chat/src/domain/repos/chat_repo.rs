use std::collections::HashMap;

use crate::domain::models::Chat;
use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Repository trait for chat persistence operations.
///
/// All methods accept:
/// - `conn: &C` where `C: DBRunner` - database runner (connection or transaction)
/// - `scope: &AccessScope` - security scope prepared by the service layer
#[async_trait]
pub trait ChatRepository: Send + Sync {
    /// Find a chat by ID within the given security scope.
    /// Returns `None` if not found or soft-deleted.
    async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Chat>, DomainError>;

    /// List chats with cursor-based pagination (`updated_at DESC`).
    async fn list_page<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Chat>, DomainError>;

    /// Create a new chat.
    async fn create<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat: Chat,
    ) -> Result<Chat, DomainError>;

    /// Update an existing chat (title + `updated_at` only).
    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat: Chat,
    ) -> Result<Chat, DomainError>;

    /// Record activity on a chat (a message was sent): bumps `updated_at`,
    /// which orders the chat list. Returns `true` if a row was affected.
    async fn touch_activity<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Soft-delete a chat by ID. Returns `true` if a row was affected.
    async fn soft_delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Find a chat by ID with a `SELECT ... FOR UPDATE` lock.
    /// Used to serialize concurrent uploads for per-chat limit enforcement.
    async fn get_for_update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Chat>, DomainError>;

    /// Count non-deleted messages belonging to a chat.
    async fn count_messages<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<i64, DomainError>;

    /// Batch count non-deleted messages for multiple chats.
    async fn count_messages_batch<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, i64>, DomainError>;

    // ── System-scoped methods (background workers) ──────────────────────

    /// Check whether a chat has been soft-deleted (system context, no access scope).
    /// Returns `true` if `deleted_at IS NOT NULL`.
    async fn is_deleted_system<C: DBRunner>(
        &self,
        conn: &C,
        chat_id: Uuid,
    ) -> Result<bool, DomainError>;
}
