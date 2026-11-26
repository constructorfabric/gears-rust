//! The consumption primitives of the storage contract, forwarded to the
//! [`ConsumptionStore`] port with the contract's own signatures.
//!
//! Like the Quota primitives, these are inherent methods; the contract's
//! trait implementation delegates to them.

use quota_enforcement_sdk::{
    Actor, ApplicableQuotas, AppliedMutation, ChargeReturn, EvaluatedDebit, EvaluatedMutation,
    HeldAllocationCharges, IdempotencyRecord, IdempotencyScope, NotificationEvent, PageRequest,
    PageResult, PartialIdempotencyWrite, QuotaId, QuotaSnapshot, RollbackTarget, StorageError,
    TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_security::AccessScope;

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
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        self.consumption
            .apply_debit_plan(actor, scope, mutation, events)
            .await
    }

    /// Evaluate and apply an atomic batch under one envelope key.
    ///
    /// # Errors
    ///
    /// As the contract documents for `apply_batch_debit`.
    pub async fn apply_batch_debit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        batch: &quota_enforcement_sdk::EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        self.consumption
            .apply_batch_debit(actor, scope, batch, events)
            .await
    }

    /// Return consumption to one Quota.
    ///
    /// # Errors
    ///
    /// As the contract documents for `apply_credit`.
    pub async fn apply_credit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.consumption
            .apply_credit(actor, scope, quota_id, amount, idempotency, events)
            .await
    }

    /// Give back part of a charge.
    ///
    /// # Errors
    ///
    /// As the contract documents for `apply_return`.
    pub async fn apply_return(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        ret: &ChargeReturn,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.consumption
            .apply_return(actor, scope, ret, events)
            .await
    }

    /// Give back everything that remains of a charge.
    ///
    /// # Errors
    ///
    /// As the contract documents for `apply_rollback`.
    pub async fn apply_rollback(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        target: &RollbackTarget,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.consumption
            .apply_rollback(actor, scope, target, events)
            .await
    }

    /// Per-Quota state of one applicable set.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    pub async fn read_quota_snapshot(
        &self,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        self.consumption
            .read_quota_snapshot(scope, applicable)
            .await
    }

    /// One page of the per-Quota state of many applicable sets.
    ///
    /// # Errors
    ///
    /// `InvalidCursor` for a cursor this plugin did not issue; `Unavailable`
    /// when the backend cannot answer.
    pub async fn bulk_read_quota_snapshot(
        &self,
        scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
        in_window_only: bool,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError> {
        self.consumption
            .bulk_read_quota_snapshot(scope, pairs, page, in_window_only)
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

    /// Remove charges deleted at `now`.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    pub async fn reclaim_expired_charges(
        &self,
        batch_size: u32,
        now: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        self.consumption
            .reclaim_expired_charges(batch_size, now)
            .await
    }

    /// The held allocation charges past their return window, per metric.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    pub async fn read_held_allocation_charge_stats(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<HeldAllocationCharges>, StorageError> {
        self.consumption
            .read_held_allocation_charge_stats(before)
            .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "consumption_tests.rs"]
mod tests;
