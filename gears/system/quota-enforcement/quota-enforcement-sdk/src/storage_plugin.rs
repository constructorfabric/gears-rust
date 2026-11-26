//! Storage plugin contract: [`QuotaEnforcementStoragePluginV1`].
//!
//! This trait and its closed [`StorageError`] enum form QE's persistence
//! boundary. Locking, indexing, isolation, and table layout are plugin details.
//!
//! # Invariants every implementation upholds
//!
//! - **I1 Atomicity.** Counter changes, charges, idempotency, outbox events,
//!   and the operation log commit in one transaction.
//! - **I2 Idempotency.** A key is scoped by `(tenant, operation, key)`.
//!   Replay returns the original outcome; a different payload under the same
//!   scope, other subjects included, returns
//!   [`StorageError::IdempotencyPayloadMismatch`]. The charge of an allowed
//!   debit or lease commit answers for it for as long as the charge lives.
//! - **I3 Read-only.** Reads do not mutate state, and snapshot reads take no
//!   row lock. A consumption Quota with no row for its current window reads
//!   as zero there; the next mutating operation creates the row and closes
//!   the elapsed period.
//! - **I4-I5 Lease periods.** Expired leases are treated as released; lease
//!   settlement and rollback target the acquisition period.
//! - **I6-I8 Bounds.** Cap updates, active-lease limits, and contention
//!   timeouts are enforced under the relevant row locks.
//! - **I9-I11 Consistency.** Mutations serialize in acquisition order, become
//!   visible within tenant scope, and commit events in the same transaction.
//! - **I12-I14 Schema and rows.** Schema majors must match; new period rows
//!   reset threshold markers; notification thresholds require a bounded cap.
//!
//! # In-transaction evaluation
//!
//! Debit and lease methods select and evaluate policy under the row locks. A
//! missing artifact rolls back with [`StorageError::PreparationRequired`] so
//! the caller can compile outside the transaction and retry.
//!
//! Every tenant-scoped call receives the [`AccessScope`] that admission
//! returned, unmodified, and binds it through `SecureConn`. No scoped
//! operation runs without it. Every mutation that serves a request also
//! receives the [`Actor`] the operation log and its events name.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_security::AccessScope;

use crate::engine::{EvaluationFailure, EvaluationLimits, TransactionEvaluator};
use crate::models::{
    ActiveQuotaCounts, Actor, ApplicableQuotas, AppliedMutation, AttributionDigest, BatchDebitItem,
    BootstrapBundle, ChargeReturn, ConfigDefaults, DeactivateOutcome, EvaluatedDebit,
    EvaluatedLease, ExpiredLease, HeldAllocationCharges, IdempotencyRecord, IdempotencyScope,
    IdempotencyWrite, LeaseToken, MetricId, NotificationEvent, PageRequest, PageResult,
    PartialIdempotencyWrite, PolicyDraft, PolicyId, PolicyScope, PolicyUpdate, PolicyVersion,
    PolicyVersionMeta, Quota, QuotaDraft, QuotaFilter, QuotaId, QuotaPatch, QuotaSnapshot,
    RollbackTarget, TransitionOutcome,
};

/// Major version of this contract. Coupled to the gear's major version. A
/// storage plugin that implements another major is not supported (I12).
pub const CONTRACT_MAJOR: u32 = 1;

impl BootstrapBundle {
    /// Foundation bundle: schema check against [`CONTRACT_MAJOR`] and default
    /// configuration rows only. Defined here, not in `models`, because it is
    /// the one model constructor bound to this contract's version.
    #[must_use]
    pub fn foundation() -> Self {
        Self {
            contract_major: CONTRACT_MAJOR,
            config_defaults: ConfigDefaults::default(),
            global_policy: None,
        }
    }
}

