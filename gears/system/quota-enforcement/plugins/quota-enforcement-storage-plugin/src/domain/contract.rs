//! `QuotaEnforcementStoragePluginV1` for [`StoragePlugin`]: every primitive
//! delegates to the inherent method that forwards it to its store port, so
//! the contract and the plugin cannot drift apart.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quota_enforcement_sdk::{
    ActiveQuotaCounts, ApplicableQuotas, AppliedMutation, BootstrapBundle, DeactivateOutcome,
    EvaluatedBatch, EvaluatedDebit, EvaluatedLease, EvaluatedMutation, ExpiredLease,
    IdempotencyRecord, IdempotencyScope, IdempotencyWrite, LeaseToken, MetricId,
    NotificationDeliveryHandle, NotificationDeliveryV1, NotificationEvent, PageRequest, PageResult,
    PartialIdempotencyWrite, PolicyDraft, PolicyId, PolicyScope, PolicyUpdate, PolicyVersion,
    PolicyVersionMeta, ProjectionBinding, Quota, QuotaDraft, QuotaEnforcementStoragePluginV1,
    QuotaFilter, QuotaId, QuotaPatch, QuotaSnapshot, RollbackTarget, StorageError,
    TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};

use super::bootstrap::StoragePlugin;

// @cpt-dod:cpt-cf-quota-enforcement-dod-sdk-contracts:p1
#[async_trait]
impl QuotaEnforcementStoragePluginV1 for StoragePlugin {
    async fn bootstrap(&self, bundle: &BootstrapBundle) -> Result<(), StorageError> {
        StoragePlugin::bootstrap(self, bundle)
            .await
            .map(|_seeded| ())
    }

    async fn read_active_projection_bindings(
        &self,
    ) -> Result<HashSet<ProjectionBinding>, StorageError> {
        StoragePlugin::read_active_projection_bindings(self).await
    }

    async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StorageError> {
        StoragePlugin::read_active_quota_counts(self).await
    }

    async fn create_quota(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<QuotaId, StorageError> {
        StoragePlugin::create_quota(self, ctx, scope, draft, events).await
    }

    async fn update_quota(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StorageError> {
        StoragePlugin::update_quota(self, ctx, scope, quota_id, patch, events).await
    }

    async fn deactivate_quota(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StorageError> {
        StoragePlugin::deactivate_quota(self, ctx, scope, quota_id, events).await
    }

    async fn read_quotas(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<Quota>, StorageError> {
        StoragePlugin::read_quotas(self, ctx, scope, filter, page).await
    }

    async fn apply_debit_plan(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        StoragePlugin::apply_debit_plan(self, ctx, scope, mutation, events).await
    }

    async fn apply_batch_debit(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        batch: &EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        StoragePlugin::apply_batch_debit(self, ctx, scope, batch, events).await
    }

    async fn apply_credit(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::apply_credit(self, ctx, scope, quota_id, amount, idempotency, events).await
    }

    async fn apply_rollback(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        target: &RollbackTarget,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::apply_rollback(self, ctx, scope, target, idempotency, events).await
    }

    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        StoragePlugin::acquire_lease(self, ctx, scope, mutation, ttl).await
    }

    async fn commit_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        token: LeaseToken,
        actual_amount: Option<u64>,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::commit_lease(self, ctx, scope, token, actual_amount, idempotency, events)
            .await
    }

    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        token: LeaseToken,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::release_lease(self, ctx, scope, token, idempotency, events).await
    }

    async fn read_quota_snapshot(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        StoragePlugin::read_quota_snapshot(self, ctx, scope, applicable).await
    }

    async fn bulk_read_quota_snapshot(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError> {
        StoragePlugin::bulk_read_quota_snapshot(self, ctx, scope, pairs, page).await
    }

    async fn create_policy(
        &self,
        ctx: &SecurityContext,
        draft: PolicyDraft,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        StoragePlugin::create_policy(self, ctx, draft, events).await
    }

    async fn update_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        update: PolicyUpdate,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        StoragePlugin::update_policy(self, ctx, policy_id, update, events).await
    }

    async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        target_version: u32,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError> {
        StoragePlugin::rollback_policy(self, ctx, policy_id, target_version, comment, events).await
    }

    async fn delete_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError> {
        StoragePlugin::delete_policy(self, ctx, policy_id, comment, events).await
    }

    async fn read_policy(
        &self,
        scope: &PolicyScope,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        StoragePlugin::read_policy(self, scope).await
    }

    async fn read_active_policy_by_id(
        &self,
        policy_id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        StoragePlugin::read_active_policy_by_id(self, policy_id).await
    }

    async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError> {
        StoragePlugin::read_active_policies(self).await
    }

    async fn read_policy_version(
        &self,
        policy_id: &PolicyId,
        version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        StoragePlugin::read_policy_version(self, policy_id, version).await
    }

    async fn list_policy_versions(
        &self,
        policy_id: &PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError> {
        StoragePlugin::list_policy_versions(self, policy_id, page).await
    }

    async fn lookup_idempotency(
        &self,
        scope: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError> {
        StoragePlugin::lookup_idempotency(self, scope).await
    }

    async fn start_notification_delivery(
        &self,
        delivery: Arc<dyn NotificationDeliveryV1>,
    ) -> Result<Box<dyn NotificationDeliveryHandle>, StorageError> {
        StoragePlugin::start_notification_delivery(self, delivery).await
    }

    async fn count_expired_unreclaimed_leases(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<(MetricId, u64)>, StorageError> {
        StoragePlugin::count_expired_unreclaimed_leases(self, before).await
    }

    async fn reclaim_expired_leases(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError> {
        StoragePlugin::reclaim_expired_leases(self, batch_size, before).await
    }

    async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        StoragePlugin::reclaim_expired_idempotency(self, batch_size, before).await
    }

    async fn reclaim_operation_log(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        StoragePlugin::reclaim_operation_log(self, batch_size, before).await
    }
}
