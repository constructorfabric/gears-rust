use async_trait::async_trait;
use modkit_db::secure::DBRunner;
use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::ThreadSummaryModel;

/// Repository for thread summary persistence operations.
///
/// P1: No `thread_summaries` table exists yet — always returns `None`.
/// When the summarization job is implemented (P2+), this will query the DB.
pub struct ThreadSummaryRepository;

#[async_trait]
impl crate::domain::repos::ThreadSummaryRepository for ThreadSummaryRepository {
    async fn get_latest<C: DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _chat_id: Uuid,
    ) -> Result<Option<ThreadSummaryModel>, DomainError> {
        // Graceful degradation: no summary table in P1.
        Ok(None)
    }
}