/// Closed error set of [`QuotaEnforcementStoragePluginV1`].
///
/// Variants are grouped by concern. The gear lifts every variant 1:1 into its
/// domain error, with two exceptions: `QuotaNotFound` becomes a generic
/// not-found and `SubjectOutOfScope` becomes a PDP denial.
/// `SchemaVersionMismatch` never surfaces at runtime; `bootstrap()` fails fast.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    // --- operations ---
    /// No charge answers a rollback or a return: none under its scope, a
    /// deleted one, or, for a rollback, one with another attribution digest.
    #[error("no charge is registered under the requested key")]
    OperationNotFound {
        /// The original idempotency key the caller named.
        key: String,
    },
    /// A return asks for more than remains of its charge.
    #[error("return of {requested} exceeds the {remaining} that remains of the charge")]
    ReturnExceedsCharge {
        /// The amount the return asked for.
        requested: u64,
        /// What remains of the charge.
        remaining: u64,
    },
    /// No entry the amount reaches can take a part of it: each is on a rate
    /// Quota, or on a consumption Quota outside the return window or in a
    /// settled period.
    #[error("no quota of the charge can take the returned amount")]
    ReturnNotApplicable,
    /// A return names a metric type other than its charge's.
    #[error("metric type {requested} is not the charge's {charged}")]
    MetricTypeMismatch {
        /// The metric type the request named.
        requested: String,
        /// The metric type of the charge.
        charged: String,
    },
    /// No lease with this token exists inside the caller's authorized scope.
    /// A token belonging to another tenant is reported the same way, so the
    /// error reveals nothing about tokens the caller may not address.
    #[error("no lease {token} exists")]
    LeaseNotFound {
        /// The lease token.
        token: LeaseToken,
    },
    /// Commit or release against a lease that exists but is expired or already
    /// resolved.
    #[error("lease {token} is not active")]
    LeaseNotActive {
        /// The lease token.
        token: LeaseToken,
    },
    /// The per-`(tenant, metric)` active-lease cap would be exceeded (I7).
    #[error("active-lease cap reached for the (tenant, metric) pair")]
    LeaseInflightLimitExceeded,
    /// The acquisition contention timeout elapsed (I8).
    #[error("acquisition contention timeout elapsed")]
    LeaseContentionTimeout,
    /// A batch's evaluation outlasted the batch-level timeout. The batch was
    /// rolled back and nothing was written.
    #[error("batch evaluation exceeded the batch timeout")]
    BatchTimeout,
    /// Commit amount exceeds the reserved amount.
    #[error("commit amount {actual} exceeds reserved amount {reserved}")]
    OverCommitNotAuthorized {
        /// Reserved at acquisition.
        reserved: u64,
        /// Requested at commit.
        actual: u64,
    },

    // --- idempotency and versioning ---
    /// Same idempotency scope, different payload (I2).
    #[error("idempotency key replayed with a different payload")]
    IdempotencyPayloadMismatch,
    /// Optimistic-concurrency version mismatch.
    #[error("version conflict: expected {expected}, found {actual}")]
    VersionConflict {
        /// Version the caller expected.
        expected: u32,
        /// Version found in storage.
        actual: u32,
    },
    /// An active policy already occupies the exact scope.
    #[error("policy scope already occupied: {scope:?}")]
    PolicyScopeOccupied {
        /// Occupied scope.
        scope: PolicyScope,
    },
    /// The policy identifier was never created.
    #[error("policy {policy_id} not found")]
    PolicyNotFound {
        /// Missing policy identifier.
        policy_id: PolicyId,
    },
    /// The policy was soft-deleted and cannot be reactivated.
    #[error("policy {policy_id} is deleted")]
    PolicyDeleted {
        /// Deleted policy identifier.
        policy_id: PolicyId,
    },
    /// The global fallback policy cannot be deleted.
    #[error("cannot delete seeded global policy")]
    CannotDeleteSeededGlobalPolicy,
    /// The named policy version does not exist.
    #[error("unknown version {version} of policy {policy_id}")]
    UnknownPolicyVersion {
        /// The policy.
        policy_id: PolicyId,
        /// The missing version.
        version: u32,
    },
    /// Rollback target is in the terminal `rolled_back` state.
    #[error("version {version} of policy {policy_id} was rolled back")]
    VersionRolledBack {
        /// The policy.
        policy_id: PolicyId,
        /// The rolled-back version.
        version: u32,
    },

    // --- quota lifecycle ---
    /// A cap reduction below the current consumed amount (I6).
    #[error("cap {new_cap} is below the consumed amount {consumed}")]
    CapBelowConsumed {
        /// Requested cap.
        new_cap: u64,
        /// Current consumed or in-flight amount.
        consumed: u64,
    },
    /// No Quota with this identifier in the caller's scope.
    #[error("quota {id} not found")]
    QuotaNotFound {
        /// The identifier.
        id: QuotaId,
    },
    /// The Quota is deactivated.
    #[error("quota {id} is deactivated")]
    QuotaDeactivated {
        /// The identifier.
        id: QuotaId,
    },
    /// The merged row would carry notification thresholds with an unbounded
    /// cap (I14).
    #[error("notification thresholds require a bounded cap")]
    ThresholdsRequireBoundedCap,
    /// The target period is closed.
    #[error("the target period is closed")]
    PeriodClosed,

    // --- metric and subject types ---
    /// The metric is not registered.
    #[error("metric {metric} is not registered")]
    MetricNotRegistered {
        /// The metric identifier.
        metric: String,
    },
    /// The metric is registered but not quota-gated.
    #[error("metric {metric} is not quota-gated")]
    MetricNotQuotaGated {
        /// The metric identifier.
        metric: String,
    },
    /// The subject type is not registered.
    #[error("subject type {projection} is not registered")]
    ProjectionNotRegistered {
        /// The subject type identifier.
        projection: String,
    },

    // --- post-PDP defense in depth ---
    /// A subject outside the authorized scope reached storage.
    #[error("subject is outside the authorized scope")]
    SubjectOutOfScope,

    // --- caller input ---
    /// The continuation cursor of a page request does not decode. Cursors are
    /// opaque to callers, so this is caller input, not a backend failure.
    #[error("malformed continuation cursor")]
    InvalidCursor,

    // --- in-transaction evaluation ---
    /// The policy the transaction selected has no resident compiled artifact.
    /// Nothing was written and the transaction rolled back: compile the named
    /// version outside any transaction, publish it, and call again. The retry
    /// budget is the caller's (`preparation_max_attempts`).
    #[error("policy {policy_id} version {version} must be prepared before evaluation")]
    PreparationRequired {
        /// Policy the transaction selected.
        policy_id: PolicyId,
        /// Version it selected.
        version: u32,
    },
    /// The engine refused, or its decision violated a shared plan invariant.
    /// The transaction rolled back; no counter moved and no record was kept.
    #[error("evaluation by engine `{engine_id}` failed: {failure}")]
    EvaluationFailed {
        /// Engine of the policy the transaction selected.
        engine_id: String,
        /// The closed engine or invariant failure, for the caller to lift.
        failure: EvaluationFailure,
    },

    // --- operational ---
    /// Transport or backend reachability failure.
    #[error("storage backend unavailable: {0}")]
    Unavailable(String),
    /// Installed schema major differs from the contract major (I12).
    #[error("installed schema major {installed} does not match contract major {expected}")]
    SchemaVersionMismatch {
        /// Major found in the schema metadata.
        installed: u32,
        /// Major the gear was compiled against.
        expected: u32,
    },
    /// Last-resort opaque failure.
    #[error("storage plugin internal error: {0}")]
    Internal(String),

    // --- bulk envelopes ---
    /// One item of a bulk envelope failed; the whole envelope rolled back.
    /// Only item-specific failures are wrapped, an item key that is already
    /// taken among them: a failure of the envelope as a whole (the backend, a
    /// mismatch of the envelope's own key, contention) is raised as is.
    #[error("bulk item {index}: {cause}")]
    BulkItem {
        /// Position of the failing item in the request.
        index: usize,
        /// What failed, as the single-item primitive reports it.
        cause: Box<Self>,
    },
}

