//! REST DTOs for the usage-collector gateway.

use chrono::{DateTime, FixedOffset, Utc};
use usage_collector_sdk::models::{
    AggregationFn, AggregationResult, BucketSize, GroupByDimension, Subject, UsageKind,
};
use usage_collector_sdk::{Page, UsageRecord};
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

/// Query parameters for `GET /usage-collector/v1/aggregated`.
///
/// `tenant_id` is NEVER accepted here — it is derived from `SecurityContext` only.
/// Datetime fields are parsed via [`parse_rfc3339_utc`], which rejects offset-aware
/// values with a deserialization error so the gateway returns `400 Bad Request`.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct AggregatedQueryParams {
    /// Aggregation function to apply.
    #[serde(rename = "fn")]
    pub fn_: AggregationFn,
    /// Start of the time range (RFC 3339 UTC, exclusive lower bound). Offset-aware
    /// values (e.g. `+05:00`) are rejected; only `Z` / `+00:00` are accepted.
    #[serde(deserialize_with = "deserialize_rfc3339_utc")]
    pub from: DateTime<Utc>,
    /// End of the time range (RFC 3339 UTC, exclusive upper bound). Offset-aware
    /// values are rejected.
    #[serde(deserialize_with = "deserialize_rfc3339_utc")]
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

/// Response body for `GET /usage-collector/v1/raw` — a single page of records
/// plus pagination metadata in `page_info`.
///
/// Thin newtype around [`Page<UsageRecord>`] introduced so the `OpenAPI` registry
/// can attach a `ResponseApiDto` schema; the SDK's `Page<T>` type is generic
/// across modkit-odata and does not carry that marker by itself.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct RawQueryResponse {
    /// Page contents (zero or more usage records).
    pub items: Vec<UsageRecord>,
    /// Pagination metadata: next/prev cursor + caller-supplied `limit`.
    pub page_info: PageInfoDto,
}

/// Pagination metadata DTO mirroring [`modkit_odata::PageInfo`].
///
/// Absence of `next_cursor` indicates the final page; absence of `prev_cursor`
/// indicates the first page. `limit` echoes the caller-supplied `page_size`.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct PageInfoDto {
    /// Opaque cursor for the next page; absent on the final page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Opaque cursor for the previous page; absent on the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_cursor: Option<String>,
    /// Page size limit echoed back from the request.
    pub limit: u64,
}

impl From<Page<UsageRecord>> for RawQueryResponse {
    fn from(p: Page<UsageRecord>) -> Self {
        // modkit-odata `PageInfo` already carries the cursors as opaque
        // base64-encoded strings, so the DTO mapping is a 1:1 copy.
        Self {
            items: p.items,
            page_info: PageInfoDto {
                next_cursor: p.page_info.next_cursor,
                prev_cursor: p.page_info.prev_cursor,
                limit: p.page_info.limit,
            },
        }
    }
}

/// One row in an aggregated query result.
///
/// Option fields are absent (not null) in JSON when the corresponding
/// `GroupByDimension` was not requested.
///
/// # Why this is a structural duplicate of [`AggregationResult`]
///
/// [`usage_collector_sdk::AggregationResult`] already derives `utoipa::ToSchema`
/// and carries the same fields with the same serde shape. The duplication exists
/// solely so this REST boundary can attach the `#[modkit_macros::api_dto(response)]`
/// marker that the in-tree `OpenAPI` registry consumes; that marker is a
/// gateway-side framework concern that does not belong on a plugin-facing SDK
/// type. Adding a new grouping dimension therefore requires three coordinated
/// edits — SDK struct, this DTO, and the `From` mapping below — by design, not
/// by accident. Changing the SDK without changing the DTO is a compile error,
/// so the duplication cannot silently drift. If the modkit marker is ever
/// liftable to the SDK behind a feature flag, or generatable via a macro, this
/// DTO should be removed in favor of re-exporting the SDK type.
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
/// Datetime fields are parsed via [`parse_rfc3339_utc`]; offset-aware values are rejected.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct RawQueryParams {
    /// Start of the time range (RFC 3339 UTC, exclusive lower bound). Offset-aware
    /// values are rejected.
    #[serde(deserialize_with = "deserialize_rfc3339_utc")]
    pub from: DateTime<Utc>,
    /// End of the time range (RFC 3339 UTC, exclusive upper bound). Offset-aware
    /// values are rejected.
    #[serde(deserialize_with = "deserialize_rfc3339_utc")]
    pub to: DateTime<Utc>,
    /// Opaque pagination cursor from a previous response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Number of records per page. Defaults to `DEFAULT_PAGE_SIZE`; must be in `[1, MAX_PAGE_SIZE]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
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

