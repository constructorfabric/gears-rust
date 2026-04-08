use async_trait::async_trait;
use mini_chat_sdk::PolicySnapshot;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Provides `PolicySnapshot` resolution for quota operations.
///
/// `QuotaService` depends on this trait — the caching/persistence
/// implementation is a separate concern.
// Used by QuotaService, not yet wired into the turn handler.
#[allow(dead_code)]
#[async_trait]
pub trait PolicySnapshotProvider: Send + Sync {
    /// Get the immutable shared `PolicySnapshot` for a specific version.
    async fn get_snapshot(
        &self,
        user_id: Uuid,
        policy_version: u64,
    ) -> Result<PolicySnapshot, DomainError>;

    /// Get the current policy version for the specified `user_id`.
    async fn get_current_version(&self, user_id: Uuid) -> Result<u64, DomainError>;
}
