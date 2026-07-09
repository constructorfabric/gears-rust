//! Wire DTOs for the foundation REST surface.
//!
//! Two resource families share this module:
//!
//! * `UsageType` catalog — per-row response body and the register-request DTO
//!   for the `/usage-collector/v1/usage-types` catalog routes. List-page
//!   envelopes use [`toolkit_odata::Page`] directly; `OData` query parameters
//!   (`limit`, `cursor`) are parsed by the toolkit `OData` extractor and need
//!   no module-local DTO.
//! * `UsageRecord` create / deactivation — batch create request /
//!   response shapes for `POST /usage-collector/v1/records`. Deactivation
//!   returns no body (HTTP 204 No Content) so it carries no response DTO.
//!
//! Every wire-facing type is declared as a thin DTO with
//! `#[toolkit_macros::api_dto(...)]` so the emitted OAS references a stable
//! schema component; the SDK models stay free of `utoipa::ToSchema` to keep
//! `utoipa` out of the plugin SDK's transitive deps.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use time::OffsetDateTime;
use toolkit_canonical_errors::Problem;
use usage_collector_sdk::{
    AggregationBucket, AggregationDimension, AggregationOp, AggregationResult, AggregationSpec,
    MetadataKey, ResourceRef, SubjectRef, UsageCollectorError, UsageKind, UsageRecord,
    UsageRecordStatus, UsageType,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// UsageType catalog DTOs
// ---------------------------------------------------------------------------

/// Wire projection of [`usage_collector_sdk::UsageType`]. `gts_id` is
/// flattened to `String` so the type can derive `utoipa::ToSchema`
/// without pulling `utoipa` into the SDK crate (the SDK's
/// `UsageTypeGtsId` newtype carries the validation semantics). `kind` is
/// projected to its lowercase string form (`"counter"` / `"gauge"`) for
/// the same reason — `UsageKind`'s closed-enum serde shape lives in the
/// SDK; the host-side wire DTO mirrors it via `String`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct UsageTypeDto {
    pub gts_id: String,
    pub kind: String,
    pub metadata_fields: Vec<String>,
}

impl From<UsageType> for UsageTypeDto {
    fn from(value: UsageType) -> Self {
        Self {
            gts_id: value.gts_id.to_string(),
            kind: match value.kind {
                UsageKind::Counter => "counter".to_owned(),
                UsageKind::Gauge => "gauge".to_owned(),
            },
            metadata_fields: value
                .metadata_fields
                .into_iter()
                .map(MetadataKey::into_inner)
                .collect(),
        }
    }
}

/// Register-request body for `POST /usage-collector/v1/usage-types`.
///
/// Carries `gts_id` as a permissive `String` rather than the validating
/// [`usage_collector_sdk::UsageTypeGtsId`] newtype so the handler can
/// synthesise the canonical `invalid_base_gts_id` `Problem` envelope on
/// rejection — relying on the newtype's `Deserialize` would surface
/// bad-base payloads as axum's default `text/plain` 422. `kind` is the
/// closed counter / gauge discriminator carried as `String` for the same
/// `utoipa`-isolation reason as `gts_id`; the handler parses it through
/// the SDK [`usage_collector_sdk::UsageKind`] enum so unknown values are
/// rejected (per ADR-0012 amendment 2026-06-08).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct CreateUsageTypeRequest {
    pub gts_id: String,
    pub kind: String,
    pub metadata_fields: Vec<String>,
}

// ---------------------------------------------------------------------------
// UsageRecord create DTOs
// ---------------------------------------------------------------------------

/// Wire-projection of [`usage_collector_sdk::ResourceRef`]. Mirrors the SDK
/// shape verbatim; the SDK struct stays `utoipa`-free.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct ResourceRefDto {
    pub resource_id: String,
    pub resource_type: String,
}

impl From<ResourceRef> for ResourceRefDto {
    fn from(value: ResourceRef) -> Self {
        Self {
            resource_id: value.resource_id().to_owned(),
            resource_type: value.resource_type().to_owned(),
        }
    }
}

impl TryFrom<ResourceRefDto> for ResourceRef {
    type Error = UsageCollectorError;

    fn try_from(value: ResourceRefDto) -> Result<Self, Self::Error> {
        ResourceRef::new(value.resource_id, value.resource_type)
    }
}

/// Wire-projection of [`usage_collector_sdk::SubjectRef`].
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct SubjectRefDto {
    pub subject_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
}

impl From<SubjectRef> for SubjectRefDto {
    fn from(value: SubjectRef) -> Self {
        Self {
            subject_id: value.subject_id().to_owned(),
            subject_type: value.subject_type().map(str::to_owned),
        }
    }
}

impl TryFrom<SubjectRefDto> for SubjectRef {
    type Error = UsageCollectorError;

    fn try_from(value: SubjectRefDto) -> Result<Self, Self::Error> {
        SubjectRef::new(value.subject_id, value.subject_type)
    }
}