/// Parse an RFC 3339 timestamp into [`DateTime<Utc>`], rejecting any value
/// whose offset is not zero (i.e. only `Z` or `+00:00` are accepted).
///
/// The plain `serde` impl for [`DateTime<Utc>`] silently converts offset-aware
/// values to UTC; the CDSL (`inst-agg-3`/`inst-raw-3`) requires the gateway to
/// reject offset-aware datetimes with `400` so the caller knows the input was
/// not UTC. This helper enforces that policy at the deserialization boundary.
///
/// # Errors
///
/// Returns `Err` when the string does not parse as RFC 3339, or when the parsed
/// offset is non-zero.
fn parse_rfc3339_utc(s: &str) -> Result<DateTime<Utc>, String> {
    let parsed = DateTime::<FixedOffset>::parse_from_rfc3339(s)
        .map_err(|e| format!("invalid RFC 3339 datetime: {e}"))?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err("datetime must be in UTC (use 'Z' or '+00:00')".to_owned());
    }
    Ok(parsed.with_timezone(&Utc))
}

fn deserialize_rfc3339_utc<'de, D>(d: D) -> Result<DateTime<Utc>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct V;

    impl Visitor<'_> for V {
        type Value = DateTime<Utc>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an RFC 3339 UTC datetime string (offset 'Z' or '+00:00')")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<DateTime<Utc>, E> {
            parse_rfc3339_utc(v).map_err(E::custom)
        }
    }

    d.deserialize_str(V)
}

/// Deserialize `Vec<GroupByDimension>` from either a single string (`serde_urlencoded` delivers
/// query-param values as strings, not sequences) or a JSON array.
///
/// A single string is split on commas so that `group_by=resource,usage_type` also works.
/// Duplicate dimensions are dropped in input order (first occurrence wins): the plugin
/// contract does not document idempotent group-by, so out-of-tree backends could otherwise
/// emit duplicate columns or inflated row counts for a `?group_by=usage_type,usage_type`
/// request. Deduping here keeps the wire-level normalization in one place.
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
            let parsed: Result<Vec<GroupByDimension>, E> = v
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| {
                    let owned: String = s.trim().to_owned();
                    GroupByDimension::deserialize(owned.into_deserializer())
                })
                .collect();
            Ok(dedup_preserve_order(parsed?))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Vec<GroupByDimension>, A::Error> {
            let mut vec = Vec::new();
            while let Some(item) = seq.next_element()? {
                vec.push(item);
            }
            Ok(dedup_preserve_order(vec))
        }
    }

    d.deserialize_any(GroupByVecVisitor)
}

/// Drop later duplicates while preserving the first occurrence's position.
///
/// `GroupByDimension` is small (5 variants) so an `O(n²)` `contains` scan is
/// strictly cheaper than allocating a `HashSet`; the dimension list is also
/// bounded by 5 in practice.
fn dedup_preserve_order(items: Vec<GroupByDimension>) -> Vec<GroupByDimension> {
    let mut out: Vec<GroupByDimension> = Vec::with_capacity(items.len());
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}
