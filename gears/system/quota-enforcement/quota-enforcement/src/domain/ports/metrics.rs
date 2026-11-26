//! Output port for the gear-specific instruments (PRD section 5.16).
//!
//! The foundation owns `denial_total{reason}`, the count of admission denials
//! by closed reason. The projection-contracts feature adds
//! `contract_validation_failures_total{surface, reason}` and
//! `admitted_metric_violations_total{surface}`. Later features add their
//! instruments to this port; none of them leaves the catalogue.
//!
//! Cardinality rule (`cpt-cf-quota-enforcement-constraint-bounded-cardinality`):
//! every label value is a `&'static str` from a closed enum, or a
//! [`MetricLabel`], whose values are the metric types the catalogue serves —
//! types the registry holds, never raw caller input. `tenant_id`,
//! `subject_id`, `quota_id`, `policy_id`, idempotency keys, lease tokens,
//! subject types, and caller attribution never appear as labels. They belong
//! to spans and structured logs.

use std::fmt;
use std::sync::Arc;

use toolkit_macros::domain_model;

/// Label key of `denial_total` and of `contract_validation_failures_total`.
pub const REASON_LABEL: &str = "reason";

/// Label key of the contract-validation counters.
pub const SURFACE_LABEL: &str = "surface";

/// Label key of the lease instruments.
pub const METRIC_LABEL: &str = "metric";

/// A `metric` label value: the id of a metric type the catalogue serves.
///
/// It is made only from a metric the catalogue served, so the values an
/// instrument can carry are registered metric types and never raw caller
/// input.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricLabel(Arc<str>);

impl MetricLabel {
    /// The label of a served metric. Crate-private: `metric` must have passed
    /// the catalogue.
    pub(crate) fn admitted(metric: &quota_enforcement_sdk::MetricId) -> Self {
        Self(Arc::from(metric.as_str()))
    }

    /// The label value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The label value, shared rather than copied.
    #[must_use]
    pub fn shared(&self) -> Arc<str> {
        Arc::clone(&self.0)
    }
}

/// A `sink_id` label value: the id of a notification sink resolved at
/// bootstrap.
///
/// Only the dispatcher makes one, from the sink set bootstrap resolved, so the
/// values an instrument can carry are exactly the deployment's sinks.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SinkLabel(Arc<str>);

impl SinkLabel {
    /// The label of a resolved sink. Crate-private: the dispatcher is the
    /// only caller.
    pub(crate) fn resolved(id: &str) -> Self {
        Self(Arc::from(id))
    }

    /// The label value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The label value, shared rather than copied.
    #[must_use]
    pub fn shared(&self) -> Arc<str> {
        Arc::clone(&self.0)
    }
}

/// Closed `reason` set of `denial_total`.
// @cpt-dod:cpt-cf-quota-enforcement-dod-telemetry-conventions:p1
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialReason {
    /// The PDP denied, or its constraints did not compile.
    PermissionDenied,
    /// The PDP could not be reached. Fail closed.
    PdpUnavailable,
    /// The public request shape was invalid before the PDP call.
    InvalidArgument,
    /// Bootstrap has not completed.
    NotReady,
    /// An applicable Quota would be exceeded.
    QuotaExceeded,
    /// No active Quota covers the metric, so nothing was recorded.
    NoApplicableQuota,
    /// The policy's engine denied for a reason of its own. Engine reasons are
    /// authored in policy and are unbounded, so they collapse into this one
    /// label rather than opening the label set to arbitrary strings.
    EngineDenied,
    /// The authorization outcome was of a class this gear does not know: an
    /// internal failure, counted apart from denials and PDP unavailability.
    Internal,
}

impl DenialReason {
    /// Every value, for conformance tests.
    pub const ALL: [Self; 8] = [
        Self::PermissionDenied,
        Self::PdpUnavailable,
        Self::InvalidArgument,
        Self::NotReady,
        Self::QuotaExceeded,
        Self::NoApplicableQuota,
        Self::EngineDenied,
        Self::Internal,
    ];

