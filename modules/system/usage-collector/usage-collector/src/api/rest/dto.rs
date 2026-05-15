//! REST DTOs for the usage-collector gateway.

use chrono::{DateTime, Utc};
use usage_collector_sdk::models::{Subject, UsageKind};
use uuid::Uuid;

/// Request body to create one usage record (ingest).
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct CreateUsageRecordRequest {
    /// Name of the module emitting this record.
    pub module: String,
    /// Tenant that owns the record.
    pub tenant_id: Uuid,
    /// Logical type of the metered resource.
    pub resource_type: String,
    /// Identifier of the metered resource instance.
    pub resource_id: Uuid,
    /// Subject (user or service) performing the request.
    /// `None` when no subject context is available; PDP subject validation is skipped in that case.
    /// Wire shape is nested: `{ "subject": { "id": "...", "type": "user" } }`; the `subject` key
    /// is absent when there is no subject. A payload providing `type` without `id` is structurally
    /// unrepresentable and fails to decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Subject>,
    /// Metric name for this observation.
    pub metric: String,
    /// Idempotency key. Required for counter metrics (non-empty); optional for gauge metrics —
    /// when omitted for a gauge, the gateway generates a UUID while building the record. Counter
    /// records without a key are rejected with `422 Unprocessable Entity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Numeric value for this usage observation.
    pub value: f64,
    /// Observation timestamp (UTC).
    pub timestamp: DateTime<Utc>,
    /// Optional caller-supplied metadata. Serialized size MUST NOT exceed the
    /// per-deployment `max_metadata_bytes` limit (default `8192`, configurable in
    /// the `[modules.<module>]` server config in range `0..=1 MiB`; `0` disables
    /// metadata entirely). Callers can read the effective limit from
    /// `GET /usage-collector/v1/modules/{name}/config` (`max_metadata_bytes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// One allowed metric entry in a module config response.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct AllowedMetricResponse {
    /// Metric name.
    pub name: String,
    /// Gauge vs counter semantics.
    pub kind: UsageKind,
}

/// Response body for the get-module-config endpoint.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct ModuleConfigResponse {
    /// Metrics the module is allowed to emit.
    pub allowed_metrics: Vec<AllowedMetricResponse>,
    /// Maximum serialized size of `UsageRecord.metadata` enforced by the emitter (`0` disables metadata).
    pub max_metadata_bytes: u32,
}
