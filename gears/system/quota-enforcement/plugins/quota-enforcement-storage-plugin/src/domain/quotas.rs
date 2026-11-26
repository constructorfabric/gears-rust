//! The Quota primitives of the storage contract, forwarded to the
//! [`QuotaStore`] port with the contract's signatures. The
//! `impl QuotaEnforcementStoragePluginV1 for StoragePlugin` delegates here.
//!
//! The actor is what the operation log records; the access scope is what
//! admission returned, re-applied by `SecureORM` on every row.

use quota_enforcement_sdk::{
    ActiveQuotaCounts, BulkCreateEnvelope, BulkCreated, BulkDeactivateEnvelope, BulkDeactivated,
    BulkUpdateEnvelope, BulkUpdated, DeactivateOutcome, NotificationEvent, PageRequest, PageResult,
    Quota, QuotaDraft, QuotaFilter, QuotaId, QuotaPatch, StorageError, TransitionOutcome,
};
use toolkit_security::AccessScope;

use super::bootstrap::{FromStore, StoragePlugin};
use super::ports::{Actor, QuotaStore};

impl StoragePlugin {
    /// The Quota store this plugin forwards to.
    #[must_use]
    pub fn quota_store(&self) -> &dyn QuotaStore {
        self.quotas.as_ref()
    }

    /// Persist a new Quota and enqueue `events` in the same transaction.
    ///
    /// # Errors
    ///
    /// As the contract documents: `SubjectOutOfScope`, `Unavailable`, and
    /// `Internal` for a draft that does not fit its columns.
    pub async fn create_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<QuotaId, StorageError> {
        self.quotas
            .create_quota(actor, scope, draft, events)
            .await
            .map_err(StorageError::from_store)
    }

    /// Apply `patch` under the row lock and return the committed row.
    ///
    /// # Errors
    ///
    /// `QuotaNotFound`, `QuotaDeactivated`, `CapBelowConsumed` (I6),
    /// `ThresholdsRequireBoundedCap` (I14), `Unavailable`.
    pub async fn update_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StorageError> {
        self.quotas
            .update_quota(actor, scope, quota_id, patch, events)
            .await
            .map_err(StorageError::from_store)
    }

    /// Deactivate a Quota. The lease cascade slot is filled by the
    /// lease-operations feature; until then no lease is resolved.
    ///
    /// # Errors
    ///
    /// `QuotaNotFound`, `QuotaDeactivated`, `Unavailable`.
    pub async fn deactivate_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StorageError> {
        self.quotas
            .deactivate_quota(actor, scope, quota_id, events)
            .await
            .map_err(StorageError::from_store)
    }

    /// Create every draft of `envelope` in one transaction, or none.
    ///
    /// # Errors
    ///
    /// As the contract documents for `bulk_create_quotas`.
    pub async fn bulk_create_quotas(
        &self,
        actor: &Actor,
        envelope: &BulkCreateEnvelope,
    ) -> Result<TransitionOutcome<BulkCreated>, StorageError> {
        self.quotas
            .bulk_create_quotas(actor, envelope)
            .await
            .map_err(StorageError::from_store)
    }

    /// Apply every patch of `envelope` in one transaction, or none.
    ///
    /// # Errors
    ///
    /// As the contract documents for `bulk_update_quotas`.
    pub async fn bulk_update_quotas(
        &self,
        actor: &Actor,
        envelope: &BulkUpdateEnvelope,
    ) -> Result<TransitionOutcome<BulkUpdated>, StorageError> {
        self.quotas
            .bulk_update_quotas(actor, envelope)
            .await
            .map_err(StorageError::from_store)
    }

    /// Deactivate every Quota of `envelope` in one transaction, or none.
    ///
    /// # Errors
    ///
    /// As the contract documents for `bulk_deactivate_quotas`.
    pub async fn bulk_deactivate_quotas(
        &self,
        actor: &Actor,
        envelope: &BulkDeactivateEnvelope,
    ) -> Result<TransitionOutcome<BulkDeactivated>, StorageError> {
        self.quotas
            .bulk_deactivate_quotas(actor, envelope)
            .await
            .map_err(StorageError::from_store)
    }

    /// One page of Quotas inside `scope`, ordered by id ascending.
    ///
    /// # Errors
    ///
    /// `InvalidCursor` for a malformed cursor; `Unavailable`; `Internal` for
    /// an over-long id filter.
    pub async fn read_quotas(
        &self,
        scope: &AccessScope,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<Quota>, StorageError> {
        self.quotas
            .read_quotas(scope, filter, page)
            .await
            .map_err(StorageError::from_store)
    }

    /// Active-Quota counts behind the lifecycle gauges.
    ///
    /// # Errors
    ///
    /// `Unavailable`; `Internal` for a row that does not read back.
    pub async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StorageError> {
        self.quotas
            .read_active_quota_counts()
            .await
            .map_err(StorageError::from_store)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "quotas_tests.rs"]
mod quotas_tests;
