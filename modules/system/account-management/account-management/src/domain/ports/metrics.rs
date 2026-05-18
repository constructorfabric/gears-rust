//! AM observability ports — typed, segregated metric-emission traits.
//!
//! Each trait owns one cohesive subdomain of the AM metric catalog
//! declared in [`crate::domain::metrics`]. A single infra adapter
//! (introduced in a follow-up PR) implements every trait on one
//! OpenTelemetry-backed struct; DI hands each service the trait(s) it
//! actually needs.
//!
//! ## Design choices
//!
//! * **Trait segregation.** Six narrow traits instead of one fat trait
//!   — each service depends on the minimum surface it needs. The
//!   adapter struct still implements all six; only the `dyn Trait`
//!   views are split.
//! * **Typed label values.** Each `&str` label that was previously
//!   passed as a literal becomes a typed enum or a sealed newtype with
//!   `pub const` literals. The compiler enforces the closed set; no
//!   typo can leak into a dashboard.
//! * **Bridging existing failure types.** Label values that are derived
//!   from SDK failure enums via `<Failure>::as_metric_label()` are
//!   modelled as sealed newtype wrappers (`BootstrapClassification`,
//!   `DependencyOutcome`, `TenantRetentionOutcome`) with `From<&Failure>`
//!   impls — the variant→string mapping continues to live on the
//!   failure type and is not duplicated here.
//! * **Cardinality discipline.** No label accepts a free `&str`.
//!   Numeric labels (`threshold` on `hierarchy_depth_exceedance`) are
//!   typed primitives; the adapter renders them at emit time.
//! * **Metric names** keep the dot-separated form
//!   (`am.dependency_health`, `am.bootstrap_lifecycle`) from
//!   [`crate::domain::metrics`]. Unit hints are attached on the
//!   adapter via `.with_unit("ms")`/`.with_unit("s")`, not embedded
//!   in the metric name.
//!
//! ## Phasing
//!
//! This file is **port-only**. No adapter, no wire-up, no migration of
//! call sites. Follow-up PRs:
//!
//! 1. `infra/metrics.rs` — `AmMetricsMeter` adapter implementing every
//!    trait against `opentelemetry::global::meter_with_scope(...)`.
//! 2. `module.rs` wire-up: construct one `Arc<AmMetricsMeter>` and
//!    coerce per-trait `Arc<dyn ...>` views to inject into services.
//! 3. Per-subdomain call-site migration from the stringly-typed
//!    `emit_metric` facade onto these typed methods.
//! 4. Removal of [`crate::domain::metrics`] facade once all call sites
//!    have moved.

use account_management_sdk::idp::{IdpDeprovisionFailure, IdpProvisionFailure};
use account_management_sdk::idp_user::IdpUserOperationFailure;
use modkit_macros::domain_model;

use crate::domain::tenant::integrity::IntegrityCategory;
use crate::domain::tenant::retention::HardDeleteOutcome;

// ════════════════════════════════════════════════════════════════════
//  Label-value taxonomy
// ════════════════════════════════════════════════════════════════════
//
//  Enums for closed, statically-known value sets. Sealed newtypes
//  (`pub struct X(&'static str)`) for value sets that need to bridge
//  existing SDK/domain failure types via their `as_metric_label()`
//  helpers.

// ── am.bootstrap_lifecycle ──────────────────────────────────────────

/// `phase` label on `am.bootstrap_lifecycle`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapPhase {
    IdpPrecheck,
    IdpProvisioning,
    IdpWaiting,
    ProvisioningWait,
    GtsPreflight,
    RootCreating,
    Step3Compensation,
    Completed,
    Failed,
}

impl BootstrapPhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IdpPrecheck => "idp_precheck",
            Self::IdpProvisioning => "idp_provisioning",
            Self::IdpWaiting => "idp_waiting",
            Self::ProvisioningWait => "provisioning_wait",
            Self::GtsPreflight => "gts_preflight",
            Self::RootCreating => "root_creating",
            Self::Step3Compensation => "step3_compensation",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// `outcome` label on `am.bootstrap_lifecycle`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapOutcome {
    Success,
    Failure,
    Retry,
    Timeout,
    Available,
    DeferredToReaper,
    Reclassify,
}

impl BootstrapOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::Available => "available",
            Self::DeferredToReaper => "deferred_to_reaper",
            Self::Reclassify => "reclassify",
        }
    }
}

