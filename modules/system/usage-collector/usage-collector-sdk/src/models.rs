use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Kind of numeric usage observation (gauge vs counter).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema,
)]
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

/// Subject context attached to a usage record.
///
/// Grouping `id` and `kind` into one struct ensures the two fields are either
/// both present or both absent, preventing the inconsistent state where only
/// one is `Some`. When `None`, PDP authorization for the subject is skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubjectRef {
    /// Identifier of the subject (user or service) performing the metered action.
    pub id: Uuid,
    /// Logical type of the subject (e.g. GTS id or domain name).
    pub kind: String,
}

/// A single usage record submitted to the collector.
///
/// All fields are public for direct construction, serde, and tests.
/// For emission from source modules, use the `usage-emitter` crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub value: f64,
    /// Identifier of the metered resource instance.
    pub resource_id: Uuid,
    /// Logical type of the metered resource (e.g. GTS id or domain name).
    pub resource_type: String,
    /// Subject context. `None` when no subject is available; PDP validation is skipped.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subject: Option<SubjectRef>,
    /// Idempotency key for at-least-once delivery.
    ///
    /// Using `Uuid` rather than `String` is intentional: the type communicates the
    /// expected format, prevents typos, and avoids an allocation at the call site.
    pub idempotency_key: Uuid,
    /// Timestamp of the observation.
    pub timestamp: DateTime<Utc>,
    /// Optional caller-supplied metadata (max 8 192 bytes serialized).
    /// Absent when not provided; serializes as absent JSON field, not `null`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metadata: Option<serde_json::Value>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;