    /// The label of a decision's closed reason token.
    ///
    /// Anything an engine authored that is not one of the two tokens the
    /// platform defines counts as an engine denial, which keeps the label set
    /// closed however a policy is written.
    #[must_use]
    pub fn from_decision_reason(reason: &str) -> Self {
        match reason {
            quota_enforcement_sdk::NO_APPLICABLE_QUOTA => Self::NoApplicableQuota,
            "QUOTA_EXCEEDED" => Self::QuotaExceeded,
            _ => Self::EngineDenied,
        }
    }

    /// Stable label value.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::PdpUnavailable => "pdp_unavailable",
            Self::InvalidArgument => "invalid_argument",
            Self::NotReady => "not_ready",
            Self::QuotaExceeded => "quota_exceeded",
            Self::NoApplicableQuota => "no_applicable_quota",
            Self::EngineDenied => "engine_denied",
            Self::Internal => "internal",
        }
    }
}

/// Closed `operation` set of the hot-path latency histogram and the replay
/// counter.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    /// Charge against every applicable Quota.
    Debit,
    /// Return consumption to one Quota.
    Credit,
    /// Give back what remains of a charge.
    Rollback,
    /// Give back part of a charge.
    Return,
    /// Evaluate without mutating.
    Preview,
    /// Acquire a lease.
    Reserve,
    /// Commit a lease.
    Commit,
    /// Release a lease.
    Release,
}

impl OperationKind {
    /// Every value, for conformance tests.
    pub const ALL: [Self; 8] = [
        Self::Debit,
        Self::Credit,
        Self::Rollback,
        Self::Return,
        Self::Preview,
        Self::Reserve,
        Self::Commit,
        Self::Release,
    ];

    /// Stable label value.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Debit => "debit",
            Self::Credit => "credit",
            Self::Rollback => "rollback",
            Self::Return => "return",
            Self::Preview => "preview",
            Self::Reserve => "reserve",
            Self::Commit => "commit",
            Self::Release => "release",
        }
    }
}

/// Closed `table` set of the retention counters.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionTable {
    /// Replay records.
    Idempotency,
    /// The operation ledger.
    OperationLog,
    /// Deleted charges of the charge ledger.
    Charges,
}

impl RetentionTable {
    /// Every value, for conformance tests.
    pub const ALL: [Self; 3] = [Self::Idempotency, Self::OperationLog, Self::Charges];

    /// Stable label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idempotency => "idempotency",
            Self::OperationLog => "operation_log",
            Self::Charges => "charges",
        }
    }
}

/// Closed `surface` set of the contract-validation counters (DESIGN section
/// 4.1): where a metric type or subject check ran.
// @cpt-dod:cpt-cf-quota-enforcement-dod-contract-validation-telemetry:p1
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValidationSurface {
    /// A subject of an evaluation or snapshot request.
    RequestSubject,
    /// The metric object of an evaluation request.
    RequestMetric,
    /// The public shape of the caller-supplied attribution, before the PDP.
    CallerAttribution,
    /// The subject or metadata of a Quota write.
    QuotaWrite,
    /// A Policy write.
    Policy,
    /// A read of the metric catalogue.
    Bootstrap,
}

impl ValidationSurface {
    /// Every value, for conformance tests.
    pub const ALL: [Self; 6] = [
        Self::RequestSubject,
        Self::RequestMetric,
        Self::CallerAttribution,
        Self::QuotaWrite,
        Self::Policy,
        Self::Bootstrap,
    ];

    /// Stable label value.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::RequestSubject => "request_subject",
            Self::RequestMetric => "request_metric",
            Self::CallerAttribution => "caller_attribution",
            Self::QuotaWrite => "quota_write",
            Self::Policy => "policy",
            Self::Bootstrap => "bootstrap",
        }
    }
}