/// `classification` label on `am.bootstrap_lifecycle`.
///
/// Sealed newtype: the closed set of literals lives as `pub const`
/// associated constants, and `From<&IdpProvisionFailure>` bridges
/// the SDK failure enum whose `as_metric_label()` helper already
/// owns the variant→string mapping. No public constructor — values
/// must come from a constant or a `From` impl, so the cardinality
/// surface stays closed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapClassification(&'static str);

impl BootstrapClassification {
    pub const FRESH: Self = Self("fresh");
    pub const IDP_TIMEOUT: Self = Self("idp_timeout");
    pub const IN_PROGRESS_ELSEWHERE: Self = Self("in_progress_elsewhere");
    pub const INVALID_TENANT_TYPE: Self = Self("invalid_tenant_type");
    pub const INVARIANT_VIOLATION: Self = Self("invariant_violation");
    pub const NO_ROOT_POST_DEADLINE: Self = Self("no_root_post_deadline");
    pub const RACE_LOSER: Self = Self("race_loser");
    pub const ROOT_ID_DRIFT: Self = Self("root_id_drift");
    pub const SERVICE_UNAVAILABLE: Self = Self("service_unavailable");
    pub const SKIPPED: Self = Self("skipped");
    pub const TIMEOUT: Self = Self("timeout");
    pub const TYPE_NOT_ALLOWED: Self = Self("type_not_allowed");
    pub const UNSUPPORTED_REQUIRED: Self = Self("unsupported_required");
    pub const DEFERRED_TO_REAPER: Self = Self("deferred_to_reaper");

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl From<&IdpProvisionFailure> for BootstrapClassification {
    fn from(f: &IdpProvisionFailure) -> Self {
        Self(f.as_metric_label())
    }
}

// ── am.conversion_lifecycle ─────────────────────────────────────────

/// `op` label on `am.conversion_lifecycle`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionOp {
    ExpirePending,
    ListInboundForParentNameLookup,
}

impl ConversionOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExpirePending => "expire_pending",
            Self::ListInboundForParentNameLookup => "list_inbound_for_parent_name_lookup",
        }
    }
}

/// `outcome` label on `am.conversion_lifecycle`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionOutcome {
    DegradedSnapshotFallback,
    PerRowFailure,
}

impl ConversionOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DegradedSnapshotFallback => "degraded_snapshot_fallback",
            Self::PerRowFailure => "per_row_failure",
        }
    }
}

// ── am.dependency_health ────────────────────────────────────────────

/// `target` label on `am.dependency_health`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyTarget {
    Idp,
    ResourceGroup,
    Gts,
    TypesRegistry,
    MetadataUpsert,
}

impl DependencyTarget {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idp => "idp",
            Self::ResourceGroup => "resource_group",
            Self::Gts => "gts",
            Self::TypesRegistry => "types_registry",
            Self::MetadataUpsert => "metadata_upsert",
        }
    }
}

/// `op` label on `am.dependency_health`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyOp {
    ProvisionTenant,
    DeprovisionTenant,
    CreateType,
    GetType,
    GetTypeSchemasByUuid,
    ListGroups,
    RegisterUserGroupType,
    RegisterUserGroupTypeRaceReread,
    UniqueViolationRace,
    CascadeListGroups,
    CascadeDeleteGroup,
    CascadeCleanup,
    UserCleanupListGroups,
    UserCleanupMemberships,
    UserCleanupRemoveMembership,
}

impl DependencyOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProvisionTenant => "provision_tenant",
            Self::DeprovisionTenant => "deprovision_tenant",
            Self::CreateType => "create_type",
            Self::GetType => "get_type",
            Self::GetTypeSchemasByUuid => "get_type_schemas_by_uuid",
            Self::ListGroups => "list_groups",
            Self::RegisterUserGroupType => "register_user_group_type",
            Self::RegisterUserGroupTypeRaceReread => "register_user_group_type_race_reread",
            Self::UniqueViolationRace => "unique_violation_race",
            Self::CascadeListGroups => "cascade_list_groups",
            Self::CascadeDeleteGroup => "cascade_delete_group",
            Self::CascadeCleanup => "cascade_cleanup",
            Self::UserCleanupListGroups => "user_cleanup_list_groups",
            Self::UserCleanupMemberships => "user_cleanup_memberships",
            Self::UserCleanupRemoveMembership => "user_cleanup_remove_membership",
        }
    }
}

