//! Ports the storage plugin's domain depends on. The SQL adapters live in
//! `infra::storage`; the domain never imports them.

use std::collections::HashSet;

use async_trait::async_trait;
use quota_enforcement_sdk::{
    ActiveQuotaCounts, ApplicableQuotas, AppliedMutation, ConfigDefaults, DeactivateOutcome,
    EvaluatedDebit, EvaluatedMutation, IdempotencyRecord, IdempotencyScope, IdempotencyWrite,
    NotificationEvent, PageRequest, PageResult, PartialIdempotencyWrite, PolicyDraft, PolicyId,
    PolicyScope, PolicyUpdate, PolicyVersion, PolicyVersionMeta, ProjectionBinding, Quota,
    QuotaDraft, QuotaFilter, QuotaId, QuotaPatch, QuotaSnapshot, RollbackTarget, StorageError,
    TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

/// What a `seed_defaults` call did.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeedReport {
    /// Rows this call inserted (0 to 3).
    pub inserted: u8,
    /// Rows that already existed.
    pub present: u8,
}

impl SeedReport {
    /// Count one row outcome.
    pub const fn count(&mut self, inserted: bool) {
        if inserted {
            self.inserted += 1;
        } else {
            self.present += 1;
        }
    }
}

/// Failure of a store operation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// A configured default does not fit its column type.
    #[error("configuration default {field}={value} does not fit the column type")]
    DefaultOutOfRange {
        /// The `ConfigDefaults` field.
        field: &'static str,
        /// The offending value.
        value: u64,
    },
    /// The database rejected the call. Detail stays in the adapter's log.
    #[error("database call failed during {operation}")]
    Unavailable {
        /// The store operation that failed.
        operation: &'static str,
    },

    // --- quota lifecycle, one to one with the contract ---
    /// No row with this id lies inside the caller's scope.
    #[error("quota {id} not found")]
    QuotaNotFound {
        /// The requested id.
        id: QuotaId,
    },
    /// The row is deactivated; nothing was written.
    #[error("quota {id} is deactivated")]
    QuotaDeactivated {
        /// The deactivated Quota.
        id: QuotaId,
    },
    /// Invariant I6: the merged cap is below what is already consumed.
    #[error("cap {new_cap} is below the consumed amount {consumed}")]
    CapBelowConsumed {
        /// The cap the patch would set.
        new_cap: u64,
        /// What the counters hold.
        consumed: u64,
    },
    /// Invariant I14: the merged row carries thresholds on an unbounded cap.
    #[error("notification thresholds require a bounded cap")]
    ThresholdsRequireBoundedCap,
    /// The draft's tenant lies outside the authorized scope.
    #[error("subject is outside the authorized scope")]
    SubjectOutOfScope,

    // --- caller input the contract maps to `Internal` ---
    /// The continuation cursor does not decode.
    #[error("malformed continuation cursor")]
    InvalidCursor,
    /// The patch is not one the gear can produce: a `metadata` change without
    /// the contract it was validated against.
    #[error("invalid patch: {detail}")]
    InvalidPatch {
        /// What is missing.
        detail: String,
    },
    /// A filter exceeds the plugin's bounds.
    #[error("invalid filter: {detail}")]
    InvalidFilter {
        /// What was out of bounds.
        detail: String,
    },
    /// A value does not fit its column type.
    #[error("{field}={value} does not fit the column type")]
    ValueOutOfRange {
        /// The field.
        field: &'static str,
        /// The offending value.
        value: String,
    },
    /// A stored row does not read back as the contract type, or a write
    /// the transaction relied on affected no row.
    #[error("storage state is inconsistent during {operation}: {detail}")]
    Corrupt {
        /// The store operation.
        operation: &'static str,
        /// What did not add up.
        detail: String,
    },
}

/// Who performed a mutation, for the operation log.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// The caller's subject id.
    pub subject_id: Uuid,
    /// The caller's subject type, when the security context carries one.
    pub subject_type: Option<String>,
}

/// Schema metadata and the platform-default configuration rows.
#[async_trait]
pub trait FoundationStore: Send + Sync {
    /// The installed contract major, if the schema was ever bootstrapped.
    async fn read_installed_major(&self) -> Result<Option<i32>, StoreError>;

    /// Record `major`. Returns `true` when this call wrote the row and `false`
    /// when a concurrent bootstrap wrote it first.
    async fn record_major(&self, major: i32) -> Result<bool, StoreError>;

    /// Insert the platform-default rows that are missing.
    async fn seed_defaults(&self, defaults: &ConfigDefaults) -> Result<SeedReport, StoreError>;
}

/// The Quota tables: the four lifecycle primitives and the two caller-less
/// platform-plane reads of the storage contract, with the contract's
/// semantics (I1, I6, I11, I14) and the plugin's own bounds. `events` are
/// enqueued in the same transaction as the rows; an event whose `quota_id` is
/// `None` receives the created id.
#[async_trait]
pub trait QuotaStore: Send + Sync {
    /// Insert a Quota, its counter row, one operation-log row, and `events`.
    async fn create_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<QuotaId, StoreError>;

    /// Apply `patch` under the row lock and return the committed row.
    async fn update_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StoreError>;

    /// Flip the row to `deactivated` under the row lock.
    async fn deactivate_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StoreError>;