/// Per-record create payload. Carries `gts_id` as a permissive `String`
/// (same rationale as [`CreateUsageTypeRequest`]) so a bad-prefix value
/// surfaces as the per-record `Problem` instead of axum's default
/// `text/plain` 422 for the entire batch. per-record problem envelopes
/// still surface for closed-shape membership, size-cap, and key
/// validation.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct CreateUsageRecordRequest {
    pub uuid: Uuid,
    pub gts_id: String,
    pub tenant_id: Uuid,
    pub resource_ref: ResourceRefDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<SubjectRefDto>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(with = "rust_decimal::serde::str")]
    pub value: Decimal,
    /// Mandatory caller-supplied idempotency key per
    /// `cpt-cf-usage-collector-dod-usage-emission-fr-idempotency`. The
    /// plugin SPI dedups every persisted record on
    /// `(tenant_id, usage_type_gts_id, idempotency_key)`; a missing key
    /// surfaces as a request-deserialization failure.
    pub idempotency_key: String,
    /// When set, marks this submission as a counter compensation
    /// referencing a previously emitted ordinary usage row. Absent on
    /// ordinary submissions. The four-cell value matrix and L1 referential
    /// rule are enforced at the gateway per
    /// `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects_id: Option<Uuid>,
    /// Caller-supplied measurement timestamp (RFC 3339 UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Batch create request body for `POST /usage-collector/v1/records`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct CreateUsageRecordsRequest {
    pub records: Vec<CreateUsageRecordRequest>,
}

/// Wire-projection of [`usage_collector_sdk::UsageRecord`]. `gts_id` is
/// flattened to `String` (same rationale as [`UsageTypeDto`]) so the type
/// can derive `utoipa::ToSchema` without pulling `utoipa` into the SDK
/// crate; `created_at` is emitted as RFC 3339 to match the SDK wire shape.
/// `status` is projected to its lowercase string form for the same reason.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct UsageRecordDto {
    pub uuid: Uuid,
    pub gts_id: String,
    pub tenant_id: Uuid,
    pub resource_ref: ResourceRefDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<SubjectRefDto>,
    /// Closed-shape key/value map per the OAS `RecordMetadata` schema.
    /// Omitted from the wire when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(with = "rust_decimal::serde::str")]
    pub value: Decimal,
    /// Mandatory caller-supplied idempotency key per
    /// `cpt-cf-usage-collector-dod-usage-emission-fr-idempotency`. Every
    /// persisted record carries a non-empty key.
    pub idempotency_key: String,
    /// Present when this record is a counter compensation referencing a
    /// previously emitted ordinary usage row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects_id: Option<Uuid>,
    /// Lifecycle status: `"active"` on a fresh insert, `"inactive"` after a
    /// depth-1 deactivation cascade.
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<UsageRecord> for UsageRecordDto {
    fn from(value: UsageRecord) -> Self {
        Self {
            uuid: value.uuid,
            gts_id: value.gts_id.to_string(),
            tenant_id: value.tenant_id,
            resource_ref: value.resource_ref.into(),
            subject_ref: value.subject_ref.map(Into::into),
            metadata: value
                .metadata
                .into_iter()
                .map(|(k, v)| (k.into_inner(), v))
                .collect(),
            value: value.value,
            idempotency_key: value.idempotency_key.into_inner(),
            corrects_id: value.corrects_id,
            status: match value.status {
                UsageRecordStatus::Active => "active".to_owned(),
                UsageRecordStatus::Inactive => "inactive".to_owned(),
            },
            created_at: value.created_at,
        }
    }
}

/// Per-record outcome inside [`CreateUsageRecordsResponse`]. Externally
/// tagged on `outcome` (snake-case via the `api_dto`-applied `rename_all`)
/// so accepted and rejected records share a uniform envelope keyed by
/// input order. An accepted entry carries the persisted record body; a
/// rejected entry carries the per-record canonical `Problem` body
/// verbatim — the same envelope a single-record failure would surface,
/// folded into the batch response.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
#[serde(tag = "outcome")]
pub enum CreateUsageRecordResultDto {
    Accepted {
        index: usize,
        record: UsageRecordDto,
    },
    Rejected {
        index: usize,
        error: Problem,
    },
}

/// Batch create response body. `results` preserves input order; a partial
/// failure surfaces as HTTP `207 Multi-Status` with per-record `Rejected`
/// entries (callers must inspect each `outcome`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct CreateUsageRecordsResponse {
    pub results: Vec<CreateUsageRecordResultDto>,
}

// ---------------------------------------------------------------------------
// Aggregated-query DTOs
// ---------------------------------------------------------------------------

/// Wire projection of [`usage_collector_sdk::AggregationOp`]. Identical
/// lowercase encoding — the wrapper exists so the OAS can pin the closed
/// enum schema without pulling `utoipa` into the SDK. (The macro-applied
/// `rename_all = "snake_case"` collapses to the same single-token form
/// as the SDK's `rename_all = "lowercase"` for these variants.)
#[derive(Debug, Clone, Copy)]
#[toolkit_macros::api_dto(request, response)]
pub enum AggregationOpDto {
    Sum,
    Count,
    Min,
    Max,
    Avg,
}