/// `outcome` label on `am.dependency_health`.
///
/// Sealed newtype bridging the closed set of literal outcomes
/// produced directly at call sites and the SDK failure enums
/// (`IdpProvisionFailure`, `IdpDeprovisionFailure`, `IdpUserOperationFailure`,
/// `DeprovisionUserOutcome`) that own their own
/// `as_metric_label()` mappings. The (op, outcome) tuple stays the
/// stable dimension downstream dashboards key on.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyOutcome(&'static str);

impl DependencyOutcome {
    pub const SUCCESS: Self = Self("success");
    pub const ERROR: Self = Self("error");
    pub const RETRY: Self = Self("retry");
    pub const TIMEOUT: Self = Self("timeout");
    pub const ALREADY_DELETED: Self = Self("already_deleted");
    pub const ALREADY_GONE: Self = Self("already_gone");
    pub const ALREADY_PRESENT: Self = Self("already_present");
    pub const BUDGET_EXCEEDED: Self = Self("budget_exceeded");
    pub const REGISTERED_NEW: Self = Self("registered_new");
    pub const RETRIES_EXHAUSTED: Self = Self("retries_exhausted");

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl From<&IdpProvisionFailure> for DependencyOutcome {
    fn from(f: &IdpProvisionFailure) -> Self {
        Self(f.as_metric_label())
    }
}

impl From<&IdpDeprovisionFailure> for DependencyOutcome {
    fn from(f: &IdpDeprovisionFailure) -> Self {
        Self(f.as_metric_label())
    }
}

impl From<&IdpUserOperationFailure> for DependencyOutcome {
    fn from(f: &IdpUserOperationFailure) -> Self {
        Self(f.as_metric_label())
    }
}

// ── am.hierarchy_depth_exceedance ───────────────────────────────────

/// `mode` label on `am.hierarchy_depth_exceedance`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyDepthMode {
    Strict,
    Advisory,
}

impl HierarchyDepthMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Advisory => "advisory",
        }
    }
}

/// `outcome` label on `am.hierarchy_depth_exceedance`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyDepthOutcome {
    Reject,
    Warn,
}

impl HierarchyDepthOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reject => "reject",
            Self::Warn => "warn",
        }
    }
}

// ── am.hierarchy_integrity_* ────────────────────────────────────────

/// `outcome` label on `am.hierarchy_integrity_runs` and
/// `am.hierarchy_integrity_repair_runs`. The set is documented as
/// fixed in [`crate::domain::metrics`].
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityRunOutcome {
    Completed,
    SkippedInProgress,
    Failed,
}

impl IntegrityRunOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::SkippedInProgress => "skipped_in_progress",
            Self::Failed => "failed",
        }
    }
}

/// `phase` label on `am.hierarchy_integrity_duration`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityPhase {
    Check,
    Repair,
}

impl IntegrityPhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Repair => "repair",
        }
    }
}

/// `bucket` label on `am.hierarchy_integrity_repaired`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityBucket {
    Repaired,
    Deferred,
}

impl IntegrityBucket {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repaired => "repaired",
            Self::Deferred => "deferred",
        }
    }
}

/// `event` label on `am.integrity_lock_events`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityLockEvent {
    EvictedBySweep,
}

impl IntegrityLockEvent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EvictedBySweep => "evicted_by_sweep",
        }
    }
}

// ── am.tenant_retention ─────────────────────────────────────────────

/// `job` label on `am.tenant_retention`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantRetentionJob {
    ProvisioningReaper,
    HardDelete,
    SagaCompensation,
}

impl TenantRetentionJob {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProvisioningReaper => "provisioning_reaper",
            Self::HardDelete => "hard_delete",
            Self::SagaCompensation => "saga_compensation",
        }
    }
}

/// `outcome` label on `am.tenant_retention`.
///
/// Sealed newtype: literal call-site outcomes as `pub const` plus a
/// `From<HardDeleteOutcome>` bridge whose
/// [`HardDeleteOutcome::as_metric_label`] owns the per-variant
/// mapping. Sources that compute the outcome string via local helpers
/// (e.g. saga compensation paths) construct via the appropriate
/// constant; no free-form `&str` constructor is exposed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantRetentionOutcome(&'static str);

