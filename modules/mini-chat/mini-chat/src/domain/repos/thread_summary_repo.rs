use async_trait::async_trait;
use modkit_db::secure::DBRunner;
use modkit_macros::domain_model;
use modkit_security::AccessScope;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Domain model for a thread summary used in context assembly.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ThreadSummaryModel {
    pub content: String,
    pub boundary_message_id: Uuid,
    pub boundary_created_at: OffsetDateTime,
}

/// Repository trait for thread summary persistence operations.
#[async_trait]
pub trait ThreadSummaryRepository: Send + Sync {
    /// Fetch the latest thread summary for a chat.
    ///
    /// Returns `None` if no summary exists (graceful degradation for P1).
    async fn get_latest<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<ThreadSummaryModel>, DomainError>;
}