impl From<AggregationOpDto> for AggregationOp {
    fn from(value: AggregationOpDto) -> Self {
        match value {
            AggregationOpDto::Sum => AggregationOp::Sum,
            AggregationOpDto::Count => AggregationOp::Count,
            AggregationOpDto::Min => AggregationOp::Min,
            AggregationOpDto::Max => AggregationOp::Max,
            AggregationOpDto::Avg => AggregationOp::Avg,
        }
    }
}

impl From<AggregationOp> for AggregationOpDto {
    fn from(value: AggregationOp) -> Self {
        match value {
            AggregationOp::Sum => AggregationOpDto::Sum,
            AggregationOp::Count => AggregationOpDto::Count,
            AggregationOp::Min => AggregationOpDto::Min,
            AggregationOp::Max => AggregationOpDto::Max,
            AggregationOp::Avg => AggregationOpDto::Avg,
        }
    }
}

/// Wire projection of [`usage_collector_sdk::AggregationDimension`]. The
/// closed dimensions are encoded as snake-case bare strings; the
/// `metadata` form carries the declared key inline as
/// `{"metadata": "<key>"}` to mirror the SDK's `Metadata(MetadataKey)`
/// variant under the same lowercased external tag. (The macro-applied
/// `rename_all = "snake_case"` matches the SDK encoding here.)
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub enum AggregationDimensionDto {
    TenantId,
    ResourceId,
    ResourceType,
    SubjectId,
    SubjectType,
    Metadata(String),
}

impl TryFrom<AggregationDimensionDto> for AggregationDimension {
    type Error = UsageCollectorError;

    fn try_from(value: AggregationDimensionDto) -> Result<Self, Self::Error> {
        Ok(match value {
            AggregationDimensionDto::TenantId => AggregationDimension::TenantId,
            AggregationDimensionDto::ResourceId => AggregationDimension::ResourceId,
            AggregationDimensionDto::ResourceType => AggregationDimension::ResourceType,
            AggregationDimensionDto::SubjectId => AggregationDimension::SubjectId,
            AggregationDimensionDto::SubjectType => AggregationDimension::SubjectType,
            AggregationDimensionDto::Metadata(key) => {
                AggregationDimension::Metadata(MetadataKey::new(key)?)
            }
        })
    }
}

impl From<AggregationDimension> for AggregationDimensionDto {
    fn from(value: AggregationDimension) -> Self {
        match value {
            AggregationDimension::TenantId => AggregationDimensionDto::TenantId,
            AggregationDimension::ResourceId => AggregationDimensionDto::ResourceId,
            AggregationDimension::ResourceType => AggregationDimensionDto::ResourceType,
            AggregationDimension::SubjectId => AggregationDimensionDto::SubjectId,
            AggregationDimension::SubjectType => AggregationDimensionDto::SubjectType,
            AggregationDimension::Metadata(key) => {
                AggregationDimensionDto::Metadata(key.into_inner())
            }
        }
    }
}

/// Aggregated-query request body for
/// `POST /usage-collector/v1/records/aggregate`. The typed `gts_id`, the
/// `OData` `$filter`, and the `metadata.<key>` side-channel remain query
/// parameters (mirroring `GET /usage-collector/v1/records`); only the
/// aggregation spec (operator + group-by dimensions) ships in the body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct QueryAggregatedUsageRecordsRequest {
    pub op: AggregationOpDto,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<AggregationDimensionDto>,
}

impl TryFrom<QueryAggregatedUsageRecordsRequest> for AggregationSpec {
    type Error = UsageCollectorError;

    fn try_from(value: QueryAggregatedUsageRecordsRequest) -> Result<Self, Self::Error> {
        let group_by = value
            .group_by
            .into_iter()
            .map(AggregationDimension::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AggregationSpec {
            op: value.op.into(),
            group_by,
        })
    }
}

/// Wire projection of [`usage_collector_sdk::AggregationBucket`]. `value`
/// is carried as a string (matching the same `rust_decimal::serde::str`
/// shape as `UsageRecord.value`); `None` materializes as `null` per the
/// SDK contract for empty-set buckets.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct AggregationBucketDto {
    #[serde(default)]
    pub key: Vec<String>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub value: Option<Decimal>,
}

impl From<AggregationBucket> for AggregationBucketDto {
    fn from(value: AggregationBucket) -> Self {
        Self {
            key: value.key,
            value: value.value,
        }
    }
}

/// Aggregated-query response body. Mirrors
/// [`usage_collector_sdk::AggregationResult`] — buckets are emitted in
/// plugin-defined order and a no-grouping query yields a single bucket
/// with an empty `key`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct AggregationResultDto {
    pub buckets: Vec<AggregationBucketDto>,
}

impl From<AggregationResult> for AggregationResultDto {
    fn from(value: AggregationResult) -> Self {
        Self {
            buckets: value.buckets.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "dto_tests.rs"]
mod dto_tests;
