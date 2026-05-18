//! AM observability metric catalog.
//!
//! Declares the AM metric families from PRD §5.9 / FEATURE §5 "Metric
//! Catalog". Metric constants and [`MetricKind`] were previously carried
//! by the SDK's `metric_names` module; they are defined here so the
//! runtime crate is self-contained and peer SDKs do not expose metric
//! constants (see `resource-group-sdk`, `tenant-resolver-sdk`).
//!
//! ## Emission pipeline
//!
//! Existing call sites emit through the stringly-typed helpers
//! [`emit_metric`], [`emit_gauge_value`], and [`emit_histogram_value`].
//! When the infra adapter has installed a [`MetricsFacadeBridge`] via
//! [`install_facade_bridge`] (done in [`crate::module`] init), emissions
//! are forwarded to OpenTelemetry instruments. Before installation —
//! and in tests that do not wire the adapter — the helpers are silent
//! no-ops, preserving the pre-init posture.
//!
//! The bridge is a **transitional surface**: typed port traits in
//! [`crate::domain::ports::metrics`] are the long-term API. Per-subdomain
//! call-site migration onto the typed ports proceeds family-by-family;
//! the bridge and the [`emit_*`] helpers are removed in the final
//! cleanup PR once every call site has moved.

use std::sync::Arc;
use std::sync::LazyLock;

use arc_swap::ArcSwap;
use modkit_macros::domain_model;

// @cpt-begin:cpt-cf-account-management-dod-errors-observability-metric-catalog:p1:inst-dod-metric-catalog-constants
/// Dependency-call health: `IdP` / Resource Group / GTS / `AuthZ` outbound calls.
pub const AM_DEPENDENCY_HEALTH: &str = "am.dependency_health";

/// Tenant-metadata resolution operations and inheritance policy outcomes.
pub const AM_METADATA_RESOLUTION: &str = "am.metadata_resolution";

/// Root-tenant bootstrap lifecycle (phase transitions, IdP-wait timeouts).
pub const AM_BOOTSTRAP_LIFECYCLE: &str = "am.bootstrap_lifecycle";

/// Provisioning reaper / hard-delete / deprovision background job telemetry.
pub const AM_TENANT_RETENTION: &str = "am.tenant_retention";

/// Invalid retention-window configuration encountered while evaluating due-ness.
pub const AM_RETENTION_INVALID_WINDOW: &str = "am.retention.invalid_window";

/// Mode-conversion request transitions and outcomes.
pub const AM_CONVERSION_LIFECYCLE: &str = "am.conversion_lifecycle";

/// Hierarchy-depth threshold exceedance (warning-band + hard-limit rejects).
pub const AM_HIERARCHY_DEPTH_EXCEEDANCE: &str = "am.hierarchy_depth_exceedance";

/// Cross-tenant denial counter (security-alert candidate family).
pub const AM_CROSS_TENANT_DENIAL: &str = "am.cross_tenant_denial";

/// Hierarchy-integrity violation telemetry (one per integrity category).
pub const AM_HIERARCHY_INTEGRITY_VIOLATIONS: &str = "am.hierarchy_integrity_violations";

/// Periodic integrity-check job tick outcome (`outcome` ∈ `completed` |
/// `skipped_in_progress` | `failed`). Distinguishes "no violations
/// because the check ran cleanly" from "no violations because the job
/// hasn't run successfully" — the latter is invisible from
/// [`AM_HIERARCHY_INTEGRITY_VIOLATIONS`] alone (which would just keep
/// reporting stale-zero gauges).
///
/// **Outcome label set is fixed**: dashboards keyed on this counter
/// rely on the three values above. Auto-repair tick outcomes live on
/// [`AM_HIERARCHY_INTEGRITY_REPAIR_RUNS`] instead so this counter's
/// label set stays stable across releases.
pub const AM_HIERARCHY_INTEGRITY_RUNS: &str = "am.hierarchy_integrity_runs";

