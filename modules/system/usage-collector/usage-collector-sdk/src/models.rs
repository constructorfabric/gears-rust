use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Kind of numeric usage observation (gauge vs counter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    Gauge,
    Counter,
}

/// A single allowed metric definition returned by [`crate::UsageCollectorClientV1::get_module_config`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AllowedMetric {
    /// Metric name.
    pub name: String,
    /// Gauge vs counter semantics for this metric.
    pub kind: UsageKind,
}

/// Per-module configuration returned by [`crate::UsageCollectorClientV1::get_module_config`].
///
/// Extensible: future fields may include rate limit config, max metadata size, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ModuleConfig {
    /// Metrics this module is allowed to emit.
    pub allowed_metrics: Vec<AllowedMetric>,
}

/// A single usage record submitted to the collector.
///
/// # Field visibility
///
/// All fields are public by design so that emitters, storage plugins, tests,
/// and serde can construct records directly without going through a builder.
/// This is a deliberate trade-off: the SDK keeps the construction surface
/// minimal, and the **emitter is the validation gateway** (see below). Source
/// modules SHOULD NOT construct [`UsageRecord`] directly in production code —
/// use the `usage-emitter` crate, which performs the validation listed below
/// before forwarding to the collector.
///
/// # Validation contract
///
/// This type is an unvalidated data carrier. It does NOT enforce:
///
/// - finite `value` — NaN / ±∞ are accepted by the Rust type. JSON has no
///   representation for non-finite floats, and `serde_json` with default
///   settings encodes them as JSON `null` rather than erroring. The encoded
///   payload then fails to deserialize back into [`UsageRecord`] (or is
///   rejected by the storage backend's schema) — so the operational signal
///   is a *decode-time* error at the next hop, not a clean emit failure.
///   Emitters MUST filter non-finite values before submission; do not rely
///   on the serializer to surface them.
/// - `metadata` size (the "max 8 192 bytes serialized" budget is advisory at
///   this layer);
/// - `idempotency_key` length or format.
///
/// Validation of these invariants is the **emitter's** responsibility — see
/// the `usage-emitter` crate. Storage plugins MAY perform additional defensive
/// checks but should not rely on this type to enforce them.
///
/// # Wire strictness
///
/// `#[serde(deny_unknown_fields)]` is applied: this is an authorization-relevant
/// payload, and silently dropping a typo such as `subject_idd` would downgrade
/// PDP scoping without a visible error. Adding a field to this struct is
/// therefore a wire-breaking change for any older deserializer — bump the
/// type's GTS version when extending the surface.
///
/// For emission from source modules, always use the `usage-emitter` crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageRecord {
    /// Name of the module that emitted this record.
    pub module: String,
    /// Tenant that owns this usage observation.
    pub tenant_id: Uuid,
    /// Metric name for this observation.
    pub metric: String,
    /// Gauge vs counter semantics.
    pub kind: UsageKind,
    /// Numeric value for this usage observation.
    ///
    /// NaN / ±∞ are accepted by this type. JSON has no encoding for non-finite
    /// floats: `serde_json` silently emits them as `null`, which then fails to
    /// round-trip back into `f64`. Emitters MUST filter non-finite values
    /// before submission — see the type-level "Validation contract" section.
    pub value: f64,
    /// Identifier of the metered resource instance.
    pub resource_id: Uuid,
    /// Logical type of the metered resource (e.g. GTS id or domain name).
    pub resource_type: String,
    /// Identifier of the subject (user or service) performing the metered action.
    /// `None` when no subject context is available; PDP validation is skipped in that case.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subject_id: Option<Uuid>,
    /// Logical type of the subject (e.g. GTS id or domain name).
    /// `None` when no subject context is available.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subject_type: Option<String>,
    /// Idempotency key for at-least-once delivery.
    ///
    /// Length and format constraints (typically a UUID v4 string) are
    /// enforced by the emitter, not by this type.
    pub idempotency_key: String,
    /// Timestamp of the observation.
    pub timestamp: DateTime<Utc>,
    /// Optional caller-supplied metadata. The 8 192-byte serialized-size
    /// budget is advisory at this layer — emitters enforce it before
    /// submission.
    /// Absent when not provided; serializes as absent JSON field, not `null`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metadata: Option<serde_json::Value>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;
