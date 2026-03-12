use async_trait::async_trait;
use mini_chat_sdk::models::ChatVectorStore;
use modkit_db::secure::DBRunner;
use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Repository trait for vector store persistence operations.
///
/// Implementations are stateless; the caller provides a `DBRunner`
/// (connection or transaction handle) per call.
#[async_trait]
pub trait VectorStoreRepository: Send + Sync {
    /// Attempt to insert a row with `vector_store_id = NULL`.
    /// Returns Ok(store) on success, or Err if unique constraint violated.
    async fn insert_if_absent<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        chat_id: Uuid,
        provider: &str,
    ) -> Result<ChatVectorStore, DomainError>;

    /// Conditional update: SET `vector_store_id` WHERE `vector_store_id` IS NULL.
    /// Returns true if the update succeeded (`rows_affected` == 1).
    async fn set_vector_store_id<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        vector_store_id: &str,
    ) -> Result<bool, DomainError>;

    async fn find_by_chat<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        chat_id: Uuid,
    ) -> Result<Option<ChatVectorStore>, DomainError>;

    async fn increment_file_count<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<(), DomainError>;

    /// Delete row only if `vector_store_id` is still NULL (orphan cleanup).
    async fn delete_if_null<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<u64, DomainError>;
}