impl StorageError {
    /// `self` as the failure of the bulk envelope's item at `index`.
    #[must_use]
    pub fn at_item(self, index: usize) -> Self {
        Self::BulkItem {
            index,
            cause: Box::new(self),
        }
    }
}

/// One mutation the plugin evaluates inside its own transaction.
///
/// The caller supplies the request-shaped half of the environment and the
/// bounds its configuration fixes. Everything the decision depends on —
/// which policy applies, what the counters currently are — is read by the
/// transaction under its own locks, so no decision can be computed against a
/// state the mutation does not then apply to.
pub struct EvaluatedMutation<'a> {
    /// Admitted subject set of the operation.
    pub applicable: &'a ApplicableQuotas,
    /// Requested amount.
    pub amount: u64,
    /// The validated `metadata` of the request's metric object.
    pub request: &'a serde_json::Value,
    /// The `id` of the request's metric object, for the Engine.
    pub metric_id: Option<&'a str>,
    /// The operator clamp this deployment loaded. The budget is resolved from
    /// it against the policy the transaction selects, never before: the caller
    /// does not know which version that will be, and an activation between the
    /// two would otherwise apply the previous version's requested timeout.
    pub limits: EvaluationLimits,
    /// Idempotency key and payload digest. The charge or record written under
    /// it carries the decision this transaction produced.
    pub idempotency: &'a IdempotencyWrite,
    /// Digest of the authorized attribution ([`AttributionDigest::of`]). The
    /// charge of an allowed debit keeps it, and so does a lease, for the
    /// charge of its commit.
    pub authorized: AttributionDigest,
    /// Synchronous, side-effect-free evaluation callback. It performs no I/O,
    /// holds no database handle, and may be retried after preparation.
    pub evaluate: Arc<TransactionEvaluator>,
}

/// The batch-level evaluation timeout: one deadline for every attempt of one
/// batch.
///
/// It is armed by the first attempt that holds its locks, not when the call
/// starts: lock waits are the contention budget's (I8), and this bounds the
/// evaluation. A later attempt (a contention retry, a preparation retry)
/// finds it already armed and gets only what is left.
#[derive(Debug)]
pub struct BatchTimer {
    timeout: Duration,
    armed: std::sync::OnceLock<std::time::Instant>,
}

