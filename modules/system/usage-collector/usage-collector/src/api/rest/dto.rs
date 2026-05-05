//! REST DTOs for the usage-collector gateway.

use chrono::{DateTime, Utc};
use usage_collector_sdk::models::{
    AggregationFn, AggregationResult, BucketSize, GroupByDimension, UsageKind,
};
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
    /// Identifier of the subject (user/service account) performing the request.
    /// `None` when no subject context is available; PDP subject validation is skipped in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<Uuid>,
    /// Type of the subject (e.g. `"user"`, `"service_account"`).
    /// `None` when no subject context is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    /// Metric name for this observation.
    pub metric: String,
    /// Optional idempotency key; if omitted, one is generated when building the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Numeric value for this usage observation.
    pub value: f64,
    /// Observation timestamp (UTC).
    pub timestamp: DateTime<Utc>,
    /// Optional caller-supplied metadata. Serialized size MUST NOT exceed 8 192 bytes.
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
}

/// Query parameters for `GET /usage-collector/v1/aggregated`.
///
/// `tenant_id` is NEVER accepted here — it is derived from `SecurityContext` only.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct AggregatedQueryParams {
    /// Aggregation function to apply.
    #[serde(rename = "fn")]
    pub fn_: AggregationFn,
    /// Start of the time range (RFC 3339 UTC, exclusive lower bound).
    pub from: DateTime<Utc>,
    /// End of the time range (RFC 3339 UTC, exclusive upper bound).
    pub to: DateTime<Utc>,
    /// Dimensions to group by.
    #[serde(default, deserialize_with = "deserialize_group_by")]
    pub group_by: Vec<GroupByDimension>,
    /// Required when `group_by` includes `TimeBucket`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_size: Option<BucketSize>,
    /// Filter by usage type. Max length: `MAX_FILTER_STRING_LEN` bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_type: Option<String>,
    /// Filter by subject UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<Uuid>,
    /// Filter by subject type string. Max length: `MAX_FILTER_STRING_LEN` bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    /// Filter by resource UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<Uuid>,
    /// Filter by resource type string. Max length: `MAX_FILTER_STRING_LEN` bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// Filter by source module. Max length: `MAX_FILTER_STRING_LEN` bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Response type alias for `GET /usage-collector/v1/aggregated`.
#[allow(dead_code)]
pub type AggregationResultResponse = Vec<AggregationResultDto>;

/// One row in an aggregated query result.
///
/// Option fields are absent (not null) in JSON when the corresponding
/// `GroupByDimension` was not requested.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct AggregationResultDto {
    pub function: AggregationFn,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl From<AggregationResult> for AggregationResultDto {
    fn from(r: AggregationResult) -> Self {
        Self {
            function: r.function,
            value: r.value,
            bucket_start: r.bucket_start,
            usage_type: r.usage_type,
            subject_id: r.subject_id,
            subject_type: r.subject_type,
            resource_id: r.resource_id,
            resource_type: r.resource_type,
            source: r.source,
        }
    }
}

/// Query parameters for `GET /usage-collector/v1/raw`.
///
/// `tenant_id` is NEVER accepted here — it is derived from `SecurityContext` only.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct RawQueryParams {
    /// Start of the time range (RFC 3339 UTC, exclusive lower bound).
    pub from: DateTime<Utc>,
    /// End of the time range (RFC 3339 UTC, exclusive upper bound).
    pub to: DateTime<Utc>,
    /// Opaque pagination cursor from a previous response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Number of records per page. Defaults to `DEFAULT_PAGE_SIZE`; must be in `[1, MAX_PAGE_SIZE]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    /// Filter by usage type. Max length: `MAX_FILTER_STRING_LEN` bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_type: Option<String>,
    /// Filter by subject UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<Uuid>,
    /// Filter by subject type string. Max length: `MAX_FILTER_STRING_LEN` bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    /// Filter by resource UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<Uuid>,
    /// Filter by resource type string. Max length: `MAX_FILTER_STRING_LEN` bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
}

/// Deserialize `Vec<GroupByDimension>` from either a single string (`serde_urlencoded` delivers
/// query-param values as strings, not sequences) or a JSON array.
///
/// A single string is split on commas so that `group_by=resource,usage_type` also works.
fn deserialize_group_by<'de, D>(d: D) -> Result<Vec<GroupByDimension>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, IntoDeserializer, Visitor};

    struct GroupByVecVisitor;

    impl<'de> Visitor<'de> for GroupByVecVisitor {
        type Value = Vec<GroupByDimension>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a group-by dimension or comma-separated list of dimensions")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Vec<GroupByDimension>, E> {
            use serde::Deserialize as _;
            v.split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| {
                    let owned: String = s.trim().to_owned();
                    GroupByDimension::deserialize(owned.into_deserializer())
                })
                .collect()
        }

        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Vec<GroupByDimension>, A::Error> {
            let mut vec = Vec::new();
            while let Some(item) = seq.next_element()? {
                vec.push(item);
            }
            Ok(vec)
        }
    }

    d.deserialize_any(GroupByVecVisitor)
}
