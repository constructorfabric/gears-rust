use async_trait::async_trait;
use mini_chat_sdk::UserLimits;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Provides per-user credit allocations for quota operations.
///
/// `QuotaService` depends on this trait — the caching/persistence
/// implementation is a separate concern.
// Used by QuotaService, not yet wired into the turn handler.
#[allow(dead_code)]
#[async_trait]
pub trait UserLimitsProvider: Send + Sync {
    /// Get per-user credit allocations for a specific policy version.
    async fn get_limits(
        &self,
        user_id: Uuid,
        policy_version: u64,
    ) -> Result<UserLimits, DomainError>;
}
