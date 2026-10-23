//! The snapshot route: the per-Quota state of explicit targets.
//!
//! One request shape serves a single target and many; a consuming product's
//! backend uses the same route to render an end-user view for the user and
//! tenant it names. The response is the per-Quota list only: no policy
//! attribution and no aggregate figure. Every failure is a `Problem`, never a
//! decision.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json, Router};
use quota_enforcement_sdk::{
    PageResult, PeriodWindow, QuotaSnapshot, SnapshotRequest, SnapshotSubject, TenantId,
};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit::api::rest::extract::Json as JsonBody;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{SubjectRefDto, ValidityWindowDto};
use super::error::ApiResult;
use crate::domain::Service;

/// One target: a subject scope, its id, and a metric.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct SnapshotSubjectDto {
    /// Scope kind: the full GTS id of a scope instance, for example
    /// `gts.cf.core.qe.scope.v1~cf.core.qe.user.v1`. The tenant scope
    /// `gts.cf.core.qe.scope.v1~cf.core.qe.tenant.v1` selects the tenant's own
    /// Quotas only, and its id must then be `tenant_id`.
    pub kind: String,
    /// Subject identifier.
    pub id: String,
    /// Metric instance id.
    pub metric: String,
}

/// Read the per-Quota state of `1..N` targets in one tenant.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRequestDto {
    /// Target tenant.
    pub tenant_id: Uuid,
    /// The targets; each is authorized by the PDP before anything is read.
    pub subjects: Vec<SnapshotSubjectDto>,
    /// Page size, at most the configured page size; that size when absent.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

impl From<SnapshotRequestDto> for SnapshotRequest {
    fn from(value: SnapshotRequestDto) -> Self {
        Self {
            tenant_id: TenantId::new(value.tenant_id),
            subjects: value
                .subjects
                .into_iter()
                .map(|subject| SnapshotSubject {
                    kind: subject.kind,
                    id: subject.id,
                    metric: subject.metric,
                })
                .collect(),
            limit: value.limit,
            cursor: value.cursor,
        }
    }
}

/// A consumption Quota's current period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct PeriodWindowDto {
    /// Start, inclusive, RFC 3339.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub start: OffsetDateTime,
    /// End, exclusive, RFC 3339.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub end: OffsetDateTime,
    /// When the counter next resets, RFC 3339.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub next_reset: OffsetDateTime,
}

impl From<PeriodWindow> for PeriodWindowDto {
    fn from(window: PeriodWindow) -> Self {
        Self {
            start: window.start,
            end: window.end,
            next_reset: window.next_reset,
        }
    }
}

/// One Quota's state at the time of the read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct QuotaSnapshotDto {
    /// The Quota.
    pub quota_id: Uuid,
    /// Bound subject.
    pub subject: SubjectRefDto,
    /// Metric instance id.
    pub metric: String,
    /// Quota type instance id.
    pub quota_type: String,
    /// Enforcement mode instance id.
    pub enforcement_mode: String,
    /// Cap; `null` for unbounded.
    pub cap: Option<u64>,
    /// Consumed in the current period, or in flight for an allocation Quota.
    pub consumed: u64,
    /// Remaining; `null` exactly when `cap` is.
    pub remaining: Option<u64>,
    /// The current period of a consumption Quota; `null` for allocation.
    pub period: Option<PeriodWindowDto>,
    /// The full metadata object.
    #[schema(value_type = Object)]
    pub metadata: BTreeMap<String, Value>,
    /// Validity bounds.
    pub validity_window: Option<ValidityWindowDto>,
    /// Server-computed: the read's time lies within the validity window.
    pub currently_within_window: bool,
}

impl From<QuotaSnapshot> for QuotaSnapshotDto {
    fn from(snapshot: QuotaSnapshot) -> Self {
        Self {
            quota_id: snapshot.quota_id.as_uuid(),
            subject: SubjectRefDto::from(snapshot.subject),
            metric: snapshot.metric.as_str().to_owned(),
            quota_type: snapshot.quota_type.as_gts_id().to_owned(),
            enforcement_mode: snapshot.enforcement_mode.as_gts_id().to_owned(),
            cap: snapshot.cap,
            consumed: snapshot.consumed,
            remaining: snapshot.remaining,
            period: snapshot.period.map(PeriodWindowDto::from),
            metadata: snapshot.metadata.into_iter().collect(),
            validity_window: snapshot.validity_window.map(ValidityWindowDto::from),
            currently_within_window: snapshot.currently_within_window,
        }
    }
}

/// One page of Quota state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct SnapshotPageDto {
    /// The items of this page, in `quota_id` order.
    pub items: Vec<QuotaSnapshotDto>,
    /// Cursor of the next page; `null` on the last page.
    pub next_cursor: Option<String>,
}

impl From<PageResult<QuotaSnapshot>> for SnapshotPageDto {
    fn from(page: PageResult<QuotaSnapshot>) -> Self {
        Self {
            items: page.items.into_iter().map(QuotaSnapshotDto::from).collect(),
            next_cursor: page.next_cursor,
        }
    }
}

async fn snapshot(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<SnapshotRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let page = service.operations()?.snapshot(&ctx, body.into()).await?;
    Ok(Json(SnapshotPageDto::from(page)))
}

/// Mount the snapshot endpoint.
// @cpt-dod:cpt-cf-quota-enforcement-dod-snapshot-endpoint:p1
pub fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let prefix = super::routes::PATH_PREFIX;
    OperationBuilder::post(format!("{prefix}/snapshot"))
        .operation_id("quota_enforcement.snapshot")
        .summary("Read the per-Quota state of explicit targets")
        .description(
            "Returns every active Quota the targets select, each once, ordered by quota_id and \
             cursor-paginated: a user target selects the user's and the tenant's Quotas, a \
             tenant target (id = tenant_id) the tenant's only. Quotas outside their validity \
             window are included with currently_within_window = false. The PDP authorizes \
             every target first; one refused target refuses the request. No policy attribution \
             and no aggregate figure are returned. A target matching nothing is an empty page.",
        )
        .tag("Snapshots")
        .authenticated()
        .no_license_required()
        .json_request::<SnapshotRequestDto>(openapi, "The targets to read")
        .handler(snapshot)
        .json_response_with_schema::<SnapshotPageDto>(openapi, StatusCode::OK, "One page")
        .standard_errors(openapi)
        .register(router, openapi)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "snapshot_tests.rs"]
mod tests;