impl fmt::Display for ValidationSurface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_label())
    }
}

/// Closed `reason` set of `contract_validation_failures_total`: why a request,
/// a write, or a catalogue entry was rejected.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValidationReason {
    /// The public request shape was malformed (missing field, empty id,
    /// duplicate subject type, malformed type id).
    ShapeInvalid,
    /// The required metadata object was absent.
    MetadataMissing,
    /// A document, or a condition of Quota metadata, violated the resolved
    /// schema of its type.
    SchemaViolation,
    /// A resolved schema could not be compiled.
    SchemaInvalid,
    /// A subject type is not registered.
    Unregistered,
    /// A type is abstract where a concrete one is required.
    Abstract,
    /// A type does not derive from the required base.
    NotDerived,
    /// A metric type is not registered.
    MetricUnregistered,
    /// A metric type's own version is major 0: types-registry admits any
    /// revision of such a type, so QE does not serve it.
    MajorVersionZero,
    /// A trait of a metric type is wrong.
    TraitInvalid,
    /// A write names a metric whose catalogue entry is unavailable.
    ProjectionNotResolvable,
}

impl ValidationReason {
    /// Every value, for conformance tests.
    pub const ALL: [Self; 11] = [
        Self::ShapeInvalid,
        Self::MetadataMissing,
        Self::SchemaViolation,
        Self::SchemaInvalid,
        Self::Unregistered,
        Self::Abstract,
        Self::NotDerived,
        Self::MetricUnregistered,
        Self::MajorVersionZero,
        Self::TraitInvalid,
        Self::ProjectionNotResolvable,
    ];

    /// Stable label value.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::ShapeInvalid => "shape_invalid",
            Self::MetadataMissing => "metadata_missing",
            Self::SchemaViolation => "schema_violation",
            Self::SchemaInvalid => "schema_invalid",
            Self::Unregistered => "unregistered",
            Self::Abstract => "abstract",
            Self::NotDerived => "not_derived",
            Self::MetricUnregistered => "metric_unregistered",
            Self::MajorVersionZero => "major_version_zero",
            Self::TraitInvalid => "trait_invalid",
            Self::ProjectionNotResolvable => "projection_not_resolvable",
        }
    }
}

impl fmt::Display for ValidationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_label())
    }
}

/// Closed `phase` set of `catalog_read_failures_total`: which kind of
/// registry read failed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogReadPhase {
    /// The read at start of the metric types of the active Quotas and Policies.
    Start,
    /// The read of a type a request or a write names for the first time.
    FirstUse,
    /// The scheduled conditional read of every held type.
    Scheduled,
}

impl CatalogReadPhase {
    /// Every value, for conformance tests.
    pub const ALL: [Self; 3] = [Self::Start, Self::FirstUse, Self::Scheduled];

    /// Stable label value.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::FirstUse => "first_use",
            Self::Scheduled => "scheduled",
        }
    }
}

/// Gear-specific instruments. Implemented on the platform meter in
/// `infra::metrics`; a no-op double serves tests.
// @cpt-dod:cpt-cf-quota-enforcement-dod-policy-engine-telemetry:p1
pub trait QeMetrics: Send + Sync {
    /// Registration or persisted-artifact bootstrap failure.
    fn record_engine_bootstrap_failure(&self, engine: EngineLabel);
    /// Evaluation duration on both success and failure.
    fn record_engine_evaluation(&self, engine: EngineLabel, elapsed: std::time::Duration);
    /// Rejected engine decision by closed invariant label.
    fn record_plan_violation(
        &self,
        engine: EngineLabel,
        invariant: quota_enforcement_sdk::engine::DebitPlanInvariant,
    );
    /// One committed transition, excluding no-op retries.
    fn record_policy_transition(&self, transition: PolicyTransition);
    /// One rejected optimistic concurrency check.
    fn record_policy_conflict(&self);

