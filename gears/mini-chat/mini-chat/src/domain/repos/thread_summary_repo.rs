use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Inclusive summary frontier in the per-chat message order `(created_at ASC, id ASC)`.
///
/// Identifies the last message represented in the summary text.
/// `created_at` alone is insufficient because multiple messages may share the
/// same timestamp; `message_id` is the deterministic UUID tie-breaker.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryFrontier {
    pub created_at: OffsetDateTime,
    pub message_id: Uuid,
}

/// Domain model for a thread summary used in context assembly.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ThreadSummaryModel {
    pub content: String,
    pub frontier: SummaryFrontier,
    pub token_estimate: i32,
}

/// Repository trait for thread summary persistence operations.
#[async_trait]
pub trait ThreadSummaryRepository: Send + Sync {
    /// Fetch the latest thread summary for a chat.
    ///
    /// Returns `None` if no summary exists yet.
    async fn get_latest<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<ThreadSummaryModel>, DomainError>;

    /// CAS-protected upsert: insert or update the summary only if the stored
    /// frontier matches `expected_base_frontier`.
    ///
    /// Returns rows affected: 1 = success, 0 = CAS conflict.
    #[allow(clippy::too_many_arguments)]
    async fn upsert_with_cas<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
        tenant_id: Uuid,
        expected_base_frontier: Option<&SummaryFrontier>,
        new_frontier: &SummaryFrontier,
        summary_text: &str,
        token_estimate: i32,
    ) -> Result<u64, DomainError>;

    /// Delete the chat's summary and clear `is_compressed` on the chat's
    /// messages, so context assembly and the next summary see them again.
    /// Used when a retry, edit or delete changes a turn the summary already
    /// covers. Returns summary rows deleted.
    async fn delete_for_chat<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<u64, DomainError>;
}