    /// One page ordered by id ascending; the cursor carries position only.
    async fn read_quotas(
        &self,
        scope: &AccessScope,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<Quota>, StoreError>;

    /// Distinct `(metric, projection_type)` pairs of active Quotas.
    async fn read_active_projection_bindings(
        &self,
    ) -> Result<HashSet<ProjectionBinding>, StoreError>;

    /// Active-Quota counts behind the lifecycle gauges.
    async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StoreError>;
}

/// The consumption primitives: counter mutations, their replay records, and the
/// snapshot read.
///
/// Like [`PolicyStore`], this port speaks the contract's own `StorageError`
/// rather than [`StoreError`]: its failure modes are the contract's, so a
/// translation layer would only restate them.
#[async_trait]
pub trait ConsumptionStore: Send + Sync {
    /// Evaluate the applicable policy against the locked rows and apply the
    /// plan it produced, atomically with the record and the events.
    ///
    /// # Errors
    ///
    /// The contract's variants for `apply_debit_plan`.
    async fn apply_debit_plan(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError>;

    /// Return consumption to one Quota, deriving the idempotency scope under
    /// the row lock.
    ///
    /// # Errors
    ///
    /// The contract's variants for `apply_credit`.
    async fn apply_credit(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError>;

    /// Reverse the committed debit the target names, against its acquisition
    /// period.
    ///
    /// # Errors
    ///
    /// The contract's variants for `apply_rollback`.
    async fn apply_rollback(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        target: &RollbackTarget,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError>;

    /// Per-Quota state of one applicable set, materializing a missing current
    /// period row and nothing else (the I3 exception).
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    async fn read_quota_snapshot(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError>;

    /// The unexpired record under `scope_of`, if one exists.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    async fn lookup_idempotency(
        &self,
        scope_of: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError>;

    /// Delete up to `batch_size` records expired before `before`.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError>;

    /// Delete up to `batch_size` operation-log rows older than `before`.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    async fn reclaim_operation_log(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError>;
}

/// Versioned platform policies. Every mutation is one transaction that moves
/// the version state, the header pointer, the audit row and the outbox entry
/// together, so no reader observes a pointer that disagrees with the states.
///
/// Policy rows are platform-plane: they carry no tenant and no owner, so unlike
/// [`QuotaStore`] these methods take neither an [`AccessScope`] nor an
/// [`Actor`]. Operator authorization happens in the gear, above this port.
#[async_trait]
pub trait PolicyStore: Send + Sync {
    /// Create version 1 at an unoccupied scope.
    ///
    /// # Errors
    /// `PolicyScopeOccupied` when a live policy holds the exact scope; a
    /// backend failure commits no version, audit row or event.
    async fn create_policy(
        &self,
        ctx: &SecurityContext,
        draft: PolicyDraft,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError>;

    /// Create the next version above the high-water mark, under the header lock.
    ///
    /// # Errors
    /// `PolicyNotFound`, `PolicyDeleted`, `VersionConflict`, or a backend
    /// failure. Nothing is written unless the whole transition commits.
    async fn update_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        update: PolicyUpdate,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError>;

    /// Reactivate a retained, non-terminal version.
    ///
    /// # Errors
    /// `PolicyNotFound`, `PolicyDeleted`, `UnknownPolicyVersion`,
    /// `VersionRolledBack`, or a backend failure. Replaying the already-active
    /// target is `NoOp`, not an error, and writes no audit row or event.
    async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        target_version: u32,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError>;

    /// Soft-delete a narrow-scope policy, clearing its pointer and retaining
    /// its history.
    ///
    /// # Errors
    /// `CannotDeleteSeededGlobalPolicy`, `PolicyNotFound`, or a backend
    /// failure. A repeat against an already-deleted policy is `NoOp`.
    async fn delete_policy(
        &self,
        ctx: &SecurityContext,
        policy_id: PolicyId,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError>;

    /// The active version at the exact `scope`, with no fallback to a broader
    /// one: selecting `global` for an unoccupied metric scope is an evaluation
    /// decision, and callers also use this to ask whether a scope is occupied.
    ///
    /// # Errors
    /// A backend or payload-decoding failure. An unoccupied scope is `Ok(None)`.
    async fn read_policy(&self, scope: &PolicyScope)
    -> Result<Option<PolicyVersion>, StorageError>;

    /// The active version of one policy ID.
    ///
    /// # Errors
    /// A backend or payload-decoding failure. A missing or deleted ID is
    /// `Ok(None)`.
    async fn read_active_policy_by_id(
        &self,
        policy_id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError>;

    /// Every active policy, for the bootstrap engine and catalogue scan.
    /// Caller-less by contract: an internal platform read, not a list API.
    ///
    /// # Errors
    /// A backend or payload-decoding failure.
    async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError>;

    /// One retained version, terminal states included.
    ///
    /// # Errors
    /// A backend or payload-decoding failure. A missing version is `Ok(None)`.
    async fn read_policy_version(
        &self,
        policy_id: &PolicyId,
        version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError>;

    /// One bounded page of version history, ascending by version.
    ///
    /// # Errors
    /// `PolicyNotFound` for an ID that was never created, `InvalidCursor` for a
    /// cursor this listing did not issue, or a backend failure.
    async fn list_policy_versions(
        &self,
        policy_id: &PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError>;
}