    /// `denial_total{reason}` += 1.
    fn record_denial(&self, reason: DenialReason);

    /// `contract_validation_failures_total{surface, reason}` += 1.
    fn record_contract_validation_failure(
        &self,
        surface: ValidationSurface,
        reason: ValidationReason,
    );

    /// `admitted_metric_violations_total{surface}` += 1.
    fn record_admitted_metric_violation(&self, surface: ValidationSurface);

    /// `evaluation_seconds{operation}` observes one hot-path call, on success
    /// and on failure alike: a slow refusal is as interesting as a slow commit.
    fn record_evaluation(&self, operation: OperationKind, elapsed: std::time::Duration);

    /// `idempotency_replays_total{operation}` += 1.
    fn record_idempotency_replay(&self, operation: OperationKind);

    /// `contention_rejected_total{operation}` += 1: an operation refused on
    /// the contention timeout. A batch counts as a debit.
    fn record_contention_rejected(&self, operation: OperationKind);

    /// `retention_reclaimed_total{table}` += `rows`.
    fn record_retention_reclaimed(&self, table: RetentionTable, rows: u64);

    /// `retention_sweep_failures_total{table}` += 1.
    fn record_retention_failure(&self, table: RetentionTable);

    /// `lease_acquisition_wait_seconds{metric}` observes one acquisition,
    /// from the storage call to its answer: admitted, denied, or refused on
    /// the cap or on contention.
    fn record_lease_acquisition_wait(&self, metric: &MetricLabel, elapsed: std::time::Duration);

    /// `lease_contention_rejected_total{metric}` += 1.
    fn record_lease_contention_rejected(&self, metric: &MetricLabel);

    /// `lease_inflight_limit_exceeded_total{metric}` += 1.
    fn record_lease_inflight_limit_exceeded(&self, metric: &MetricLabel);

    /// `notification_dispatch_failures_total{sink_id, event_kind}` += 1.
    fn record_notification_dispatch_failure(
        &self,
        sink: &SinkLabel,
        kind: quota_enforcement_sdk::NotificationEventKind,
    );

    /// `outbox_rejections_total{queue}` += 1: the notification queue
    /// dead-lettered one event.
    fn record_outbox_rejection(&self);

    /// `policy_evaluation_failures_total{engine_id, metric}` += 1.
    fn record_policy_evaluation_failure(&self, engine: EngineLabel, metric: &MetricLabel);

    /// `catalog_read_failures_total{phase}` += 1.
    fn record_catalog_read_failure(&self, phase: CatalogReadPhase);

    /// `catalog_last_scheduled_read_timestamp_seconds` := now.
    fn record_catalog_scheduled_read(&self);

    /// `catalog_unavailable_metrics` := `count`.
    fn set_catalog_unavailable_metrics(&self, count: u64);
}

/// One backlog sample: expired, unreclaimed leases per admitted metric.
pub type LeaseBacklog = Vec<(MetricLabel, u64)>;

/// Where the lease sweeper publishes the expired-but-unreclaimed backlog that
/// `lease_unreclaimed_expired{metric}` observes. `None` withdraws the sample:
/// a replica that does not lead has nothing to report.
pub trait LeaseBacklogSink: Send + Sync {
    /// Replace the published sample.
    fn publish(&self, backlog: Option<LeaseBacklog>);
}

/// Publishes nothing. For tests and pre-init contexts.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopLeaseBacklog;

impl LeaseBacklogSink for NoopLeaseBacklog {
    fn publish(&self, _backlog: Option<LeaseBacklog>) {}
}

/// The allocation charges of one metric that hold an amount on an active
/// Quota and are older than the replay window.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeldCharges {
    /// How many there are.
    pub count: u64,
    /// Age of the oldest, in seconds; zero when there are none.
    pub oldest_age_secs: u64,
}