impl BatchTimer {
    /// A timer of `timeout`, not yet armed.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            armed: std::sync::OnceLock::new(),
        }
    }

    /// Arm the timer if no attempt has, and return what remains: zero once
    /// the deadline has passed.
    #[must_use]
    pub fn arm(&self) -> Duration {
        let armed = *self.armed.get_or_init(std::time::Instant::now);
        (armed + self.timeout).saturating_duration_since(std::time::Instant::now())
    }

    /// What remains of an armed timer; the whole timeout while unarmed.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.armed.get().map_or(self.timeout, |armed| {
            (*armed + self.timeout).saturating_duration_since(std::time::Instant::now())
        })
    }

    /// What remains once the timer is armed; `None` while it is not. A wait
    /// outside the transaction (a lock retry's pause, a preparation) is
    /// bounded by this, so no attempt outlives the deadline.
    #[must_use]
    pub fn armed_remaining(&self) -> Option<Duration> {
        self.armed.get().map(|armed| {
            (*armed + self.timeout).saturating_duration_since(std::time::Instant::now())
        })
    }

    /// Whether the timer was armed and its deadline has passed.
    #[must_use]
    pub fn expired(&self) -> bool {
        self.armed_remaining().is_some_and(|left| left.is_zero())
    }
}

/// One item of an atomic batch together with the scope its own authorization
/// produced.
pub struct BatchEntry<'a> {
    /// The item.
    pub item: &'a BatchDebitItem,
    /// The item's admitted scope. Its Quotas are discovered, read and written
    /// under this scope, never another item's.
    pub scope: &'a AccessScope,
}

/// An atomic batch under one envelope key. Every item is evaluated, in order,
/// against the counters every earlier allowed item would leave; the union of
/// the plans is applied only when every item is allowed, and a denied batch
/// writes only its record. Every item names the envelope's tenant.
pub struct EvaluatedBatch<'a> {
    /// Envelope idempotency key and payload digest.
    pub envelope: &'a IdempotencyWrite,
    /// The items, each with its own scope.
    pub items: &'a [BatchEntry<'a>],
    /// The cost limit each item's engine runs under. The time limit is the
    /// batch timer's remainder, which supersedes the per-policy timeout.
    pub limits: EvaluationLimits,
    /// Synchronous, side-effect-free evaluation callback.
    pub evaluate: Arc<TransactionEvaluator>,
    /// The batch-level timeout, shared by every attempt; the transaction
    /// that arms it may run on another task, hence the `Arc`.
    pub timer: Arc<BatchTimer>,
}

