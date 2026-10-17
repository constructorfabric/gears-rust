//! The consumption primitives of the storage contract, forwarded to the
//! [`ConsumptionStore`] port with the contract's own signatures.
//!
//! Like the Quota primitives, these are inherent methods rather than a trait
//! implementation: the `ClientHub` client stays unpublished until every primitive
//! of the contract exists, so leases, batch debit and the bulk snapshot read
//! are still missing.

use quota_enforcement_sdk::{
    ApplicableQuotas, AppliedMutation, EvaluatedDebit, EvaluatedMutation, IdempotencyRecord,
    IdempotencyScope, IdempotencyWrite, NotificationEvent, PartialIdempotencyWrite, QuotaId,
    QuotaSnapshot, RollbackTarget, StorageError, TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};

use super::bootstrap::StoragePlugin;
use super::ports::ConsumptionStore;

impl StoragePlugin {
    /// The consumption store this plugin forwards to.
    #[must_use]
    pub fn consumption_store(&self) -> &dyn ConsumptionStore {
        self.consumption.as_ref()
    }

    /// Evaluate and apply a debit plan atomically with its record and events.
    ///
    /// # Errors
    ///
    /// As the contract documents for `apply_debit_plan`.
    pub async fn apply_debit_plan(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        self.consumption
            .apply_debit_plan(ctx, scope, mutation, events)
            .await
    }

    /// Return consumption to one Quota.
    ///
    /// # Errors
    ///
    /// As the contract documents for `apply_credit`.
    pub async fn apply_credit(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.consumption
            .apply_credit(ctx, scope, quota_id, amount, idempotency, events)
            .await
    }

    /// Reverse a committed debit against its acquisition period.
    ///
    /// # Errors
    ///
    /// As the contract documents for `apply_rollback`.
    pub async fn apply_rollback(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        target: &RollbackTarget,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.consumption
            .apply_rollback(ctx, scope, target, idempotency, events)
            .await
    }

    /// Per-Quota state of one applicable set.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    pub async fn read_quota_snapshot(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        self.consumption
            .read_quota_snapshot(ctx, scope, applicable)
            .await
    }

    /// The unexpired record under `scope_of`, if one exists.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    pub async fn lookup_idempotency(
        &self,
        scope_of: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError> {
        self.consumption.lookup_idempotency(scope_of).await
    }

    /// Reclaim expired idempotency records, oldest first.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    pub async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        self.consumption
            .reclaim_expired_idempotency(batch_size, before)
            .await
    }

    /// Reclaim operation-log rows older than `before`.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    pub async fn reclaim_operation_log(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        self.consumption
            .reclaim_operation_log(batch_size, before)
            .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "consumption_tests.rs"]
mod tests;
