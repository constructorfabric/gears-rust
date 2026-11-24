//! `QuotaEnforcementStoragePluginV1` for [`StoragePlugin`]: every primitive
//! delegates to the inherent method that forwards it to its store port, so
//! the contract and the plugin cannot drift apart.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quota_enforcement_sdk::{
    ActiveQuotaCounts, Actor, ApplicableQuotas, AppliedMutation, BootstrapBundle, ChargeReturn,
    DeactivateOutcome, EvaluatedBatch, EvaluatedDebit, EvaluatedLease, EvaluatedMutation,
    ExpiredLease, HeldAllocationCharges, IdempotencyRecord, IdempotencyScope, LeaseToken, MetricId,
    NotificationDeliveryHandle, NotificationDeliveryV1, NotificationEvent, PageRequest, PageResult,
    PartialIdempotencyWrite, PolicyDraft, PolicyId, PolicyScope, PolicyUpdate, PolicyVersion,
    PolicyVersionMeta, Quota, QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaFilter, QuotaId,
    QuotaPatch, QuotaSnapshot, RollbackTarget, StorageError, TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_security::AccessScope;

use super::bootstrap::StoragePlugin;

// @cpt-dod:cpt-cf-quota-enforcement-dod-sdk-contracts:p1
#[async_trait]
impl QuotaEnforcementStoragePluginV1 for StoragePlugin {
    #[tracing::instrument(name = "quota_enforcement.storage.bootstrap", skip_all)]
    async fn bootstrap(&self, bundle: &BootstrapBundle) -> Result<(), StorageError> {
        StoragePlugin::bootstrap(self, bundle)
            .await
            .map(|_seeded| ())
    }

    async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StorageError> {
        StoragePlugin::read_active_quota_counts(self).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.create_quota", skip_all)]
    async fn create_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<QuotaId, StorageError> {
        StoragePlugin::create_quota(self, actor, scope, draft, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.update_quota", skip_all)]
    async fn update_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StorageError> {
        StoragePlugin::update_quota(self, actor, scope, quota_id, patch, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.deactivate_quota", skip_all)]
    async fn deactivate_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StorageError> {
        StoragePlugin::deactivate_quota(self, actor, scope, quota_id, events).await
    }

    async fn read_quotas(
        &self,
        scope: &AccessScope,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<Quota>, StorageError> {
        StoragePlugin::read_quotas(self, scope, filter, page).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.bulk_create_quotas", skip_all, fields(item_count = envelope.items.len()))]
    async fn bulk_create_quotas(
        &self,
        actor: &Actor,
        envelope: &quota_enforcement_sdk::BulkCreateEnvelope,
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::BulkCreated>, StorageError> {
        StoragePlugin::bulk_create_quotas(self, actor, envelope).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.bulk_update_quotas", skip_all, fields(item_count = envelope.items.len()))]
    async fn bulk_update_quotas(
        &self,
        actor: &Actor,
        envelope: &quota_enforcement_sdk::BulkUpdateEnvelope,
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::BulkUpdated>, StorageError> {
        StoragePlugin::bulk_update_quotas(self, actor, envelope).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.bulk_deactivate_quotas", skip_all, fields(item_count = envelope.items.len()))]
    async fn bulk_deactivate_quotas(
        &self,
        actor: &Actor,
        envelope: &quota_enforcement_sdk::BulkDeactivateEnvelope,
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::BulkDeactivated>, StorageError> {
        StoragePlugin::bulk_deactivate_quotas(self, actor, envelope).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.apply_debit_plan", skip_all)]
    async fn apply_debit_plan(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        StoragePlugin::apply_debit_plan(self, actor, scope, mutation, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.apply_batch_debit", skip_all, fields(item_count = batch.items.len()))]
    async fn apply_batch_debit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        batch: &EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        StoragePlugin::apply_batch_debit(self, actor, scope, batch, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.apply_credit", skip_all)]
    async fn apply_credit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::apply_credit(self, actor, scope, quota_id, amount, idempotency, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.apply_return", skip_all)]
    async fn apply_return(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        ret: &ChargeReturn,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::apply_return(self, actor, scope, ret, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.apply_rollback", skip_all)]
    async fn apply_rollback(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        target: &RollbackTarget,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::apply_rollback(self, actor, scope, target, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.acquire_lease", skip_all)]
    async fn acquire_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        StoragePlugin::acquire_lease(self, actor, scope, mutation, ttl).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.commit_lease", skip_all)]
    async fn commit_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        actual_amount: Option<u64>,
        metric: Option<&MetricId>,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::commit_lease(
            self,
            actor,
            scope,
            token,
            actual_amount,
            metric,
            idempotency,
            events,
        )
        .await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.release_lease", skip_all)]
    async fn release_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        StoragePlugin::release_lease(self, actor, scope, token, idempotency, events).await
    }

    async fn read_quota_snapshot(
        &self,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        StoragePlugin::read_quota_snapshot(self, scope, applicable).await
    }

    async fn bulk_read_quota_snapshot(
        &self,
        scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
        in_window_only: bool,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError> {
        StoragePlugin::bulk_read_quota_snapshot(self, scope, pairs, page, in_window_only).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.create_policy", skip_all)]
    async fn create_policy(
        &self,
        actor: &Actor,
        draft: PolicyDraft,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        StoragePlugin::create_policy(self, actor, draft, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.update_policy", skip_all)]
    async fn update_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        update: PolicyUpdate,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        StoragePlugin::update_policy(self, actor, policy_id, update, events).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.rollback_policy", skip_all)]
    async fn rollback_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        target_version: u32,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError> {
        StoragePlugin::rollback_policy(self, actor, policy_id, target_version, comment, events)
            .await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.delete_policy", skip_all)]
    async fn delete_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError> {
        StoragePlugin::delete_policy(self, actor, policy_id, comment, events).await
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

    #[tracing::instrument(name = "quota_enforcement.storage.reclaim_expired_leases", skip_all)]
    async fn reclaim_expired_leases(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError> {
        StoragePlugin::reclaim_expired_leases(self, batch_size, before).await
    }

    #[tracing::instrument(
        name = "quota_enforcement.storage.reclaim_expired_idempotency",
        skip_all
    )]
    async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        StoragePlugin::reclaim_expired_idempotency(self, batch_size, before).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.reclaim_operation_log", skip_all)]
    async fn reclaim_operation_log(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        StoragePlugin::reclaim_operation_log(self, batch_size, before).await
    }

    #[tracing::instrument(name = "quota_enforcement.storage.reclaim_expired_charges", skip_all)]
    async fn reclaim_expired_charges(
        &self,
        batch_size: u32,
        now: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        StoragePlugin::reclaim_expired_charges(self, batch_size, now).await
    }

    async fn read_held_allocation_charge_stats(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<HeldAllocationCharges>, StorageError> {
        StoragePlugin::read_held_allocation_charge_stats(self, before).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "contract_tests.rs"]
mod contract_tests;