/// Persistence contract for Quotas, counters, leases, policies, idempotency,
/// and the operation log.
// @cpt-dod:cpt-cf-quota-enforcement-dod-sdk-contracts:p1
#[async_trait]
pub trait QuotaEnforcementStoragePluginV1: Send + Sync + 'static {
    // --- lifecycle ---

    /// Idempotent bootstrap: verify the schema major (I12) and seed the default
    /// configuration rows and, when present, the global policy.
    ///
    /// # Errors
    ///
    /// - [`StorageError::SchemaVersionMismatch`] when the installed schema major
    ///   differs from `bundle.contract_major`. The gear fails fast.
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    async fn bootstrap(&self, bundle: &BootstrapBundle) -> Result<(), StorageError>;

    /// Returns active-Quota counts for lifecycle gauges: zero cap, unbounded
    /// cap, and per metric. Validity windows do not affect the counts.
    ///
    /// # Errors
    ///
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StorageError>;

    // --- quota CRUD ---

    /// Persist a new Quota and enqueue `events` in the same transaction (I1,
    /// I11). The row starts `active` with `record_version = 1`. An event
    /// whose `quota_id` is `None` receives the assigned id, since the caller
    /// cannot know it before the call. The draft's `tenant_id` must lie inside
    /// `scope`.
    ///
    /// # Errors
    ///
    /// - [`StorageError::SubjectOutOfScope`] when the draft's tenant is outside
    ///   `scope`.
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    async fn create_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<QuotaId, StorageError>;

    /// Apply `patch` under a row lock and return the committed row (I1, I6,
    /// I11, I14). Every accepted patch increments `record_version` once and
    /// sets `updated_at`.
    ///
    /// # Errors
    ///
    /// - [`StorageError::QuotaNotFound`] when no row with this id lies inside
    ///   `scope`.
    /// - [`StorageError::QuotaDeactivated`] when the row is deactivated;
    ///   nothing is written.
    /// - [`StorageError::CapBelowConsumed`] (I6) and
    ///   [`StorageError::ThresholdsRequireBoundedCap`] (I14), both evaluated on
    ///   the merged row inside the transaction.
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    async fn update_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StorageError>;

    /// Deactivate a Quota and resolve its active leases atomically (I1, I11).
    /// The plugin constructs and enqueues one `lease-resolved-by-deactivation`
    /// event per resolved lease itself; callers pass only the `quota-changed`
    /// event, since only the transaction knows the leases. Leases past their
    /// expiry are already released (I4) and are not resolved again.
    ///
    /// # Errors
    ///
    /// - [`StorageError::QuotaNotFound`] when no row with this id lies inside
    ///   `scope`.
    /// - [`StorageError::QuotaDeactivated`] when the row is already
    ///   deactivated; no second cascade runs and nothing is written.
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    async fn deactivate_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StorageError>;

    /// Read Quotas within the caller's scope, ordered by `quota_id` ascending
    /// (`UUIDv7`, creation order). The cursor is opaque and carries position
    /// only: tenant and PDP scope are re-applied on every page, so a cursor
    /// never grants access. `page.limit` may be clamped to the plugin's
    /// maximum. Deactivated Quotas remain readable.
    ///
    /// # Errors
    ///
    /// - [`StorageError::InvalidCursor`] for a malformed `page.cursor`.
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    /// - [`StorageError::Internal`] for an over-long `filter.ids`.
    async fn read_quotas(
        &self,
        scope: &AccessScope,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<Quota>, StorageError>;

    // --- bulk quota CRUD ---

    /// Create every draft of `envelope` in one transaction, or none (PRD
    /// section 5.2). Each item has the effects of
    /// [`QuotaEnforcementStoragePluginV1::create_quota`]; the envelope's
    /// idempotency record, holding the returned outcome, commits with them. A
    /// replay under the same scope and digest returns the stored outcome as
    /// `NoOp` and writes nothing, once the envelope tenant is found inside
    /// every item's current scope. The record expires after the longest
    /// idempotency retention configured for any item's metric.
    ///
    /// # Errors
    ///
    /// - [`StorageError::BulkItem`] wrapping the single-item error of the
    ///   first failing item in submission order.
    /// - [`StorageError::IdempotencyPayloadMismatch`] for the key under a
    ///   different payload (I2).
    /// - [`StorageError::LeaseContentionTimeout`] when a concurrent writer of
    ///   the envelope's scope holds it past the contention budget.
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    async fn bulk_create_quotas(
        &self,
        actor: &Actor,
        envelope: &crate::bulk::BulkCreateEnvelope,
    ) -> Result<TransitionOutcome<crate::bulk::BulkCreated>, StorageError>;

    /// Apply every patch of `envelope` in one transaction, or none. Each item
    /// has the effects and the under-lock guards of
    /// [`QuotaEnforcementStoragePluginV1::update_quota`]. Every target row is
    /// locked in ascending `quota_id` order before any item runs; items then
    /// run in submission order. A Quota outside its item's scope or of another
    /// tenant than the envelope's is not found. Replay as for
    /// [`QuotaEnforcementStoragePluginV1::bulk_create_quotas`], once every
    /// target is visible under its item's current scope.
    ///
    /// # Errors
    ///
    /// As [`QuotaEnforcementStoragePluginV1::bulk_create_quotas`]; the wrapped
    /// item errors are those of `update_quota`.
    async fn bulk_update_quotas(
        &self,
        actor: &Actor,
        envelope: &crate::bulk::BulkUpdateEnvelope,
    ) -> Result<TransitionOutcome<crate::bulk::BulkUpdated>, StorageError>;

    /// Deactivate every Quota of `envelope` in one transaction, or none,
    /// resolving the active leases of each as
    /// [`QuotaEnforcementStoragePluginV1::deactivate_quota`] does; every
    /// lease-resolution event commits with the envelope. Locking, ordering and
    /// replay as for [`QuotaEnforcementStoragePluginV1::bulk_update_quotas`].
    ///
    /// # Errors
    ///
    /// As [`QuotaEnforcementStoragePluginV1::bulk_create_quotas`]; the wrapped
    /// item errors are those of `deactivate_quota`.
    async fn bulk_deactivate_quotas(
        &self,
        actor: &Actor,
        envelope: &crate::bulk::BulkDeactivateEnvelope,
    ) -> Result<TransitionOutcome<crate::bulk::BulkDeactivated>, StorageError>;

    // --- counter mutation ---

    /// Selects and evaluates the applicable policy under the Quota row locks,
    /// then applies its plan atomically (I1, I2, I9).
    ///
    /// # Errors
    ///
    /// - [`StorageError::PreparationRequired`] when the selected version has no
    ///   resident artifact. Nothing was written; prepare and call again.
    /// - [`StorageError::EvaluationFailed`] when the engine refused or its
    ///   decision violated a plan invariant. Nothing was written.
    /// - [`StorageError::IdempotencyPayloadMismatch`] for a replay with a
    ///   different payload (I2).
    /// - [`StorageError::QuotaDeactivated`] when a planned Quota is inactive.
    /// - [`StorageError::LeaseContentionTimeout`] when the wait on a contended
    ///   row exceeds the metric's budget (I8). The seeded default is 50 ms; with
    ///   0 ms a concurrent writer on the same row is refused at once; nothing was
    ///   written and the call is safe to retry.
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    ///
    /// # Replay and retention
    ///
    /// A replay returns `NoOp` with the stored [`crate::Decision`], retention
    /// deadline, and an empty [`crate::MutationResult`].
    ///
    /// An allowed debit writes a charge, which is its record: the request
    /// fingerprint, the attribution digest, the decision, the metric, the
    /// amount, and per plan entry its chain, position, Quota, period, and
    /// charged amount. Its retention deadline is the end of its return window.
    /// A denial writes an idempotency record, except
    /// [`crate::NO_APPLICABLE_QUOTA`], which must be re-evaluated after
    /// provisioning changes.
    async fn apply_debit_plan(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError>;

    /// Evaluate and apply every item of an atomic batch under one envelope key.
    /// A failure on any item leaves the whole batch unwritten; a denied batch
    /// records its decisions and moves no counter. The record expires after
    /// the longest idempotency retention configured for any item's metric.
    /// An applied batch writes one `debit` charge per item under the item's
    /// key. A batch is replayed only as a whole, by its envelope key.
    ///
    /// # Errors
    ///
    /// The variants of [`QuotaEnforcementStoragePluginV1::apply_debit_plan`],
    /// raised for the first item that fails;
    /// [`StorageError::IdempotencyPayloadMismatch`] at the first item whose
    /// key already names a `debit` charge or the record of a denied debit,
    /// wrapped in [`StorageError::BulkItem`] and checked under the item keys'
    /// locks, which changes nothing; and
    /// [`StorageError::BatchTimeout`] when the batch timer runs out before the
    /// write phase.
    async fn apply_batch_debit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        batch: &EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError>;

    /// Credit one named Quota, returning consumption to it.
    ///
    /// Storage checks replay under the locked Quota row before the
    /// fresh-credit guards. Thus a credit that succeeded before deactivation
    /// still replays.
    ///
    /// # Errors
    ///
    /// - [`StorageError::QuotaNotFound`] when no such Quota is visible under
    ///   `scope`: an absent row and a row outside the scope are one answer.
    ///   [`StorageError::SubjectOutOfScope`] when the row is visible but belongs
    ///   to a tenant other than the request's.
    /// - [`StorageError::QuotaDeactivated`] for a fresh credit to an inactive
    ///   Quota, and [`StorageError::PeriodClosed`] when the Quota's latest
    ///   period has ended. Neither is raised for a replay.
    /// - [`StorageError::IdempotencyPayloadMismatch`] for a replay with a
    ///   different payload (I2).
    /// - [`StorageError::LeaseContentionTimeout`] when the wait on a contended
    ///   row exceeds the metric's budget (I8). The seeded default is 50 ms; with
    ///   0 ms a concurrent writer on the same row is refused at once; nothing was
    ///   written and the call is safe to retry.
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    async fn apply_credit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError>;

    /// Give back part of the charge `ret` names (ADR-0011): unwind the amount
    /// over its entries (ADR-0010) and apply each by its Quota's type, all or
    /// nothing. An allocation Quota's held amount decreases; a consumption
    /// Quota's counter of the charge's period decreases inside the return
    /// window while that period is not settled; a rate Quota's counter does
    /// not change. A deactivated Quota takes its part as an active one does,
    /// and no counter goes below zero. Each entry's returned amount grows by
    /// its whole part either way.
    ///
    /// An applied return writes one return record (key, charge, amount),
    /// unique per tenant and key, which is deleted with its charge. A repeat
    /// with the same charge and amount changes nothing and returns `NoOp`
    /// with an empty plan. The charge is kept for one replay window from now.
    ///
    /// # Errors
    ///
    /// - [`StorageError::OperationNotFound`] when no live charge is under
    ///   the scope.
    /// - [`StorageError::IdempotencyPayloadMismatch`] when the key names a
    ///   return of another charge or amount.
    /// - [`StorageError::MetricTypeMismatch`] when the metric is not the
    ///   charge's.
    /// - [`StorageError::ReturnExceedsCharge`] when the amount exceeds what
    ///   remains of the charge.
    /// - [`StorageError::ReturnNotApplicable`] when no entry the amount
    ///   reaches can take a part of it.
    /// - [`StorageError::LeaseContentionTimeout`] when the wait on a contended
    ///   row exceeds the metric's budget (I8).
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    async fn apply_return(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        ret: &ChargeReturn,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError>;

    /// Give back everything that remains of the charge `target` names, by the
    /// rules of [`Self::apply_return`], with no key and no return record. On
    /// a charge with nothing left it changes nothing and returns `NoOp`.
    ///
    /// # Errors
    ///
    /// - [`StorageError::OperationNotFound`] when no live charge answers the
    ///   target, including an attribution mismatch. These cases are
    ///   deliberately indistinguishable to the caller.
    /// - [`StorageError::ReturnNotApplicable`] when something remains but no
    ///   entry can take a part of it.
    /// - [`StorageError::LeaseContentionTimeout`] and
    ///   [`StorageError::Unavailable`] as for [`Self::apply_return`].
    async fn apply_rollback(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        target: &RollbackTarget,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError>;

    // --- leases ---

    /// Evaluate the applicable policy and hold the resulting plan atomically
    /// (I5, I7, I8). A denied acquisition holds nothing and returns no token.
    ///
    /// The active-lease cap is checked only once the decision allowed the
    /// acquisition, so a denial is returned as its verdict whether or not the
    /// cap is full. Expired leases never count toward it (I4).
    ///
    /// # Errors
    ///
    /// The variants of [`QuotaEnforcementStoragePluginV1::apply_debit_plan`],
    /// plus [`StorageError::LeaseInflightLimitExceeded`] (I7).
    async fn acquire_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError>;

    /// Convert an active lease into a debit. `actual_amount` defaults to the
    /// reserved amount; `Some(0)` returns every hold and commits nothing.
    ///
    /// Replay is checked under the commit's key lock before the lease-state
    /// guards, so a commit that succeeded before its lease was resolved still
    /// replays. A commit writes the charge of `actual_amount`, which is its
    /// record and takes its attribution digest from the lease.
    ///
    /// The unused part `reserved - actual` is taken back from the holds by
    /// [`crate::models::unwind`], so every chain keeps exactly `actual`.
    ///
    /// # Errors
    ///
    /// - [`StorageError::LeaseNotFound`] when no such lease lies inside `scope`.
    /// - [`StorageError::LeaseNotActive`] when it is expired (I4) or terminal.
    /// - [`StorageError::MetricTypeMismatch`] when `metric` is given and is
    ///   not the lease's; nothing changes.
    /// - [`StorageError::OverCommitNotAuthorized`] when `actual_amount` exceeds
    ///   the reserved amount.
    /// - [`StorageError::IdempotencyPayloadMismatch`] for a replay with a
    ///   different payload (I2).
    /// - [`StorageError::LeaseContentionTimeout`] when the wait on a contended
    ///   row exceeds the configured budget (I8).
    /// - [`StorageError::Unavailable`] when the backend cannot answer.
    #[allow(
        clippy::too_many_arguments,
        reason = "the settlement names its lease, amount, metric, key and events"
    )]
    async fn commit_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        actual_amount: Option<u64>,
        metric: Option<&MetricId>,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError>;

    /// Return the full held amount of an active lease to the acquisition
    /// period (I5), committing no debit.
    ///
    /// # Errors
    ///
    /// The variants of [`QuotaEnforcementStoragePluginV1::commit_lease`],
    /// minus the over-commit.
    async fn release_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError>;

    // --- snapshot reads ---

    /// Per-Quota state for one applicable set: every active Quota whose
    /// subject is in the set, in `quota_id` order.
    ///
    /// Writes nothing and takes no row lock (I3): a consumption Quota whose
    /// current window has no row yet reads as zero over that window.
    async fn read_quota_snapshot(
        &self,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError>;

    /// Paginated per-Quota state for many applicable sets.
    ///
    /// Selects every active Quota under `scope` whose tenant and metric match
    /// a pair and whose subject is in that pair's subjects. The union is
    /// ordered by `quota_id` ascending and returns each Quota once, however
    /// many pairs select it. Quotas outside their validity window at the
    /// plugin's clock are included unless `in_window_only` is set, in which
    /// case they are left out before the page is cut. At most `page.limit` rows are returned (a zero limit means
    /// the default). `next_cursor` is opaque and holds only the last
    /// `quota_id` returned, so the next page resumes after it and a walk to
    /// exhaustion yields each row once; it is `None` on the last page. The
    /// page is read in one transaction and, like
    /// [`Self::read_quota_snapshot`], writes nothing.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidCursor`] for a cursor this plugin did not issue;
    /// [`StorageError::Unavailable`] when the backend cannot serve the page.
    async fn bulk_read_quota_snapshot(
        &self,
        scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
        in_window_only: bool,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError>;

    // --- policies (platform-wide, operator-scoped) ---

    /// Create version 1 of a new policy.
    ///
    /// # Errors
    /// `PolicyScopeOccupied` if a live policy occupies the exact scope; backend
    /// errors leave no version, transition audit or event committed.
    async fn create_policy(
        &self,
        actor: &Actor,
        draft: PolicyDraft,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError>;

    /// Create the next immutable version. Enforces `if_match_version`.
    ///
    /// # Errors
    /// `PolicyNotFound`, `PolicyDeleted`, or `VersionConflict`; version exhaustion
    /// and backend failures are `Internal`. Failures commit no writes or events.
    async fn update_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        update: PolicyUpdate,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError>;

    /// Make `target_version` active again. Replaying the active target changes
    /// nothing and emits no event or audit entry, and reports `NoOp` carrying the
    /// already-active version. Version creation fields remain immutable.
    ///
    /// # Errors
    /// `PolicyNotFound`, `PolicyDeleted`, `UnknownPolicyVersion`,
    /// `VersionRolledBack`, or a backend failure; no partial transition commits.
    async fn rollback_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        target_version: u32,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError>;

    /// Soft-delete a narrow-scope policy. A retry against an already-deleted
    /// policy reports `NoOp` and writes neither a new event nor an audit row.
    ///
    /// # Errors
    /// `CannotDeleteSeededGlobalPolicy`, `PolicyNotFound`, or a backend failure.
    async fn delete_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError>;

    /// The latest active version at the exact `scope`, if any. No fallback.
    ///
    /// # Errors
    /// A backend failure. An unoccupied scope is `Ok(None)`.
    async fn read_policy(&self, scope: &PolicyScope)
    -> Result<Option<PolicyVersion>, StorageError>;

    /// Active version of one policy ID; a deleted policy has no active version.
    ///
    /// # Errors
    /// Backend failure; missing or deleted IDs return `Ok(None)`.
    async fn read_active_policy_by_id(
        &self,
        policy_id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError>;

    /// Caller-less snapshot of every active policy version, read at bootstrap.
    ///
    /// # Errors
    /// Backend failure. This is an internal platform read, not a public list API.
    async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError>;

    /// One specific retained version, including terminal versions, if it exists.
    ///
    /// # Errors
    /// A backend failure. A missing version is `Ok(None)`.
    async fn read_policy_version(
        &self,
        policy_id: &PolicyId,
        version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError>;

    /// Ordered version list of a policy.
    ///
    /// # Errors
    /// `PolicyNotFound`, `InvalidCursor`, or a backend failure.
    async fn list_policy_versions(
        &self,
        policy_id: &PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError>;

    // --- idempotency ---

    /// Read-only lookup of a stored record (I3). For a debit or a lease commit
    /// it also answers from the charge under `scope`, for as long as the
    /// charge lives.
    async fn lookup_idempotency(
        &self,
        scope: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError>;

    // --- notification delivery ---

    /// Start the one notification outbox pipeline of this plugin instance,
    /// handing every claimed event to `delivery`, and bind it for enqueueing.
    ///
    /// Until this returns, events enqueued by a mutation stay queued; an empty
    /// enqueue never needs it. The pipeline is fenced by the outbox lease, so
    /// every replica may run one. Keep the handle and stop it on shutdown.
    ///
    /// # Errors
    ///
    /// [`StorageError::Internal`] when this instance already started one;
    /// [`StorageError::Unavailable`] when the backend cannot start it.
    async fn start_notification_delivery(
        &self,
        delivery: std::sync::Arc<dyn crate::notifications::NotificationDeliveryV1>,
    ) -> Result<Box<dyn crate::notifications::NotificationDeliveryHandle>, StorageError>;

    // --- sweeper and reclamation ---

    /// How many expired leases are still unreclaimed, by metric, behind the
    /// `lease_unreclaimed_expired` gauge. Counts rows, so it stays cheap while
    /// the sweeper is down and the backlog is what an operator needs to see.
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`] when the backend cannot answer.
    async fn count_expired_unreclaimed_leases(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<(MetricId, u64)>, StorageError>;

    /// Physically reclaim up to `batch_size` leases expired before `before`.
    async fn reclaim_expired_leases(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError>;

    /// Delete up to `batch_size` idempotency records expired before `before`.
    /// Returns the number of deleted records. Charges are not idempotency
    /// records and are not deleted here.
    async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError>;

    /// Delete up to `batch_size` operation-log entries older than `before`.
    /// Returns the number of deleted entries.
    async fn reclaim_operation_log(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError>;

    /// Remove up to `batch_size` charges deleted at `now` (ADR-0011): the
    /// replay window has passed since their creation and since the last
    /// return or rollback, and no allocation entry holds an amount on an
    /// active Quota. The delete checks the rule itself, so a charge that
    /// became live again or replaced a deleted one under its key stays.
    /// Their return records go with them. Returns the number removed.
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`] when the backend cannot answer.
    async fn reclaim_expired_charges(
        &self,
        batch_size: u32,
        now: OffsetDateTime,
    ) -> Result<u64, StorageError>;

    /// Per metric, the allocation charges whose return window ended before
    /// `before` and that still hold an amount on an active Quota, with the
    /// creation time of the oldest. Metrics with none are left out. A backend
    /// may also count a hold on a deactivated Quota until it releases it.
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`] when the backend cannot answer.
    async fn read_held_allocation_charge_stats(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<HeldAllocationCharges>, StorageError>;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "storage_plugin_tests.rs"]
mod storage_plugin_tests;
