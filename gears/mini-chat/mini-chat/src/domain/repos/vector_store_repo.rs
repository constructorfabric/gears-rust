use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::db::entity::chat_vector_store::Model as VectorStoreModel;

/// Parameters for inserting a new `chat_vector_stores` row with `vector_store_id = NULL`.
#[domain_model]
pub struct InsertVectorStoreParams {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub provider: String,
}

/// Repository trait for chat vector store persistence operations.
///
/// Supports the insert-first CAS protocol for get-or-create:
/// 1. `insert` (may fail with unique violation on (`tenant_id`, `chat_id`))
/// 2. `cas_set_vector_store_id` (winner sets the provider ID)
/// 3. `find_by_chat` (loser polls until `vector_store_id` is non-NULL)
#[async_trait]
pub trait VectorStoreRepository: Send + Sync {
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: InsertVectorStoreParams,
    ) -> Result<VectorStoreModel, DomainError>;
    async fn cas_set_vector_store_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        vector_store_id: &str,
    ) -> Result<u64, DomainError>;
    async fn find_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<VectorStoreModel>, DomainError>;
    /// Best-effort delete a placeholder row by ID.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<u64, DomainError>;
    /// Delete a placeholder row only if it still has no `vector_store_id`
    /// and was created at or before `cutoff`. Returns the rows deleted;
    /// 0 means the creator finished or the row is not stale.
    async fn delete_stale_placeholder<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        cutoff: time::OffsetDateTime,
    ) -> Result<u64, DomainError>;
}