/// Periodic auto-repair tick outcome (`outcome` ∈ `completed` |
/// `skipped_in_progress` | `failed`). Sister metric to
/// [`AM_HIERARCHY_INTEGRITY_RUNS`] kept on its own family so the
/// check-loop counter's documented label set is not silently widened
/// when auto-repair lands. Dashboards filter by family rather than
/// `outcome` prefix to avoid label-name collisions.
pub const AM_HIERARCHY_INTEGRITY_REPAIR_RUNS: &str = "am.hierarchy_integrity_repair_runs";

/// Periodic integrity-check tick wall-clock duration in milliseconds.
/// The `phase` label disaggregates the check phase (`phase = "check"`)
/// from the chained auto-repair phase (`phase = "repair"`) so
/// dashboards can tell a slow check from a slow check + repair.
/// Drives capacity-planning alerts ("p95 > 60s"), distinct from
/// [`AM_HIERARCHY_INTEGRITY_RUNS`] which is a tick-outcome counter.
pub const AM_HIERARCHY_INTEGRITY_DURATION: &str = "am.hierarchy_integrity_duration";

/// Unix-epoch seconds of the last successful integrity-check tick.
/// Used for a freshness watchdog (alert when `last_success` is older
/// than twice the configured interval) that the violation gauge
/// cannot satisfy on its own — a stuck job and a perfectly-clean tree
/// look identical at the violation-gauge level until this gauge stops
/// advancing.
pub const AM_HIERARCHY_INTEGRITY_LAST_SUCCESS: &str = "am.hierarchy_integrity_last_success";

/// Unix-epoch seconds of the last integrity-check tick that did NOT
/// complete successfully (gate-conflict or generic error). Sister
/// gauge to [`AM_HIERARCHY_INTEGRITY_LAST_SUCCESS`]: an alert wired
/// to "`LAST_SUCCESS` older than threshold" alone cannot tell
/// "sustained-failure-since-Y" from "never-ran" because the success
/// gauge keeps the last good timestamp indefinitely. Emitting both
/// gauges from the loop's failure arms lets operators triage which
/// kind of staleness they're looking at.
pub const AM_HIERARCHY_INTEGRITY_LAST_FAILURE: &str = "am.hierarchy_integrity_last_failure";

/// Lock-lifecycle event counter for `integrity_check_runs`. Emitted
/// from [`crate::infra::storage::integrity::lock::release`] when the
/// release DELETE affects zero rows — the row this worker inserted
/// was reclaimed by a contender's stale-lock sweep, which means the
/// check or repair exceeded
/// [`crate::infra::storage::integrity::lock::MAX_LOCK_AGE`] AND a
/// peer raced in. Distinct from
/// [`AM_HIERARCHY_INTEGRITY_RUNS`] (which documents a fixed
/// scheduler-tick outcome set) so dashboards keyed on
/// `RUNS{outcome=*}` stay stable; this counter exists for
/// lock-health alerting.
pub const AM_INTEGRITY_LOCK_EVENTS: &str = "am.integrity_lock_events";

/// Hierarchy-integrity repair telemetry. Emits one gauge sample per
/// run with `category` ∈ all 10
/// [`IntegrityCategory`](crate::domain::tenant::integrity::IntegrityCategory)
/// values and `bucket` ∈ {`repaired`, `deferred`} so dashboards see a
/// stable shape across runs (zero-valued samples for categories that
/// did not appear). The five derivable categories carry counts only
/// in `bucket = repaired`; the five operator-triage categories carry
/// counts only in `bucket = deferred`.
pub const AM_HIERARCHY_INTEGRITY_REPAIRED: &str = "am.hierarchy_integrity_repaired";

/// SERIALIZABLE-isolation retry telemetry for the AM repo's
/// `with_serializable_retry` helper.
pub const AM_SERIALIZABLE_RETRY: &str = "am.serializable_retry";
// @cpt-end:cpt-cf-account-management-dod-errors-observability-metric-catalog:p1:inst-dod-metric-catalog-constants

/// Kinds of metric samples the emitter supports.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