impl TenantRetentionOutcome {
    pub const CLAIM_CLEAR_FAILED: Self = Self("claim_clear_failed");
    pub const COMPENSATE_FAILED: Self = Self("compensate_failed");
    pub const IDP_UNCONFIRMED: Self = Self("idp_unconfirmed");
    pub const PARK_FAILED: Self = Self("park_failed");
    pub const RETRYABLE: Self = Self("retryable");
    pub const SCAN_FAILED: Self = Self("scan_failed");
    pub const TERMINAL: Self = Self("terminal");
    pub const TERMINAL_LOST_CLAIM: Self = Self("terminal_lost_claim");
    pub const TERMINAL_MARK_FAILED: Self = Self("terminal_mark_failed");
    pub const UNKNOWN: Self = Self("unknown");
    pub const UNSUPPORTED_REQUIRED: Self = Self("unsupported_required");

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl From<&HardDeleteOutcome> for TenantRetentionOutcome {
    fn from(o: &HardDeleteOutcome) -> Self {
        Self(o.as_metric_label())
    }
}

// ── am.serializable_retry ───────────────────────────────────────────

/// `outcome` label on `am.serializable_retry`. The repo helper emits
/// only a single terminal outcome today; further variants land
/// alongside their call sites.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializableRetryOutcome {
    Exhausted,
}

impl SerializableRetryOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exhausted => "exhausted",
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Port traits — one per subdomain
// ════════════════════════════════════════════════════════════════════
//
//  Method names mirror the family path (`am.<family>`) with the
//  dot dropped. Counter methods have no return-typed result and no
//  numeric argument; gauge methods carry the value (`i64` for counts,
//  `i64` epoch-seconds for timestamps); histogram methods carry the
//  observation (`f64`, with the unit fixed by the family — `ms` for
//  duration).

/// `am.bootstrap_lifecycle` — root-tenant bootstrap saga telemetry.
///
/// See [`crate::domain::metrics::AM_BOOTSTRAP_LIFECYCLE`] for the
/// catalog entry. `classification` is optional because ~10 of today's
/// call sites emit only `(phase, outcome)` — preserving the existing
/// shape during migration.
pub trait BootstrapMetricsPort: Send + Sync + 'static {
    fn bootstrap_lifecycle(
        &self,
        phase: BootstrapPhase,
        outcome: BootstrapOutcome,
        classification: Option<BootstrapClassification>,
    );
}

/// `am.conversion_lifecycle` — mode-conversion request transitions.
///
/// See [`crate::domain::metrics::AM_CONVERSION_LIFECYCLE`].
pub trait ConversionMetricsPort: Send + Sync + 'static {
    fn conversion_lifecycle(&self, op: ConversionOp, outcome: ConversionOutcome);
}

/// `am.dependency_health` — outbound dependency-call health
/// (`IdP` / Resource Group / GTS / `AuthZ` / Types Registry / metadata upsert).
///
/// See [`crate::domain::metrics::AM_DEPENDENCY_HEALTH`]. `outcome` is
/// optional because one call site (`infra/types_registry/checker.rs`)
/// emits only `(op, target)` today — preserved by `None`.
pub trait DependencyMetricsPort: Send + Sync + 'static {
    fn dependency_health(
        &self,
        op: DependencyOp,
        target: DependencyTarget,
        outcome: Option<DependencyOutcome>,
    );
}

/// `am.hierarchy_integrity_*` — periodic integrity-check telemetry.
///
/// Covers the eight families
/// (`runs`, `repair_runs`, `duration`, `last_success`, `last_failure`,
/// `violations`, `repaired`, `integrity_lock_events`) declared in
/// [`crate::domain::metrics`]. Kept on one trait so the integrity
/// service receives a single port.
pub trait IntegrityMetricsPort: Send + Sync + 'static {
    fn hierarchy_integrity_runs(&self, outcome: IntegrityRunOutcome);
    fn hierarchy_integrity_repair_runs(&self, outcome: IntegrityRunOutcome);
    fn hierarchy_integrity_duration_ms(&self, phase: IntegrityPhase, millis: f64);
    fn hierarchy_integrity_last_success(&self, epoch_seconds: i64);
    fn hierarchy_integrity_last_failure(&self, outcome: IntegrityRunOutcome, epoch_seconds: i64);
    fn hierarchy_integrity_violations(&self, category: IntegrityCategory, count: i64);
    fn hierarchy_integrity_repaired(
        &self,
        category: IntegrityCategory,
        bucket: IntegrityBucket,
        count: i64,
    );
    fn integrity_lock_event(&self, event: IntegrityLockEvent);
}

