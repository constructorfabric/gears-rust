//! Quota Enforcement SDK
//!
//! Public, transport-agnostic contract of the `quota-enforcement` gear. The
//! foundation ships the plugin side first, so plugin authors implement against
//! one dependency:
//!
//! - [`QuotaEnforcementStoragePluginV1`] with the closed [`StorageError`] and
//!   the I1 to I14 invariants (see the [`storage_plugin`] module docs).
//! - The domain types the contract references ([`models`]), including the
//!   wire attribution types of a subject-based evaluation request
//!   ([`EvaluationAttribution`]) and the scope discriminator ([`SubjectScope`]).
//! - GTS plugin spec, resource identifiers, and the QE-owned projection
//!   contract bases ([`gts`], ADR-0007).
//!
//! Singleton coordination for the sweepers is not a contract of this SDK: the
//! gear consumes the platform `cluster` gear's leader election (ADR-0006).
//!
//! The manager client trait ([`QuotaManagerClientV1`]) carries the Quota
//! lifecycle; the consumer and operator traits land with their features.
//!
//! Enable the `test-util` feature for a complete in-memory double of the
//! storage contract in [`testing`].
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod client;
pub mod engine;
pub mod gts;
pub mod models;
pub mod period;
pub mod storage_plugin;
pub mod thresholds;

#[cfg(feature = "test-util")]
pub mod testing;

pub use client::{
    QuotaEnforcementClientV1, QuotaEnforcementError, QuotaManagerClientV1, QuotaOperatorClientV1,
};
pub use engine::{
    EngineConfigError, EngineError, EngineValidationInput, EnvironmentInputs, EvaluationBudget,
    EvaluationContext, EvaluationFailure, EvaluationMeter, EvaluationOutcome, EvaluationQuota,
    MetricEnvironmentSchema, PolicySchemaSnapshot, QuotaResolutionEngineV1, QuotaScopeTier,
    ValidatedConfig,
};
pub use gts::{
    CONSTRAINT_BASE, LEASE_RESOURCE, METRIC_BASE_TYPE, OPERATION_RESOURCE, OwnedDefinition,
    POLICY_RESOURCE, QUOTA_RESOURCE, QuotaEnforcementStoragePluginSpecV1, REQUEST_BASE,
    RESOURCE_BASE, SCOPE_TENANT, SCOPE_TYPE, SCOPE_USER, SUBJECT_BASE, owned_definitions,
};
pub use models::{
    ActiveQuotaCounts, ApplicableQuotas, AppliedMutation, AttributionDigest, BatchDebitItem,
    BootstrapBundle, CapPatch, ConfigDefaults, ContractRef, CounterSnapshot, CreditRequest,
    DECISION_BLOB_VERSION, DeactivateOutcome, DebitPlan, DebitRequest, Decision, DecisionPreview,
    DecisionResult, EnforcementMode, EvaluatedDebit, EvaluatedLease, EvaluationAttribution,
    EventId, ExpiredLease, IdempotencyRecord, IdempotencyScope, IdempotencySubjectKey,
    IdempotencyWrite, LeaseHold, LeaseState, LeaseToken, MetricId, MetricKind, MutationResult,
    NO_APPLICABLE_QUOTA, NotificationEvent, NotificationEventKind, NotificationScope,
    OperationType, PageRequest, PageResult, PartialIdempotencyWrite, PayloadHash, PeriodId,
    PeriodType, PeriodWindow, PolicyDraft, PolicyId, PolicyPatch, PolicyScope, PolicySpec,
    PolicyUpdate, PolicyVersion, PolicyVersionMeta, PolicyVersionState, PreviewRequest,
    ProjectionBinding, Quota, QuotaDebitPlan, QuotaDraft, QuotaFilter, QuotaId, QuotaPatch,
    QuotaSnapshot, QuotaSource, QuotaSpec, QuotaStatus, QuotaType, QuotaView, ResourceProjection,
    Retention, RollbackRequest, RollbackTarget, ScopeError, SubjectClaim, SubjectRef, SubjectScope,
    TenantId, ThresholdCrossing, TransitionOutcome, UnknownValue, ValidityWindow,
    ValidityWindowPatch, positive_amount,
};
pub use storage_plugin::{
    CONTRACT_MAJOR, EvaluatedBatch, EvaluatedMutation, QuotaEnforcementStoragePluginV1,
    StorageError,
};
pub use thresholds::threshold_crossings;
