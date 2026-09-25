//! The lease primitives of the storage contract, forwarded to the
//! [`LeaseStore`] port with the contract's own signatures.
//!
//! Inherent methods for the reason the consumption primitives are: the
//! `ClientHub` client stays unpublished until every primitive of the contract
//! exists.

use quota_enforcement_sdk::{
    AppliedMutation, EvaluatedLease, EvaluatedMutation, ExpiredLease, LeaseToken, MetricId,
    NotificationEvent, PartialIdempotencyWrite, StorageError, TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};

use super::bootstrap::StoragePlugin;
use super::ports::LeaseStore;

impl StoragePlugin {
    /// The lease store this plugin forwards to.
    #[must_use]
    pub fn lease_store(&self) -> &dyn LeaseStore {
        self.leases.as_ref()
    }

    /// Evaluate the policy under the Quota locks and hold the resulting plan.
    ///
    /// # Errors
    ///
    /// As the contract documents for `acquire_lease`.
    pub async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: std::time::Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        self.leases.acquire_lease(ctx, scope, mutation, ttl).await
    }

    /// Convert an active lease into a debit for what was used.
    ///
    /// # Errors
    ///
    /// As the contract documents for `commit_lease`.
    pub async fn commit_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        token: LeaseToken,
        actual_amount: Option<u64>,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.leases
            .commit_lease(ctx, scope, token, actual_amount, idempotency, events)
            .await
    }

    /// Return an active lease's full held capacity.
    ///
    /// # Errors
    ///
    /// As the contract documents for `release_lease`.
    pub async fn release_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        token: LeaseToken,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.leases
            .release_lease(ctx, scope, token, idempotency, events)
            .await
    }

    /// Transition expired leases and give back what they still hold.
    ///
    /// # Errors
    ///
    /// As the contract documents for `reclaim_expired_leases`.
    pub async fn reclaim_expired_leases(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError> {
        self.leases.reclaim_expired_leases(batch_size, before).await
    }

    /// Expired leases nobody has reclaimed, by metric.
    ///
    /// # Errors
    ///
    /// As the contract documents for `count_expired_unreclaimed_leases`.
    pub async fn count_expired_unreclaimed_leases(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<(MetricId, u64)>, StorageError> {
        self.leases.count_expired_unreclaimed_leases(before).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "leases_tests.rs"]
mod tests;