/// `am.metadata_resolution` — tenant-metadata resolution operations.
///
/// See [`crate::domain::metrics::AM_METADATA_RESOLUTION`]. Declared
/// today with no live emitters; the trait reserves the surface so
/// the future repo-side `metadata` adapter can fill in without
/// reshaping DI.
pub trait MetadataMetricsPort: Send + Sync + 'static {}

/// Tenant-lifecycle telemetry — retention, hierarchy-depth thresholds,
/// invalid-window detection, and cross-tenant denials.
///
/// Covers four families:
/// `am.tenant_retention`, `am.retention.invalid_window`,
/// `am.hierarchy_depth_exceedance`, `am.cross_tenant_denial`.
pub trait TenantMetricsPort: Send + Sync + 'static {
    fn tenant_retention(&self, job: TenantRetentionJob, outcome: TenantRetentionOutcome);

    fn retention_invalid_window(&self);

    /// `threshold` is the configured numeric depth limit. The adapter
    /// renders it to a stable string at emit time. Cardinality is
    /// bounded by config-cardinality (one value per deployment), not
    /// per-request.
    fn hierarchy_depth_exceedance(
        &self,
        mode: HierarchyDepthMode,
        outcome: HierarchyDepthOutcome,
        threshold: u32,
    );

    /// Declared today with no live emitters; the trait reserves the
    /// surface for the AuthZ-side cross-tenant denial signal.
    fn cross_tenant_denial(&self);
}

/// Repo-helper telemetry. Hosts `am.serializable_retry`; reserved for
/// further storage-floor metrics that are not tenant- or
/// integrity-specific.
pub trait StorageMetricsPort: Send + Sync + 'static {
    fn serializable_retry(&self, outcome: SerializableRetryOutcome);
}

// ════════════════════════════════════════════════════════════════════
//  No-op default implementation
// ════════════════════════════════════════════════════════════════════

/// No-op implementation of every port. Used:
///
/// * As the safe pre-init default before [`crate::module`] constructs
///   the real adapter.
/// * As the in-test default for services whose unit tests do not
///   assert on metrics.
///
/// Zero-sized; `Arc<NoopMetrics>` shares cheaply.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl BootstrapMetricsPort for NoopMetrics {
    fn bootstrap_lifecycle(
        &self,
        _phase: BootstrapPhase,
        _outcome: BootstrapOutcome,
        _classification: Option<BootstrapClassification>,
    ) {
    }
}

impl ConversionMetricsPort for NoopMetrics {
    fn conversion_lifecycle(&self, _op: ConversionOp, _outcome: ConversionOutcome) {}
}

impl DependencyMetricsPort for NoopMetrics {
    fn dependency_health(
        &self,
        _op: DependencyOp,
        _target: DependencyTarget,
        _outcome: Option<DependencyOutcome>,
    ) {
    }
}

impl IntegrityMetricsPort for NoopMetrics {
    fn hierarchy_integrity_runs(&self, _outcome: IntegrityRunOutcome) {}
    fn hierarchy_integrity_repair_runs(&self, _outcome: IntegrityRunOutcome) {}
    fn hierarchy_integrity_duration_ms(&self, _phase: IntegrityPhase, _millis: f64) {}
    fn hierarchy_integrity_last_success(&self, _epoch_seconds: i64) {}
    fn hierarchy_integrity_last_failure(&self, _outcome: IntegrityRunOutcome, _epoch_seconds: i64) {
    }
    fn hierarchy_integrity_violations(&self, _category: IntegrityCategory, _count: i64) {}
    fn hierarchy_integrity_repaired(
        &self,
        _category: IntegrityCategory,
        _bucket: IntegrityBucket,
        _count: i64,
    ) {
    }
    fn integrity_lock_event(&self, _event: IntegrityLockEvent) {}
}

impl MetadataMetricsPort for NoopMetrics {}

impl TenantMetricsPort for NoopMetrics {
    fn tenant_retention(&self, _job: TenantRetentionJob, _outcome: TenantRetentionOutcome) {}
    fn retention_invalid_window(&self) {}
    fn hierarchy_depth_exceedance(
        &self,
        _mode: HierarchyDepthMode,
        _outcome: HierarchyDepthOutcome,
        _threshold: u32,
    ) {
    }
    fn cross_tenant_denial(&self) {}
}

impl StorageMetricsPort for NoopMetrics {
    fn serializable_retry(&self, _outcome: SerializableRetryOutcome) {}
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod metrics_tests;
