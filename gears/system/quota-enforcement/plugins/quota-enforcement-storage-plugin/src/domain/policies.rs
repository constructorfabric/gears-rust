//! The policy primitives of the storage contract, forwarded to the
//! [`PolicyStore`] port, which already speaks the contract's types.

use quota_enforcement_sdk::{
    NotificationEvent, PageRequest, PageResult, PolicyDraft, PolicyId, PolicyScope, PolicyUpdate,
    PolicyVersion, PolicyVersionMeta, StorageError, TransitionOutcome,
};
use toolkit_security::SecurityContext;

use super::bootstrap::StoragePlugin;

impl StoragePlugin {
    /// Forwarded to [`super::ports::PolicyStore::create_policy`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn create_policy(
        &self,
        ctx: &SecurityContext,
        draft: PolicyDraft,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        self.policies.create_policy(ctx, draft, events).await
    }

    /// Forwarded to [`super::ports::PolicyStore::update_policy`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn update_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        update: PolicyUpdate,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        self.policies
            .update_policy(ctx, policy_id, update, events)
            .await
    }

    /// Forwarded to [`super::ports::PolicyStore::rollback_policy`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        target_version: u32,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError> {
        self.policies
            .rollback_policy(ctx, policy_id, target_version, comment, events)
            .await
    }

    /// Forwarded to [`super::ports::PolicyStore::delete_policy`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn delete_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError> {
        self.policies
            .delete_policy(ctx, policy_id, comment, events)
            .await
    }

    /// Forwarded to [`super::ports::PolicyStore::read_policy`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn read_policy(
        &self,
        scope: &PolicyScope,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        self.policies.read_policy(scope).await
    }

    /// Forwarded to [`super::ports::PolicyStore::read_active_policy_by_id`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn read_active_policy_by_id(
        &self,
        policy_id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        self.policies.read_active_policy_by_id(policy_id).await
    }

    /// Forwarded to [`super::ports::PolicyStore::read_active_policies`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError> {
        self.policies.read_active_policies().await
    }

    /// Forwarded to [`super::ports::PolicyStore::read_policy_version`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn read_policy_version(
        &self,
        policy_id: &PolicyId,
        version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        self.policies.read_policy_version(policy_id, version).await
    }

    /// Forwarded to [`super::ports::PolicyStore::list_policy_versions`].
    ///
    /// # Errors
    ///
    /// As the contract documents.
    pub async fn list_policy_versions(
        &self,
        policy_id: &PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError> {
        self.policies.list_policy_versions(policy_id, page).await
    }
}