/// One sample of the held allocation charges, per admitted metric.
pub type ChargeBacklog = Vec<(MetricLabel, HeldCharges)>;

/// Where the retention sweeper publishes what
/// `allocation_charges_past_replay_window{metric}` and
/// `allocation_charge_max_age_seconds{metric}` observe. `None` withdraws the
/// sample: a replica that does not lead has nothing to report.
pub trait ChargeBacklogSink: Send + Sync {
    /// Replace the published sample.
    fn publish(&self, backlog: Option<ChargeBacklog>);
}

/// Publishes nothing. For tests and pre-init contexts.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopChargeBacklog;

impl ChargeBacklogSink for NoopChargeBacklog {
    fn publish(&self, _backlog: Option<ChargeBacklog>) {}
}

/// Records nothing. For tests and pre-init contexts.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl QeMetrics for NoopMetrics {
    fn record_engine_bootstrap_failure(&self, _: EngineLabel) {}
    fn record_engine_evaluation(&self, _: EngineLabel, _: std::time::Duration) {}
    fn record_plan_violation(
        &self,
        _: EngineLabel,
        _: quota_enforcement_sdk::engine::DebitPlanInvariant,
    ) {
    }
    fn record_policy_transition(&self, _: PolicyTransition) {}
    fn record_policy_conflict(&self) {}

    fn record_denial(&self, _reason: DenialReason) {}

    fn record_contract_validation_failure(
        &self,
        _surface: ValidationSurface,
        _reason: ValidationReason,
    ) {
    }

    fn record_admitted_metric_violation(&self, _surface: ValidationSurface) {}
    fn record_evaluation(&self, _operation: OperationKind, _elapsed: std::time::Duration) {}
    fn record_idempotency_replay(&self, _operation: OperationKind) {}
    fn record_contention_rejected(&self, _operation: OperationKind) {}
    fn record_retention_reclaimed(&self, _table: RetentionTable, _rows: u64) {}
    fn record_retention_failure(&self, _table: RetentionTable) {}
    fn record_lease_acquisition_wait(&self, _: &MetricLabel, _: std::time::Duration) {}
    fn record_lease_contention_rejected(&self, _: &MetricLabel) {}
    fn record_lease_inflight_limit_exceeded(&self, _: &MetricLabel) {}
    fn record_notification_dispatch_failure(
        &self,
        _: &SinkLabel,
        _: quota_enforcement_sdk::NotificationEventKind,
    ) {
    }
    fn record_outbox_rejection(&self) {}
    fn record_policy_evaluation_failure(&self, _: EngineLabel, _: &MetricLabel) {}
    fn record_catalog_read_failure(&self, _: CatalogReadPhase) {}
    fn record_catalog_scheduled_read(&self) {}
    fn set_catalog_unavailable_metrics(&self, _: u64) {}
}

/// The telemetry label of a linked engine: its own `id()`.
///
/// Only a `&'static str` builds one, so the label set is the engines compiled
/// into the binary; an operator-submitted `engine_id` (a runtime `String`)
/// cannot become a label.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineLabel(&'static str);

impl EngineLabel {
    /// The label of a linked engine.
    #[must_use]
    pub fn of(engine: &dyn quota_enforcement_sdk::QuotaResolutionEngineV1) -> Self {
        Self(engine.id())
    }

    /// The label for an engine id that came from a linked engine's `id()`.
    #[must_use]
    pub(crate) const fn from_linked_id(id: &'static str) -> Self {
        Self(id)
    }

    /// Static telemetry value.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        self.0
    }
}

/// Committed policy transition labels.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyTransition {
    /// Policy creation.
    Create,
    /// A new version.
    Update,
    /// Reactivation of historical version.
    Rollback,
    /// Soft deletion.
    Delete,
}
impl PolicyTransition {
    /// Static telemetry value.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Rollback => "rollback",
            Self::Delete => "delete",
        }
    }
}