impl MetricKind {
    /// Stable string tag used in emitted samples.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Transitional facade-bridge
// ════════════════════════════════════════════════════════════════════
//
// The bridge plugs the stringly-typed [`emit_metric`] /
// [`emit_gauge_value`] / [`emit_histogram_value`] helpers into the
// OpenTelemetry-backed adapter without requiring every call site to
// migrate at once. The adapter installs an implementation during
// module init; calls before installation are silent no-ops.
//
// Removed in the same PR that retires the last `emit_*` call site.

/// Forwarder used by the [`emit_*`] helpers to reach a real metrics
/// adapter without the domain layer depending on infra. The infra
/// adapter implements this trait; [`install_facade_bridge`] installs
/// the implementation at module-init time.
pub trait MetricsFacadeBridge: Send + Sync + 'static {
    /// Forward an [`emit_metric`] call (counter-only today; the helper
    /// rejects gauge / histogram kinds at the call site).
    fn emit(&self, family: &'static str, kind: MetricKind, labels: &[(&'static str, &str)]);

    /// Forward an [`emit_gauge_value`] call.
    fn emit_gauge(&self, family: &'static str, value: i64, labels: &[(&'static str, &str)]);

    /// Forward an [`emit_histogram_value`] call.
    fn emit_histogram(&self, family: &'static str, value: f64, labels: &[(&'static str, &str)]);
}

/// `Arc`-wrapped `dyn MetricsFacadeBridge`. Aliased to keep the
/// `ArcSwap<Option<_>>` parametrisation readable — `arc_swap`'s
/// `RefCnt` impl requires the inner type to be `Sized`, which a bare
/// `dyn Trait` is not, so we wrap it in an `Arc` *first* (Sized) and
/// then wrap that `Option` in `ArcSwap`.
type BridgeArc = Arc<dyn MetricsFacadeBridge>;

/// Process-wide bridge slot. `ArcSwap` gives lock-free reads on the
/// emit hot path and lets [`install_facade_bridge`] *replace* the
/// active bridge — needed when a test harness swaps the global meter
/// provider between AM module inits (the new adapter's instruments
/// are bound to the new provider; an unconditionally first-wins
/// `OnceLock` would freeze emissions on the stale instruments).
static FACADE_BRIDGE: LazyLock<ArcSwap<Option<BridgeArc>>> =
    LazyLock::new(|| ArcSwap::from(Arc::new(None)));

/// Install (or replace) the process-wide facade bridge. Called once
/// during AM module init; idempotent across re-inits — the most
/// recent installation wins. The bridge stays installed for the
/// lifetime of the process unless overwritten, matching the
/// `opentelemetry::global::set_meter_provider` posture which itself
/// supports overwrite.
///
/// Returns `true` if this call installed the *first* bridge, `false`
/// if a prior bridge was replaced. The boolean is informational —
/// callers can log on the rare "already installed" branch (parallel
/// module init in test harnesses, meter-provider hot-swap) but should
/// not treat it as an error.
pub fn install_facade_bridge(bridge: BridgeArc) -> bool {
    let prev = FACADE_BRIDGE.swap(Arc::new(Some(bridge)));
    prev.is_none()
}

/// Emit a metric sample (fire-and-forget).
///
/// Forwards to the installed [`MetricsFacadeBridge`] when one is
/// present; otherwise a silent no-op. Counter-only — gauge and
/// histogram families use [`emit_gauge_value`] / [`emit_histogram_value`].
#[inline]
pub fn emit_metric(family: &'static str, kind: MetricKind, labels: &[(&'static str, &str)]) {
    if let Some(bridge) = FACADE_BRIDGE.load().as_ref() {
        bridge.emit(family, kind, labels);
    }
}

/// Emit a value-carrying gauge sample (fire-and-forget).
#[inline]
pub fn emit_gauge_value(family: &'static str, value: i64, labels: &[(&'static str, &str)]) {
    if let Some(bridge) = FACADE_BRIDGE.load().as_ref() {
        bridge.emit_gauge(family, value, labels);
    }
}

/// Emit a value-carrying histogram sample (fire-and-forget).
#[inline]
pub fn emit_histogram_value(family: &'static str, value: f64, labels: &[(&'static str, &str)]) {
    if let Some(bridge) = FACADE_BRIDGE.load().as_ref() {
        bridge.emit_histogram(family, value, labels);
    }
}
